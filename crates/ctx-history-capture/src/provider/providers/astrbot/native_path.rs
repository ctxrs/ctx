use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
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
    StoreError,
};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    complete_content::{
        attach_verified_content_locator, verified_content_profile, CompleteContentBodyDigest,
        CompleteContentSourceFamily, VerifiedContentLocatorV1, VerifiedContentRole,
    },
    native_source::{NativeLocator, NativeSqliteValue},
    provider::{
        importer::{
            provider_event_import_identity_with_exact_legacy_source, provider_import_session_uuid,
            provider_path_identity, provider_scoped_source_identity_key,
            provider_scoped_source_uuid, provider_source_cursor_stream_for_path,
            provider_source_identity, provider_sync_metadata, timestamps, CertifiedProviderCursor,
        },
        normalization::{
            provider_capped_json, provider_json_text, provider_policy_body,
            provider_policy_event_text, provider_result_identifier_evidence,
            provider_result_outcome_evidence, provider_timestamp_millis, provider_value_text,
        },
        sqlite::{open_provider_sqlite_readonly, sqlite_schema_fingerprint},
    },
    CaptureError, CaptureWorkLimit, ImportProfile, OutputAssociations, OutputNativeCoordinate,
    OutputNativeCursor, OutputObservationKind, OutputOutcome, OutputOutcomeMetadata,
    OutputSourceIdentity, OutputSourceLocator, ProOutputMaterializationPage, ProOutputObservation,
    ProOutputSourceDisposition, ProviderAdapterContext, ProviderImportFailure,
    ProviderImportOptions, ProviderImportSummary, ProviderImportWorkResult, Result,
    ASTRBOT_SQLITE_SOURCE_FORMAT, MAX_PROVIDER_SQLITE_VALUE_BYTES, PROVIDER_MAX_PREVIEW_CHARS,
    PROVIDER_MAX_TEXT_CHARS,
};

use super::{
    model::{
        checkpoint_id, item_id, item_is_output, item_role, item_text, output_outcome,
        provider_session_id, ConversationRow, LegacyOrderKey, PlatformMessageLink,
        PlatformMessageRow,
    },
    preferences::astrbot_selected_conversation_bounded,
    source::{
        astrbot_source_revision, astrbot_source_snapshot, fetch_candidate, hydrate_conversation,
        hydrate_platform_message, AstrBotSql, RowCandidate,
    },
    ASTRBOT_CAPTURE_REVISION, ASTRBOT_POLICY_REVISION,
};

const CURSOR_VERSION: u32 = 1;
const FRONTIER_VERSION: u32 = 1;
const OUTPUT_CURSOR_VERSION: u32 = 1;
const PAGE_MAX_SOURCE_UNITS: usize = 64;
const PAGE_MAX_CORE_BYTES: usize = ctx_history_store::NATIVE_PATH_MAX_RETAINED_PAGE_BYTES;
const PAGE_MAX_OUTPUT_BYTES: usize =
    ctx_history_store::NATIVE_PATH_MAX_RETAINED_PAGE_BYTES - (256 * 1024);
const CURSOR_STREAM_REVISION: &str = "astrbot-nativepath-v1";
const OUTPUT_PARSER_REVISION: &str = "astrbot-nativepath-output-v1";
const PUBLICATION_PREFIX: &str = "astrbot-nativepath-page-v1:";
const RETIREMENT_PREFIX: &str = "astrbot-nativepath-retirement-v1:";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ScanPhase {
    Conversations,
    PlatformMessages,
    Complete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConversationInRow {
    physical_rowid: i64,
    row_sha256: [u8; 32],
    next_item_index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AstrBotFrontier {
    version: u32,
    phase: ScanPhase,
    conversation_after_rowid: Option<i64>,
    conversation_in_row: Option<ConversationInRow>,
    platform_after_rowid: Option<i64>,
    conversation_prefix_sha256: [u8; 32],
    platform_prefix_sha256: [u8; 32],
    last_conversation_order: Option<LegacyOrderKey>,
    last_platform_order: Option<LegacyOrderKey>,
    next_native_ordinal: u64,
}

impl AstrBotFrontier {
    fn initial() -> Self {
        Self {
            version: FRONTIER_VERSION,
            phase: ScanPhase::Conversations,
            conversation_after_rowid: None,
            conversation_in_row: None,
            platform_after_rowid: None,
            conversation_prefix_sha256: [0; 32],
            platform_prefix_sha256: [0; 32],
            last_conversation_order: None,
            last_platform_order: None,
            next_native_ordinal: 0,
        }
    }

    fn append_start(&self) -> Self {
        let mut frontier = self.clone();
        frontier.phase = ScanPhase::Conversations;
        frontier
    }

    fn terminal(&self) -> bool {
        self.phase == ScanPhase::Complete
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AstrBotStoreCursor {
    version: u32,
    provider: String,
    source_format: String,
    locator_identity: String,
    source_identity: String,
    source_revision: String,
    source_incarnation: String,
    schema_authority: String,
    frontier: AstrBotFrontier,
    terminal: bool,
    generation: u64,
    rejected_records: u64,
    retired: bool,
}

impl AstrBotStoreCursor {
    fn encode(&self) -> Result<String> {
        serde_json::to_string(self).map_err(CaptureError::from)
    }

    fn decode(encoded: &str) -> Result<Self> {
        let cursor: Self = serde_json::from_str(encoded)?;
        if cursor.version != CURSOR_VERSION
            || cursor.provider != CaptureProvider::AstrBot.as_str()
            || cursor.source_format != ASTRBOT_SQLITE_SOURCE_FORMAT
            || cursor.frontier.version != FRONTIER_VERSION
            || cursor.terminal != cursor.frontier.terminal()
        {
            return Err(CaptureError::InvalidPayload(
                "AstrBot NativePath cursor authority is invalid".to_owned(),
            ));
        }
        Ok(cursor)
    }
}

#[derive(Debug)]
// Cursor decoding keeps released and current wire shapes explicit; boxing
// would add allocation to this short-lived control-flow value.
#[allow(clippy::large_enum_variant)]
enum PriorCursor {
    None,
    Native {
        encoded: String,
        cursor: AstrBotStoreCursor,
    },
    Released {
        encoded: String,
        rejected_records: u64,
    },
}

#[derive(Debug, Clone)]
struct SourceAuthority {
    raw_source_path: String,
    source_root: String,
    locator_identity: String,
    cursor_stream: String,
    source_identity: String,
    source_revision: String,
    source_incarnation: String,
    schema_authority: String,
    user_version: i64,
    schema_fingerprint: String,
    selected_conversation: Option<String>,
}

#[derive(Debug, Clone)]
struct SessionFact {
    provider_session_id: String,
    external_agent_id: Option<String>,
    role_hint: &'static str,
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    metadata: Value,
    preserve_existing: bool,
}

#[derive(Debug)]
struct EventFact {
    provider_event_index: u64,
    legacy_provider_event_index: Option<u64>,
    event_type: EventType,
    role: Option<EventRole>,
    occurred_at: DateTime<Utc>,
    payload: Value,
    metadata: Value,
}

#[derive(Debug)]
struct CoreUnit {
    session: SessionFact,
    event: Option<EventFact>,
}

#[derive(Debug)]
struct OutputFact {
    observation: ProOutputObservation,
    estimated_bytes: usize,
}

#[derive(Debug)]
struct PageRejection {
    line: usize,
    detail: String,
}

#[derive(Debug)]
struct AstrBotPage {
    expected_frontier: AstrBotFrontier,
    next_frontier: AstrBotFrontier,
    terminal: bool,
    retained_core_bytes: usize,
    units: Vec<CoreUnit>,
    outputs: Vec<OutputFact>,
    rejections: Vec<PageRejection>,
}

struct ActiveConversation {
    physical_rowid: i64,
    order: LegacyOrderKey,
    row_sha256: [u8; 32],
    row: ConversationRow,
    items: Vec<Value>,
    next_item_index: usize,
    rejection: Option<String>,
}

struct AstrBotReader<'a> {
    conn: &'a Connection,
    sql: AstrBotSql,
    frontier: AstrBotFrontier,
    active_conversation: Option<ActiveConversation>,
    relationship_projection_ready: bool,
}

pub(super) fn import_astrbot_native_path(
    path: &Path,
    store: &mut Store,
    mut context: ProviderAdapterContext,
    options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    if context.source_path.is_none() {
        context.source_path = Some(path.to_path_buf());
    }
    if !path.exists() {
        return retire_missing_source(path, store, &context);
    }
    if let Some(expected) = options.inventory_observation_token.as_deref() {
        let current = crate::observe_ordinary_file(path)?;
        if current.token_hex() != expected {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
    }

    let snapshot = astrbot_source_snapshot(path)?;
    let canonical_path = fs::canonicalize(path)?;
    let conn = open_provider_sqlite_readonly(path)?;
    if !snapshot.revalidate(path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let sql = AstrBotSql::new(&conn)?;
    let user_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let schema_fingerprint = sqlite_schema_fingerprint(&conn)?;
    let selected_conversation = astrbot_selected_conversation_bounded(&conn)?;
    if !snapshot.revalidate(path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }

    let raw_source_path = context
        .source_path
        .as_deref()
        .unwrap_or(path)
        .display()
        .to_string();
    let configured_source_root = context
        .source_root
        .as_deref()
        .map(|root| root.display().to_string());
    let source_root = context
        .source_root
        .as_deref()
        .or_else(|| canonical_path.parent())
        .unwrap_or(&canonical_path)
        .display()
        .to_string();
    let locator_identity = provider_path_identity(&canonical_path)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::AstrBot,
        ASTRBOT_SQLITE_SOURCE_FORMAT,
        &locator_identity,
    );
    let source_identity = provider_source_identity(
        CaptureProvider::AstrBot,
        ASTRBOT_SQLITE_SOURCE_FORMAT,
        configured_source_root.as_deref(),
        Some(&raw_source_path),
        None,
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "AstrBot NativePath source has no canonical identity",
    ))?;
    let source_revision = astrbot_source_revision(&snapshot, user_version, &schema_fingerprint);
    let authority = SourceAuthority {
        raw_source_path,
        source_root,
        locator_identity,
        cursor_stream,
        source_identity,
        source_incarnation: source_incarnation(&source_revision),
        schema_authority: format!(
            "capture={ASTRBOT_CAPTURE_REVISION};policy={ASTRBOT_POLICY_REVISION};user_version={user_version};schema={schema_fingerprint}"
        ),
        source_revision,
        user_version,
        schema_fingerprint,
        selected_conversation,
    };

    let stored = store.get_sync_cursor(None, &context.machine_id, &authority.cursor_stream)?;
    let prior = decode_prior_cursor(stored)?;
    let core_terminal_unchanged = matches!(
        &prior,
        PriorCursor::Native { cursor, .. }
            if !cursor.retired
                && cursor.terminal
                && cursor.source_revision == authority.source_revision
                && cursor.schema_authority == authority.schema_authority
                && cursor.locator_identity == authority.locator_identity
                && cursor.source_identity == authority.source_identity
    );

    let summary = if options.import_profile.is_replay_only() {
        ProviderImportSummary::default()
    } else if core_terminal_unchanged {
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        summary
    } else {
        import_core(
            path, store, &conn, &sql, &snapshot, &authority, &context, &options, prior,
        )?
    };

    if matches!(
        options.import_profile,
        ImportProfile::CoreAndPro(_) | ImportProfile::ProReplayOnly(_)
    ) && !summary.work_remaining
    {
        if let Err(error) = replay_outputs(
            path,
            store,
            &conn,
            &snapshot,
            &authority,
            &context,
            &options.import_profile,
        ) {
            if let Some(sink) = options.import_profile.sink() {
                sink.mark_behind(crate::ProOutputSinkError::new(
                    "astrbot_nativepath_output_replay",
                    error.to_string(),
                ));
            }
        }
    }
    Ok(summary)
}

#[allow(clippy::too_many_arguments)]
fn import_core(
    path: &Path,
    store: &mut Store,
    conn: &Connection,
    sql: &AstrBotSql,
    snapshot: &crate::provider::sqlite::ProviderSqliteSourceSnapshot,
    authority: &SourceAuthority,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    prior: PriorCursor,
) -> Result<ProviderImportSummary> {
    let (start, generation, mut rejected_records, mut expected_encoded) =
        classify_core_start(conn, sql, authority, prior)?;
    let mut reader = AstrBotReader::new(conn, AstrBotSql::new(conn)?, start);
    let committed_store = Store::open_read_only(store.path())?;
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let mut summary = ProviderImportSummary::default();
        let mut changed_groups = 0_usize;
        let mut accounted_sessions = BTreeSet::new();
        while let Some(page) = reader.next_page(false)? {
            if !snapshot.revalidate(path)? {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            rejected_records = rejected_records
                .saturating_add(u64::try_from(page.rejections.len()).unwrap_or(u64::MAX));
            let next_cursor = AstrBotStoreCursor {
                version: CURSOR_VERSION,
                provider: CaptureProvider::AstrBot.as_str().to_owned(),
                source_format: ASTRBOT_SQLITE_SOURCE_FORMAT.to_owned(),
                locator_identity: authority.locator_identity.clone(),
                source_identity: authority.source_identity.clone(),
                source_revision: authority.source_revision.clone(),
                source_incarnation: authority.source_incarnation.clone(),
                schema_authority: authority.schema_authority.clone(),
                frontier: page.next_frontier.clone(),
                terminal: page.terminal,
                generation,
                rejected_records,
                retired: false,
            };
            let page_summary = publish_core_page(
                store,
                &committed_store,
                &bulk_guard,
                snapshot,
                path,
                authority,
                context,
                options,
                &page,
                expected_encoded.clone(),
                &next_cursor,
                &mut accounted_sessions,
            )?;
            if page_summary.work_result() == ProviderImportWorkResult::Changed {
                changed_groups = changed_groups.saturating_add(1);
            }
            summary.merge_from(page_summary);
            expected_encoded = store
                .get_sync_cursor(None, &context.machine_id, &authority.cursor_stream)?
                .map(|cursor| cursor.cursor);
            if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup
                && changed_groups != 0
                && !page.terminal
            {
                summary.work_remaining = true;
                break;
            }
        }
        Ok(summary)
    })();
    let finish = store
        .finish_event_search_bulk_mode(&bulk_guard)
        .map_err(CaptureError::from);
    match (operation, finish) {
        (Ok(summary), Ok(())) => Ok(summary),
        (_, Err(error)) => Err(error),
        (Err(error), Ok(())) => Err(error),
    }
}

fn classify_core_start(
    conn: &Connection,
    sql: &AstrBotSql,
    authority: &SourceAuthority,
    prior: PriorCursor,
) -> Result<(AstrBotFrontier, u64, u64, Option<String>)> {
    match prior {
        PriorCursor::None => Ok((AstrBotFrontier::initial(), 0, 0, None)),
        PriorCursor::Released {
            encoded,
            rejected_records,
        } => Ok((
            AstrBotFrontier::initial(),
            0,
            rejected_records,
            Some(encoded),
        )),
        PriorCursor::Native { encoded, cursor } => {
            if cursor.locator_identity != authority.locator_identity
                || cursor.source_identity != authority.source_identity
            {
                return Err(CaptureError::InvalidPayload(
                    "AstrBot NativePath cursor route does not match this source".to_owned(),
                ));
            }
            if cursor.retired {
                return Ok((
                    AstrBotFrontier::initial(),
                    cursor.generation.saturating_add(1),
                    0,
                    Some(encoded),
                ));
            }
            if cursor.schema_authority == authority.schema_authority
                && cursor.source_revision == authority.source_revision
                && !cursor.terminal
            {
                return Ok((
                    cursor.frontier,
                    cursor.generation,
                    cursor.rejected_records,
                    Some(encoded),
                ));
            }
            let same_incarnation = cursor.source_incarnation == authority.source_incarnation;
            let append_safe = same_incarnation
                && cursor.schema_authority == authority.schema_authority
                && validate_frontier(conn, sql, &cursor.frontier)?;
            if append_safe {
                return Ok((
                    cursor.frontier.append_start(),
                    cursor.generation,
                    cursor.rejected_records,
                    Some(encoded),
                ));
            }
            Ok((
                AstrBotFrontier::initial(),
                cursor
                    .generation
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "AstrBot NativePath generation exhausted",
                    ))?,
                0,
                Some(encoded),
            ))
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn publish_core_page(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    snapshot: &crate::provider::sqlite::ProviderSqliteSourceSnapshot,
    path: &Path,
    authority: &SourceAuthority,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    page: &AstrBotPage,
    expected_encoded: Option<String>,
    next_cursor: &AstrBotStoreCursor,
    accounted_sessions: &mut BTreeSet<String>,
) -> Result<ProviderImportSummary> {
    let provider_cursor = next_cursor.encode()?;
    let next = SyncCursor {
        id: Uuid::new_v4(),
        team_id: None,
        device_id: context.machine_id.clone(),
        stream: authority.cursor_stream.clone(),
        cursor: provider_cursor,
        last_synced_at: Some(context.imported_at),
        timestamps: timestamps(context.imported_at),
    };
    let transition = NativePathCursorTransition::new(expected_encoded, next);
    let publication_id = page_publication_id(authority, page, next_cursor)?;
    let retained_bytes = page
        .retained_core_bytes
        .saturating_add(transition.next().cursor.len())
        .min(PAGE_MAX_CORE_BYTES);
    let accounting = NativePathGroupAccounting::new(1, 1, retained_bytes)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
        NativePathCursorSetClassification::AllNextSameGroup { .. } => {
            group.commit()?;
            let mut summary = ProviderImportSummary::default();
            summary.skipped_sessions = page
                .units
                .iter()
                .filter(|unit| accounted_sessions.insert(unit.session.provider_session_id.clone()))
                .count();
            summary.skipped_events = page
                .units
                .iter()
                .filter(|unit| unit.event.is_some())
                .count();
            summary.skipped = summary
                .skipped_sessions
                .saturating_add(summary.skipped_events);
            summary.set_work_result(ProviderImportWorkResult::NoOp);
            return Ok(summary);
        }
        NativePathCursorSetClassification::AllExpected => {}
    }

    let resolution =
        group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::AstrBot,
            source_format: ASTRBOT_SQLITE_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity: authority.locator_identity.clone(),
            cursor_stream: authority.cursor_stream.clone(),
            proposed_source_identity: authority.source_identity.clone(),
            raw_source_path: Some(authority.raw_source_path.clone()),
            source_revision: authority.source_revision.clone(),
            observed_at_ms: context.imported_at.timestamp_millis(),
        })?;

    let mut summary = ProviderImportSummary::default();
    let mut sessions = BTreeMap::<String, (SessionFact, Uuid, Session)>::new();
    for unit in &page.units {
        if sessions.contains_key(&unit.session.provider_session_id) {
            continue;
        }
        let source_id = provider_scoped_source_uuid(
            CaptureProvider::AstrBot,
            &unit.session.provider_session_id,
            ASTRBOT_SQLITE_SOURCE_FORMAT,
            Some(&authority.raw_source_path),
        );
        let source = if unit.session.preserve_existing {
            match committed_store.get_capture_source(source_id) {
                Ok(existing) => existing,
                Err(StoreError::NotFound(_)) => capture_source(
                    authority,
                    context,
                    &unit.session,
                    source_id,
                    &resolution.canonical_source_identity,
                ),
                Err(error) => return Err(error.into()),
            }
        } else {
            capture_source(
                authority,
                context,
                &unit.session,
                source_id,
                &resolution.canonical_source_identity,
            )
        };
        group.upsert_capture_source(&source)?;
        group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;
        let session = session(
            committed_store,
            authority,
            context,
            options,
            &unit.session,
            source_id,
            &resolution.canonical_source_identity,
        )?;
        let existed = committed_store.get_session(session.id).is_ok();
        group.upsert_session(&session)?;
        if accounted_sessions.insert(unit.session.provider_session_id.clone()) {
            if existed {
                summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
                summary.skipped = summary.skipped.saturating_add(1);
            } else {
                summary.imported_sessions = summary.imported_sessions.saturating_add(1);
                summary.imported = summary.imported.saturating_add(1);
            }
        }
        sessions.insert(
            unit.session.provider_session_id.clone(),
            (unit.session.clone(), source_id, session),
        );
    }

    for unit in &page.units {
        let Some(fact) = &unit.event else {
            continue;
        };
        let (_, source_id, session) = sessions.get(&unit.session.provider_session_id).ok_or(
            CaptureError::SystemInvariant("AstrBot NativePath event lost its session"),
        )?;
        let normalized = normalized_event(
            committed_store,
            options,
            &unit.session,
            *source_id,
            session,
            fact,
        )?;
        if group.reconcile_provider_event(
            &normalized,
            ProviderEventHashAuthority::NormalizedPayloadFallback,
        )? {
            summary.imported_events = summary.imported_events.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        } else {
            summary.skipped_events = summary.skipped_events.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
        }
    }
    for rejection in &page.rejections {
        summary.record_failure(ProviderImportFailure {
            line: rejection.line,
            error: rejection.detail.clone(),
        });
    }

    if !snapshot.revalidate(path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

fn capture_source(
    authority: &SourceAuthority,
    context: &ProviderAdapterContext,
    fact: &SessionFact,
    source_id: Uuid,
    canonical_source_identity: &str,
) -> CaptureSource {
    CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::AstrBot,
            machine_id: context.machine_id.clone(),
            process_id: None,
            cwd: None,
            raw_source_path: Some(authority.raw_source_path.clone()),
            source_format: Some(ASTRBOT_SQLITE_SOURCE_FORMAT.to_owned()),
            source_root: Some(authority.source_root.clone()),
            source_identity: Some(canonical_source_identity.to_owned()),
            external_session_id: Some(fact.provider_session_id.clone()),
        },
        started_at: fact.started_at,
        ended_at: fact.ended_at,
        sync: provider_sync_metadata(
            Fidelity::Partial,
            json!({
                "adapter": ASTRBOT_SQLITE_SOURCE_FORMAT,
                "sqlite_user_version": authority.user_version,
                "schema_fingerprint": authority.schema_fingerprint,
                "support_level": "supported",
                "provider_session_id": fact.provider_session_id,
                "source_format": ASTRBOT_SQLITE_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "source_identity": canonical_source_identity,
                "source_root": authority.source_root,
                "source_revision": authority.source_revision,
                "source_identity_key": provider_scoped_source_identity_key(
                    CaptureProvider::AstrBot,
                    &fact.provider_session_id,
                    ASTRBOT_SQLITE_SOURCE_FORMAT,
                    Some(&authority.raw_source_path),
                ),
                "nativepath_publication": CURSOR_STREAM_REVISION,
            }),
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn session(
    committed_store: &Store,
    authority: &SourceAuthority,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    fact: &SessionFact,
    source_id: Uuid,
    canonical_source_identity: &str,
) -> Result<Session> {
    let id = provider_import_session_uuid(
        committed_store,
        CaptureProvider::AstrBot,
        &fact.provider_session_id,
        source_id,
        Some(canonical_source_identity),
    )?;
    if fact.preserve_existing {
        if let Ok(existing) = committed_store.get_session(id) {
            return Ok(existing);
        }
    }
    let mut session_metadata = fact.metadata.clone();
    if let Some(metadata) = session_metadata.as_object_mut() {
        metadata.insert(
            "selected_conversation".to_owned(),
            authority
                .selected_conversation
                .clone()
                .map_or(Value::Null, Value::String),
        );
    }
    Ok(Session {
        id,
        history_record_id: options.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::AstrBot,
        external_session_id: Some(fact.provider_session_id.clone()),
        external_agent_id: fact.external_agent_id.clone(),
        agent_type: AgentType::Primary,
        role_hint: Some(fact.role_hint.to_owned()),
        is_primary: true,
        status: if fact.ended_at.is_some() {
            SessionStatus::Completed
        } else {
            SessionStatus::Imported
        },
        transcript_blob_id: None,
        started_at: fact.started_at,
        ended_at: fact.ended_at,
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Partial,
            json!({
                "provider_session_id": fact.provider_session_id,
                "source_format": ASTRBOT_SQLITE_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "session_idempotency_key": format!(
                    "provider-session:{}:{}",
                    CaptureProvider::AstrBot.as_str(),
                    fact.provider_session_id
                ),
                "metadata": session_metadata,
                "nativepath_publication": CURSOR_STREAM_REVISION,
                "source_revision": authority.source_revision,
            }),
        ),
    })
}

fn normalized_event(
    committed_store: &Store,
    options: &ProviderImportOptions,
    session_fact: &SessionFact,
    source_id: Uuid,
    session: &Session,
    fact: &EventFact,
) -> Result<Event> {
    let event_hash = crate::compute_payload_hash(&fact.payload)?;
    let identity = provider_event_import_identity_with_exact_legacy_source(
        committed_store,
        CaptureProvider::AstrBot,
        &session_fact.provider_session_id,
        source_id,
        fact.provider_event_index,
        fact.provider_event_index,
        &event_hash,
        None,
        fact.legacy_provider_event_index,
        true,
    )?;
    let dedupe_key =
        Store::provider_event_dedupe_key_with_payload_hash(&identity.dedupe_key, &event_hash)
            .unwrap_or(identity.dedupe_key);
    let mut payload = fact.payload.clone();
    if let Some(object) = payload.as_object_mut() {
        object.insert(
            "provider".to_owned(),
            Value::String(CaptureProvider::AstrBot.as_str().to_owned()),
        );
        object.insert(
            "provider_session_id".to_owned(),
            Value::String(session_fact.provider_session_id.clone()),
        );
        object.insert(
            "provider_event_index".to_owned(),
            json!(fact.provider_event_index),
        );
        object.insert(
            "provider_event_hash".to_owned(),
            Value::String(event_hash.clone()),
        );
    }
    Ok(Event {
        id: identity.id,
        seq: identity.seq,
        history_record_id: options.history_record_id,
        session_id: Some(session.id),
        run_id: None,
        event_type: fact.event_type,
        role: fact.role,
        occurred_at: fact.occurred_at,
        capture_source_id: Some(source_id),
        payload,
        payload_blob_id: None,
        dedupe_key: Some(dedupe_key),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": session_fact.provider_session_id,
                "provider_event_index": fact.provider_event_index,
                "provider_event_hash": event_hash,
                "provider_event_hash_authority": "normalized_payload_fallback",
                "source_format": ASTRBOT_SQLITE_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "metadata": fact.metadata,
            }),
        ),
    })
}

impl<'a> AstrBotReader<'a> {
    fn new(conn: &'a Connection, sql: AstrBotSql, frontier: AstrBotFrontier) -> Self {
        Self {
            conn,
            sql,
            frontier,
            active_conversation: None,
            relationship_projection_ready: false,
        }
    }

    fn next_page(&mut self, collect_outputs: bool) -> Result<Option<AstrBotPage>> {
        if self.frontier.terminal() {
            return Ok(None);
        }
        let expected_frontier = self.frontier.clone();
        let mut units = Vec::new();
        let mut outputs = Vec::new();
        let mut rejections = Vec::new();
        let mut source_units = 0_usize;
        let mut core_bytes = 1024_usize;
        let mut output_bytes = 0_usize;

        loop {
            if source_units >= PAGE_MAX_SOURCE_UNITS {
                break;
            }
            match self.frontier.phase {
                ScanPhase::Conversations => {
                    if self.active_conversation.is_none() {
                        self.active_conversation = self.load_active_conversation()?;
                    }
                    let Some(active) = self.active_conversation.as_mut() else {
                        self.frontier.phase = ScanPhase::PlatformMessages;
                        if source_units != 0 {
                            break;
                        }
                        continue;
                    };
                    let item_count = active.items.len().max(1);
                    let item_index = active.next_item_index;
                    let session = conversation_session_fact(&active.row);
                    let item = active.items.get(item_index);
                    let (event, output, rejection) = conversation_event(
                        &active.row,
                        active.physical_rowid,
                        item_index,
                        item,
                        self.frontier.next_native_ordinal,
                        collect_outputs,
                    )?;
                    let rejection = rejection.or_else(|| {
                        if item_index == 0 {
                            active.rejection.take()
                        } else {
                            None
                        }
                    });
                    let include_session = item_index == 0;
                    let unit =
                        (include_session || event.is_some()).then_some(CoreUnit { session, event });
                    let unit_bytes = unit.as_ref().map_or(64, estimated_unit_bytes);
                    let output_estimate =
                        output.as_ref().map_or(0, |output| output.estimated_bytes);
                    if source_units != 0
                        && (core_bytes.saturating_add(unit_bytes) > PAGE_MAX_CORE_BYTES
                            || output_bytes.saturating_add(output_estimate) > PAGE_MAX_OUTPUT_BYTES)
                    {
                        break;
                    }
                    source_units = source_units.saturating_add(1);
                    core_bytes = core_bytes
                        .saturating_add(unit_bytes)
                        .min(PAGE_MAX_CORE_BYTES);
                    if let Some(unit) = unit {
                        units.push(unit);
                    }
                    if let Some(output) = output {
                        if output_estimate <= PAGE_MAX_OUTPUT_BYTES {
                            output_bytes = output_bytes.saturating_add(output_estimate);
                            outputs.push(output);
                        } else {
                            rejections.push(PageRejection {
                                line: ordinal_line(self.frontier.next_native_ordinal),
                                detail: "AstrBot output exceeds the bounded Pro replay page"
                                    .to_owned(),
                            });
                        }
                    }
                    if let Some(detail) = rejection {
                        rejections.push(PageRejection {
                            line: ordinal_line(self.frontier.next_native_ordinal),
                            detail,
                        });
                    }
                    self.frontier.next_native_ordinal =
                        self.frontier.next_native_ordinal.saturating_add(1);
                    active.next_item_index = active.next_item_index.saturating_add(1);
                    if active.next_item_index >= item_count {
                        finish_conversation_row(&mut self.frontier, active);
                        self.active_conversation = None;
                    } else {
                        self.frontier.conversation_in_row = Some(ConversationInRow {
                            physical_rowid: active.physical_rowid,
                            row_sha256: active.row_sha256,
                            next_item_index: u32::try_from(active.next_item_index)
                                .unwrap_or(u32::MAX),
                        });
                    }
                }
                ScanPhase::PlatformMessages => {
                    if !self.relationship_projection_ready {
                        prepare_relationship_projection(self.conn, &self.sql)?;
                        self.relationship_projection_ready = true;
                    }
                    let Some(initial) = self.sql.platform_message_candidate_initial.as_deref()
                    else {
                        self.frontier.phase = ScanPhase::Complete;
                        break;
                    };
                    let after = self.sql.platform_message_candidate_after.as_deref().ok_or(
                        CaptureError::SystemInvariant(
                            "AstrBot platform-message keyset SQL is incomplete",
                        ),
                    )?;
                    let Some(candidate) = fetch_candidate(
                        self.conn,
                        initial,
                        after,
                        self.frontier.platform_after_rowid,
                    )?
                    else {
                        self.frontier.phase = ScanPhase::Complete;
                        break;
                    };
                    let (unit, rejection, row_sha256) =
                        self.platform_unit(candidate, collect_outputs)?;
                    let unit_bytes = unit.as_ref().map_or(64, estimated_unit_bytes);
                    if source_units != 0
                        && core_bytes.saturating_add(unit_bytes) > PAGE_MAX_CORE_BYTES
                    {
                        break;
                    }
                    source_units = source_units.saturating_add(1);
                    core_bytes = core_bytes
                        .saturating_add(unit_bytes)
                        .min(PAGE_MAX_CORE_BYTES);
                    if let Some(unit) = unit {
                        units.push(unit);
                    }
                    if let Some(detail) = rejection {
                        rejections.push(PageRejection {
                            line: ordinal_line(self.frontier.next_native_ordinal),
                            detail,
                        });
                    }
                    self.frontier.platform_after_rowid = Some(candidate.physical_rowid);
                    self.frontier.platform_prefix_sha256 =
                        chain_hash(self.frontier.platform_prefix_sha256, row_sha256);
                    self.frontier.last_platform_order = Some(candidate.legacy_order);
                    self.frontier.next_native_ordinal =
                        self.frontier.next_native_ordinal.saturating_add(1);
                }
                ScanPhase::Complete => break,
            }
        }

        Ok(Some(AstrBotPage {
            expected_frontier,
            next_frontier: self.frontier.clone(),
            terminal: self.frontier.terminal(),
            retained_core_bytes: core_bytes,
            units,
            outputs,
            rejections,
        }))
    }

    fn load_active_conversation(&self) -> Result<Option<ActiveConversation>> {
        if let Some(in_row) = &self.frontier.conversation_in_row {
            let row = hydrate_conversation(
                self.conn,
                &self.sql.conversation_hydration,
                in_row.physical_rowid,
            )?;
            let row_sha256 = serialized_hash(b"astrbot-conversation-row-v1\0", &row)?;
            if row_sha256 != in_row.row_sha256 {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            let items = conversation_items(&row.content);
            let next_item_index = usize::try_from(in_row.next_item_index).map_err(|_| {
                CaptureError::InvalidPayload(
                    "AstrBot conversation item frontier exceeds platform limits".to_owned(),
                )
            })?;
            if next_item_index >= items.len().max(1) {
                return Err(CaptureError::InvalidPayload(
                    "AstrBot conversation item frontier is out of range".to_owned(),
                ));
            }
            return Ok(Some(ActiveConversation {
                physical_rowid: in_row.physical_rowid,
                order: LegacyOrderKey {
                    timestamp_is_present: row.created_at.is_some(),
                    timestamp: row.created_at.unwrap_or(0),
                    logical_id: row.row_id,
                    physical_rowid: in_row.physical_rowid,
                },
                row_sha256,
                row,
                items,
                next_item_index,
                rejection: None,
            }));
        }
        let Some(candidate) = fetch_candidate(
            self.conn,
            &self.sql.conversation_candidate_initial,
            &self.sql.conversation_candidate_after,
            self.frontier.conversation_after_rowid,
        )?
        else {
            return Ok(None);
        };
        if self
            .frontier
            .last_conversation_order
            .is_some_and(|previous| previous > candidate.legacy_order)
        {
            return Ok(Some(rejected_conversation(
                candidate,
                "AstrBot conversations rows are not in legacy timestamp/id order by physical rowid",
            )));
        }
        let observed = candidate.observed_bytes()?;
        if observed > u64::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).unwrap_or(u64::MAX) {
            return Ok(Some(rejected_conversation(
                candidate,
                "AstrBot conversation row exceeds the provider record limit",
            )));
        }
        let row = hydrate_conversation(
            self.conn,
            &self.sql.conversation_hydration,
            candidate.physical_rowid,
        )?;
        let row_sha256 = serialized_hash(b"astrbot-conversation-row-v1\0", &row)?;
        let items = conversation_items(&row.content);
        Ok(Some(ActiveConversation {
            physical_rowid: candidate.physical_rowid,
            order: candidate.legacy_order,
            row_sha256,
            row,
            items,
            next_item_index: 0,
            rejection: None,
        }))
    }

    fn platform_unit(
        &self,
        candidate: RowCandidate,
        _collect_outputs: bool,
    ) -> Result<(Option<CoreUnit>, Option<String>, [u8; 32])> {
        if self
            .frontier
            .last_platform_order
            .is_some_and(|previous| previous > candidate.legacy_order)
        {
            return Ok((
                None,
                Some(
                    "AstrBot platform_message_history rows are not in legacy timestamp/id order by physical rowid"
                        .to_owned(),
                ),
                candidate_hash(b"astrbot-platform-oversize-v1\0", candidate),
            ));
        }
        if candidate.observed_bytes()?
            > u64::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).unwrap_or(u64::MAX)
        {
            return Ok((
                None,
                Some("AstrBot platform-message row exceeds the provider record limit".to_owned()),
                candidate_hash(b"astrbot-platform-oversize-v1\0", candidate),
            ));
        }
        let hydration =
            self.sql
                .platform_message_hydration
                .as_deref()
                .ok_or(CaptureError::SystemInvariant(
                    "AstrBot platform-message hydration SQL is missing",
                ))?;
        let row = hydrate_platform_message(self.conn, hydration, candidate.physical_rowid)?;
        let row_sha256 = serialized_hash(b"astrbot-platform-row-v1\0", &row)?;
        let link = linked_platform_message_parent(self.conn, row.llm_checkpoint_id.as_deref())?;
        let Some(text) = row
            .content
            .as_deref()
            .map(provider_json_text)
            .as_ref()
            .and_then(provider_value_text)
            .filter(|text| !text.trim().is_empty())
        else {
            return Ok((None, None, row_sha256));
        };
        let session = platform_session_fact(&row, link.as_ref());
        let role = if row.sender_id.as_deref() == row.user_id.as_deref() {
            Some(EventRole::User)
        } else {
            Some(EventRole::Assistant)
        };
        let event_index = 1_000_000u64.saturating_add(u64::try_from(row.id).unwrap_or(0));
        let event_type = EventType::Message;
        let occurred_at = timestamp(row.created_at, session.started_at);
        let body = json!({
            "message_id": row.id,
            "platform_id": row.platform_id,
            "user_id": row.user_id,
            "sender_id": row.sender_id,
            "sender_name": row.sender_name,
            "content": row.content.as_deref().map(provider_json_text),
            "llm_checkpoint_id": row.llm_checkpoint_id,
        });
        Ok((
            Some(CoreUnit {
                session,
                event: Some(EventFact {
                    provider_event_index: event_index,
                    legacy_provider_event_index: Some(event_index),
                    event_type,
                    role,
                    occurred_at,
                    payload: astrbot_event_payload(event_type, &text, &body),
                    metadata: json!({
                        "source": "astrbot_platform_message_history",
                        "source_format": ASTRBOT_SQLITE_SOURCE_FORMAT,
                        "message_id": row.id,
                    }),
                }),
            }),
            None,
            row_sha256,
        ))
    }
}

fn conversation_event(
    row: &ConversationRow,
    physical_rowid: i64,
    item_index: usize,
    item: Option<&Value>,
    native_ordinal: u64,
    collect_output: bool,
) -> Result<(Option<EventFact>, Option<OutputFact>, Option<String>)> {
    let Some(item) = item else {
        return Ok((None, None, None));
    };
    if checkpoint_id(item).is_some() {
        return Ok((None, None, None));
    }
    let Some(text) = item_text(item).filter(|text| !text.trim().is_empty()) else {
        return Ok((None, None, None));
    };
    let provider_session_id = provider_session_id(row);
    let output = item_is_output(item);
    let outcome = output.then(|| output_outcome(item));
    let event_type = if output {
        EventType::ToolOutput
    } else {
        EventType::Message
    };
    let event_index = u64::try_from(item_index).unwrap_or(u64::MAX);
    let provider_event_hash = item_id(item).map(|id| format!("conversation:{id}"));
    let cursor = format!("conversation:{}:item:{item_index}", row.conversation_id);
    let body = item.clone();
    let mut event = EventFact {
        provider_event_index: event_index,
        legacy_provider_event_index: Some(event_index),
        event_type,
        role: item_role(item),
        occurred_at: timestamp(row.created_at, DateTime::<Utc>::UNIX_EPOCH),
        payload: astrbot_event_payload(event_type, &text, &body),
        metadata: json!({
            "source": "astrbot_conversations",
            "source_format": ASTRBOT_SQLITE_SOURCE_FORMAT,
            "conversation_id": row.conversation_id,
            "inner_conversation_id": row.inner_conversation_id,
            "item_index": item_index,
        }),
    };
    if !output {
        let locator = super::astrbot_complete_message_locator(physical_rowid, item_index)?;
        let native_record_id = provider_event_hash.as_deref().unwrap_or(cursor.as_str());
        attach_astrbot_complete_content_locator(
            &mut event,
            &locator,
            &super::model::conversation_values(row.clone()),
            &text,
            native_record_id,
        )?;
    }
    let output_fact = if output && collect_output {
        let locator = super::astrbot_complete_message_locator(physical_rowid, item_index)?;
        let content = text.into_bytes();
        let estimated_bytes = content
            .len()
            .saturating_add(provider_session_id.len())
            .saturating_add(1024);
        Some(OutputFact {
            observation: ProOutputObservation {
                kind: OutputObservationKind::Tool,
                coordinate: OutputNativeCoordinate {
                    unit_key: format!("astrbot/{}/{item_index:010}", row.conversation_id),
                    native_sequence: native_ordinal,
                    native_record_id: item_id(item).map(str::to_owned),
                    source_record_ordinal: Some(native_ordinal),
                    source_record_subrecord_index: u32::try_from(item_index).ok(),
                    byte_start: None,
                    byte_end_exclusive: None,
                },
                occurred_at_unix_ms: row.created_at,
                associations: OutputAssociations {
                    direct_session_id: provider_session_id.clone(),
                    root_session_id: provider_session_id.clone(),
                    parent_session_id: None,
                    provider_session_id: Some(provider_session_id),
                    agent_id: row.persona_id.clone(),
                    repository: None,
                },
                call_id: item
                    .get("call_id")
                    .or_else(|| item.get("tool_call_id"))
                    .or_else(|| item.get("toolCallId"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                command: None,
                outcome: OutputOutcomeMetadata {
                    outcome: outcome.unwrap_or(OutputOutcome::Unknown),
                    exit_code: item
                        .get("exit_code")
                        .or_else(|| item.get("exitCode"))
                        .and_then(Value::as_i64)
                        .and_then(|value| i32::try_from(value).ok()),
                    duration_ms: item
                        .get("duration_ms")
                        .or_else(|| item.get("durationMs"))
                        .and_then(Value::as_u64),
                },
                locator: OutputSourceLocator {
                    version: 1,
                    kind: locator.kind().to_owned(),
                    payload: locator.value().to_vec(),
                },
                content,
            },
            estimated_bytes,
        })
    } else {
        None
    };
    Ok((Some(event), output_fact, None))
}

fn astrbot_event_payload(event_type: EventType, text: &str, body: &Value) -> Value {
    let retained_text = provider_policy_event_text(event_type, text, body);
    let retained_body = provider_policy_body(event_type, body);
    json!({
        "text": retained_text.text,
        "text_retention": retained_text.retention.as_json(),
        "result_evidence": provider_result_identifier_evidence(event_type, text, body),
        "result_outcome": provider_result_outcome_evidence(event_type, body),
        "source_format": ASTRBOT_SQLITE_SOURCE_FORMAT,
        "body": provider_capped_json(&retained_body, PROVIDER_MAX_PREVIEW_CHARS),
    })
}

fn attach_astrbot_complete_content_locator(
    event: &mut EventFact,
    locator: &NativeLocator,
    values: &[NativeSqliteValue],
    complete_text: &str,
    native_record_id: &str,
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
        CaptureError::SystemInvariant("AstrBot complete content exceeds ContentRef bounds"),
    )?;
    let profile = verified_content_profile(
        CaptureProvider::AstrBot,
        ASTRBOT_SQLITE_SOURCE_FORMAT,
        CompleteContentSourceFamily::Sqlite,
        VerifiedContentRole::MessageBody,
    )
    .ok_or(CaptureError::SystemInvariant(
        "AstrBot complete-content profile is not registered",
    ))?;
    let persisted = VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        profile,
        content_ref,
        CompleteContentSourceFamily::Sqlite,
        locator.kind(),
        locator.value(),
        native_record_id,
        astrbot_logical_record_digest(values)?,
    )
    .ok_or(CaptureError::SystemInvariant(
        "AstrBot complete-content locator exceeds its typed bounds",
    ))?;
    attach_verified_content_locator(&mut event.metadata, persisted).ok_or(
        CaptureError::SystemInvariant("AstrBot complete-content locator metadata is malformed"),
    )?;
    Ok(())
}

fn astrbot_logical_record_digest(
    values: &[NativeSqliteValue],
) -> Result<CompleteContentBodyDigest> {
    let mut digest = Sha256::new();
    digest.update(b"ctx-complete-content-sqlite-logical-row-v1\0");
    digest.update((values.len() as u64).to_be_bytes());
    for value in values {
        match value {
            NativeSqliteValue::Null => digest.update([0]),
            NativeSqliteValue::Integer(value) => {
                digest.update([1]);
                digest.update(value.to_be_bytes());
            }
            NativeSqliteValue::RealBits(value) => {
                digest.update([2]);
                digest.update(value.to_be_bytes());
            }
            NativeSqliteValue::Text(value) => {
                digest.update([3]);
                digest.update((value.len() as u64).to_be_bytes());
                digest.update(value.as_bytes());
            }
            NativeSqliteValue::Blob(value) => {
                digest.update([4]);
                digest.update((value.len() as u64).to_be_bytes());
                digest.update(value);
            }
        }
    }
    CompleteContentBodyDigest::parse(format!("{:x}", digest.finalize())).ok_or(
        CaptureError::SystemInvariant("AstrBot logical-row digest formatting failed"),
    )
}

fn conversation_session_fact(row: &ConversationRow) -> SessionFact {
    let started_at = timestamp(row.created_at, DateTime::<Utc>::UNIX_EPOCH);
    SessionFact {
        provider_session_id: provider_session_id(row),
        external_agent_id: row.platform_id.clone(),
        role_hint: "llm-context",
        started_at,
        ended_at: row
            .updated_at
            .map(|value| timestamp(Some(value), started_at)),
        metadata: json!({
            "source_format": ASTRBOT_SQLITE_SOURCE_FORMAT,
            "conversation_id": row.conversation_id,
            "inner_conversation_id": row.inner_conversation_id,
            "platform_id": capped_optional(row.platform_id.as_deref()),
            "user_id": capped_optional(row.user_id.as_deref()),
            "title": capped_optional(row.title.as_deref()),
            "persona_id": capped_optional(row.persona_id.as_deref()),
            "token_usage": row.token_usage.as_deref().map(provider_json_text),
            "fidelity_gap": "The AstrBot importer reads local LLM context plus available platform history from data_v4.db; platform-native chats may still be partial when upstream stores non-LLM replies on the IM platform",
        }),
        preserve_existing: false,
    }
}

fn platform_session_fact(
    row: &PlatformMessageRow,
    link: Option<&PlatformMessageLink>,
) -> SessionFact {
    let provider_session_id = link
        .map(|link| link.provider_session_id.clone())
        .unwrap_or_else(|| {
            format!(
                "platform/{}/{}",
                row.platform_id.as_deref().unwrap_or("unknown"),
                row.user_id.as_deref().unwrap_or("unknown")
            )
        });
    let started_at = link
        .and_then(|link| link.parent_created_at)
        .map(|value| timestamp(Some(value), DateTime::<Utc>::UNIX_EPOCH))
        .unwrap_or_else(|| timestamp(row.created_at, DateTime::<Utc>::UNIX_EPOCH));
    SessionFact {
        provider_session_id,
        external_agent_id: row.platform_id.clone(),
        role_hint: if link.is_some() {
            "llm-context"
        } else {
            "platform-history"
        },
        started_at,
        ended_at: None,
        metadata: json!({
            "source_format": ASTRBOT_SQLITE_SOURCE_FORMAT,
            "linked_checkpoint_id": row.llm_checkpoint_id,
            "platform_id": capped_optional(row.platform_id.as_deref()),
            "user_id": capped_optional(row.user_id.as_deref()),
            "fidelity_gap": (!link.is_some()).then_some(
                "platform history row was not linked to a conversations checkpoint"
            ),
        }),
        preserve_existing: link.is_some(),
    }
}

fn conversation_items(raw: &str) -> Vec<Value> {
    match provider_json_text(raw) {
        Value::Array(items) => items,
        value => vec![value],
    }
}

fn finish_conversation_row(frontier: &mut AstrBotFrontier, active: &ActiveConversation) {
    frontier.conversation_after_rowid = Some(active.physical_rowid);
    frontier.conversation_prefix_sha256 =
        chain_hash(frontier.conversation_prefix_sha256, active.row_sha256);
    frontier.last_conversation_order = Some(active.order);
    frontier.conversation_in_row = None;
}

fn rejected_conversation(candidate: RowCandidate, detail: &str) -> ActiveConversation {
    let row_sha256 = candidate_hash(b"astrbot-conversation-oversize-v1\0", candidate);
    ActiveConversation {
        physical_rowid: candidate.physical_rowid,
        order: candidate.legacy_order,
        row_sha256,
        row: ConversationRow {
            row_id: candidate.legacy_order.logical_id,
            inner_conversation_id: None,
            conversation_id: format!("oversize-row-{}", candidate.physical_rowid),
            platform_id: None,
            user_id: None,
            content: Value::Null.to_string(),
            title: None,
            persona_id: None,
            token_usage: None,
            created_at: None,
            updated_at: None,
        },
        items: Vec::new(),
        next_item_index: 0,
        rejection: Some(detail.to_owned()),
    }
}

fn prepare_relationship_projection(conn: &Connection, sql: &AstrBotSql) -> Result<()> {
    if relationship_projection_exists(conn)? {
        return Ok(());
    }
    let original_query_only: i64 = conn.pragma_query_value(None, "query_only", |row| row.get(0))?;
    let operation = (|| {
        conn.pragma_update(None, "query_only", false)?;
        conn.execute_batch(
            "pragma temp_store = file;
             drop table if exists temp.astrbot_nativepath_checkpoint_sessions;
             create temp table astrbot_nativepath_checkpoint_sessions (
                 checkpoint_id text primary key,
                 provider_session_id text not null,
                 parent_created_at integer
             ) without rowid;",
        )?;
        let mut insert = conn.prepare(
            "insert into temp.astrbot_nativepath_checkpoint_sessions
                 (checkpoint_id, provider_session_id, parent_created_at)
             values (?1, ?2, ?3)
             on conflict(checkpoint_id) do update set
                 provider_session_id = excluded.provider_session_id,
                 parent_created_at = excluded.parent_created_at",
        )?;
        let mut after = None;
        loop {
            let Some(candidate) = fetch_candidate(
                conn,
                &sql.conversation_candidate_initial,
                &sql.conversation_candidate_after,
                after,
            )?
            else {
                break;
            };
            after = Some(candidate.physical_rowid);
            if candidate.observed_bytes()?
                > u64::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).unwrap_or(u64::MAX)
            {
                continue;
            }
            let row =
                hydrate_conversation(conn, &sql.conversation_hydration, candidate.physical_rowid)?;
            let session_id = provider_session_id(&row);
            for item in conversation_items(&row.content) {
                if let Some(checkpoint) = checkpoint_id(&item) {
                    insert.execute(rusqlite::params![checkpoint, session_id, row.created_at])?;
                }
            }
        }
        Ok(())
    })();
    let restore = conn
        .pragma_update(None, "query_only", original_query_only)
        .map_err(CaptureError::from);
    match (operation, restore) {
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

fn relationship_projection_exists(conn: &Connection) -> Result<bool> {
    conn.query_row(
        "select exists(
             select 1 from temp.sqlite_temp_master
             where type = 'table' and name = 'astrbot_nativepath_checkpoint_sessions'
         )",
        [],
        |row| row.get::<_, i64>(0),
    )
    .map(|exists| exists != 0)
    .map_err(CaptureError::from)
}

fn linked_platform_message_parent(
    conn: &Connection,
    checkpoint: Option<&str>,
) -> Result<Option<PlatformMessageLink>> {
    let Some(checkpoint) = checkpoint else {
        return Ok(None);
    };
    conn.query_row(
        "select provider_session_id, parent_created_at
         from temp.astrbot_nativepath_checkpoint_sessions
         where checkpoint_id = ?1",
        [checkpoint],
        |row| {
            Ok(PlatformMessageLink {
                provider_session_id: row.get(0)?,
                parent_created_at: row.get(1)?,
            })
        },
    )
    .optional()
    .map_err(CaptureError::from)
}

fn validate_frontier(
    conn: &Connection,
    sql: &AstrBotSql,
    frontier: &AstrBotFrontier,
) -> Result<bool> {
    if frontier.version != FRONTIER_VERSION {
        return Ok(false);
    }
    let (conversation_hash, conversation_order) = recompute_prefix(
        conn,
        &sql.conversation_candidate_initial,
        &sql.conversation_candidate_after,
        frontier.conversation_after_rowid,
        |candidate| {
            if candidate.observed_bytes()?
                > u64::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).unwrap_or(u64::MAX)
            {
                Ok(candidate_hash(
                    b"astrbot-conversation-oversize-v1\0",
                    candidate,
                ))
            } else {
                let row = hydrate_conversation(
                    conn,
                    &sql.conversation_hydration,
                    candidate.physical_rowid,
                )?;
                serialized_hash(b"astrbot-conversation-row-v1\0", &row)
            }
        },
    )?;
    if conversation_hash != frontier.conversation_prefix_sha256
        || conversation_order != frontier.last_conversation_order
    {
        return Ok(false);
    }
    if let Some(in_row) = &frontier.conversation_in_row {
        let row = hydrate_conversation(conn, &sql.conversation_hydration, in_row.physical_rowid)?;
        if serialized_hash(b"astrbot-conversation-row-v1\0", &row)? != in_row.row_sha256
            || usize::try_from(in_row.next_item_index).unwrap_or(usize::MAX)
                >= conversation_items(&row.content).len().max(1)
        {
            return Ok(false);
        }
    }
    let Some(platform_initial) = sql.platform_message_candidate_initial.as_deref() else {
        return Ok(frontier.platform_after_rowid.is_none()
            && frontier.platform_prefix_sha256 == [0; 32]
            && frontier.last_platform_order.is_none());
    };
    let platform_after =
        sql.platform_message_candidate_after
            .as_deref()
            .ok_or(CaptureError::SystemInvariant(
                "AstrBot platform-message keyset SQL is incomplete",
            ))?;
    let (platform_hash, platform_order) = recompute_prefix(
        conn,
        platform_initial,
        platform_after,
        frontier.platform_after_rowid,
        |candidate| {
            if candidate.observed_bytes()?
                > u64::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).unwrap_or(u64::MAX)
            {
                Ok(candidate_hash(b"astrbot-platform-oversize-v1\0", candidate))
            } else {
                let hydration = sql.platform_message_hydration.as_deref().ok_or(
                    CaptureError::SystemInvariant(
                        "AstrBot platform-message hydration SQL is missing",
                    ),
                )?;
                let row = hydrate_platform_message(conn, hydration, candidate.physical_rowid)?;
                serialized_hash(b"astrbot-platform-row-v1\0", &row)
            }
        },
    )?;
    Ok(platform_hash == frontier.platform_prefix_sha256
        && platform_order == frontier.last_platform_order)
}

fn recompute_prefix(
    conn: &Connection,
    initial_sql: &str,
    after_sql: &str,
    through_rowid: Option<i64>,
    mut row_hash: impl FnMut(RowCandidate) -> Result<[u8; 32]>,
) -> Result<([u8; 32], Option<LegacyOrderKey>)> {
    let Some(through_rowid) = through_rowid else {
        return Ok(([0; 32], None));
    };
    let mut after = None;
    let mut digest = [0; 32];
    let mut order = None;
    loop {
        let Some(candidate) = fetch_candidate(conn, initial_sql, after_sql, after)? else {
            return Ok(([u8::MAX; 32], None));
        };
        if candidate.physical_rowid > through_rowid {
            return Ok(([u8::MAX; 32], None));
        }
        if order.is_some_and(|previous| previous > candidate.legacy_order) {
            return Ok(([u8::MAX; 32], None));
        }
        digest = chain_hash(digest, row_hash(candidate)?);
        order = Some(candidate.legacy_order);
        after = Some(candidate.physical_rowid);
        if candidate.physical_rowid == through_rowid {
            return Ok((digest, order));
        }
    }
}

fn decode_prior_cursor(stored: Option<SyncCursor>) -> Result<PriorCursor> {
    let Some(stored) = stored else {
        return Ok(PriorCursor::None);
    };
    if let Ok(committed) = decode_native_path_committed_cursor(&stored.cursor) {
        return Ok(PriorCursor::Native {
            encoded: stored.cursor,
            cursor: AstrBotStoreCursor::decode(committed.provider_cursor())?,
        });
    }
    if let Some(released) = CertifiedProviderCursor::decode_if_certified(&stored.cursor)? {
        return Ok(PriorCursor::Released {
            encoded: stored.cursor,
            rejected_records: released.rejected_records(),
        });
    }
    if stored.cursor.trim().is_empty() || !stored.cursor.trim_start().starts_with('{') {
        return Ok(PriorCursor::Released {
            encoded: stored.cursor,
            rejected_records: 0,
        });
    }
    Err(CaptureError::InvalidPayload(
        "AstrBot cursor is neither NativePath nor a released migration cursor".to_owned(),
    ))
}

fn replay_outputs(
    path: &Path,
    store: &Store,
    conn: &Connection,
    snapshot: &crate::provider::sqlite::ProviderSqliteSourceSnapshot,
    authority: &SourceAuthority,
    context: &ProviderAdapterContext,
    profile: &ImportProfile,
) -> Result<()> {
    let sink = profile.sink().ok_or(CaptureError::SystemInvariant(
        "AstrBot output replay has no output sink",
    ))?;
    let stored = store
        .get_sync_cursor(None, &context.machine_id, &authority.cursor_stream)?
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "AstrBot output replay requires committed NativePath Core".to_owned(),
            )
        })?;
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    let core = AstrBotStoreCursor::decode(committed.provider_cursor())?;
    if core.retired
        || !core.terminal
        || core.source_revision != authority.source_revision
        || core.schema_authority != authority.schema_authority
    {
        return Err(CaptureError::InvalidPayload(
            "AstrBot output replay source no longer matches committed Core authority".to_owned(),
        ));
    }
    let output_source = OutputSourceIdentity {
        provider: CaptureProvider::AstrBot.as_str().to_owned(),
        namespace_id: core.source_identity.clone(),
        source_id: core.source_identity.clone(),
    };
    let progress = match sink.observe_source(&output_source) {
        Ok(progress) => progress,
        Err(error) => {
            sink.mark_behind(error);
            return Ok(());
        }
    };
    let parser_revision = format!(
        "{OUTPUT_PARSER_REVISION}:capture={ASTRBOT_CAPTURE_REVISION}:policy={ASTRBOT_POLICY_REVISION}"
    );
    let progress_frontier = progress
        .as_ref()
        .and_then(|progress| progress.cursor.as_ref())
        .and_then(|cursor| decode_output_frontier(cursor).ok());
    let compatible_progress = progress.as_ref().is_some_and(|progress| {
        progress.observed_revision == authority.source_revision
            && progress.parser_revision == parser_revision
            && progress.materializer_revision == sink.materializer_revision()
            && progress_frontier.is_some()
    });
    if compatible_progress
        && progress.as_ref().is_some_and(|progress| progress.terminal)
        && progress_frontier
            .as_ref()
            .is_some_and(|frontier| frontier == &core.frontier)
    {
        return Ok(());
    }
    let resumable_frontier =
        if compatible_progress && progress.as_ref().is_some_and(|progress| !progress.terminal) {
            match progress_frontier {
                Some(frontier)
                    if frontier.next_native_ordinal <= core.frontier.next_native_ordinal
                        && validate_frontier(conn, &AstrBotSql::new(conn)?, &frontier)? =>
                {
                    Some(frontier)
                }
                _ => None,
            }
        } else {
            None
        };
    let (source_epoch, expected_epoch, expected_cursor, disposition, reader_start) =
        match (&progress, resumable_frontier) {
            (Some(progress), Some(frontier)) => (
                progress.source_epoch,
                Some(progress.source_epoch),
                progress.cursor.clone(),
                ProOutputSourceDisposition::AppendOrResume,
                frontier,
            ),
            (Some(progress), None) => (
                progress
                    .source_epoch
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "AstrBot output source epoch exhausted",
                    ))?,
                Some(progress.source_epoch),
                progress.cursor.clone(),
                ProOutputSourceDisposition::Rewrite,
                AstrBotFrontier::initial(),
            ),
            (None, None) => (
                0,
                None,
                None,
                ProOutputSourceDisposition::NewSource,
                AstrBotFrontier::initial(),
            ),
            (None, Some(_)) => {
                return Err(CaptureError::SystemInvariant(
                    "AstrBot output replay derived progress without a sink source",
                ));
            }
        };
    let mut expected_epoch = expected_epoch;
    let mut expected_cursor = expected_cursor;
    let mut disposition = disposition;
    let mut reader = AstrBotReader::new(conn, AstrBotSql::new(conn)?, reader_start);
    while let Some(page) = reader.next_page(true)? {
        if !snapshot.revalidate(path)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        if let Some(rejection) = page.rejections.first() {
            sink.mark_behind(crate::ProOutputSinkError::new(
                "astrbot_output_incomplete",
                rejection.detail.clone(),
            ));
            return Ok(());
        }
        let next_cursor = encode_output_frontier(&page.next_frontier)?;
        let observations = page
            .outputs
            .into_iter()
            .map(|output| output.observation)
            .collect::<Vec<_>>();
        let materialization = ProOutputMaterializationPage {
            inventory_generation: sink.inventory_generation(),
            source: output_source.clone(),
            source_epoch,
            observed_revision: authority.source_revision.clone(),
            parser_revision: parser_revision.clone(),
            materializer_revision: sink.materializer_revision().to_owned(),
            disposition,
            expected_prior_source_epoch: expected_epoch,
            expected_prior_cursor: expected_cursor.clone(),
            next_safe_cursor: next_cursor.clone(),
            terminal: page.terminal,
            observations,
        };
        match sink.materialize_page(materialization) {
            Ok(result)
                if result.source_epoch == source_epoch
                    && result.committed_cursor == next_cursor =>
            {
                expected_epoch = Some(source_epoch);
                expected_cursor = Some(next_cursor);
                disposition = ProOutputSourceDisposition::AppendOrResume;
            }
            Ok(_) => {
                sink.mark_behind(crate::ProOutputSinkError::new(
                    "astrbot_output_receipt",
                    "AstrBot output sink returned a mismatched receipt",
                ));
                return Ok(());
            }
            Err(error) => {
                sink.mark_behind(error);
                return Ok(());
            }
        }
    }
    if reader.frontier != core.frontier {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    Ok(())
}

fn retire_missing_source(
    path: &Path,
    store: &mut Store,
    context: &ProviderAdapterContext,
) -> Result<ProviderImportSummary> {
    let locator_identity = provider_path_identity(path)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::AstrBot,
        ASTRBOT_SQLITE_SOURCE_FORMAT,
        &locator_identity,
    );
    let Some(stored) = store.get_sync_cursor(None, &context.machine_id, &cursor_stream)? else {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "AstrBot data_v4.db does not exist",
        });
    };
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    let mut prior = AstrBotStoreCursor::decode(committed.provider_cursor())?;
    if prior.retired {
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }
    let retirement = ProviderSourceRouteRetirement {
        provider: CaptureProvider::AstrBot,
        source_format: ASTRBOT_SQLITE_SOURCE_FORMAT.to_owned(),
        machine_id: context.machine_id.clone(),
        locator_identity: prior.locator_identity.clone(),
        cursor_stream: cursor_stream.clone(),
        expected_canonical_source_identity: prior.source_identity.clone(),
        expected_source_revision: prior.source_revision.clone(),
        retired_at_ms: context.imported_at.timestamp_millis(),
        reason: ProviderSourceRouteRetirementReason::SourceMissing,
    };
    prior.retired = true;
    let next = SyncCursor {
        id: Uuid::new_v4(),
        team_id: None,
        device_id: context.machine_id.clone(),
        stream: cursor_stream,
        cursor: prior.encode()?,
        last_synced_at: Some(context.imported_at),
        timestamps: timestamps(context.imported_at),
    };
    let transition = NativePathCursorTransition::new(Some(stored.cursor), next);
    let publication_id = retirement_publication_id(&retirement, transition.next().cursor.as_str());
    let accounting = NativePathGroupAccounting::new(0, 1, transition.next().cursor.len())?;
    let bulk = store.begin_event_search_bulk_mode()?;
    let admission = store.admit_event_search_bulk_group(&bulk)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    let disposition =
        match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
            NativePathCursorSetClassification::AllNextSameGroup { .. } => {
                group.commit()?;
                ProviderSourceRouteRetirementDisposition::AlreadyRetired
            }
            NativePathCursorSetClassification::AllExpected => {
                let disposition = group.retire_provider_source_route(&retirement)?;
                group.prepare_journal_checkpoint()?;
                group.publish_cursor_set()?;
                group.commit()?;
                disposition
            }
        };
    store.finish_event_search_bulk_mode(&bulk)?;
    let mut summary = ProviderImportSummary::default();
    match disposition {
        ProviderSourceRouteRetirementDisposition::Retired => {
            summary.skipped_sessions = 1;
            summary.skipped = 1;
            summary.set_work_result(ProviderImportWorkResult::Changed);
        }
        ProviderSourceRouteRetirementDisposition::AlreadyRetired => {
            summary.set_work_result(ProviderImportWorkResult::NoOp);
        }
    }
    Ok(summary)
}

fn page_publication_id(
    authority: &SourceAuthority,
    page: &AstrBotPage,
    cursor: &AstrBotStoreCursor,
) -> Result<String> {
    let mut hash = Sha256::new();
    hash.update(b"ctx-astrbot-nativepath-page-v1\0");
    hash_field(&mut hash, authority.locator_identity.as_bytes());
    hash_field(&mut hash, authority.source_revision.as_bytes());
    hash_field(&mut hash, cursor.encode()?.as_bytes());
    hash_field(
        &mut hash,
        &serde_json::to_vec(&page.expected_frontier).map_err(CaptureError::from)?,
    );
    for unit in &page.units {
        hash_field(&mut hash, unit.session.provider_session_id.as_bytes());
        if let Some(event) = &unit.event {
            hash.update(event.provider_event_index.to_le_bytes());
            hash_field(
                &mut hash,
                crate::compute_payload_hash(&event.payload)?.as_bytes(),
            );
        }
    }
    for rejection in &page.rejections {
        hash_field(&mut hash, rejection.detail.as_bytes());
    }
    Ok(format!("{PUBLICATION_PREFIX}{}", hex(&hash.finalize())))
}

fn retirement_publication_id(
    retirement: &ProviderSourceRouteRetirement,
    next_cursor: &str,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"ctx-astrbot-nativepath-retirement-v1\0");
    hash_field(&mut hash, retirement.locator_identity.as_bytes());
    hash_field(&mut hash, retirement.expected_source_revision.as_bytes());
    hash_field(&mut hash, next_cursor.as_bytes());
    format!("{RETIREMENT_PREFIX}{}", hex(&hash.finalize()))
}

fn source_incarnation(revision: &str) -> String {
    let database = revision
        .split("database=")
        .nth(1)
        .and_then(|value| value.split(";wal=").next())
        .unwrap_or(revision);
    let device = database
        .split("device=")
        .nth(1)
        .and_then(|value| value.split(';').next())
        .unwrap_or("none");
    let inode = database
        .split("inode=")
        .nth(1)
        .and_then(|value| value.split(';').next())
        .unwrap_or("none");
    format!("device={device};inode={inode}")
}

fn encode_output_frontier(frontier: &AstrBotFrontier) -> Result<OutputNativeCursor> {
    Ok(OutputNativeCursor {
        version: OUTPUT_CURSOR_VERSION,
        payload: serde_json::to_vec(frontier)?,
    })
}

fn decode_output_frontier(cursor: &OutputNativeCursor) -> Result<AstrBotFrontier> {
    if cursor.version != OUTPUT_CURSOR_VERSION {
        return Err(CaptureError::InvalidPayload(
            "AstrBot output cursor version is unsupported".to_owned(),
        ));
    }
    let frontier: AstrBotFrontier = serde_json::from_slice(&cursor.payload)?;
    if frontier.version != FRONTIER_VERSION {
        return Err(CaptureError::InvalidPayload(
            "AstrBot output frontier version is unsupported".to_owned(),
        ));
    }
    Ok(frontier)
}

fn serialized_hash(value_domain: &[u8], value: &impl Serialize) -> Result<[u8; 32]> {
    let encoded = serde_json::to_vec(value)?;
    let mut hash = Sha256::new();
    hash.update(value_domain);
    hash_field(&mut hash, &encoded);
    Ok(hash.finalize().into())
}

fn candidate_hash(domain: &[u8], candidate: RowCandidate) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(domain);
    hash.update(candidate.physical_rowid.to_le_bytes());
    hash.update(candidate.retained_bytes.to_le_bytes());
    hash.update(candidate.legacy_order.logical_id.to_le_bytes());
    hash.update(candidate.legacy_order.timestamp.to_le_bytes());
    hash.finalize().into()
}

fn chain_hash(prior: [u8; 32], row: [u8; 32]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"ctx-astrbot-prefix-chain-v1\0");
    hash.update(prior);
    hash.update(row);
    hash.finalize().into()
}

fn hash_field(hash: &mut Sha256, value: &[u8]) {
    hash.update((value.len() as u64).to_le_bytes());
    hash.update(value);
}

fn hex(bytes: &[u8]) -> String {
    let mut value = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(value, "{byte:02x}");
    }
    value
}

fn timestamp(value: Option<i64>, fallback: DateTime<Utc>) -> DateTime<Utc> {
    provider_timestamp_millis(value, fallback)
}

fn capped_optional(value: Option<&str>) -> Option<String> {
    value.map(|value| value.chars().take(PROVIDER_MAX_TEXT_CHARS).collect())
}

fn estimated_unit_bytes(unit: &CoreUnit) -> usize {
    serde_json::to_vec(&unit.session.metadata)
        .map(|bytes| bytes.len())
        .unwrap_or(PAGE_MAX_CORE_BYTES)
        .saturating_add(
            unit.event
                .as_ref()
                .and_then(|event| serde_json::to_vec(&event.payload).ok())
                .map(|bytes| bytes.len())
                .unwrap_or_default(),
        )
        .saturating_add(
            unit.event
                .as_ref()
                .and_then(|event| serde_json::to_vec(&event.metadata).ok())
                .map(|bytes| bytes.len())
                .unwrap_or_default(),
        )
        .saturating_add(2048)
}

fn ordinal_line(ordinal: u64) -> usize {
    usize::try_from(ordinal)
        .unwrap_or(usize::MAX)
        .saturating_add(1)
}
