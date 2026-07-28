//! Bounded transition from the released canonical Store to source-backed
//! projections.
//!
//! This module deliberately sits above `ctx_history_store::Store::open`.
//! Released schema-46 stores must be classified before a writable Store open
//! can migrate them in place. Fresh and current roots receive an empty,
//! disposable lexical generation for the daemon to rebuild from provider
//! sources. A released Store is retained as an immutable compatibility source;
//! only rows whose exact provider source is no longer present may be copied
//! into the bounded legacy preview projection.

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
use ctx_history_index::{GenerationWriter, VerifiedIndex, WriterOptions};
use fs2::FileExt;
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

mod legacy;

const MIGRATION_SCHEMA_VERSION: u32 = 1;
const RELEASED_STORE_SCHEMA_VERSION: i64 = 46;
const CURRENT_STORE_SCHEMA_VERSION: i64 = 47;
const MIGRATION_DIRECTORY: &str = "migrations/source-backed-v1";
const MIGRATION_JOURNAL: &str = "state.jsonl";
const MIGRATION_LOCK: &str = "migration.lock";
const SOURCE_BACKED_INDEX_DIRECTORY: &str = "source-backed-lexical-v0";
const MAX_JOURNAL_BYTES: u64 = 4 * 1024 * 1024;
const MAX_ERROR_CHARS: usize = 4_096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AvailableProviderSource {
    pub(crate) provider: String,
    pub(crate) source_format: String,
    pub(crate) path: PathBuf,
}

impl AvailableProviderSource {
    pub(crate) fn new(
        provider: impl Into<String>,
        source_format: impl Into<String>,
        path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            provider: provider.into(),
            source_format: source_format.into(),
            path: path.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MigrationOrigin {
    Fresh,
    CurrentV47,
    ReleasedV46,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MigrationPhase {
    Detected,
    RebuildPending,
    LegacyProjectionBuilding,
    LegacyProjectionFailed,
    SourceRebuildFailed,
    Ready,
    RolledBack,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LegacyProjectionSummary {
    pub(crate) path: PathBuf,
    pub(crate) examined_events: u64,
    pub(crate) source_backed_events: u64,
    pub(crate) legacy_only_events: u64,
    pub(crate) last_event_seq: i64,
    pub(crate) chain_sha256: String,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) legacy_projection: Option<LegacyProjectionSummary>,
    #[serde(default)]
    pub(crate) resumable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
}

impl MigrationMarker {
    fn detected(data_root: &Path, origin: MigrationOrigin) -> Self {
        Self {
            schema_version: MIGRATION_SCHEMA_VERSION,
            migration_id: format!("dm_{}", Uuid::now_v7()),
            origin,
            phase: MigrationPhase::Detected,
            source_rebuild_required: true,
            lexical_projection_path: lexical_projection_path(data_root),
            lexical_generation_id: None,
            legacy_store_path: (origin == MigrationOrigin::ReleasedV46)
                .then(|| database_path(data_root.to_path_buf())),
            legacy_projection: None,
            resumable: false,
            error: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MigrationDecision {
    RebuildFromSources(MigrationMarker),
    RebuildFromSourcesWithLegacyException(MigrationMarker),
    Ready(MigrationMarker),
}

impl MigrationDecision {
    #[allow(dead_code)] // Integration hook for setup/import and the daemon lane.
    pub(crate) fn marker(&self) -> &MigrationMarker {
        match self {
            Self::RebuildFromSources(marker)
            | Self::RebuildFromSourcesWithLegacyException(marker)
            | Self::Ready(marker) => marker,
        }
    }

    #[allow(dead_code)] // Integration hook for setup/import and the daemon lane.
    pub(crate) fn daemon_rebuild_required(&self) -> bool {
        self.marker().source_rebuild_required
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetectedStore {
    Fresh,
    CurrentV47,
    ReleasedV46,
}

impl DetectedStore {
    fn origin(self) -> MigrationOrigin {
        match self {
            Self::Fresh => MigrationOrigin::Fresh,
            Self::CurrentV47 => MigrationOrigin::CurrentV47,
            Self::ReleasedV46 => MigrationOrigin::ReleasedV46,
        }
    }
}

/// Read-only classification. This never creates a root, opens a writable
/// Store, initializes a projection, or appends to the migration journal.
pub(crate) fn inspect(data_root: &Path) -> Result<Option<MigrationMarker>> {
    let detected = detect_store(data_root)?;
    if let Some(marker) = read_last_marker(data_root)? {
        return validate_marker(data_root, detected, marker).map(Some);
    }
    Ok(Some(MigrationMarker::detected(
        data_root,
        detected.origin(),
    )))
}

/// Initializes or resumes the source-backed migration state machine.
///
/// The legacy reader is intentionally bounded and query-only. Provider source
/// parsing and the daemon rebuild worker are separate integration points.
pub(crate) fn prepare(
    data_root: &Path,
    available_sources: &[AvailableProviderSource],
) -> Result<MigrationDecision> {
    prepare_with_chunk_limit(data_root, available_sources, None)
}

fn prepare_with_chunk_limit(
    data_root: &Path,
    available_sources: &[AvailableProviderSource],
    chunk_limit: Option<usize>,
) -> Result<MigrationDecision> {
    let observed = detect_store(data_root)?;
    create_private_directory_all(data_root)
        .with_context(|| format!("create private ctx data root {}", data_root.display()))?;
    let _lock = MigrationLock::acquire(data_root)?;
    let detected = detect_store(data_root)?;
    if detected != observed {
        bail!("ctx Store classification changed while acquiring the data migration lock");
    }
    let mut marker = compatible_marker(data_root, detected)?;

    if marker.phase == MigrationPhase::Ready {
        if marker.source_rebuild_required {
            bail!("ready source-backed migration marker still requires a source rebuild");
        }
        return Ok(MigrationDecision::Ready(marker));
    }

    let generation = ensure_empty_lexical_generation(&marker.lexical_projection_path)?;
    let generation_changed = marker.lexical_generation_id.as_deref() != Some(&generation);
    marker.lexical_generation_id = Some(generation);
    marker.error = None;

    if marker.phase == MigrationPhase::RebuildPending {
        if generation_changed {
            append_marker(data_root, &marker)?;
        }
        return Ok(pending_decision(marker));
    }
    if marker.phase == MigrationPhase::SourceRebuildFailed {
        marker.phase = MigrationPhase::RebuildPending;
        marker.resumable = true;
        append_marker(data_root, &marker)?;
        return Ok(pending_decision(marker));
    }

    match detected {
        DetectedStore::Fresh | DetectedStore::CurrentV47 => {
            if !matches!(
                marker.phase,
                MigrationPhase::Detected | MigrationPhase::RolledBack
            ) {
                bail!(
                    "unexpected {:?} migration phase for a source-only rebuild",
                    marker.phase
                );
            }
            marker.phase = MigrationPhase::RebuildPending;
            marker.resumable = true;
            append_marker(data_root, &marker)?;
            Ok(MigrationDecision::RebuildFromSources(marker))
        }
        DetectedStore::ReleasedV46 => {
            if !matches!(
                marker.phase,
                MigrationPhase::Detected
                    | MigrationPhase::LegacyProjectionBuilding
                    | MigrationPhase::LegacyProjectionFailed
                    | MigrationPhase::RolledBack
            ) {
                bail!(
                    "unexpected {:?} migration phase for a released Store rebuild",
                    marker.phase
                );
            }
            marker.phase = MigrationPhase::LegacyProjectionBuilding;
            marker.resumable = true;
            append_marker(data_root, &marker)?;
            match legacy::build_or_resume(data_root, &marker, available_sources, chunk_limit) {
                Ok(Some(summary)) => {
                    marker.phase = MigrationPhase::RebuildPending;
                    marker.legacy_projection = Some(summary);
                    marker.resumable = true;
                    append_marker(data_root, &marker)?;
                    Ok(MigrationDecision::RebuildFromSourcesWithLegacyException(
                        marker,
                    ))
                }
                Ok(None) if chunk_limit.is_some() => {
                    marker.legacy_projection =
                        legacy::stage_summary(data_root, &marker.migration_id)?;
                    append_marker(data_root, &marker)?;
                    Ok(MigrationDecision::RebuildFromSourcesWithLegacyException(
                        marker,
                    ))
                }
                Ok(None) => {
                    marker.phase = MigrationPhase::RebuildPending;
                    marker.resumable = true;
                    marker.legacy_projection = None;
                    append_marker(data_root, &marker)?;
                    Ok(MigrationDecision::RebuildFromSources(marker))
                }
                Err(error) => {
                    marker.phase = MigrationPhase::LegacyProjectionFailed;
                    marker.resumable = true;
                    marker.error = Some(bounded_error(&format!("{error:#}")));
                    let _ = append_marker(data_root, &marker);
                    Err(error)
                }
            }
        }
    }
}

/// Records the generation published by the daemon rebuild worker.
///
/// Completion is accepted only after the legacy exception decision is durable
/// and the supplied generation is the one currently published at the
/// migration's lexical projection path.
#[allow(dead_code)] // Called by the separately-owned daemon rebuild worker.
pub(crate) fn complete_source_rebuild(
    data_root: &Path,
    generation_id: &str,
) -> Result<MigrationDecision> {
    let _lock = MigrationLock::acquire(data_root)?;
    let detected = detect_store(data_root)?;
    let marker = read_last_marker(data_root)?
        .ok_or_else(|| anyhow!("source-backed data migration has not been prepared"))?;
    let mut marker = validate_marker(data_root, detected, marker)?;
    if marker.phase == MigrationPhase::Ready {
        if marker.lexical_generation_id.as_deref() == Some(generation_id)
            && !marker.source_rebuild_required
        {
            return Ok(MigrationDecision::Ready(marker));
        }
        bail!("source-backed migration is already ready with a different generation");
    }
    if !matches!(
        marker.phase,
        MigrationPhase::RebuildPending | MigrationPhase::SourceRebuildFailed
    ) {
        bail!(
            "cannot complete source rebuild while migration is in {:?}",
            marker.phase
        );
    }
    let index = VerifiedIndex::open(&marker.lexical_projection_path).with_context(|| {
        format!(
            "verify rebuilt lexical projection {}",
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

/// Records a resumable daemon rebuild failure without disturbing either the
/// released Store or an already-published legacy exception projection.
#[allow(dead_code)] // Called by the separately-owned daemon rebuild worker.
pub(crate) fn record_source_rebuild_failure(
    data_root: &Path,
    error: &str,
) -> Result<MigrationMarker> {
    let _lock = MigrationLock::acquire(data_root)?;
    let detected = detect_store(data_root)?;
    let marker = read_last_marker(data_root)?
        .ok_or_else(|| anyhow!("source-backed data migration has not been prepared"))?;
    let mut marker = validate_marker(data_root, detected, marker)?;
    if !matches!(
        marker.phase,
        MigrationPhase::RebuildPending | MigrationPhase::SourceRebuildFailed
    ) {
        bail!(
            "cannot record source rebuild failure while migration is in {:?}",
            marker.phase
        );
    }
    marker.phase = MigrationPhase::SourceRebuildFailed;
    marker.source_rebuild_required = true;
    marker.resumable = true;
    marker.error = Some(bounded_error(error));
    append_marker(data_root, &marker)?;
    Ok(marker)
}

/// Rolls back only unpublished, disposable migration work. The released Store
/// and any already-published read-only legacy projection are retained.
#[allow(dead_code)] // Upgrade recovery hook; currently exercised by focused tests.
pub(crate) fn rollback_unpublished(data_root: &Path) -> Result<Option<MigrationMarker>> {
    let _lock = MigrationLock::acquire(data_root)?;
    let Some(mut marker) = read_last_marker(data_root)? else {
        return Ok(None);
    };
    if !matches!(
        marker.phase,
        MigrationPhase::LegacyProjectionBuilding | MigrationPhase::LegacyProjectionFailed
    ) {
        bail!(
            "migration phase {:?} has no unpublished legacy projection to roll back",
            marker.phase
        );
    }
    legacy::discard_unpublished_stage(data_root, &marker.migration_id)?;
    marker.phase = MigrationPhase::RolledBack;
    marker.resumable = false;
    marker.error = None;
    append_marker(data_root, &marker)?;
    Ok(Some(marker))
}

pub(crate) fn migration_directory(data_root: &Path) -> PathBuf {
    data_root.join(MIGRATION_DIRECTORY)
}

pub(crate) fn lexical_projection_path(data_root: &Path) -> PathBuf {
    data_root.join(SOURCE_BACKED_INDEX_DIRECTORY)
}

fn compatible_marker(data_root: &Path, detected: DetectedStore) -> Result<MigrationMarker> {
    let Some(marker) = read_last_marker(data_root)? else {
        let marker = MigrationMarker::detected(data_root, detected.origin());
        append_marker(data_root, &marker)?;
        return Ok(marker);
    };
    validate_marker(data_root, detected, marker)
}

fn validate_marker(
    data_root: &Path,
    detected: DetectedStore,
    marker: MigrationMarker,
) -> Result<MigrationMarker> {
    if marker.schema_version != MIGRATION_SCHEMA_VERSION {
        bail!(
            "unsupported source-backed data migration marker schema {}",
            marker.schema_version
        );
    }
    if marker.origin != detected.origin() {
        bail!(
            "source-backed data migration origin changed from {:?} to {:?}; refusing to reuse migration state",
            marker.origin,
            detected.origin()
        );
    }
    if marker.lexical_projection_path != lexical_projection_path(data_root)
        || marker.legacy_store_path
            != (detected == DetectedStore::ReleasedV46)
                .then(|| database_path(data_root.to_path_buf()))
    {
        bail!("source-backed data migration marker paths do not match this data root");
    }
    Ok(marker)
}

fn pending_decision(marker: MigrationMarker) -> MigrationDecision {
    if marker.legacy_projection.is_some() {
        MigrationDecision::RebuildFromSourcesWithLegacyException(marker)
    } else {
        MigrationDecision::RebuildFromSources(marker)
    }
}

fn bounded_error(error: &str) -> String {
    error.chars().take(MAX_ERROR_CHARS).collect()
}

fn detect_store(data_root: &Path) -> Result<DetectedStore> {
    let path = database_path(data_root.to_path_buf());
    if !path.exists() {
        return Ok(DetectedStore::Fresh);
    }
    let metadata =
        fs::metadata(&path).with_context(|| format!("inspect ctx Store {}", path.display()))?;
    if metadata.len() == 0 {
        return Ok(DetectedStore::Fresh);
    }
    let conn = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("open ctx Store read-only {}", path.display()))?;
    conn.execute_batch("PRAGMA query_only = ON; PRAGMA trusted_schema = OFF;")?;
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    match version {
        RELEASED_STORE_SCHEMA_VERSION => Ok(DetectedStore::ReleasedV46),
        CURRENT_STORE_SCHEMA_VERSION => Ok(DetectedStore::CurrentV47),
        other => Err(anyhow!(
            "unsupported ctx Store schema {other}; source-backed migration accepts only released schema {RELEASED_STORE_SCHEMA_VERSION} or current schema {CURRENT_STORE_SCHEMA_VERSION}"
        )),
    }
}

fn ensure_empty_lexical_generation(path: &Path) -> Result<String> {
    if path.join("meta.json").is_file() {
        if let Ok(index) = VerifiedIndex::open(path) {
            return Ok(index.generation_id().to_owned());
        }
    }
    let writer = GenerationWriter::open(
        path,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 32 * 1024 * 1024,
        },
    )
    .with_context(|| {
        format!(
            "initialize disposable source-backed projection {}",
            path.display()
        )
    })?;
    if let Some(base) = writer.base_manifest() {
        return base.generation_id().map_err(Into::into);
    }
    Ok(writer.commit(|_| true)?.generation_id)
}

fn journal_path(data_root: &Path) -> PathBuf {
    migration_directory(data_root).join(MIGRATION_JOURNAL)
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
            "source-backed migration journal {} exceeds its {}-byte bound",
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
        if let Ok(marker) = serde_json::from_str::<MigrationMarker>(&line) {
            last = Some(marker);
        }
    }
    Ok(last)
}

fn append_marker(data_root: &Path, marker: &MigrationMarker) -> Result<()> {
    let directory = migration_directory(data_root);
    create_private_directory_all(&directory)
        .with_context(|| format!("create migration directory {}", directory.display()))?;
    let path = journal_path(data_root);
    let existing = fs::metadata(&path).map(|value| value.len()).unwrap_or(0);
    let mut encoded = serde_json::to_vec(marker)?;
    encoded.push(b'\n');
    if existing > 0 {
        encoded.insert(0, b'\n');
    }
    if existing.saturating_add(encoded.len() as u64) > MAX_JOURNAL_BYTES {
        bail!(
            "source-backed migration journal {} would exceed its {}-byte bound",
            path.display(),
            MAX_JOURNAL_BYTES
        );
    }
    let mut file = OpenOptions::new()
        .append(true)
        .create(true)
        .open(&path)
        .with_context(|| format!("append migration journal {}", path.display()))?;
    restrict_private_file(&path)?;
    file.write_all(&encoded)?;
    file.sync_all()?;
    sync_parent(&directory)
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .with_context(|| format!("sync migration directory {}", path.display()))
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> Result<()> {
    Ok(())
}

struct MigrationLock {
    file: File,
}

impl MigrationLock {
    fn acquire(data_root: &Path) -> Result<Self> {
        let directory = migration_directory(data_root);
        create_private_directory_all(&directory)
            .with_context(|| format!("create migration directory {}", directory.display()))?;
        let path = directory.join(MIGRATION_LOCK);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("open migration lock {}", path.display()))?;
        restrict_private_file(&path)?;
        file.try_lock_exclusive()
            .map_err(|_| anyhow!("a source-backed data migration already owns this data root"))?;
        Ok(Self { file })
    }
}

impl Drop for MigrationLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
