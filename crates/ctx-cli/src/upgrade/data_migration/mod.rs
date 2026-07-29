//! Non-destructive v0.26 history-epoch activation.
//!
//! v0.26 never migrates history from an earlier ctx Store. This module only
//! records whether such a Store existed when the new epoch was initialized,
//! leaves that file untouched for rollback or manual recovery, and tracks the
//! fresh provider-source rebuild through atomic activation.

use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
};

use anyhow::{anyhow, bail, Context, Result};
use ctx_history_core::{
    database_path,
    platform_security::{create_private_directory_all, restrict_private_file},
};
use ctx_history_index::VerifiedIndex;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const EPOCH_SCHEMA_VERSION: u32 = 1;
const EPOCH_DIRECTORY: &str = "epochs/v26";
const EPOCH_JOURNAL: &str = "activation.jsonl";
const EPOCH_LOCK: &str = "activation.lock";
const SOURCE_BACKED_INDEX_DIRECTORY: &str = "source-backed-lexical-v0";
const MAX_JOURNAL_BYTES: u64 = 1024 * 1024;
const MAX_ERROR_CHARS: usize = 4_096;

/// Kept as a compatibility input while setup/import call sites converge.
/// Provider sources never affect old-Store classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AvailableProviderSource;

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MigrationOrigin {
    Fresh,
    PreviousHistoryStore,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MigrationPhase {
    Detected,
    RebuildPending,
    SourceRebuildFailed,
    Ready,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct MigrationMarker {
    pub(crate) schema_version: u32,
    pub(crate) migration_id: String,
    pub(crate) origin: MigrationOrigin,
    pub(crate) phase: MigrationPhase,
    pub(crate) source_rebuild_required: bool,
    pub(crate) lexical_projection_path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) lexical_generation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) legacy_store_path: Option<PathBuf>,
    pub(crate) resumable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
}

impl MigrationMarker {
    fn detected(data_root: &Path, origin: MigrationOrigin) -> Self {
        Self {
            schema_version: EPOCH_SCHEMA_VERSION,
            migration_id: format!("epoch26_{}", Uuid::now_v7()),
            origin,
            phase: MigrationPhase::Detected,
            source_rebuild_required: true,
            lexical_projection_path: lexical_projection_path(data_root),
            lexical_generation_id: None,
            legacy_store_path: (origin == MigrationOrigin::PreviousHistoryStore)
                .then(|| database_path(data_root.to_path_buf())),
            resumable: false,
            error: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MigrationDecision {
    RebuildFromSources(MigrationMarker),
    Ready(MigrationMarker),
}

impl MigrationDecision {
    pub(crate) fn marker(&self) -> &MigrationMarker {
        match self {
            Self::RebuildFromSources(marker) | Self::Ready(marker) => marker,
        }
    }

    pub(crate) fn daemon_rebuild_required(&self) -> bool {
        self.marker().source_rebuild_required
    }
}

/// Read-only epoch inspection. It never creates the data root or opens the
/// previous Store.
pub(crate) fn inspect(data_root: &Path) -> Result<Option<MigrationMarker>> {
    if let Some(marker) = read_last_marker(data_root)? {
        return validate_marker(data_root, marker).map(Some);
    }
    Ok(Some(MigrationMarker::detected(
        data_root,
        detect_origin(data_root)?,
    )))
}

/// Initializes or resumes a fresh v0.26 provider-source rebuild.
///
/// The compatibility argument is deliberately ignored: old history and the
/// available provider-source set are never compared or merged here.
pub(crate) fn prepare(
    data_root: &Path,
    _available_sources: &[AvailableProviderSource],
) -> Result<MigrationDecision> {
    let observed_origin = detect_origin(data_root)?;
    create_private_directory_all(data_root)
        .with_context(|| format!("create private ctx data root {}", data_root.display()))?;
    let _lock = EpochLock::acquire(data_root)?;
    let mut marker = match read_last_marker(data_root)? {
        Some(marker) => validate_marker(data_root, marker)?,
        None => {
            let marker = MigrationMarker::detected(data_root, observed_origin);
            append_marker(data_root, &marker)?;
            marker
        }
    };

    if marker.phase == MigrationPhase::Ready {
        if marker.source_rebuild_required {
            bail!("ready v0.26 epoch still requires a source rebuild");
        }
        return Ok(MigrationDecision::Ready(marker));
    }

    marker.phase = MigrationPhase::RebuildPending;
    marker.source_rebuild_required = true;
    marker.resumable = true;
    marker.error = None;
    append_marker(data_root, &marker)?;
    Ok(MigrationDecision::RebuildFromSources(marker))
}

/// Atomically activates only the exact verified v0.26 lexical generation.
pub(crate) fn complete_source_rebuild(
    data_root: &Path,
    generation_id: &str,
) -> Result<MigrationDecision> {
    let _lock = EpochLock::acquire(data_root)?;
    let marker = read_last_marker(data_root)?
        .ok_or_else(|| anyhow!("v0.26 history epoch has not been initialized"))?;
    let mut marker = validate_marker(data_root, marker)?;
    if marker.phase == MigrationPhase::Ready {
        if marker.lexical_generation_id.as_deref() == Some(generation_id)
            && !marker.source_rebuild_required
        {
            return Ok(MigrationDecision::Ready(marker));
        }
        bail!("v0.26 history epoch is already active with a different generation");
    }
    if !matches!(
        marker.phase,
        MigrationPhase::RebuildPending | MigrationPhase::SourceRebuildFailed
    ) {
        bail!(
            "cannot activate v0.26 history while the epoch is in {:?}",
            marker.phase
        );
    }
    let index = VerifiedIndex::open(&marker.lexical_projection_path).with_context(|| {
        format!(
            "verify rebuilt v0.26 lexical projection {}",
            marker.lexical_projection_path.display()
        )
    })?;
    if index.generation_id() != generation_id {
        bail!(
            "rebuilt lexical generation mismatch: published {}, reported {generation_id}",
            index.generation_id()
        );
    }
    marker.phase = MigrationPhase::Ready;
    marker.source_rebuild_required = false;
    marker.lexical_generation_id = Some(generation_id.to_owned());
    marker.resumable = false;
    marker.error = None;
    append_marker(data_root, &marker)?;
    Ok(MigrationDecision::Ready(marker))
}

pub(crate) fn record_source_rebuild_failure(
    data_root: &Path,
    error: &str,
) -> Result<MigrationMarker> {
    let _lock = EpochLock::acquire(data_root)?;
    let marker = read_last_marker(data_root)?
        .ok_or_else(|| anyhow!("v0.26 history epoch has not been initialized"))?;
    let mut marker = validate_marker(data_root, marker)?;
    if !matches!(
        marker.phase,
        MigrationPhase::RebuildPending | MigrationPhase::SourceRebuildFailed
    ) {
        bail!(
            "cannot record a v0.26 rebuild failure while the epoch is in {:?}",
            marker.phase
        );
    }
    marker.phase = MigrationPhase::SourceRebuildFailed;
    marker.source_rebuild_required = true;
    marker.resumable = true;
    marker.error = Some(error.chars().take(MAX_ERROR_CHARS).collect());
    append_marker(data_root, &marker)?;
    Ok(marker)
}

pub(crate) fn migration_directory(data_root: &Path) -> PathBuf {
    data_root.join(EPOCH_DIRECTORY)
}

pub(crate) fn lexical_projection_path(data_root: &Path) -> PathBuf {
    data_root.join(SOURCE_BACKED_INDEX_DIRECTORY)
}

fn detect_origin(data_root: &Path) -> Result<MigrationOrigin> {
    let store_path = database_path(data_root.to_path_buf());
    let metadata = match fs::symlink_metadata(&store_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(MigrationOrigin::Fresh);
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "inspect previous ctx history Store {}",
                    store_path.display()
                )
            });
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!(
            "previous ctx history Store path {} is not a regular file",
            store_path.display()
        );
    }
    Ok(MigrationOrigin::PreviousHistoryStore)
}

fn validate_marker(data_root: &Path, marker: MigrationMarker) -> Result<MigrationMarker> {
    if marker.schema_version != EPOCH_SCHEMA_VERSION {
        bail!(
            "unsupported v0.26 history epoch marker schema {}",
            marker.schema_version
        );
    }
    if marker.lexical_projection_path != lexical_projection_path(data_root) {
        bail!("v0.26 history epoch projection path does not match this data root");
    }
    let expected_store = (marker.origin == MigrationOrigin::PreviousHistoryStore)
        .then(|| database_path(data_root.to_path_buf()));
    if marker.legacy_store_path != expected_store {
        bail!("v0.26 history epoch previous-Store path does not match this data root");
    }
    Ok(marker)
}

fn journal_path(data_root: &Path) -> PathBuf {
    migration_directory(data_root).join(EPOCH_JOURNAL)
}

fn read_last_marker(data_root: &Path) -> Result<Option<MigrationMarker>> {
    let path = journal_path(data_root);
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    if file.metadata()?.len() > MAX_JOURNAL_BYTES {
        bail!(
            "v0.26 history epoch journal {} exceeds its {}-byte bound",
            path.display(),
            MAX_JOURNAL_BYTES
        );
    }
    let mut last = None;
    for line in BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        last = Some(
            serde_json::from_str::<MigrationMarker>(&line)
                .with_context(|| format!("decode v0.26 history epoch marker {}", path.display()))?,
        );
    }
    Ok(last)
}

fn append_marker(data_root: &Path, marker: &MigrationMarker) -> Result<()> {
    let directory = migration_directory(data_root);
    create_private_directory_all(&directory)
        .with_context(|| format!("create epoch directory {}", directory.display()))?;
    let path = journal_path(data_root);
    let existing = fs::metadata(&path).map(|value| value.len()).unwrap_or(0);
    let mut encoded = serde_json::to_vec(marker)?;
    encoded.push(b'\n');
    if existing.saturating_add(encoded.len() as u64) > MAX_JOURNAL_BYTES {
        bail!(
            "v0.26 history epoch journal {} would exceed its {}-byte bound",
            path.display(),
            MAX_JOURNAL_BYTES
        );
    }
    let mut file = OpenOptions::new()
        .append(true)
        .create(true)
        .open(&path)
        .with_context(|| format!("append epoch journal {}", path.display()))?;
    restrict_private_file(&path)?;
    file.write_all(&encoded)?;
    file.sync_all()?;
    sync_directory(&directory)
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .with_context(|| format!("sync epoch directory {}", path.display()))
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}

struct EpochLock {
    file: File,
}

impl EpochLock {
    fn acquire(data_root: &Path) -> Result<Self> {
        let directory = migration_directory(data_root);
        create_private_directory_all(&directory)
            .with_context(|| format!("create epoch directory {}", directory.display()))?;
        let path = directory.join(EPOCH_LOCK);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("open epoch activation lock {}", path.display()))?;
        restrict_private_file(&path)?;
        file.try_lock_exclusive()
            .map_err(|_| anyhow!("another v0.26 history activation owns this data root"))?;
        Ok(Self { file })
    }
}

impl Drop for EpochLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
