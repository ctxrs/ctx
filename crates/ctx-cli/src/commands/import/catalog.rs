use std::{
    fs,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use anyhow::{anyhow, Context, Result};
use ctx_history_capture::{observe_ordinary_file, stable_capture_uuid, OrdinaryFileObservation};
use ctx_history_core::HistoryRecord;
use sha2::{Digest, Sha256};

use crate::{
    commands::import::SourceStats, history_source_plugins::HistorySourcePluginSource,
    provider_args::ImportFormatArg, provider_sources::SourceInfo,
};

pub(crate) fn source_uses_incremental_event_search(source: &SourceInfo) -> bool {
    source.import_support.is_importable()
}

pub(crate) fn source_stats(path: &Path) -> Result<SourceStats> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("stat import source {}", path.display()))?;
    let mut stats = SourceStats::default();
    let mut change_entries = Vec::new();
    if metadata.file_type().is_file() {
        let observation = observe_ordinary_file(path)
            .with_context(|| format!("observe import source {}", path.display()))?;
        add_source_observation(
            &mut stats,
            &mut change_entries,
            path.parent().unwrap_or(path),
            path,
            &observation,
            true,
        );
        for suffix in ["-wal", "-journal"] {
            let mut sidecar = path.as_os_str().to_os_string();
            sidecar.push(suffix);
            let sidecar = PathBuf::from(sidecar);
            match fs::symlink_metadata(&sidecar) {
                Ok(metadata) if metadata.file_type().is_file() => {
                    let observation = observe_ordinary_file(&sidecar).with_context(|| {
                        format!("observe import source sidecar {}", sidecar.display())
                    })?;
                    add_source_observation(
                        &mut stats,
                        &mut change_entries,
                        path.parent().unwrap_or(path),
                        &sidecar,
                        &observation,
                        false,
                    );
                }
                Ok(_) => {
                    return Err(anyhow!(
                        "import source sidecar is not a regular file: {}",
                        sidecar.display()
                    ));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("stat import source sidecar {}", sidecar.display())
                    });
                }
            }
        }
        stats.change_token = Some(source_change_token(change_entries));
        return Ok(stats);
    }
    if !metadata.file_type().is_dir() {
        return Ok(SourceStats::default());
    }

    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)
            .with_context(|| format!("read import source directory {}", dir.display()))?
        {
            let entry = entry
                .with_context(|| format!("read import source entry under {}", dir.display()))?;
            let entry_path = entry.path();
            let file_type = entry
                .file_type()
                .with_context(|| format!("stat import source entry {}", entry_path.display()))?;
            if file_type.is_dir() {
                stack.push(entry_path);
            } else if file_type.is_file() {
                let metadata = entry
                    .metadata()
                    .with_context(|| format!("stat import source file {}", entry_path.display()))?;
                let include_in_token = !entry_path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with("-shm"));
                if include_in_token {
                    let observation = observe_ordinary_file(&entry_path).with_context(|| {
                        format!("observe import source file {}", entry_path.display())
                    })?;
                    add_source_observation(
                        &mut stats,
                        &mut change_entries,
                        path,
                        &entry_path,
                        &observation,
                        true,
                    );
                } else {
                    stats.files += 1;
                    stats.bytes = stats.bytes.saturating_add(metadata.len());
                }
            }
        }
    }
    stats.change_token = Some(source_change_token(change_entries));
    Ok(stats)
}

struct SourceChangeEntry {
    path: PathBuf,
    len: u64,
    modified_secs: u64,
    modified_nanos: u32,
    observation_token: [u8; 32],
}

fn add_source_observation(
    stats: &mut SourceStats,
    change_entries: &mut Vec<SourceChangeEntry>,
    base: &Path,
    path: &Path,
    observation: &OrdinaryFileObservation,
    include_in_totals: bool,
) {
    if include_in_totals {
        stats.files += 1;
        stats.bytes = stats.bytes.saturating_add(observation.len());
    }
    let modified = observation
        .modified_at()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    change_entries.push(SourceChangeEntry {
        path: path.strip_prefix(base).unwrap_or(path).to_path_buf(),
        len: observation.len(),
        modified_secs: modified.as_secs(),
        modified_nanos: modified.subsec_nanos(),
        observation_token: *observation.token(),
    });
}

fn source_change_token(mut entries: Vec<SourceChangeEntry>) -> [u8; 32] {
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    let mut hasher = Sha256::new();
    for entry in entries {
        let path = entry.path.as_os_str().as_encoded_bytes();
        hasher.update((path.len() as u64).to_le_bytes());
        hasher.update(path);
        hasher.update(entry.len.to_le_bytes());
        hasher.update(entry.modified_secs.to_le_bytes());
        hasher.update(entry.modified_nanos.to_le_bytes());
        hasher.update(entry.observation_token);
    }
    hasher.finalize().into()
}

pub(crate) fn import_record_for_source(source: &SourceInfo) -> HistoryRecord {
    let key = format!(
        "agent-history:{}:{}",
        source.provider.as_str(),
        source.path.display()
    );
    let mut record = HistoryRecord::new(
        format!("{} agent history", source.provider.as_str()),
        format!(
            "Indexed local agent history from {} ({})",
            source.path.display(),
            source.source_format
        ),
        vec!["agent-history".into(), source.provider.as_str().into()],
        "agent_history",
        source.path.parent().map(|path| path.display().to_string()),
    );
    record.id = stable_capture_uuid(&key, "record");
    record
}

pub(crate) fn import_record_for_custom_history(
    path: &Path,
    format: ImportFormatArg,
) -> HistoryRecord {
    let key = format!("custom-history:{}:{}", format.as_str(), path.display());
    let mut record = HistoryRecord::new(
        "custom agent history".to_owned(),
        format!(
            "Indexed custom agent history from {} ({})",
            path.display(),
            format.as_str()
        ),
        vec![
            "agent-history".into(),
            "custom".into(),
            format.as_str().into(),
        ],
        "agent_history",
        path.parent().map(|path| path.display().to_string()),
    );
    record.id = stable_capture_uuid(&key, "record");
    record
}

pub(crate) fn import_record_for_history_source_plugin(
    source: &HistorySourcePluginSource,
) -> HistoryRecord {
    let key = format!(
        "history-source-plugin:{}:{}:{}:{}:{}",
        source.plugin_name, source.id, source.provider_key, source.source_id, source.source_format
    );
    let mut record = HistoryRecord::new(
        format!("history source plugin {}", source.label()),
        format!(
            "Indexed custom agent history from history source plugin {} ({})",
            source.label(),
            source.source_format
        ),
        vec![
            "agent-history".into(),
            "custom".into(),
            "history-source-plugin".into(),
            source.provider_key.clone(),
            source.source_format.clone(),
        ],
        "agent_history",
        source
            .manifest_path
            .parent()
            .map(|path| path.display().to_string()),
    );
    record.id = stable_capture_uuid(&key, "record");
    record
}
