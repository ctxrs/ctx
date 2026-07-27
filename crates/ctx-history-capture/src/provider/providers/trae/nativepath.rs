use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    ops::Range,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    ContentRef, Event, EventRole, EventType, Fidelity, Session, SessionStatus, SyncCursor,
};
use ctx_history_store::{
    decode_native_path_committed_cursor, EventSearchBulkGuard, NativePathCursorSetClassification,
    NativePathCursorTransition, NativePathGroupAccounting, ProviderEventHashAuthority,
    ProviderSourceLocatorObservation, ProviderSourceRouteRetirement,
    ProviderSourceRouteRetirementDisposition, ProviderSourceRouteRetirementReason, Store,
};
use rusqlite::{Connection, OptionalExtension};
use serde::{de::IgnoredAny, Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{
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
        },
        native_ingestion::{
            process_pro_replay_only, NativePageAccounting, NativeProOutputPage,
            NativeProReplayPage, NativeSafeFrontier, NativeSourceIdentity,
            NATIVE_INGESTION_PAGE_MAX_BYTES, NATIVE_INGESTION_PAGE_MAX_UNITS,
        },
        normalization::{provider_output_event_is_failure, provider_result_outcome_evidence},
        providers::task_json::task_json_string_field,
        sqlite::{
            ensure_sqlite_table_columns, open_provider_sqlite_readonly, sqlite_schema_fingerprint,
            sqlite_table_columns, sqlite_table_exists, with_sqlite_read_snapshot,
            ProviderSqliteSourceSnapshot, ReadOnlySqliteConnection, SqliteLengthPreflightGuard,
        },
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
const TRAE_NATIVE_PARSER_REVISION: &str = "trae-nativepath-parser-v1";
const TRAE_NATIVE_POLICY_REVISION: &str = "trae-nativepath-core-policy-v1";
const TRAE_OUTPUT_PARSER_REVISION: &str = "trae-nativepath-output-v1";
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
        if cursor.version != TRAE_NATIVE_CURSOR_VERSION
            || cursor.parser_revision != TRAE_NATIVE_PARSER_REVISION
            || cursor.policy_revision != TRAE_NATIVE_POLICY_REVISION
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

#[derive(Clone)]
struct TraeSourceAuthority {
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
    snapshot: ProviderSqliteSourceSnapshot,
}

struct TraeSessionPlan {
    session: TraeStreamSession,
    messages: Vec<Range<usize>>,
}

struct TraeActiveKey {
    key_index: u16,
    chat_key: &'static str,
    bytes: Vec<u8>,
    record_digest: CompleteContentBodyDigest,
    sessions: Vec<TraeSessionPlan>,
}

struct TraeScanner<'a> {
    conn: &'a Connection,
    authority: &'a TraeSourceAuthority,
    frontier: TraeFrontier,
    active: Option<TraeActiveKey>,
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
    key_index: u16,
    session_index: u32,
    message_index: u32,
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
    let configured_path = path.to_path_buf();
    let source_root = context
        .source_root
        .clone()
        .or_else(|| context.source_path.clone())
        .unwrap_or_else(|| configured_path.clone());
    let root_stream = root_cursor_stream(&configured_path)?;
    let prior_manifest = load_root_manifest(store, &context.machine_id, &root_stream)?;
    let mut paths = match collect_trae_state_vscdb_paths(path) {
        Ok(paths) => paths,
        Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error),
    };
    for candidate in &mut paths {
        *candidate = fs::canonicalize(&*candidate)?;
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
        for db_path in &paths {
            let imported = import_source_core(
                db_path,
                &source_root,
                store,
                &committed_store,
                &bulk_guard,
                &context,
                &options,
            )?;
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

#[allow(clippy::too_many_arguments)]
fn import_source_core(
    path: &Path,
    source_root: &Path,
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
) -> Result<TraeCoreImport> {
    let (authority, conn) = acquire_source(path, source_root, context.imported_at)?;
    let stored = load_source_cursor(store, &context.machine_id, &authority.cursor_stream)?;
    let (start, generation, rejected_records, expected_encoded, already_terminal) =
        plan_core_scan(&stored, &authority)?;
    if already_terminal {
        let cursor = match stored {
            StoredTraeCursor::Native { cursor, .. } => cursor,
            StoredTraeCursor::None | StoredTraeCursor::Legacy { .. } => {
                return Err(CaptureError::SystemInvariant(
                    "Trae terminal plan lost its NativePath cursor",
                ));
            }
        };
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(TraeCoreImport {
            summary,
            route: cursor.route_state(),
            changed_groups: 0,
            complete: true,
        });
    }

    let mut scanner = TraeScanner::new(&conn, &authority, start);
    let mut expected = expected_encoded;
    let mut rejected_total = rejected_records;
    let mut summary = ProviderImportSummary::default();
    let mut changed_groups = 0_usize;
    let mut last_route = None;
    while let Some(page) = scanner.next_page(true, false)? {
        if !authority.snapshot.revalidate(&authority.path)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        rejected_total =
            rejected_total.saturating_add(u64::try_from(page.rejections.len()).unwrap_or(u64::MAX));
        let next_cursor = TraeNativeCursor {
            version: TRAE_NATIVE_CURSOR_VERSION,
            parser_revision: TRAE_NATIVE_PARSER_REVISION.to_owned(),
            policy_revision: TRAE_NATIVE_POLICY_REVISION.to_owned(),
            locator_identity: authority.locator_identity.clone(),
            cursor_stream: authority.cursor_stream.clone(),
            canonical_source_identity: authority.proposed_source_identity.clone(),
            raw_source_path: authority.raw_source_path.clone(),
            source_revision: authority.source_revision.clone(),
            frontier: page.next,
            terminal: page.terminal,
            generation,
            rejected_records: rejected_total,
        };
        let next = provider_sync_cursor(
            &context.machine_id,
            authority.cursor_stream.clone(),
            next_cursor.encode()?,
            context.imported_at,
        );
        let transition = NativePathCursorTransition::new(expected.clone(), next);
        let publication_id = page_publication_id(&authority, &page, generation, &transition);
        let accounting = NativePathGroupAccounting::new(1, 1, page.estimated_bytes.max(1))?;
        let admission = store.admit_event_search_bulk_group(bulk_guard)?;
        let mut group = store.begin_native_path_publication_group(admission, accounting)?;
        let changed =
            match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
                NativePathCursorSetClassification::AllNextSameGroup { .. } => false,
                NativePathCursorSetClassification::AllExpected => {
                    let route = publish_core_page(
                        committed_store,
                        &mut group,
                        context,
                        options,
                        &authority,
                        &page,
                        &mut summary,
                    )?;
                    last_route = Some(route);
                    if !authority.snapshot.revalidate(&authority.path)? {
                        return Err(CaptureError::SourceChangedDuringCapture);
                    }
                    group.prepare_journal_checkpoint()?;
                    group.publish_cursor_set()?;
                    true
                }
            };
        group.commit()?;
        if changed {
            changed_groups = changed_groups.saturating_add(1);
            summary.set_work_result(ProviderImportWorkResult::Changed);
        }
        for rejection in page.rejections {
            summary.record_failure(rejection);
        }
        expected = store
            .get_sync_cursor(None, &context.machine_id, &authority.cursor_stream)?
            .map(|cursor| cursor.cursor);
        if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup && changed {
            summary.work_remaining = !page.terminal;
            let route = last_route.unwrap_or_else(|| next_cursor.route_state());
            return Ok(TraeCoreImport {
                summary,
                route,
                changed_groups,
                complete: page.terminal,
            });
        }
    }
    let route = last_route.unwrap_or_else(|| TraeRouteState {
        path: authority.path.clone(),
        locator_identity: authority.locator_identity.clone(),
        cursor_stream: authority.cursor_stream.clone(),
        canonical_source_identity: authority.proposed_source_identity.clone(),
        source_revision: authority.source_revision.clone(),
    });
    Ok(TraeCoreImport {
        summary,
        route,
        changed_groups,
        complete: true,
    })
}

fn acquire_source(
    path: &Path,
    source_root: &Path,
    observed_at: DateTime<Utc>,
) -> Result<(TraeSourceAuthority, ReadOnlySqliteConnection)> {
    let snapshot = ProviderSqliteSourceSnapshot::read(
        path,
        "Trae SQLite source must be a regular non-symlink file",
        "Trae SQLite sidecar must be a regular non-symlink file",
    )?;
    let conn = open_provider_sqlite_readonly(path)?;
    if !snapshot.revalidate(path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    validate_schema(&conn, path)?;
    let schema = sqlite_schema_fingerprint(&conn)?;
    let locator_identity = provider_path_identity(path)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Trae,
        TRAE_STATE_VSCDB_SOURCE_FORMAT,
        &locator_identity,
    );
    let source_revision = format!(
        "trae-nativepath-sqlite-v1;parser={TRAE_NATIVE_PARSER_REVISION};policy={TRAE_NATIVE_POLICY_REVISION};schema={schema};{}",
        snapshot.revision_component()
    );
    Ok((
        TraeSourceAuthority {
            path: path.to_path_buf(),
            source_root: source_root.to_path_buf(),
            raw_source_path: path.display().to_string(),
            workspace_id: trae_workspace_id(path),
            workspace_folder: trae_workspace_folder(path),
            locator_identity: locator_identity.clone(),
            cursor_stream,
            proposed_source_identity: format!("trae-sqlite:{locator_identity}"),
            source_revision,
            observed_at,
            snapshot,
        },
        conn,
    ))
}

fn validate_schema(conn: &Connection, path: &Path) -> Result<()> {
    if !sqlite_table_exists(conn, "ItemTable")? {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Trae state.vscdb is missing ItemTable",
        });
    }
    ensure_sqlite_table_columns(
        &sqlite_table_columns(conn, "ItemTable")?,
        "Trae ItemTable",
        &["key", "value"],
    )
}

fn load_source_cursor(store: &Store, machine_id: &str, stream: &str) -> Result<StoredTraeCursor> {
    let Some(stored) = store.get_sync_cursor(None, machine_id, stream)? else {
        return Ok(StoredTraeCursor::None);
    };
    if let Ok(committed) = decode_native_path_committed_cursor(&stored.cursor) {
        return Ok(StoredTraeCursor::Native {
            encoded: stored.cursor,
            publication_id: committed.publication_id().to_owned(),
            cursor: TraeNativeCursor::decode(committed.provider_cursor())?,
        });
    }
    match CertifiedProviderCursor::decode_if_certified(&stored.cursor)? {
        Some(_) => Ok(StoredTraeCursor::Legacy {
            encoded: stored.cursor,
        }),
        None => Err(CaptureError::InvalidPayload(
            "Trae cursor is neither a released legacy cursor nor NativePath authority".into(),
        )),
    }
}

fn plan_core_scan(
    stored: &StoredTraeCursor,
    authority: &TraeSourceAuthority,
) -> Result<(TraeFrontier, u64, u64, Option<String>, bool)> {
    match stored {
        StoredTraeCursor::None => Ok((TraeFrontier::default(), 0, 0, None, false)),
        StoredTraeCursor::Legacy { encoded } => {
            Ok((TraeFrontier::default(), 0, 0, Some(encoded.clone()), false))
        }
        StoredTraeCursor::Native {
            encoded,
            publication_id,
            cursor,
        } => {
            if cursor.locator_identity != authority.locator_identity
                || cursor.cursor_stream != authority.cursor_stream
            {
                return Err(CaptureError::InvalidPayload(
                    "Trae NativePath cursor is bound to a different route".into(),
                ));
            }
            if cursor.source_revision == authority.source_revision
                && cursor.terminal
                && publication_id.starts_with("trae-nativepath-page-v1:")
            {
                return Ok((
                    cursor.frontier,
                    cursor.generation,
                    cursor.rejected_records,
                    Some(encoded.clone()),
                    true,
                ));
            }
            if cursor.source_revision == authority.source_revision && !cursor.terminal {
                return Ok((
                    cursor.frontier,
                    cursor.generation,
                    cursor.rejected_records,
                    Some(encoded.clone()),
                    false,
                ));
            }
            Ok((
                TraeFrontier::default(),
                cursor
                    .generation
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "Trae NativePath generation exhausted",
                    ))?,
                0,
                Some(encoded.clone()),
                false,
            ))
        }
    }
}

impl<'a> TraeScanner<'a> {
    fn new(
        conn: &'a Connection,
        authority: &'a TraeSourceAuthority,
        frontier: TraeFrontier,
    ) -> Self {
        Self {
            conn,
            authority,
            frontier,
            active: None,
        }
    }

    fn next_page(
        &mut self,
        collect_core: bool,
        collect_outputs: bool,
    ) -> Result<Option<TraeScanPage>> {
        if self.frontier.is_terminal() {
            return Ok(None);
        }
        let expected = self.frontier;
        let mut page = TraeScanPage {
            expected,
            next: expected,
            terminal: false,
            logical_units: 0,
            estimated_bytes: 0,
            sessions: BTreeMap::new(),
            core: Vec::new(),
            outputs: Vec::new(),
            rejections: Vec::new(),
        };
        while page.logical_units < TRAE_PAGE_UNIT_LIMIT
            && page.estimated_bytes < TRAE_PAGE_BYTE_LIMIT
            && !self.frontier.is_terminal()
        {
            if self
                .active
                .as_ref()
                .is_none_or(|active| active.key_index != self.frontier.key_index)
            {
                self.active = None;
                match self.load_key(self.frontier.key_index)? {
                    TraeLoadedKey::Missing => {
                        self.advance_key()?;
                        continue;
                    }
                    TraeLoadedKey::Rejected(error) => {
                        page.logical_units = page.logical_units.saturating_add(1);
                        page.estimated_bytes = page
                            .estimated_bytes
                            .saturating_add(error.len())
                            .saturating_add(128);
                        page.rejections.push(ProviderImportFailure {
                            line: packed_native_index(
                                self.frontier.key_index,
                                self.frontier.session_index,
                                self.frontier.message_index,
                            )
                            .unwrap_or(u64::MAX) as usize,
                            error,
                        });
                        self.advance_key()?;
                        continue;
                    }
                    TraeLoadedKey::Active(active) => self.active = Some(active),
                }
            }
            let active = self.active.as_ref().ok_or(CaptureError::SystemInvariant(
                "Trae active ItemTable key is unavailable",
            ))?;
            let session_index = usize::try_from(self.frontier.session_index).map_err(|_| {
                CaptureError::InvalidPayload("Trae session frontier exceeds platform limits".into())
            })?;
            let Some(session_plan) = active.sessions.get(session_index) else {
                self.advance_key()?;
                continue;
            };
            let message_index = usize::try_from(self.frontier.message_index).map_err(|_| {
                CaptureError::InvalidPayload("Trae message frontier exceeds platform limits".into())
            })?;
            let Some(range) = session_plan.messages.get(message_index).cloned() else {
                self.frontier.session_index = self.frontier.session_index.checked_add(1).ok_or(
                    CaptureError::SystemInvariant("Trae session frontier exhausted"),
                )?;
                self.frontier.message_index = 0;
                continue;
            };
            let message: Value = match serde_json::from_slice(&active.bytes[range.clone()]) {
                Ok(message) => message,
                Err(error) => {
                    page.rejections.push(ProviderImportFailure {
                        line: packed_native_index(
                            self.frontier.key_index,
                            self.frontier.session_index,
                            self.frontier.message_index,
                        )
                        .unwrap_or(u64::MAX) as usize,
                        error: format!(
                            "Trae ItemTable key `{}` message is invalid JSON: {error}",
                            active.chat_key
                        ),
                    });
                    page.logical_units = page.logical_units.saturating_add(1);
                    page.estimated_bytes = page.estimated_bytes.saturating_add(256);
                    self.advance_message()?;
                    continue;
                }
            };
            let provider_session_id = format!(
                "{}/{}",
                self.authority.workspace_id, session_plan.session.native_session_id
            );
            let Some(event) = trae_event_from_owned_message(
                &provider_session_id,
                &self.authority.workspace_id,
                active.chat_key,
                message,
                message_index,
                self.authority.observed_at,
            ) else {
                page.logical_units = page.logical_units.saturating_add(1);
                self.advance_message()?;
                continue;
            };
            let output = classify_output(&event.raw_message);
            let fact = session_fact(
                &provider_session_id,
                active.chat_key,
                &session_plan.session,
                &event,
                !output,
            );
            page.sessions
                .entry(provider_session_id.clone())
                .and_modify(|existing| merge_session_fact(existing, &fact))
                .or_insert(fact);
            if output {
                if collect_outputs {
                    let row = output_row(
                        &provider_session_id,
                        self.frontier,
                        range,
                        &event,
                        self.authority.workspace_folder.as_deref(),
                    );
                    let row_bytes = output_row_bytes(&row);
                    if row_bytes > TRAE_PAGE_BYTE_LIMIT {
                        return Err(CaptureError::InvalidPayload(
                            "Trae output record exceeds the bounded NativePath output page".into(),
                        ));
                    }
                    if !page.outputs.is_empty()
                        && page.estimated_bytes.saturating_add(row_bytes) > TRAE_PAGE_BYTE_LIMIT
                    {
                        break;
                    }
                    page.estimated_bytes = page.estimated_bytes.saturating_add(row_bytes);
                    page.outputs.push(row);
                }
                if collect_core && is_failure_or_timeout(&event.raw_message) {
                    let core_event = sparse_failure_event(
                        &provider_session_id,
                        &self.authority.workspace_id,
                        active.chat_key,
                        &event,
                    );
                    page.estimated_bytes = page
                        .estimated_bytes
                        .saturating_add(core_event_bytes(&core_event));
                    page.core.push(TraeCoreRecord {
                        provider_session_id,
                        key_index: self.frontier.key_index,
                        session_index: self.frontier.session_index,
                        message_index: self.frontier.message_index,
                        event: core_event,
                    });
                }
            } else if collect_core {
                let mut core_event = trae_core_event(
                    &provider_session_id,
                    &self.authority.workspace_id,
                    active.chat_key,
                    &event,
                );
                attach_trae_complete_content_locator(
                    &mut core_event,
                    &trae_complete_message_locator(
                        self.frontier.key_index,
                        session_index,
                        message_index,
                    )?,
                    &active.record_digest,
                    &event.text,
                )?;
                page.estimated_bytes = page
                    .estimated_bytes
                    .saturating_add(core_event_bytes(&core_event));
                page.core.push(TraeCoreRecord {
                    provider_session_id,
                    key_index: self.frontier.key_index,
                    session_index: self.frontier.session_index,
                    message_index: self.frontier.message_index,
                    event: core_event,
                });
            }
            page.logical_units = page.logical_units.saturating_add(1);
            self.advance_message()?;
        }
        self.frontier = normalize_frontier(self.frontier, self.active.as_ref())?;
        page.next = self.frontier;
        page.terminal = page.next.is_terminal();
        page.estimated_bytes = page
            .estimated_bytes
            .saturating_add(page.sessions.len().saturating_mul(2048))
            .saturating_add(4096);
        if page.estimated_bytes > NATIVE_INGESTION_PAGE_MAX_BYTES {
            return Err(CaptureError::InvalidPayload(
                "Trae Core page exceeds NativePath retained-byte bounds".into(),
            ));
        }
        Ok(Some(page))
    }

    fn load_key(&self, key_index: u16) -> Result<TraeLoadedKey> {
        let Some(chat_key) = TRAE_CHAT_KEYS.get(usize::from(key_index)).copied() else {
            return Ok(TraeLoadedKey::Missing);
        };
        if !self.authority.snapshot.revalidate(&self.authority.path)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let candidate = {
            let _guard = SqliteLengthPreflightGuard::new(self.conn);
            self.conn
                .query_row(
                    "select typeof(value), coalesce(octet_length(value), 0) \
                     from ItemTable where [key] = ?1",
                    [chat_key],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
                )
                .optional()?
        };
        let Some((value_type, retained_bytes)) = candidate else {
            return Ok(TraeLoadedKey::Missing);
        };
        let retained_bytes = u64::try_from(retained_bytes).map_err(|_| {
            CaptureError::InvalidPayload("Trae ItemTable value length is negative".into())
        })?;
        let observed_bytes = retained_bytes
            .saturating_add(TRAE_SQLITE_VALUE_OVERHEAD_BYTES)
            .saturating_add(u64::try_from(chat_key.len()).unwrap_or(u64::MAX));
        if observed_bytes > u64::try_from(MAX_PROVIDER_JSONL_LINE_BYTES).unwrap_or(u64::MAX) {
            return Ok(TraeLoadedKey::Rejected(format!(
                "Trae ItemTable key `{chat_key}` exceeds the provider JSON bound"
            )));
        }
        if value_type != "text" {
            return Ok(TraeLoadedKey::Rejected(format!(
                "Trae ItemTable key `{chat_key}` has unsupported SQLite type `{value_type}`"
            )));
        }
        let bytes = with_sqlite_read_snapshot(self.conn, || {
            self.conn
                .query_row(
                    "select cast(value as text) from ItemTable where [key] = ?1",
                    [chat_key],
                    |row| row.get::<_, String>(0),
                )
                .map(String::into_bytes)
                .map_err(CaptureError::from)
        })?;
        if bytes.len() != usize::try_from(retained_bytes).unwrap_or(usize::MAX) {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        if let Err(error) = serde_json::from_slice::<IgnoredAny>(&bytes) {
            return Ok(TraeLoadedKey::Rejected(format!(
                "Trae ItemTable key `{chat_key}` contains invalid JSON: {error}"
            )));
        }
        let sessions = match trae_session_selection(&bytes, chat_key) {
            Ok(None) => Vec::new(),
            Ok(Some(TraeSessionSelection::CnMessages(messages))) => vec![session_plan(
                &bytes,
                TraeStreamSession {
                    native_session_id: "trae-cn-input-history".to_owned(),
                    metadata_preview: json!({
                        "id": "trae-cn-input-history",
                        "title": "Trae CN input history",
                    }),
                    explicit_started_at: None,
                    explicit_ended_at: None,
                    explicit_title: Some("Trae CN input history".to_owned()),
                    messages,
                },
            )?],
            Ok(Some(TraeSessionSelection::Sessions(container))) => {
                let mut values = TraeJsonContainerValues::new(&bytes, container)?;
                let mut sessions = Vec::new();
                let mut session_index = 0_usize;
                while let Some(range) = values.next_range()? {
                    if let Some(session) = trae_stream_session(&bytes, range, session_index)? {
                        sessions.push(session_plan(&bytes, session)?);
                    }
                    session_index = session_index.saturating_add(1);
                }
                sessions
            }
            Err(error) => {
                return Ok(TraeLoadedKey::Rejected(format!(
                    "Trae ItemTable key `{chat_key}` cannot be decoded: {error}"
                )));
            }
        };
        if !self.authority.snapshot.revalidate(&self.authority.path)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        Ok(TraeLoadedKey::Active(TraeActiveKey {
            key_index,
            chat_key,
            record_digest: CompleteContentBodyDigest::from_bytes(&bytes),
            bytes,
            sessions,
        }))
    }

    fn advance_key(&mut self) -> Result<()> {
        self.frontier.key_index = self
            .frontier
            .key_index
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant("Trae key frontier exhausted"))?;
        self.frontier.session_index = 0;
        self.frontier.message_index = 0;
        self.active = None;
        Ok(())
    }

    fn advance_message(&mut self) -> Result<()> {
        self.frontier.message_index =
            self.frontier
                .message_index
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "Trae message frontier exhausted",
                ))?;
        Ok(())
    }
}

fn normalize_frontier(
    mut frontier: TraeFrontier,
    active: Option<&TraeActiveKey>,
) -> Result<TraeFrontier> {
    if frontier.is_terminal() {
        return Ok(TraeFrontier::terminal());
    }
    let Some(active) = active.filter(|active| active.key_index == frontier.key_index) else {
        return Ok(frontier);
    };
    loop {
        let session_index = usize::try_from(frontier.session_index).map_err(|_| {
            CaptureError::InvalidPayload("Trae session frontier exceeds platform limits".into())
        })?;
        let Some(session) = active.sessions.get(session_index) else {
            frontier.key_index = frontier
                .key_index
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant("Trae key frontier exhausted"))?;
            frontier.session_index = 0;
            frontier.message_index = 0;
            return Ok(if frontier.is_terminal() {
                TraeFrontier::terminal()
            } else {
                frontier
            });
        };
        if usize::try_from(frontier.message_index).unwrap_or(usize::MAX) < session.messages.len() {
            return Ok(frontier);
        }
        frontier.session_index =
            frontier
                .session_index
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "Trae session frontier exhausted",
                ))?;
        frontier.message_index = 0;
    }
}

fn session_plan(bytes: &[u8], session: TraeStreamSession) -> Result<TraeSessionPlan> {
    let mut values = TraeJsonArrayValues::new(bytes, session.messages.clone())?;
    let mut messages = Vec::new();
    while let Some(range) = values.next_range()? {
        messages.push(range);
    }
    Ok(TraeSessionPlan { session, messages })
}

fn session_fact(
    provider_session_id: &str,
    chat_key: &'static str,
    session: &TraeStreamSession,
    event: &TraeEventInput,
    title_eligible: bool,
) -> TraeSessionFact {
    let generated = title_eligible
        .then(|| {
            event
                .text
                .replace('\n', " ")
                .chars()
                .take(50)
                .collect::<String>()
        })
        .filter(|title| !title.trim().is_empty());
    TraeSessionFact {
        provider_session_id: provider_session_id.to_owned(),
        native_session_id: session.native_session_id.clone(),
        chat_key,
        metadata_preview: trae_session_metadata_preview(&session.metadata_preview),
        started_at: session.explicit_started_at.unwrap_or(event.occurred_at),
        ended_at: session.explicit_ended_at.or(Some(event.occurred_at)),
        title: session.explicit_title.clone().or(generated),
    }
}

fn merge_session_fact(current: &mut TraeSessionFact, next: &TraeSessionFact) {
    current.started_at = current.started_at.min(next.started_at);
    current.ended_at = match (current.ended_at, next.ended_at) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (left, right) => left.or(right),
    };
    if current.title.is_none() {
        current.title.clone_from(&next.title);
    }
}

fn classify_output(message: &Value) -> bool {
    fn normalized(value: &str) -> String {
        value
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    }
    for field in ["role", "type", "kind", "messageType", "message_type"] {
        if message
            .get(field)
            .and_then(Value::as_str)
            .map(normalized)
            .is_some_and(|value| {
                matches!(
                    value.as_str(),
                    "tool"
                        | "toolresult"
                        | "tooloutput"
                        | "functionresult"
                        | "functionoutput"
                        | "commandresult"
                        | "commandoutput"
                )
            })
        {
            return true;
        }
    }
    let Value::Object(object) = message else {
        return false;
    };
    object.keys().any(|key| {
        matches!(
            normalized(key).as_str(),
            "toolresult"
                | "tooloutput"
                | "functionresult"
                | "functionoutput"
                | "commandresult"
                | "commandoutput"
        )
    }) || (object.keys().any(|key| {
        matches!(
            normalized(key).as_str(),
            "toolcallid" | "tooluseid" | "callid"
        )
    }) && object
        .keys()
        .any(|key| matches!(normalized(key).as_str(), "result" | "output")))
}

fn is_failure_or_timeout(message: &Value) -> bool {
    provider_output_event_is_failure(message)
}

fn output_outcome(message: &Value) -> OutputOutcomeMetadata {
    let evidence = provider_result_outcome_evidence(EventType::ToolOutput, message);
    let timeout = recursively_has_timeout(message, &mut 4096);
    let outcome = if timeout {
        OutputOutcome::Timeout
    } else {
        match evidence.as_str() {
            Some("success") => OutputOutcome::Success,
            Some("failure") => OutputOutcome::Failure,
            _ => OutputOutcome::Unknown,
        }
    };
    let exit_code =
        find_i64(message, &["exit_code", "exitCode"]).and_then(|value| i32::try_from(value).ok());
    let duration_ms = find_i64(message, &["duration_ms", "durationMs"])
        .and_then(|value| u64::try_from(value).ok());
    OutputOutcomeMetadata {
        outcome,
        exit_code,
        duration_ms,
    }
}

fn recursively_has_timeout(value: &Value, remaining: &mut usize) -> bool {
    if *remaining == 0 {
        return false;
    }
    *remaining -= 1;
    match value {
        Value::Object(object) => object.iter().any(|(key, value)| {
            (matches!(key.as_str(), "timed_out" | "timedOut" | "timeout")
                && value.as_bool() == Some(true))
                || (matches!(key.as_str(), "status" | "state" | "outcome")
                    && value
                        .as_str()
                        .is_some_and(|value| value.to_ascii_lowercase().contains("timeout")))
                || recursively_has_timeout(value, remaining)
        }),
        Value::Array(values) => values
            .iter()
            .any(|value| recursively_has_timeout(value, remaining)),
        _ => false,
    }
}

fn find_i64(value: &Value, keys: &[&str]) -> Option<i64> {
    match value {
        Value::Object(object) => {
            for key in keys {
                if let Some(value) = object.get(*key).and_then(Value::as_i64) {
                    return Some(value);
                }
            }
            object.values().find_map(|value| find_i64(value, keys))
        }
        Value::Array(values) => values.iter().find_map(|value| find_i64(value, keys)),
        _ => None,
    }
}

fn output_row(
    provider_session_id: &str,
    frontier: TraeFrontier,
    byte_range: Range<usize>,
    event: &TraeEventInput,
    cwd: Option<&str>,
) -> TraeOutputRow {
    let call_id = task_json_string_field(
        &event.raw_message,
        &[
            "toolCallId",
            "tool_call_id",
            "callId",
            "call_id",
            "toolUseId",
        ],
    );
    let tool_name = task_json_string_field(
        &event.raw_message,
        &["toolName", "tool_name", "name", "functionName"],
    );
    let command = task_json_string_field(
        &event.raw_message,
        &["command", "cmd", "input", "toolInput"],
    );
    TraeOutputRow {
        provider_session_id: provider_session_id.to_owned(),
        key_index: frontier.key_index,
        session_index: frontier.session_index,
        message_index: frontier.message_index,
        native_message_id: event.native_message_id.clone(),
        occurred_at: event.occurred_at,
        call_id,
        command: command.map(|command| OutputCommandContext {
            tool_name: tool_name.unwrap_or_else(|| "trae-tool".to_owned()),
            command,
            working_directory: cwd.map(str::to_owned),
        }),
        outcome: output_outcome(&event.raw_message),
        byte_range,
        content: event.text.as_bytes().to_vec(),
    }
}

fn output_row_bytes(row: &TraeOutputRow) -> usize {
    row.provider_session_id
        .len()
        .saturating_add(row.native_message_id.len())
        .saturating_add(row.call_id.as_deref().map_or(0, str::len))
        .saturating_add(
            row.command
                .as_ref()
                .map_or(0, |command| command.tool_name.len() + command.command.len()),
        )
        .saturating_add(row.content.len())
        .saturating_add(1024)
}

fn sparse_failure_event(
    provider_session_id: &str,
    workspace_id: &str,
    chat_key: &str,
    event: &TraeEventInput,
) -> TraeCoreEvent {
    let outcome = output_outcome(&event.raw_message);
    let call_id = task_json_string_field(
        &event.raw_message,
        &[
            "toolCallId",
            "tool_call_id",
            "callId",
            "call_id",
            "toolUseId",
        ],
    );
    let event_id = format!("{provider_session_id}:{}", event.native_message_id);
    TraeCoreEvent {
        provider_event_index: event.provider_event_index,
        provider_event_hash: event_id.clone(),
        cursor: format!("{chat_key}:{event_id}"),
        event_type: EventType::ToolOutput,
        role: Some(EventRole::Tool),
        occurred_at: event.occurred_at,
        fidelity: Fidelity::Partial,
        idempotency_key: format!("provider-event:trae:{TRAE_STATE_VSCDB_SOURCE_FORMAT}:{event_id}"),
        payload: json!({
            "event_id": event_id,
            "native_workspace_id": workspace_id,
            "native_message_id": event.native_message_id,
            "result_outcome": "failure",
            "exit_code": outcome.exit_code,
            "duration_ms": outcome.duration_ms,
            "timed_out": outcome.outcome == OutputOutcome::Timeout,
            "call_id": call_id,
            "output_bytes": event.text.len(),
            "artifacts": [],
        }),
        metadata: json!({
            "source": "trae_state_vscdb_itemtable",
            "source_format": TRAE_STATE_VSCDB_SOURCE_FORMAT,
            "chat_key": chat_key,
            "native_message_id": event.native_message_id,
            "output_body_retained": false,
        }),
    }
}

fn core_event_bytes(event: &TraeCoreEvent) -> usize {
    serde_json::to_vec(&event.payload)
        .map_or(usize::MAX / 2, |value| value.len())
        .saturating_add(
            serde_json::to_vec(&event.metadata).map_or(usize::MAX / 2, |value| value.len()),
        )
        .saturating_add(2048)
}

fn attach_trae_complete_content_locator(
    event: &mut TraeCoreEvent,
    locator: &NativeLocator,
    record_digest: &CompleteContentBodyDigest,
    complete_text: &str,
) -> Result<()> {
    if event.event_type != EventType::Message
        || event
            .payload
            .pointer("/text_retention/truncated")
            .and_then(Value::as_bool)
            != Some(true)
    {
        return Ok(());
    }
    let content_ref = ContentRef::from_bytes(complete_text.as_bytes()).ok_or(
        CaptureError::SystemInvariant("SQLite content length exceeds ContentRef bounds"),
    )?;
    let profile = verified_content_profile(
        CaptureProvider::Trae,
        TRAE_STATE_VSCDB_SOURCE_FORMAT,
        CompleteContentSourceFamily::Sqlite,
        VerifiedContentRole::MessageBody,
    )
    .ok_or(CaptureError::SystemInvariant(
        "supported Trae message route has no verified-content profile",
    ))?;
    let persisted = VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        profile,
        content_ref,
        CompleteContentSourceFamily::Sqlite,
        locator.kind(),
        locator.value(),
        event.provider_event_hash.clone(),
        record_digest.clone(),
    )
    .ok_or(CaptureError::SystemInvariant(
        "Trae complete-content locator exceeds the bounded canonical schema",
    ))?;
    attach_verified_content_locator(&mut event.metadata, persisted).ok_or(
        CaptureError::SystemInvariant("Trae verified-content locator collection is malformed"),
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn publish_core_page(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    authority: &TraeSourceAuthority,
    page: &TraeScanPage,
    summary: &mut ProviderImportSummary,
) -> Result<TraeRouteState> {
    let resolution =
        group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::Trae,
            source_format: TRAE_STATE_VSCDB_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity: authority.locator_identity.clone(),
            cursor_stream: authority.cursor_stream.clone(),
            proposed_source_identity: authority.proposed_source_identity.clone(),
            raw_source_path: Some(authority.raw_source_path.clone()),
            source_revision: authority.source_revision.clone(),
            observed_at_ms: context.imported_at.timestamp_millis(),
        })?;
    let mut sessions = BTreeMap::new();
    for fact in page.sessions.values() {
        let existing_source = committed_store.capture_source_by_canonical_identity_session(
            CaptureProvider::Trae,
            TRAE_STATE_VSCDB_SOURCE_FORMAT,
            &context.machine_id,
            &resolution.canonical_source_identity,
            &fact.provider_session_id,
        )?;
        let source_id = existing_source
            .as_ref()
            .map(|source| source.id)
            .unwrap_or_else(|| {
                provider_scoped_source_uuid(
                    CaptureProvider::Trae,
                    &fact.provider_session_id,
                    TRAE_STATE_VSCDB_SOURCE_FORMAT,
                    Some(&authority.raw_source_path),
                )
            });
        group.upsert_capture_source(&CaptureSource {
            id: source_id,
            descriptor: CaptureSourceDescriptor {
                kind: CaptureSourceKind::ProviderImport,
                provider: CaptureProvider::Trae,
                machine_id: context.machine_id.clone(),
                process_id: None,
                cwd: authority.workspace_folder.clone(),
                raw_source_path: Some(authority.raw_source_path.clone()),
                source_format: Some(TRAE_STATE_VSCDB_SOURCE_FORMAT.to_owned()),
                source_root: Some(authority.source_root.display().to_string()),
                source_identity: Some(resolution.canonical_source_identity.clone()),
                external_session_id: Some(fact.provider_session_id.clone()),
            },
            started_at: fact.started_at,
            ended_at: fact.ended_at,
            sync: provider_sync_metadata(
                Fidelity::Imported,
                json!({
                    "provider_session_id": fact.provider_session_id,
                    "source_format": TRAE_STATE_VSCDB_SOURCE_FORMAT,
                    "source_trust": "provider_native",
                    "imported_at": context.imported_at,
                    "source_identity": resolution.canonical_source_identity,
                    "source_root": authority.source_root,
                    "source_revision": authority.source_revision,
                    "source_identity_key": provider_scoped_source_identity_key(
                        CaptureProvider::Trae,
                        &fact.provider_session_id,
                        TRAE_STATE_VSCDB_SOURCE_FORMAT,
                        Some(&authority.raw_source_path),
                    ),
                    "nativepath_publication": TRAE_NATIVE_PARSER_REVISION,
                    "inventory_observation_token": options.inventory_observation_token,
                }),
            ),
        })?;
        group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;
        let session_id = provider_import_session_uuid(
            committed_store,
            CaptureProvider::Trae,
            &fact.provider_session_id,
            source_id,
            Some(&resolution.canonical_source_identity),
        )?;
        let existed = committed_store.get_session(session_id).is_ok();
        let session = Session {
            id: session_id,
            history_record_id: options.history_record_id,
            parent_session_id: None,
            root_session_id: None,
            capture_source_id: Some(source_id),
            provider: CaptureProvider::Trae,
            external_session_id: Some(fact.provider_session_id.clone()),
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("primary".to_owned()),
            is_primary: true,
            status: SessionStatus::Imported,
            transcript_blob_id: None,
            started_at: fact.started_at,
            ended_at: fact.ended_at,
            timestamps: timestamps(context.imported_at),
            sync: provider_sync_metadata(
                Fidelity::Imported,
                json!({
                    "provider_session_id": fact.provider_session_id,
                    "source_format": TRAE_STATE_VSCDB_SOURCE_FORMAT,
                    "source_trust": "provider_native",
                    "imported_at": context.imported_at,
                    "session_idempotency_key": format!(
                        "provider-session:trae:{}",
                        fact.provider_session_id
                    ),
                    "metadata": {
                        "display_name": "Trae",
                        "title": fact.title,
                        "native_workspace_id": authority.workspace_id,
                        "native_session_id": fact.native_session_id,
                        "workspace_folder": authority.workspace_folder,
                        "chat_key": fact.chat_key,
                        "session": fact.metadata_preview,
                        "nativepath_publication": TRAE_NATIVE_PARSER_REVISION,
                    },
                }),
            ),
        };
        group.upsert_session(&session)?;
        if existed {
            summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
        } else {
            summary.imported_sessions = summary.imported_sessions.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        }
        sessions.insert(fact.provider_session_id.clone(), (session, source_id));
    }

    for record in &page.core {
        let (session, source_id) =
            sessions
                .get(&record.provider_session_id)
                .ok_or(CaptureError::SystemInvariant(
                    "Trae Core record has no page session",
                ))?;
        let provider_event_sequence_index =
            packed_native_index(record.key_index, record.session_index, record.message_index)?;
        let event_hash = record.event.provider_event_hash.as_str();
        let provider_event_index = native_event_index(
            record.key_index,
            record.session_index,
            record.message_index,
            event_hash,
        );
        let identity = provider_event_import_identity_with_exact_legacy_source(
            committed_store,
            CaptureProvider::Trae,
            &record.provider_session_id,
            *source_id,
            provider_event_index,
            provider_event_sequence_index,
            event_hash,
            None,
            Some(u64::from(record.message_index)),
            true,
        )?;
        let dedupe_key =
            Store::provider_event_dedupe_key_with_payload_hash(&identity.dedupe_key, event_hash)
                .unwrap_or(identity.dedupe_key);
        let event = Event {
            id: identity.id,
            seq: identity.seq,
            history_record_id: options.history_record_id,
            session_id: Some(session.id),
            run_id: None,
            event_type: record.event.event_type,
            role: record.event.role,
            occurred_at: record.event.occurred_at,
            capture_source_id: Some(*source_id),
            payload: record.event.payload.clone(),
            payload_blob_id: None,
            dedupe_key: Some(dedupe_key),
            sync: provider_sync_metadata(
                record.event.fidelity,
                json!({
                    "provider_session_id": record.provider_session_id,
                    "provider_event_index": provider_event_index,
                    "provider_event_hash": event_hash,
                    "provider_event_hash_authority": "provider_supplied",
                    "source_format": TRAE_STATE_VSCDB_SOURCE_FORMAT,
                    "source_trust": "provider_native",
                    "source_record_ordinal": record.key_index,
                    "source_record_subrecord_index": record.message_index,
                    "native_session_index": record.session_index,
                    "metadata": record.event.metadata,
                }),
            ),
        };
        if group.reconcile_provider_event(&event, ProviderEventHashAuthority::ProviderSupplied)? {
            summary.imported_events = summary.imported_events.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        } else {
            summary.skipped_events = summary.skipped_events.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
        }
    }
    Ok(TraeRouteState {
        path: authority.path.clone(),
        locator_identity: authority.locator_identity.clone(),
        cursor_stream: authority.cursor_stream.clone(),
        canonical_source_identity: resolution.canonical_source_identity,
        source_revision: authority.source_revision.clone(),
    })
}

fn page_publication_id(
    authority: &TraeSourceAuthority,
    page: &TraeScanPage,
    generation: u64,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-trae-nativepath-page-v1\0");
    digest.update(authority.locator_identity.as_bytes());
    digest.update(authority.source_revision.as_bytes());
    digest.update(generation.to_le_bytes());
    digest.update(serde_json::to_vec(&page.expected).unwrap_or_default());
    digest.update(serde_json::to_vec(&page.next).unwrap_or_default());
    for record in &page.core {
        digest.update(record.provider_session_id.as_bytes());
        digest.update(record.key_index.to_le_bytes());
        digest.update(record.session_index.to_le_bytes());
        digest.update(record.message_index.to_le_bytes());
        digest.update(record.event.provider_event_hash.as_bytes());
        digest.update(serde_json::to_vec(&record.event.payload).unwrap_or_default());
    }
    for rejection in &page.rejections {
        digest.update(rejection.line.to_le_bytes());
        digest.update(rejection.error.as_bytes());
    }
    digest.update(transition.next().cursor.as_bytes());
    format!("trae-nativepath-page-v1:{:x}", digest.finalize())
}

fn packed_native_index(key: u16, session: u32, message: u32) -> Result<u64> {
    if session > 0x00ff_ffff || message > 0x00ff_ffff {
        return Err(CaptureError::InvalidPayload(
            "Trae native message coordinate exceeds packed identity bounds".into(),
        ));
    }
    Ok((u64::from(key) << 48) | (u64::from(session) << 24) | u64::from(message))
}

fn native_event_index(key: u16, session: u32, message: u32, event_hash: &str) -> u64 {
    let mut digest = Sha256::new();
    digest.update(b"ctx-trae-native-event-index-v1\0");
    digest.update(key.to_le_bytes());
    digest.update(session.to_le_bytes());
    digest.update(message.to_le_bytes());
    digest.update(event_hash.as_bytes());
    let digest = digest.finalize();
    u64::from_be_bytes(
        digest[..8]
            .try_into()
            .expect("SHA-256 prefix is eight bytes"),
    )
}

fn provider_sync_cursor(
    machine_id: &str,
    stream: String,
    cursor: String,
    observed_at: DateTime<Utc>,
) -> SyncCursor {
    SyncCursor {
        id: stable_capture_uuid(
            &format!(
                "provider-cursor:{}:{}:{}",
                CaptureProvider::Trae.as_str(),
                machine_id,
                stream
            ),
            "provider-sync-cursor",
        ),
        team_id: None,
        device_id: machine_id.to_owned(),
        stream,
        cursor,
        last_synced_at: Some(observed_at),
        timestamps: timestamps(observed_at),
    }
}

fn root_cursor_stream(path: &Path) -> Result<String> {
    let identity = provider_path_identity(path)?;
    Ok(provider_source_cursor_stream_for_path(
        CaptureProvider::Trae,
        TRAE_ROOT_SOURCE_FORMAT,
        &identity,
    ))
}

fn load_root_manifest(
    store: &Store,
    machine_id: &str,
    stream: &str,
) -> Result<Option<TraeRootManifest>> {
    let Some(stored) = store.get_sync_cursor(None, machine_id, stream)? else {
        return Ok(None);
    };
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    let manifest: TraeRootManifest = serde_json::from_str(committed.provider_cursor())
        .map_err(|_| CaptureError::InvalidPayload("Trae root manifest is corrupt".into()))?;
    if manifest.version != TRAE_ROOT_CURSOR_VERSION {
        return Err(CaptureError::InvalidPayload(
            "Trae root manifest version is unsupported".into(),
        ));
    }
    Ok(Some(manifest))
}

fn publish_root_manifest(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    stream: &str,
    manifest: &TraeRootManifest,
) -> Result<bool> {
    let encoded = serde_json::to_string(manifest)?;
    let stored = store.get_sync_cursor(None, &context.machine_id, stream)?;
    if let Some(stored) = &stored {
        let committed = decode_native_path_committed_cursor(&stored.cursor)?;
        if committed.provider_cursor() == encoded {
            return Ok(false);
        }
    }
    let transition = NativePathCursorTransition::new(
        stored.as_ref().map(|cursor| cursor.cursor.clone()),
        provider_sync_cursor(
            &context.machine_id,
            stream.to_owned(),
            encoded,
            context.imported_at,
        ),
    );
    let mut digest = Sha256::new();
    digest.update(b"ctx-trae-nativepath-root-v1\0");
    digest.update(transition.next().cursor.as_bytes());
    let publication_id = format!("trae-nativepath-root-v1:{:x}", digest.finalize());
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let accounting = NativePathGroupAccounting::new(1, 1, transition.next().cursor.len().max(1))?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    let changed = matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllExpected
    );
    if changed {
        group.prepare_journal_checkpoint()?;
        group.publish_cursor_set()?;
    }
    group.commit()?;
    Ok(changed)
}

fn retire_missing_route(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    route: &TraeRouteState,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<bool> {
    let stored = store
        .get_sync_cursor(None, &context.machine_id, &route.cursor_stream)?
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Trae root manifest references a missing source cursor".into(),
            )
        })?;
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    let cursor = TraeNativeCursor::decode(committed.provider_cursor())?;
    if cursor.locator_identity != route.locator_identity
        || cursor.canonical_source_identity != route.canonical_source_identity
    {
        return Err(CaptureError::InvalidPayload(
            "Trae retirement route no longer matches its committed cursor".into(),
        ));
    }
    let retirement = ProviderSourceRouteRetirement {
        provider: CaptureProvider::Trae,
        source_format: TRAE_STATE_VSCDB_SOURCE_FORMAT.to_owned(),
        machine_id: context.machine_id.clone(),
        locator_identity: route.locator_identity.clone(),
        cursor_stream: route.cursor_stream.clone(),
        expected_canonical_source_identity: route.canonical_source_identity.clone(),
        expected_source_revision: route.source_revision.clone(),
        retired_at_ms: context.imported_at.timestamp_millis(),
        reason,
    };
    let transition = NativePathCursorTransition::new(
        Some(stored.cursor),
        provider_sync_cursor(
            &context.machine_id,
            route.cursor_stream.clone(),
            committed.provider_cursor().to_owned(),
            context.imported_at,
        ),
    );
    let mut digest = Sha256::new();
    digest.update(b"ctx-trae-nativepath-retirement-v1\0");
    digest.update(route.locator_identity.as_bytes());
    digest.update(route.canonical_source_identity.as_bytes());
    digest.update(route.source_revision.as_bytes());
    digest.update(format!("{:?}", reason).as_bytes());
    let publication_id = format!("trae-nativepath-retirement-v1:{:x}", digest.finalize());
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store
        .begin_native_path_publication_group(admission, NativePathGroupAccounting::new(0, 1, 0)?)?;
    let changed =
        match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
            NativePathCursorSetClassification::AllNextSameGroup { .. } => false,
            NativePathCursorSetClassification::AllExpected => {
                let disposition = group.retire_provider_source_route(&retirement)?;
                group.prepare_journal_checkpoint()?;
                group.publish_cursor_set()?;
                matches!(
                    disposition,
                    ProviderSourceRouteRetirementDisposition::Retired
                )
            }
        };
    group.commit()?;
    Ok(changed)
}

fn replay_source_outputs_or_mark_behind(
    path: &Path,
    source_root: &Path,
    context: &ProviderAdapterContext,
    store: &Store,
    sink: &dyn ProOutputSink,
) {
    if let Err(error) = replay_source_outputs(path, source_root, context, store, sink) {
        sink.mark_behind(ProOutputSinkError::new(
            "trae_nativepath_output_replay",
            error.to_string(),
        ));
    }
}

fn replay_source_outputs(
    path: &Path,
    source_root: &Path,
    context: &ProviderAdapterContext,
    store: &Store,
    sink: &dyn ProOutputSink,
) -> Result<()> {
    let (authority, conn) = acquire_source(path, source_root, context.imported_at)?;
    let stored = load_source_cursor(store, &context.machine_id, &authority.cursor_stream)?;
    let StoredTraeCursor::Native { cursor, .. } = stored else {
        return Err(CaptureError::InvalidPayload(
            "Trae output replay requires committed NativePath Core".into(),
        ));
    };
    if !cursor.terminal
        || cursor.source_revision != authority.source_revision
        || cursor.canonical_source_identity != authority.proposed_source_identity
    {
        return Err(CaptureError::InvalidPayload(
            "Trae output replay source does not match committed Core authority".into(),
        ));
    }
    let output_source = OutputSourceIdentity {
        provider: CaptureProvider::Trae.as_str().to_owned(),
        namespace_id: source_root.display().to_string(),
        source_id: authority.proposed_source_identity.clone(),
    };
    let progress = sink.observe_source(&output_source).map_err(|error| {
        CaptureError::InvalidPayload(format!("Trae output sink observation failed: {error}"))
    })?;
    let (start, mut state) = TraeOutputState::new(
        output_source,
        progress,
        &authority,
        sink.materializer_revision(),
    )?;
    if state.terminal_noop {
        return Ok(());
    }
    let mut scanner = TraeScanner::new(&conn, &authority, start);
    while let Some(page) = scanner.next_page(false, true)? {
        if !authority.snapshot.revalidate(path)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let expected_frontier = output_frontier(page.expected)?;
        let next_frontier = output_frontier(page.next)?;
        let observations = page
            .outputs
            .into_iter()
            .map(|row| output_observation(&authority, row))
            .collect::<Result<Vec<_>>>()?;
        let accounting = NativePageAccounting {
            logical_units: page.logical_units.max(1),
            conservative_serialized_bytes: page.estimated_bytes.max(1),
        };
        let output = NativeProOutputPage {
            inventory_generation: sink.inventory_generation(),
            source: state.source.clone(),
            source_epoch: state.source_epoch,
            observed_revision: authority.source_revision.clone(),
            parser_revision: TRAE_OUTPUT_PARSER_REVISION.to_owned(),
            materializer_revision: sink.materializer_revision().to_owned(),
            disposition: state.disposition,
            expected_prior_source_epoch: state.expected_source_epoch,
            expected_prior_frontier: state.expected_sink_frontier.clone(),
            observations,
        };
        let replay = NativeProReplayPage::new_with_source_identity(
            NativeSourceIdentity::new(
                CaptureProvider::Trae.as_str(),
                &authority.proposed_source_identity,
            ),
            expected_frontier,
            next_frontier.clone(),
            page.terminal,
            accounting,
            output,
        )
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        if let Err(error) = process_pro_replay_only(replay, sink) {
            sink.mark_behind(ProOutputSinkError::new(
                "trae_nativepath_output_page",
                format!("{:?}", error.output_error),
            ));
            break;
        }
        state.expected_source_epoch = Some(state.source_epoch);
        state.expected_sink_frontier = Some(next_frontier);
        state.disposition = ProOutputSourceDisposition::AppendOrResume;
    }
    Ok(())
}

struct TraeOutputState {
    source: OutputSourceIdentity,
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_sink_frontier: Option<NativeSafeFrontier>,
    disposition: ProOutputSourceDisposition,
    terminal_noop: bool,
}

impl TraeOutputState {
    fn new(
        source: OutputSourceIdentity,
        progress: Option<ProOutputProgress>,
        authority: &TraeSourceAuthority,
        materializer_revision: &str,
    ) -> Result<(TraeFrontier, Self)> {
        let Some(progress) = progress else {
            return Ok((
                TraeFrontier::default(),
                Self {
                    source,
                    source_epoch: 0,
                    expected_source_epoch: None,
                    expected_sink_frontier: None,
                    disposition: ProOutputSourceDisposition::NewSource,
                    terminal_noop: false,
                },
            ));
        };
        let prior_frontier = progress
            .cursor
            .as_ref()
            .map(|cursor| NativeSafeFrontier::new(cursor.version, cursor.payload.clone()))
            .transpose()
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        let decoded = progress
            .cursor
            .as_ref()
            .filter(|cursor| cursor.version == TRAE_OUTPUT_FRONTIER_VERSION)
            .and_then(|cursor| serde_json::from_slice::<TraeFrontier>(&cursor.payload).ok());
        let can_resume = progress.parser_revision == TRAE_OUTPUT_PARSER_REVISION
            && progress.materializer_revision == materializer_revision
            && progress.observed_revision == authority.source_revision
            && decoded.is_some();
        let terminal_noop =
            can_resume && progress.terminal && decoded.is_some_and(TraeFrontier::is_terminal);
        let rewrite = !can_resume;
        let source_epoch = if rewrite {
            progress
                .source_epoch
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "Trae output source epoch exhausted",
                ))?
        } else {
            progress.source_epoch
        };
        Ok((
            if can_resume {
                decoded.unwrap_or_default()
            } else {
                TraeFrontier::default()
            },
            Self {
                source,
                source_epoch,
                expected_source_epoch: Some(progress.source_epoch),
                expected_sink_frontier: prior_frontier,
                disposition: if rewrite {
                    ProOutputSourceDisposition::Rewrite
                } else {
                    ProOutputSourceDisposition::AppendOrResume
                },
                terminal_noop,
            },
        ))
    }
}

fn output_frontier(frontier: TraeFrontier) -> Result<NativeSafeFrontier> {
    NativeSafeFrontier::new(TRAE_OUTPUT_FRONTIER_VERSION, serde_json::to_vec(&frontier)?)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

fn output_observation(
    authority: &TraeSourceAuthority,
    row: TraeOutputRow,
) -> Result<ProOutputObservation> {
    let native_sequence = packed_native_index(row.key_index, row.session_index, row.message_index)?;
    Ok(ProOutputObservation {
        kind: row
            .command
            .as_ref()
            .map_or(OutputObservationKind::Tool, |_| {
                OutputObservationKind::Command
            }),
        coordinate: OutputNativeCoordinate {
            unit_key: format!(
                "{}:{}:{}",
                row.key_index, row.session_index, row.message_index
            ),
            native_sequence,
            native_record_id: Some(row.native_message_id),
            source_record_ordinal: Some(u64::from(row.key_index)),
            source_record_subrecord_index: Some(row.message_index),
            byte_start: Some(u64::try_from(row.byte_range.start).unwrap_or(u64::MAX)),
            byte_end_exclusive: Some(u64::try_from(row.byte_range.end).unwrap_or(u64::MAX)),
        },
        occurred_at_unix_ms: Some(row.occurred_at.timestamp_millis()),
        associations: OutputAssociations {
            direct_session_id: row.provider_session_id.clone(),
            root_session_id: row.provider_session_id.clone(),
            parent_session_id: None,
            provider_session_id: Some(row.provider_session_id),
            agent_id: None,
            repository: None,
        },
        call_id: row.call_id,
        command: row.command,
        outcome: row.outcome,
        locator: OutputSourceLocator {
            version: 1,
            kind: "trae-itemtable-message-v1".to_owned(),
            payload: serde_json::to_vec(&json!({
                "path": authority.path,
                "source_revision": authority.source_revision,
                "key_index": row.key_index,
                "session_index": row.session_index,
                "message_index": row.message_index,
                "byte_start": row.byte_range.start,
                "byte_end_exclusive": row.byte_range.end,
            }))?,
        },
        content: row.content,
    })
}

#[cfg(test)]
#[path = "nativepath_tests.rs"]
mod tests;
