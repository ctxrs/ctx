use std::{
    fs,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use anyhow::{anyhow, Context, Result};
use ctx_history_capture::{
    inventory_provider_regular_paths, observe_ordinary_file, provider_regular_file_len,
    stable_capture_uuid, OrdinaryFileObservation, ProviderJsonlInventoryLimits,
};
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
            true,
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
                            true,
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
                true,
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use clap::ValueEnum;
    use ctx_history_capture::{
        CaptureError, ProviderJsonlInventoryLimit, ProviderJsonlInventoryLimits,
    };

    use super::{source_stats, source_stats_with_inventory_limits};
    use crate::{provider_args::NativeProviderArg, provider_sources::explicit_path_source};

    #[test]
    fn directory_stats_describe_the_bounded_format_neutral_corpus() {
        let temp = tempfile::tempdir().unwrap();
        let nested = temp.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        std::fs::write(temp.path().join("z.jsonl"), b"four").unwrap();
        std::fs::write(nested.join("a.jsonl"), b"tri").unwrap();
        let opaque = temp.path().join("opaque.bin");
        std::fs::write(&opaque, vec![b'x'; 64 * 1024]).unwrap();

        let first = source_stats(temp.path()).unwrap();
        let second = source_stats(temp.path()).unwrap();
        assert_eq!(first.files, 3);
        assert_eq!(first.bytes, 65_543);
        assert_eq!(first.change_token, second.change_token);

        std::fs::write(&opaque, vec![b'y'; 128 * 1024]).unwrap();
        let after_opaque_change = source_stats(temp.path()).unwrap();
        assert_eq!(after_opaque_change.files, 3);
        assert_eq!(after_opaque_change.bytes, 131_079);
        assert_ne!(after_opaque_change.change_token, first.change_token);

        std::fs::write(nested.join("a.jsonl"), b"changed").unwrap();
        let after_jsonl_change = source_stats(temp.path()).unwrap();
        assert_eq!(after_jsonl_change.files, 3);
        assert_eq!(after_jsonl_change.bytes, 131_083);
        assert_ne!(after_jsonl_change.change_token, first.change_token);
    }

    #[test]
    fn directory_stats_include_non_jsonl_provider_files_but_exclude_shm_from_token() {
        let temp = tempfile::tempdir().unwrap();
        let json = temp.path().join("session.json");
        let sqlite = temp.path().join("state.sqlite");
        let shm = temp.path().join("state.sqlite-shm");
        std::fs::write(&json, b"json").unwrap();
        std::fs::write(&sqlite, b"sqlite").unwrap();
        std::fs::write(&shm, b"shm").unwrap();

        let first = source_stats(temp.path()).unwrap();
        assert_eq!(first.files, 3);
        assert_eq!(first.bytes, 13);

        std::fs::write(&shm, b"changed-shm").unwrap();
        let after_shm_change = source_stats(temp.path()).unwrap();
        assert_eq!(after_shm_change.files, 3);
        assert_eq!(after_shm_change.bytes, 21);
        assert_eq!(after_shm_change.change_token, first.change_token);

        std::fs::write(&json, b"changed-json").unwrap();
        let after_json_change = source_stats(temp.path()).unwrap();
        assert_ne!(after_json_change.change_token, first.change_token);
    }

    #[test]
    fn all_41_provider_source_formats_use_format_neutral_directory_stats() {
        const MATRIX_FILES: [&str; 6] = [
            "session.jsonl",
            "session.json",
            "state.db",
            "state.sqlite",
            "state.vscdb",
            "state.sqlite-shm",
        ];

        let temp = tempfile::tempdir().unwrap();
        let variants = NativeProviderArg::value_variants();
        assert_eq!(variants.len(), 41, "semantic provider count changed");
        let mut provider_formats = BTreeSet::new();
        let mut source_formats = BTreeSet::new();

        for variant in variants {
            let provider = variant.capture_provider();
            let root = temp.path().join(provider.as_str());
            std::fs::create_dir(&root).unwrap();
            for file in MATRIX_FILES {
                std::fs::write(root.join(file), b"x").unwrap();
            }

            let source = explicit_path_source(provider, root.clone());
            assert!(source.import_support.is_importable());
            assert_ne!(source.source_format, "unsupported");
            assert!(
                provider_formats.insert((provider.as_str(), source.source_format)),
                "duplicate provider/source-format pair for {}",
                provider.as_str()
            );
            assert!(
                source_formats.insert(source.source_format),
                "source format {} is shared by multiple semantic providers",
                source.source_format
            );

            let stats = source_stats(&root).unwrap_or_else(|error| {
                panic!(
                    "{} ({}) source accounting failed: {error:#}",
                    provider.as_str(),
                    source.source_format
                )
            });
            assert_eq!(stats.files, MATRIX_FILES.len(), "{}", provider.as_str());
            assert_eq!(
                stats.bytes,
                u64::try_from(MATRIX_FILES.len()).unwrap(),
                "{}",
                provider.as_str()
            );
            assert!(stats.change_token.is_some(), "{}", provider.as_str());
        }

        assert_eq!(provider_formats.len(), 41);
        assert_eq!(source_formats.len(), 41);
        assert!(source_formats.iter().any(|format| format.contains("jsonl")));
        assert!(source_formats
            .iter()
            .any(|format| format.ends_with("_json")));
        assert!(source_formats
            .iter()
            .any(|format| format.contains("sqlite")));
        assert!(source_formats.contains("trae_state_vscdb"));
        assert!(source_formats.contains("nanoclaw_project"));
    }

    #[test]
    fn sqlite_file_stats_preserve_sidecar_totals_and_revision_authority() {
        let temp = tempfile::tempdir().unwrap();
        let sqlite = temp.path().join("state.sqlite");
        let wal = temp.path().join("state.sqlite-wal");
        let journal = temp.path().join("state.sqlite-journal");
        let shm = temp.path().join("state.sqlite-shm");
        std::fs::write(&sqlite, b"sqlite").unwrap();
        std::fs::write(&wal, b"wal").unwrap();
        std::fs::write(&journal, b"journal").unwrap();
        std::fs::write(&shm, b"shm").unwrap();

        let first = source_stats(&sqlite).unwrap();
        assert_eq!(first.files, 4);
        assert_eq!(first.bytes, 19);

        std::fs::write(&shm, b"changed-shm").unwrap();
        let after_shm_change = source_stats(&sqlite).unwrap();
        assert_eq!(after_shm_change.files, 4);
        assert_eq!(after_shm_change.bytes, 27);
        assert_eq!(after_shm_change.change_token, first.change_token);

        std::fs::write(&wal, b"changed-wal").unwrap();
        let after_wal_change = source_stats(&sqlite).unwrap();
        assert_eq!(after_wal_change.files, 4);
        assert_eq!(after_wal_change.bytes, 35);
        assert_ne!(after_wal_change.change_token, first.change_token);

        std::fs::write(&journal, b"changed-journal").unwrap();
        let after_journal_change = source_stats(&sqlite).unwrap();
        assert_eq!(after_journal_change.files, 4);
        assert_eq!(after_journal_change.bytes, 43);
        assert_ne!(
            after_journal_change.change_token,
            after_wal_change.change_token
        );
    }

    #[test]
    fn directory_stats_preserve_typed_inventory_limit_failures() {
        let temp = tempfile::tempdir().unwrap();
        for index in 0..3 {
            std::fs::write(temp.path().join(format!("{index}.txt")), b"x").unwrap();
        }

        let error = source_stats_with_inventory_limits(
            temp.path(),
            ProviderJsonlInventoryLimits {
                max_metadata_entries: 3,
                ..ProviderJsonlInventoryLimits::default()
            },
        )
        .unwrap_err();

        assert!(matches!(
            error.downcast_ref::<CaptureError>(),
            Some(CaptureError::ProviderJsonlInventoryLimitExceeded {
                limit: ProviderJsonlInventoryLimit::MetadataEntries,
                maximum: 3,
                observed: 4,
            })
        ));
    }
}
