use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    ops::Range,
    path::{Component, Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    compute_payload_hash, AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor,
    CaptureSourceKind, ContentRef, Event, EventRole, EventType, Fidelity, Session, SessionStatus,
    SyncCursor,
};
use ctx_history_store::{
    decode_native_path_committed_cursor, EventSearchBulkGuard, NativePathCursorSetClassification,
    NativePathCursorTransition, NativePathGroupAccounting, ProviderEventHashAuthority,
    ProviderSourceLocatorObservation, ProviderSourceRouteRetirement,
    ProviderSourceRouteRetirementDisposition, ProviderSourceRouteRetirementReason, Store,
    StoreError,
};
use rusqlite::{Connection, OptionalExtension};
use serde::{de::IgnoredAny, Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{
    common::io::{ProviderSourceDirectory, ProviderSourceRoot},
    complete_content::{
        attach_verified_content_locator, verified_content_profile, CompleteContentBodyDigest,
        CompleteContentSourceFamily, VerifiedContentLocatorV1, VerifiedContentRole,
    },
    native_source::NativeLocator,
    provider::{
        importer::{
            provider_event_import_identity_with_exact_legacy_source, provider_import_session_uuid,
            provider_path_identity, provider_scoped_source_identity_key,
            provider_scoped_source_uuid, provider_source_cursor_stream_for_path,
            provider_sync_metadata, timestamps, CertifiedProviderCursor,
            ProviderEventImportIdentity,
        },
        native_ingestion::{
            process_pro_replay_only, NativePageAccounting, NativeProOutputPage,
            NativeProReplayPage, NativeSafeFrontier, NativeSourceIdentity,
            NATIVE_INGESTION_PAGE_MAX_BYTES, NATIVE_INGESTION_PAGE_MAX_UNITS,
        },
        normalization::{provider_output_event_is_failure, provider_result_outcome_evidence},
        providers::task_json::task_json_string_field,
        sqlite::{
            sqlite_schema_fingerprint, sqlite_table_columns, sqlite_table_exists,
            SqliteLengthPreflightGuard,
        },
    },
    provider_sources::{
        open_root_handle_sqlite_source_snapshot, retain_sqlite_source_directory_authority,
        SqliteSourceAccessError, SqliteSourceDirectoryAuthority, SqliteSourceEvidence,
    },
    stable_capture_uuid, CaptureError, CaptureWorkLimit, OutputAssociations, OutputCommandContext,
    OutputNativeCoordinate, OutputObservationKind, OutputOutcome, OutputOutcomeMetadata,
    OutputSourceIdentity, OutputSourceLocator, ProOutputObservation, ProOutputProgress,
    ProOutputSink, ProOutputSinkError, ProOutputSourceDisposition, ProviderAdapterContext,
    ProviderImportFailure, ProviderImportOptions, ProviderImportSummary, ProviderImportWorkResult,
    Result, MAX_PROVIDER_JSONL_LINE_BYTES,
};

use super::{
    event::{
        trae_core_event, trae_event_from_owned_message, trae_session_metadata_preview,
        TraeCoreEvent, TraeEventInput,
    },
    json_stream::{
        trae_session_selection, trae_stream_session, TraeJsonArrayValues, TraeJsonContainerValues,
        TraeSessionSelection, TraeStreamSession,
    },
    trae_complete_message_locator,
    workspace::{collect_trae_state_vscdb_paths, trae_workspace_folder, trae_workspace_id},
    TRAE_CHAT_KEYS, TRAE_STATE_VSCDB_SOURCE_FORMAT,
};

const TRAE_NATIVE_CURSOR_VERSION: u32 = 1;
const TRAE_ROOT_CURSOR_VERSION: u32 = 1;
const TRAE_OUTPUT_FRONTIER_VERSION: u32 = 1;
const TRAE_LEGACY_NATIVE_PARSER_REVISION: &str = "trae-nativepath-parser-v1";
const TRAE_LEGACY_NATIVE_POLICY_REVISION: &str = "trae-nativepath-core-policy-v1";
const TRAE_NATIVE_PARSER_REVISION: &str = "trae-nativepath-parser-v2";
const TRAE_NATIVE_POLICY_REVISION: &str = "trae-nativepath-core-policy-v2";
const TRAE_OUTPUT_PARSER_REVISION: &str = "trae-nativepath-output-v2";
const TRAE_ROOT_SOURCE_FORMAT: &str = "trae_nativepath_root_v1";
const TRAE_PAGE_UNIT_LIMIT: usize = NATIVE_INGESTION_PAGE_MAX_UNITS - 8;
const TRAE_PAGE_BYTE_LIMIT: usize = NATIVE_INGESTION_PAGE_MAX_BYTES - 512 * 1024;
const TRAE_SQLITE_VALUE_OVERHEAD_BYTES: u64 = 16 * 64;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TraeFrontier {
    key_index: u16,
    session_index: u32,
    message_index: u32,
}

impl TraeFrontier {
    fn terminal() -> Self {
        Self {
            key_index: u16::try_from(TRAE_CHAT_KEYS.len()).unwrap_or(u16::MAX),
            session_index: 0,
            message_index: 0,
        }
    }

    fn is_terminal(self) -> bool {
        usize::from(self.key_index) >= TRAE_CHAT_KEYS.len()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TraeNativeCursor {
    version: u32,
    parser_revision: String,
    policy_revision: String,
    locator_identity: String,
    cursor_stream: String,
    canonical_source_identity: String,
    raw_source_path: String,
    source_revision: String,
    frontier: TraeFrontier,
    terminal: bool,
    generation: u64,
    rejected_records: u64,
}

impl TraeNativeCursor {
    fn decode(encoded: &str) -> Result<Self> {
        let cursor: Self = serde_json::from_str(encoded).map_err(|_| {
            CaptureError::InvalidPayload("Trae NativePath cursor is corrupt".into())
        })?;
        let supported_revision = (cursor.parser_revision == TRAE_NATIVE_PARSER_REVISION
            && cursor.policy_revision == TRAE_NATIVE_POLICY_REVISION)
            || (cursor.parser_revision == TRAE_LEGACY_NATIVE_PARSER_REVISION
                && cursor.policy_revision == TRAE_LEGACY_NATIVE_POLICY_REVISION);
        if cursor.version != TRAE_NATIVE_CURSOR_VERSION
            || !supported_revision
            || cursor.locator_identity.is_empty()
            || cursor.cursor_stream.is_empty()
            || cursor.canonical_source_identity.is_empty()
            || cursor.raw_source_path.is_empty()
            || cursor.source_revision.is_empty()
            || cursor.terminal != cursor.frontier.is_terminal()
        {
            return Err(CaptureError::InvalidPayload(
                "Trae NativePath cursor authority is inconsistent".into(),
            ));
        }
        Ok(cursor)
    }

    fn encode(&self) -> Result<String> {
        serde_json::to_string(self).map_err(CaptureError::from)
    }

    fn route_state(&self) -> TraeRouteState {
        TraeRouteState {
            path: PathBuf::from(&self.raw_source_path),
            locator_identity: self.locator_identity.clone(),
            cursor_stream: self.cursor_stream.clone(),
            canonical_source_identity: self.canonical_source_identity.clone(),
            source_revision: self.source_revision.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TraeRouteState {
    path: PathBuf,
    locator_identity: String,
    cursor_stream: String,
    canonical_source_identity: String,
    source_revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TraeRootManifest {
    version: u32,
    configured_path: PathBuf,
    source_root: PathBuf,
    sources: Vec<TraeRouteState>,
}

struct TraeSourceAuthority {
    database: TraeSqliteDatabase,
    path: PathBuf,
    source_root: PathBuf,
    raw_source_path: String,
    workspace_id: String,
    workspace_folder: Option<String>,
    locator_identity: String,
    cursor_stream: String,
    proposed_source_identity: String,
    source_revision: String,
    observed_at: DateTime<Utc>,
}

struct TraeSqliteDatabase {
    parent: ProviderSourceDirectory,
    authority: SqliteSourceDirectoryAuthority,
    database_name: OsString,
    evidence: SqliteSourceEvidence,
}

impl TraeSqliteDatabase {
    fn open<T>(path: &Path, query: impl FnOnce(&Connection) -> Result<T>) -> Result<(Self, T)> {
        let parent_path =
            path.parent()
                .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                    path: path.to_path_buf(),
                    reason: "Trae SQLite source must have a parent directory",
                })?;
        let database_name = path
            .file_name()
            .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "Trae SQLite source must have a database leaf name",
            })?
            .to_os_string();
        let parent = ProviderSourceRoot::open(parent_path)?.directory()?;
        let authority_handle = parent.try_clone_authority_handle()?;
        let authority = retain_sqlite_source_directory_authority(&authority_handle, parent_path)
            .map_err(|error| trae_sqlite_source_error(path, error))?;
        let snapshot = open_root_handle_sqlite_source_snapshot(&authority, &database_name)
            .map_err(|error| trae_sqlite_source_error(path, error))?;
        let evidence = snapshot.evidence().clone();
        let result = snapshot
            .connection()
            .map_err(|error| trae_sqlite_source_error(path, error))
            .and_then(query);
        let finished = snapshot
            .finish()
            .map_err(|error| trae_sqlite_source_error(path, error))?;
        if finished != evidence {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let database = Self {
            parent,
            authority,
            database_name,
            evidence,
        };
        database.revalidate()?;
        Ok((database, result?))
    }

    fn read<T>(&self, path: &Path, query: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        self.revalidate()?;
        let snapshot =
            open_root_handle_sqlite_source_snapshot(&self.authority, &self.database_name)
                .map_err(|error| trae_sqlite_source_error(path, error))?;
        let result = if snapshot.evidence() == &self.evidence {
            snapshot
                .connection()
                .map_err(|error| trae_sqlite_source_error(path, error))
                .and_then(query)
        } else {
            Err(CaptureError::SourceChangedDuringCapture)
        };
        let finished = snapshot
            .finish()
            .map_err(|error| trae_sqlite_source_error(path, error))?;
        self.revalidate()?;
        if finished != self.evidence {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        result
    }

    fn revalidate(&self) -> Result<()> {
        self.parent.revalidate()?;
        self.parent.authority_root().revalidate()
    }

    fn evidence(&self) -> &SqliteSourceEvidence {
        &self.evidence
    }
}

fn trae_sqlite_source_error(path: &Path, error: SqliteSourceAccessError) -> CaptureError {
    match error {
        SqliteSourceAccessError::SourceChanged
        | SqliteSourceAccessError::ConnectionIdentityMismatch => {
            CaptureError::SourceChangedDuringCapture
        }
        error => CaptureError::ProviderSource {
            provider: CaptureProvider::Trae.as_str(),
            path: path.to_path_buf(),
            kind: crate::ProviderSourceFailureKind::SourceDatabase,
            detail: error.to_string(),
        },
    }
}

struct TraeSessionPlan {
    session: TraeStreamSession,
    raw_session_index: u32,
    messages: Vec<Range<usize>>,
}

struct TraeActiveKey {
    key_index: u16,
    chat_key: &'static str,
    bytes: Vec<u8>,
    record_digest: CompleteContentBodyDigest,
    value_digest: [u8; 32],
    sessions: Vec<TraeSessionPlan>,
}

struct TraeScanner<'a> {
    authority: &'a TraeSourceAuthority,
    frontier: TraeFrontier,
    active: Option<TraeActiveKey>,
    source_content_hasher: Sha256,
    certified_source_bytes: u64,
}

#[derive(Clone)]
struct TraeSessionFact {
    provider_session_id: String,
    native_session_id: String,
    chat_key: &'static str,
    metadata_preview: Value,
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    title: Option<String>,
}

struct TraeCoreRecord {
    provider_session_id: String,
    native_session_id: String,
    native_session_id_from_provider: bool,
    native_message_id: String,
    native_message_id_from_provider: bool,
    chat_key: &'static str,
    value_digest: [u8; 32],
    key_index: u16,
    raw_session_index: u32,
    legacy_session_index: u32,
    message_index: u32,
    lexical_text: String,
    event: TraeCoreEvent,
}

struct TraeOutputRow {
    provider_session_id: String,
    key_index: u16,
    session_index: u32,
    message_index: u32,
    native_message_id: String,
    occurred_at: DateTime<Utc>,
    call_id: Option<String>,
    command: Option<OutputCommandContext>,
    outcome: OutputOutcomeMetadata,
    byte_range: Range<usize>,
    content: Vec<u8>,
}

struct TraeScanPage {
    expected: TraeFrontier,
    next: TraeFrontier,
    terminal: bool,
    logical_units: usize,
    estimated_bytes: usize,
    sessions: BTreeMap<String, TraeSessionFact>,
    core: Vec<TraeCoreRecord>,
    outputs: Vec<TraeOutputRow>,
    rejections: Vec<ProviderImportFailure>,
}

enum TraeLoadedKey {
    Missing,
    Rejected(String),
    Active(TraeActiveKey),
}

// Keep decoded persisted authority inline while distinguishing absent and legacy cursor formats.
#[allow(clippy::large_enum_variant)]
enum StoredTraeCursor {
    None,
    Legacy {
        encoded: String,
    },
    Native {
        encoded: String,
        publication_id: String,
        cursor: TraeNativeCursor,
    },
}

struct TraeCoreImport {
    summary: ProviderImportSummary,
    route: TraeRouteState,
    changed_groups: usize,
    complete: bool,
}

pub(crate) fn import_trae_nativepath(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let configured_path = absolute_trae_path(path)?;
    let source_root = context
        .source_root
        .clone()
        .or_else(|| context.source_path.clone())
        .unwrap_or_else(|| configured_path.clone());
    let root_stream = root_cursor_stream(&configured_path)?;
    let prior_manifest = load_root_manifest(store, &context.machine_id, &root_stream)?;
    let mut paths = match collect_trae_state_vscdb_paths(&configured_path) {
        Ok(paths) => paths,
        Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error),
    };
    for candidate in &mut paths {
        *candidate = absolute_trae_path(candidate)?;
    }
    paths.sort();
    paths.dedup();
    if paths.is_empty() && prior_manifest.is_none() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: configured_path,
            reason: "no Trae state.vscdb files found",
        });
    }

    if options.import_profile.is_replay_only() {
        let sink = options
            .import_profile
            .sink()
            .ok_or(CaptureError::SystemInvariant(
                "Trae Pro replay profile has no output sink",
            ))?;
        let mut summary = ProviderImportSummary::default();
        for db_path in &paths {
            replay_source_outputs_or_mark_behind(
                db_path,
                &source_root,
                &context,
                store,
                sink.as_ref(),
            );
        }
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }

    let committed_store = Store::open_read_only(store.path())?;
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let result = (|| {
        let mut summary = ProviderImportSummary::default();
        let mut current_routes = Vec::new();
        let mut changed_groups = 0_usize;
        for (source_index, db_path) in paths.iter().enumerate() {
            let imported = match import_source_core(
                db_path,
                &source_root,
                store,
                &committed_store,
                &bulk_guard,
                &context,
                &options,
            ) {
                Ok(imported) => imported,
                Err(error) if trae_source_failure_is_local(&error) => {
                    summary.record_failure(ProviderImportFailure {
                        line: source_index.saturating_add(1),
                        error: format!(
                            "Trae workspace database `{}` was skipped: {error}",
                            db_path.display()
                        ),
                    });
                    if let Some(route) = prior_manifest
                        .as_ref()
                        .and_then(|manifest| {
                            manifest.sources.iter().find(|route| route.path == *db_path)
                        })
                        .cloned()
                    {
                        current_routes.push(route);
                    }
                    continue;
                }
                Err(error) => return Err(error),
            };
            changed_groups = changed_groups.saturating_add(imported.changed_groups);
            summary.merge_from(imported.summary);
            current_routes.push(imported.route);
            if !imported.complete
                || (options.capture_work_limit == CaptureWorkLimit::OneSafeGroup
                    && changed_groups != 0)
            {
                summary.work_remaining = true;
                return Ok(summary);
            }
        }

        current_routes.sort_by(|left, right| left.path.cmp(&right.path));
        if let Some(prior) = &prior_manifest {
            let current = current_routes
                .iter()
                .map(|route| &route.path)
                .collect::<BTreeSet<_>>();
            for missing in prior
                .sources
                .iter()
                .filter(|route| !current.contains(&route.path))
            {
                let changed = retire_missing_route(
                    store,
                    &bulk_guard,
                    &context,
                    missing,
                    ProviderSourceRouteRetirementReason::SourceMissing,
                )?;
                if changed {
                    changed_groups = changed_groups.saturating_add(1);
                    summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
                    summary.skipped = summary.skipped.saturating_add(1);
                    summary.set_work_result(ProviderImportWorkResult::Changed);
                }
                if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup
                    && changed_groups != 0
                {
                    summary.work_remaining = true;
                    return Ok(summary);
                }
            }
        }

        let manifest = TraeRootManifest {
            version: TRAE_ROOT_CURSOR_VERSION,
            configured_path: path.to_path_buf(),
            source_root: source_root.clone(),
            sources: current_routes,
        };
        if publish_root_manifest(store, &bulk_guard, &context, &root_stream, &manifest)? {
            summary.set_work_result(ProviderImportWorkResult::Changed);
        }

        if let Some(sink) = options.import_profile.sink() {
            for db_path in &paths {
                replay_source_outputs_or_mark_behind(
                    db_path,
                    &source_root,
                    &context,
                    store,
                    sink.as_ref(),
                );
            }
        }
        Ok(summary)
    })();
    let finish = store
        .finish_event_search_bulk_mode(&bulk_guard)
        .map_err(CaptureError::from);
    match (result, finish) {
        (Ok(summary), Ok(())) => Ok(summary),
        (_, Err(error)) => Err(error),
        (Err(error), Ok(())) => Err(error),
    }
}

fn trae_source_failure_is_local(error: &CaptureError) -> bool {
    matches!(
        error,
        CaptureError::Io(_)
            | CaptureError::Json(_)
            | CaptureError::Sqlite(_)
            | CaptureError::Time(_)
            | CaptureError::Uuid(_)
            | CaptureError::InvalidProviderTranscriptPath { .. }
            | CaptureError::ProviderSource { .. }
            | CaptureError::SourceChangedDuringCapture
            | CaptureError::InvalidJsonLine { .. }
    )
}

fn absolute_trae_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    Ok(normalized)
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

#[allow(clippy::too_many_arguments)]
mod core_import;
mod lifecycle;
mod outputs;
mod publication;
mod source_backed;

#[cfg(test)]
#[path = "nativepath_tests.rs"]
mod tests;

use core_import::*;
use lifecycle::*;
use outputs::*;
use publication::*;
pub(crate) use source_backed::{
    hydrate_trae_source_backed_locator_v0, scan_trae_source_backed_explicit_v0,
    TraeHydratedRecordV0, TraeSourceBackedErrorV0, TraeSourceBackedPageV0, TraeSourceBackedScanV0,
};

#[cfg(test)]
mod stock_sqlite_snapshot_tests {
    use std::{cell::Cell, ffi::OsString, fs, path::Path};

    use rusqlite::{config::DbConfig, params, Connection};

    use super::TraeSqliteDatabase;

    #[test]
    fn stock_snapshot_queries_active_wal_without_persistent_writes_and_rejects_swap() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let source = temp.path().join("trae.sqlite");
        let attacker = temp.path().join("attacker.sqlite");
        let admitted = temp.path().join("admitted.sqlite");
        create_database(&source, "main");
        create_database(&attacker, "attacker");
        persist_wal_row(&source, "from-wal");
        let before_read = persistent_directory_snapshot(temp.path());

        let (database, opened_value) = TraeSqliteDatabase::open(&source, read_latest).unwrap();
        assert_eq!(opened_value, "from-wal");
        assert!(database.evidence().wal_length().is_some());
        assert!(database.evidence().shared_memory_length().is_some());
        assert_eq!(database.read(&source, read_latest).unwrap(), "from-wal");
        assert_eq!(persistent_directory_snapshot(temp.path()), before_read);

        fs::rename(&source, &admitted).unwrap();
        fs::rename(&attacker, &source).unwrap();
        let before_rejected_read = persistent_directory_snapshot(temp.path());
        let queried = Cell::new(false);
        let result = database.read(&source, |_| -> crate::Result<()> {
            queried.set(true);
            Ok(())
        });
        assert!(result.is_err());
        assert!(!queried.get());
        assert_eq!(
            persistent_directory_snapshot(temp.path()),
            before_rejected_read
        );
    }

    fn create_database(path: &Path, value: &str) {
        let connection = Connection::open(path).unwrap();
        connection
            .execute("CREATE TABLE messages (body TEXT NOT NULL)", [])
            .unwrap();
        connection
            .execute("INSERT INTO messages (body) VALUES (?1)", params![value])
            .unwrap();
    }

    fn persist_wal_row(path: &Path, value: &str) {
        let writer = Connection::open(path).unwrap();
        let mode: String = writer
            .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        writer
            .execute("INSERT INTO messages (body) VALUES (?1)", params![value])
            .unwrap();
        writer
            .set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
            .unwrap();
        drop(writer);
        assert!(path.with_file_name("trae.sqlite-wal").exists());
        assert!(path.with_file_name("trae.sqlite-shm").exists());
    }

    fn read_latest(connection: &Connection) -> crate::Result<String> {
        Ok(connection.query_row(
            "SELECT body FROM messages ORDER BY rowid DESC LIMIT 1",
            [],
            |row| row.get(0),
        )?)
    }

    fn persistent_directory_snapshot(directory: &Path) -> Vec<(OsString, Vec<u8>)> {
        let mut paths = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                !path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .ends_with("-shm")
            })
            .collect::<Vec<_>>();
        paths.sort();
        paths
            .into_iter()
            .map(|path| {
                (
                    path.file_name().unwrap().to_os_string(),
                    fs::read(path).unwrap(),
                )
            })
            .collect()
    }
}
