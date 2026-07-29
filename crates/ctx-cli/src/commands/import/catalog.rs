use std::{
    fs,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use anyhow::{anyhow, Context, Result};
use ctx_history_capture::{
    inventory_provider_regular_paths, observe_ordinary_file, provider_regular_file_len,
    OrdinaryFileObservation, ProviderJsonlInventoryLimits,
};
use sha2::{Digest, Sha256};

use crate::commands::import::SourceStats;

pub(crate) fn source_stats(path: &Path) -> Result<SourceStats> {
    source_stats_with_inventory_limits(path, ProviderJsonlInventoryLimits::default())
}

fn source_stats_with_inventory_limits(
    path: &Path,
    inventory_limits: ProviderJsonlInventoryLimits,
) -> Result<SourceStats> {
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
        );
        for suffix in ["-wal", "-journal", "-shm"] {
            let mut sidecar = path.as_os_str().to_os_string();
            sidecar.push(suffix);
            let sidecar = PathBuf::from(sidecar);
            match fs::symlink_metadata(&sidecar) {
                Ok(metadata) if metadata.file_type().is_file() => {
                    if source_file_contributes_to_revision(&sidecar) {
                        let observation = observe_ordinary_file(&sidecar).with_context(|| {
                            format!("observe import source sidecar {}", sidecar.display())
                        })?;
                        add_source_observation(
                            &mut stats,
                            &mut change_entries,
                            path.parent().unwrap_or(path),
                            &sidecar,
                            &observation,
                        );
                    } else {
                        let len = provider_regular_file_len(&sidecar).with_context(|| {
                            format!("stat import source sidecar {}", sidecar.display())
                        })?;
                        stats.files += 1;
                        stats.bytes = stats.bytes.saturating_add(len);
                    }
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

    let inventory = inventory_provider_regular_paths(path, inventory_limits)
        .with_context(|| format!("inventory import source directory {}", path.display()))?;
    for entry_path in inventory.into_paths() {
        if source_file_contributes_to_revision(&entry_path) {
            let observation = observe_ordinary_file(&entry_path)
                .with_context(|| format!("observe import source file {}", entry_path.display()))?;
            add_source_observation(
                &mut stats,
                &mut change_entries,
                path,
                &entry_path,
                &observation,
            );
        } else {
            let len = provider_regular_file_len(&entry_path)
                .with_context(|| format!("stat import source file {}", entry_path.display()))?;
            stats.files += 1;
            stats.bytes = stats.bytes.saturating_add(len);
        }
    }
    stats.change_token = Some(source_change_token(change_entries));
    Ok(stats)
}

fn source_file_contributes_to_revision(path: &Path) -> bool {
    !path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with("-shm"))
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
) {
    stats.files += 1;
    stats.bytes = stats.bytes.saturating_add(observation.len());
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
