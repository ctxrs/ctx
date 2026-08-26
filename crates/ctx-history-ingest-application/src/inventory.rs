use std::{
    fs,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use anyhow::{anyhow, Context, Result};
use ctx_history_refresh::explicit_source_path_symlink_metadata;
use ctx_history_source_io::{
    inventory_provider_regular_paths, observe_ordinary_file, provider_regular_file_len,
    OrdinaryFileObservation, ProviderJsonlInventoryLimits,
};
use sha2::{Digest, Sha256};

/// Bounded, provider-neutral source observation facts. The token is an
/// observation aid only; refresh remains the sole authority for publication.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SourceStats {
    pub files: usize,
    pub bytes: u64,
    pub change_token: Option<[u8; 32]>,
}

pub fn source_stats(path: &Path) -> Result<SourceStats> {
    source_stats_with_inventory_limits(path, ProviderJsonlInventoryLimits::default())
}

fn source_stats_with_inventory_limits(
    path: &Path,
    limits: ProviderJsonlInventoryLimits,
) -> Result<SourceStats> {
    let metadata = explicit_source_path_symlink_metadata(path)
        .with_context(|| format!("stat import source {}", path.display()))?;
    let mut stats = SourceStats::default();
    let mut entries = Vec::new();
    if metadata.file_type().is_file() {
        let observation = observe_ordinary_file(path)
            .with_context(|| format!("observe import source {}", path.display()))?;
        add_observation(
            &mut stats,
            &mut entries,
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
                    if contributes_to_revision(&sidecar) {
                        let observation = observe_ordinary_file(&sidecar).with_context(|| {
                            format!("observe import source sidecar {}", sidecar.display())
                        })?;
                        add_observation(
                            &mut stats,
                            &mut entries,
                            path.parent().unwrap_or(path),
                            &sidecar,
                            &observation,
                        );
                    } else {
                        stats.files += 1;
                        stats.bytes = stats.bytes.saturating_add(
                            provider_regular_file_len(&sidecar).with_context(|| {
                                format!("stat import source sidecar {}", sidecar.display())
                            })?,
                        );
                    }
                }
                Ok(_) => {
                    return Err(anyhow!(
                        "import source sidecar is not a regular file: {}",
                        sidecar.display()
                    ))
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("stat import source sidecar {}", sidecar.display())
                    })
                }
            }
        }
        stats.change_token = Some(source_change_token(entries));
        return Ok(stats);
    }
    if !metadata.file_type().is_dir() {
        return Ok(stats);
    }
    let inventory = inventory_provider_regular_paths(path, limits)
        .with_context(|| format!("inventory import source directory {}", path.display()))?;
    for entry in inventory.into_paths() {
        if contributes_to_revision(&entry) {
            let observation = observe_ordinary_file(&entry)
                .with_context(|| format!("observe import source file {}", entry.display()))?;
            add_observation(&mut stats, &mut entries, path, &entry, &observation);
        } else {
            stats.files += 1;
            stats.bytes = stats.bytes.saturating_add(
                provider_regular_file_len(&entry)
                    .with_context(|| format!("stat import source file {}", entry.display()))?,
            );
        }
    }
    stats.change_token = Some(source_change_token(entries));
    Ok(stats)
}

fn contributes_to_revision(path: &Path) -> bool {
    !path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with("-shm"))
}

struct ChangeEntry {
    path: PathBuf,
    len: u64,
    modified_secs: u64,
    modified_nanos: u32,
    observation_token: [u8; 32],
}

fn add_observation(
    stats: &mut SourceStats,
    entries: &mut Vec<ChangeEntry>,
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
    entries.push(ChangeEntry {
        path: path.strip_prefix(base).unwrap_or(path).to_path_buf(),
        len: observation.len(),
        modified_secs: modified.as_secs(),
        modified_nanos: modified.subsec_nanos(),
        observation_token: *observation.token(),
    });
}

fn source_change_token(mut entries: Vec<ChangeEntry>) -> [u8; 32] {
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
