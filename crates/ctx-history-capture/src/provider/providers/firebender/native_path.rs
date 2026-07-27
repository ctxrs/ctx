use std::{
    fs,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    compute_payload_hash, AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor,
    CaptureSourceKind, ContentRef, Event, EventType, Fidelity, ProviderSourceTrust, Session,
    SessionStatus, SyncCursor,
};
use ctx_history_store::{
    decode_native_path_committed_cursor, EventSearchBulkGuard, NativePathCursorSetClassification,
    NativePathCursorTransition, NativePathGroupAccounting, ProviderEventHashAuthority,
    ProviderSourceLocatorObservation, ProviderSourceRouteRetirement,
    ProviderSourceRouteRetirementDisposition, ProviderSourceRouteRetirementReason, Store,
    NATIVE_PATH_MAX_RETAINED_PAGE_BYTES,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    complete_content::{
        attach_verified_content_locator, verified_content_profile, CompleteContentBodyDigest,
        CompleteContentSourceFamily, VerifiedContentLocatorV1, VerifiedContentLocatorsV1,
        VerifiedContentRole, VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
    },
    native_source::{NativeLocator, NativeSqliteValue},
    provider::{
        importer::{
            compact_provider_result_payload,
            provider_event_import_identity_with_exact_legacy_source, provider_import_session_uuid,
            provider_path_identity, provider_scoped_source_identity_key,
            provider_scoped_source_uuid, provider_session_uuid,
            provider_source_cursor_stream_for_path, provider_source_identity,
            provider_sync_metadata, timestamps, CertifiedProviderCursor,
            ProviderEventImportIdentity,
        },
        native_ingestion::{
            process_pro_replay_only, NativePageAccounting, NativeProOutputPage,
            NativeProReplayPage, NativeSafeFrontier, NativeSourceIdentity,
            NATIVE_INGESTION_PAGE_MAX_UNITS,
        },
        normalization::{provider_capped_json, provider_json_text, provider_timestamp_millis},
        sqlite::{
            ensure_sqlite_table_columns, open_provider_sqlite_readonly, sqlite_schema_fingerprint,
            sqlite_table_columns, sqlite_table_exists, with_sqlite_read_snapshot,
            ProviderSqliteSourceSnapshot, SqliteLengthPreflightGuard,
        },
    },
    CaptureError, CaptureWorkLimit, OutputAssociations, OutputNativeCoordinate,
    OutputObservationKind, OutputOutcome, OutputOutcomeMetadata, OutputSourceIdentity,
    OutputSourceLocator, ProOutputObservation, ProOutputProgress, ProOutputSink,
    ProOutputSinkError, ProOutputSourceDisposition, ProviderAdapterContext, ProviderImportFailure,
    ProviderImportOptions, ProviderImportSummary, ProviderImportWorkResult, Result,
    FIREBENDER_SQLITE_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS,
};

use super::{
    firebender_chat_history_db_path, firebender_message_time, firebender_native_event,
    firebender_output_evidence, firebender_result_content, FirebenderNativeEvent,
    FIREBENDER_LOCATOR_KIND,
};

const FIREBENDER_NATIVE_CURSOR_VERSION: u32 = 1;
const FIREBENDER_NATIVE_FRONTIER_VERSION: u32 = 1;
const FIREBENDER_NATIVE_PARSER_REVISION: u32 = 1;
const FIREBENDER_NATIVE_POLICY_REVISION: u32 = 1;
const FIREBENDER_OUTPUT_PARSER_REVISION: &str = "firebender-native-output-v1";
const FIREBENDER_MAX_MESSAGES_PER_CORE_PAGE: usize = 60;
const FIREBENDER_MAX_OUTPUTS_PER_PAGE: usize = NATIVE_INGESTION_PAGE_MAX_UNITS;
const FIREBENDER_PAGE_OVERHEAD_BYTES: usize = 4 * 1024;
const FIREBENDER_INITIAL_PREFIX_DOMAIN: &[u8] = b"ctx-firebender-native-prefix-v1\0";
const FIREBENDER_PUBLICATION_DOMAIN: &[u8] = b"ctx-firebender-native-publication-v1\0";
const FIREBENDER_RETIREMENT_DOMAIN: &[u8] = b"ctx-firebender-native-retirement-v1\0";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FirebenderFrontier {
    version: u32,
    row_ordinal: u64,
    updated_at: i64,
    rowid: i64,
    next_message_index: u64,
    prefix_sha256: [u8; 32],
    terminal: bool,
}

impl FirebenderFrontier {
    fn initial() -> Self {
        Self {
            version: FIREBENDER_NATIVE_FRONTIER_VERSION,
            row_ordinal: 0,
            updated_at: 0,
            rowid: 0,
            next_message_index: 0,
            prefix_sha256: Sha256::digest(FIREBENDER_INITIAL_PREFIX_DOMAIN).into(),
            terminal: false,
        }
    }

    fn validate(&self) -> Result<()> {
        if self.version != FIREBENDER_NATIVE_FRONTIER_VERSION
            || (self.terminal && self.next_message_index != 0)
        {
            return Err(CaptureError::InvalidPayload(
                "Firebender NativePath cursor frontier is malformed".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FirebenderNativeCursor {
    version: u32,
    parser_revision: u32,
    policy_revision: u32,
    route_identity: String,
    canonical_source_identity: String,
    source_revision: String,
    schema_fingerprint: String,
    generation: u64,
    rejected_records: u64,
    accepted_sessions: u64,
    accepted_events: u64,
    frontier: FirebenderFrontier,
}

impl FirebenderNativeCursor {
    fn encode(&self) -> Result<String> {
        serde_json::to_string(self).map_err(CaptureError::from)
    }

    fn decode(encoded: &str) -> Result<Self> {
        let cursor: Self = serde_json::from_str(encoded)?;
        cursor.frontier.validate()?;
        if cursor.version != FIREBENDER_NATIVE_CURSOR_VERSION
            || cursor.parser_revision != FIREBENDER_NATIVE_PARSER_REVISION
            || cursor.policy_revision != FIREBENDER_NATIVE_POLICY_REVISION
            || cursor.route_identity.is_empty()
            || cursor.canonical_source_identity.is_empty()
            || cursor.source_revision.is_empty()
            || cursor.schema_fingerprint.is_empty()
        {
            return Err(CaptureError::InvalidPayload(
                "Firebender NativePath cursor is unsupported or incomplete".to_owned(),
            ));
        }
        Ok(cursor)
    }
}

#[derive(Debug)]
struct FirebenderSourceAuthority {
    configured_source_root: PathBuf,
    database_path: PathBuf,
    canonical_database_path: PathBuf,
    route_identity: String,
    cursor_stream: String,
    proposed_source_identity: String,
    canonical_source_identity: String,
    source_revision: String,
    schema_fingerprint: String,
}

#[derive(Debug)]
struct FirebenderRow {
    rowid: i64,
    row_ordinal: u64,
    id: String,
    name: String,
    created_at: i64,
    updated_at: i64,
    messages_json: String,
    metadata_json: String,
    messages: Vec<Value>,
}

impl FirebenderRow {
    fn logical_values(&self) -> Vec<NativeSqliteValue> {
        vec![
            NativeSqliteValue::Text(self.id.clone()),
            NativeSqliteValue::Text(self.name.clone()),
            NativeSqliteValue::Integer(self.created_at),
            NativeSqliteValue::Integer(self.updated_at),
            NativeSqliteValue::Text(self.messages_json.clone()),
            NativeSqliteValue::Text(self.metadata_json.clone()),
        ]
    }
}

#[derive(Debug)]
struct FirebenderPage {
    expected: FirebenderFrontier,
    next: FirebenderFrontier,
    row: Option<FirebenderRow>,
    message_start: usize,
    message_end: usize,
    rejection: Option<String>,
    retained_bytes: usize,
}

#[derive(Debug)]
struct FirebenderRowCandidate {
    rowid: i64,
    updated_at: i64,
    id_bytes: i64,
    name_bytes: i64,
    messages_bytes: i64,
    metadata_bytes: i64,
}

impl FirebenderRowCandidate {
    fn retained_bytes(&self) -> Result<usize> {
        [
            self.id_bytes,
            self.name_bytes,
            self.messages_bytes,
            self.metadata_bytes,
        ]
        .into_iter()
        .try_fold(FIREBENDER_PAGE_OVERHEAD_BYTES, |total, value| {
            let value = usize::try_from(value).map_err(|_| {
                CaptureError::InvalidPayload(
                    "Firebender SQLite text length must be nonnegative".to_owned(),
                )
            })?;
            total
                .checked_add(value)
                .ok_or(CaptureError::SystemInvariant(
                    "Firebender NativePath retained byte count overflowed",
                ))
        })
    }
}

pub(crate) fn import_firebender_nativepath(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    import_options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let database_path = absolute_path(&firebender_chat_history_db_path(path)?)?;
    if !database_path.exists() {
        return retire_missing_firebender_source(
            path,
            &database_path,
            store,
            &context,
            &import_options,
        );
    }

    let canonical_database_path = fs::canonicalize(&database_path)?;
    let snapshot = firebender_source_snapshot(&database_path)?;
    let conn = open_provider_sqlite_readonly(&database_path)?;
    if !snapshot.revalidate(&database_path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    validate_schema(&conn, &database_path)?;
    let schema_fingerprint = sqlite_schema_fingerprint(&conn)?;
    let source_revision = firebender_source_revision(&snapshot, &schema_fingerprint);
    let configured_source_root = context
        .source_root
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let route_identity = provider_path_identity(&canonical_database_path)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Firebender,
        FIREBENDER_SQLITE_SOURCE_FORMAT,
        &route_identity,
    );
    let raw_source_path = canonical_database_path.display().to_string();
    let source_root = configured_source_root.display().to_string();
    let proposed_source_identity = provider_source_identity(
        CaptureProvider::Firebender,
        FIREBENDER_SQLITE_SOURCE_FORMAT,
        Some(&source_root),
        Some(&raw_source_path),
        None,
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Firebender NativePath source has no canonical identity",
    ))?;
    let stored = store.get_sync_cursor(None, &context.machine_id, &cursor_stream)?;
    let prior = decode_core_cursor_for_migration(stored.as_ref())?;
    let generation = next_generation(prior.as_ref(), &route_identity, &source_revision)?;
    let canonical_source_identity = prior
        .as_ref()
        .map(|cursor| cursor.canonical_source_identity.clone())
        .unwrap_or_else(|| proposed_source_identity.clone());
    let mut authority = FirebenderSourceAuthority {
        configured_source_root,
        database_path,
        canonical_database_path,
        route_identity,
        cursor_stream,
        proposed_source_identity,
        canonical_source_identity,
        source_revision,
        schema_fingerprint,
    };

    let replay_only = import_options.import_profile.is_replay_only();
    let mut summary = ProviderImportSummary::default();
    if !replay_only {
        summary = import_core(
            store,
            &conn,
            &snapshot,
            &mut authority,
            &context,
            &import_options,
            prior,
            generation,
        )?;
    } else {
        require_complete_matching_core(store, &authority, &context)?;
    }

    if summary.work_remaining {
        return Ok(summary);
    }
    if let Some(sink) = import_options.import_profile.sink() {
        if replay_output(&conn, &snapshot, &authority, sink.as_ref())? {
            summary.record_failure(ProviderImportFailure {
                line: 0,
                error: "Firebender Pro output is behind committed Core".to_owned(),
            });
        }
    }
    Ok(summary)
}

#[allow(clippy::too_many_arguments)]
fn import_core(
    store: &mut Store,
    conn: &Connection,
    snapshot: &ProviderSqliteSourceSnapshot,
    authority: &mut FirebenderSourceAuthority,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    prior: Option<FirebenderNativeCursor>,
    generation: u64,
) -> Result<ProviderImportSummary> {
    let same_generation = prior.as_ref().is_some_and(|cursor| {
        cursor.route_identity == authority.route_identity
            && cursor.source_revision == authority.source_revision
            && cursor.schema_fingerprint == authority.schema_fingerprint
    });
    let mut frontier = prior
        .as_ref()
        .filter(|_| same_generation)
        .map(|cursor| cursor.frontier.clone())
        .unwrap_or_else(FirebenderFrontier::initial);
    let mut rejected_records = prior
        .as_ref()
        .filter(|_| same_generation)
        .map_or(0, |cursor| cursor.rejected_records);
    let mut accepted_sessions = prior
        .as_ref()
        .filter(|_| same_generation)
        .map_or(0, |cursor| cursor.accepted_sessions);
    let mut accepted_events = prior
        .as_ref()
        .filter(|_| same_generation)
        .map_or(0, |cursor| cursor.accepted_events);
    if frontier.terminal {
        let mut summary = ProviderImportSummary::default();
        summary.skipped_sessions = usize::try_from(accepted_sessions).unwrap_or(usize::MAX);
        summary.skipped_events = usize::try_from(accepted_events).unwrap_or(usize::MAX);
        summary.skipped = summary
            .skipped_sessions
            .saturating_add(summary.skipped_events);
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }

    let committed_store = Store::open_read_only(store.path())?;
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let mut summary = ProviderImportSummary::default();
        loop {
            if !snapshot.revalidate(&authority.database_path)? {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            let page = with_sqlite_read_snapshot(conn, || build_page(conn, &frontier, false))?;
            if !snapshot.revalidate(&authority.database_path)? {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            let next_rejected =
                rejected_records.saturating_add(u64::from(page.rejection.is_some()));
            let next_accepted_sessions = accepted_sessions
                .saturating_add(u64::from(page.row.is_some() && page.message_start == 0));
            let next_accepted_events = accepted_events
                .saturating_add(u64::try_from(core_event_count(&page)).unwrap_or(u64::MAX));
            let next_cursor = FirebenderNativeCursor {
                version: FIREBENDER_NATIVE_CURSOR_VERSION,
                parser_revision: FIREBENDER_NATIVE_PARSER_REVISION,
                policy_revision: FIREBENDER_NATIVE_POLICY_REVISION,
                route_identity: authority.route_identity.clone(),
                canonical_source_identity: authority.canonical_source_identity.clone(),
                source_revision: authority.source_revision.clone(),
                schema_fingerprint: authority.schema_fingerprint.clone(),
                generation,
                rejected_records: next_rejected,
                accepted_sessions: next_accepted_sessions,
                accepted_events: next_accepted_events,
                frontier: page.next.clone(),
            };
            let page_summary = publish_core_page(
                store,
                &committed_store,
                &bulk_guard,
                snapshot,
                authority,
                context,
                options,
                &page,
                next_cursor,
            )?;
            summary.merge_from(page_summary.summary);
            authority.canonical_source_identity = page_summary.canonical_source_identity;
            frontier = page.next;
            rejected_records = next_rejected;
            accepted_sessions = next_accepted_sessions;
            accepted_events = next_accepted_events;
            if frontier.terminal {
                break;
            }
            if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup {
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

struct PublishedPage {
    summary: ProviderImportSummary,
    canonical_source_identity: String,
}

#[allow(clippy::too_many_arguments)]
fn publish_core_page(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    snapshot: &ProviderSqliteSourceSnapshot,
    authority: &FirebenderSourceAuthority,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    page: &FirebenderPage,
    next_cursor: FirebenderNativeCursor,
) -> Result<PublishedPage> {
    let stored = store.get_sync_cursor(None, &context.machine_id, &authority.cursor_stream)?;
    let encoded = next_cursor.encode()?;
    let next = SyncCursor {
        id: Uuid::new_v4(),
        team_id: None,
        device_id: context.machine_id.clone(),
        stream: authority.cursor_stream.clone(),
        cursor: encoded,
        last_synced_at: Some(context.imported_at),
        timestamps: timestamps(context.imported_at),
    };
    let transition =
        NativePathCursorTransition::new(stored.as_ref().map(|cursor| cursor.cursor.clone()), next);
    let publication_id = publication_id(authority, page, transition.next().cursor.as_str());
    let accounting = NativePathGroupAccounting::new(1, 1, page.retained_bytes)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(PublishedPage {
            summary,
            canonical_source_identity: authority.canonical_source_identity.clone(),
        });
    }

    let raw_source_path = authority.canonical_database_path.display().to_string();
    let source_root = authority.configured_source_root.display().to_string();
    let resolution =
        group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::Firebender,
            source_format: FIREBENDER_SQLITE_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity: authority.route_identity.clone(),
            cursor_stream: authority.cursor_stream.clone(),
            proposed_source_identity: authority.proposed_source_identity.clone(),
            raw_source_path: Some(raw_source_path.clone()),
            source_revision: authority.source_revision.clone(),
            observed_at_ms: context.imported_at.timestamp_millis(),
        })?;
    let mut summary = ProviderImportSummary::default();
    if let Some(row) = page.row.as_ref() {
        let source_id = resolve_source_id(
            committed_store,
            row,
            &context.machine_id,
            &resolution.canonical_source_identity,
            &raw_source_path,
        )?;
        group.upsert_capture_source(&capture_source(
            source_id,
            row,
            authority,
            context,
            &raw_source_path,
            &source_root,
            &resolution.canonical_source_identity,
        ))?;
        group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;
        let session = session(
            committed_store,
            source_id,
            row,
            context,
            options,
            &resolution.canonical_source_identity,
        )?;
        let existed = committed_store.get_session(session.id).is_ok();
        group.upsert_session(&session)?;
        if page.message_start == 0 {
            if existed {
                summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
                summary.skipped = summary.skipped.saturating_add(1);
            } else {
                summary.imported_sessions = summary.imported_sessions.saturating_add(1);
                summary.imported = summary.imported.saturating_add(1);
            }
        }
        publish_events(
            committed_store,
            &mut group,
            source_id,
            &session,
            row,
            page.message_start,
            page.message_end,
            context,
            options,
            &mut summary,
        )?;
    }
    if let Some(rejection) = &page.rejection {
        summary.record_failure(ProviderImportFailure {
            line: usize::try_from(page.expected.row_ordinal)
                .unwrap_or(usize::MAX)
                .saturating_add(1),
            error: rejection.clone(),
        });
    }
    if !snapshot.revalidate(&authority.database_path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(PublishedPage {
        summary,
        canonical_source_identity: resolution.canonical_source_identity,
    })
}

fn resolve_source_id(
    store: &Store,
    row: &FirebenderRow,
    machine_id: &str,
    canonical_source_identity: &str,
    raw_source_path: &str,
) -> Result<Uuid> {
    Ok(store
        .capture_source_by_canonical_identity_session(
            CaptureProvider::Firebender,
            FIREBENDER_SQLITE_SOURCE_FORMAT,
            machine_id,
            canonical_source_identity,
            &row.id,
        )?
        .map(|source| source.id)
        .unwrap_or_else(|| {
            provider_scoped_source_uuid(
                CaptureProvider::Firebender,
                &row.id,
                FIREBENDER_SQLITE_SOURCE_FORMAT,
                Some(raw_source_path),
            )
        }))
}

#[allow(clippy::too_many_arguments)]
fn capture_source(
    source_id: Uuid,
    row: &FirebenderRow,
    authority: &FirebenderSourceAuthority,
    context: &ProviderAdapterContext,
    raw_source_path: &str,
    source_root: &str,
    canonical_source_identity: &str,
) -> CaptureSource {
    let started_at = provider_timestamp_millis(Some(row.created_at), context.imported_at);
    CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Firebender,
            machine_id: context.machine_id.clone(),
            process_id: None,
            cwd: None,
            raw_source_path: Some(raw_source_path.to_owned()),
            source_format: Some(FIREBENDER_SQLITE_SOURCE_FORMAT.to_owned()),
            source_root: Some(source_root.to_owned()),
            source_identity: Some(canonical_source_identity.to_owned()),
            external_session_id: Some(row.id.clone()),
        },
        started_at,
        ended_at: Some(provider_timestamp_millis(Some(row.updated_at), started_at)),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": row.id,
                "source_format": FIREBENDER_SQLITE_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "source_identity": canonical_source_identity,
                "source_root": source_root,
                "source_revision": authority.source_revision,
                "source_identity_key": provider_scoped_source_identity_key(
                    CaptureProvider::Firebender,
                    &row.id,
                    FIREBENDER_SQLITE_SOURCE_FORMAT,
                    Some(raw_source_path),
                ),
                "schema_fingerprint": authority.schema_fingerprint,
                "source_metadata": {
                    "adapter": FIREBENDER_SQLITE_SOURCE_FORMAT,
                    "schema_fingerprint": authority.schema_fingerprint,
                    "storage": ".idea/firebender/chat_history.db",
                },
                "nativepath_publication": FIREBENDER_NATIVE_PARSER_REVISION,
            }),
        ),
    }
}

fn session(
    store: &Store,
    source_id: Uuid,
    row: &FirebenderRow,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    canonical_source_identity: &str,
) -> Result<Session> {
    let started_at = provider_timestamp_millis(Some(row.created_at), context.imported_at);
    let ended_at = Some(provider_timestamp_millis(Some(row.updated_at), started_at));
    let metadata = provider_json_text(&row.metadata_json);
    Ok(Session {
        id: provider_import_session_uuid(
            store,
            CaptureProvider::Firebender,
            &row.id,
            source_id,
            Some(canonical_source_identity),
        )?,
        history_record_id: options.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Firebender,
        external_session_id: Some(row.id.clone()),
        external_agent_id: None,
        agent_type: AgentType::Primary,
        role_hint: Some("primary".to_owned()),
        is_primary: true,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at,
        ended_at,
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": row.id,
                "source_format": FIREBENDER_SQLITE_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "session_idempotency_key":
                    format!("provider-session:firebender:{}", row.id),
                "metadata": {
                    "title": row.name,
                    "metadata": provider_capped_json(&metadata, PROVIDER_MAX_PREVIEW_CHARS),
                    "storage": ".idea/firebender/chat_history.db",
                    "timestamp_note": "message rows do not carry durable per-message timestamps; ctx preserves session created_at/updated_at and import order",
                    "nativepath_publication": FIREBENDER_NATIVE_PARSER_REVISION,
                },
            }),
        ),
    })
}

#[allow(clippy::too_many_arguments)]
fn publish_events(
    store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    source_id: Uuid,
    session: &Session,
    row: &FirebenderRow,
    start: usize,
    end: usize,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let locator = NativeLocator::new(FIREBENDER_LOCATOR_KIND, row.rowid.to_be_bytes().to_vec())
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let values = row.logical_values();
    for (message_index, message) in row.messages[start..end].iter().enumerate() {
        let absolute_index = start.saturating_add(message_index);
        let provider_event_index = u64::try_from(absolute_index)
            .map_err(|_| CaptureError::SystemInvariant("Firebender message index exceeds u64"))?;
        let fallback_offset = i64::try_from(absolute_index)
            .map_err(|_| CaptureError::SystemInvariant("Firebender message index exceeds i64"))?;
        let occurred_at = firebender_message_time(
            message,
            session.started_at + chrono::Duration::milliseconds(fallback_offset),
        );
        let mut native =
            firebender_native_event(&row.id, provider_event_index, message, occurred_at);
        if native.event_type == EventType::ToolOutput {
            let evidence = firebender_output_evidence(message);
            if !evidence.failure && !evidence.timeout {
                continue;
            }
        } else {
            attach_firebender_complete_content(&mut native, &locator, &values, || {
                super::firebender_message_text(message).unwrap_or_else(|| {
                    format!(
                        "Firebender {}",
                        message
                            .get("role")
                            .and_then(Value::as_str)
                            .unwrap_or("message")
                    )
                })
            })?;
        }
        let subrecord = u32::try_from(absolute_index).map_err(|_| {
            CaptureError::InvalidPayload(
                "Firebender message index exceeds complete-content coordinates".to_owned(),
            )
        })?;
        if let Some(metadata) = native.metadata.as_object_mut() {
            metadata.insert("source_record_ordinal".to_owned(), json!(row.row_ordinal));
            metadata.insert("source_record_subrecord_index".to_owned(), json!(subrecord));
        }
        let (event_hash, authority) = native.provider_event_hash.as_ref().map_or_else(
            || {
                compute_payload_hash(&native.payload)
                    .map(|hash| (hash, ProviderEventHashAuthority::NormalizedPayloadFallback))
            },
            |hash| Ok((hash.clone(), ProviderEventHashAuthority::ProviderSupplied)),
        )?;
        let identity = provider_event_import_identity_with_exact_legacy_source(
            store,
            CaptureProvider::Firebender,
            &row.id,
            source_id,
            provider_event_index,
            provider_event_index,
            &event_hash,
            None,
            Some(provider_event_index),
            session.id == provider_session_uuid(CaptureProvider::Firebender, &row.id),
        )?;
        let line_number = absolute_index.saturating_add(1);
        let event = firebender_core_event(
            context,
            options,
            &row.id,
            source_id,
            session.id,
            line_number,
            &native,
            &event_hash,
            authority,
            &identity,
        )?;
        if group.reconcile_provider_event(&event, authority)? {
            summary.imported_events = summary.imported_events.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        } else {
            summary.skipped_events = summary.skipped_events.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn firebender_core_event(
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    provider_session_id: &str,
    source_id: Uuid,
    session_id: Uuid,
    line_number: usize,
    native: &FirebenderNativeEvent,
    event_hash: &str,
    authority: ProviderEventHashAuthority,
    identity: &ProviderEventImportIdentity,
) -> Result<Event> {
    let dedupe_key =
        Store::provider_event_dedupe_key_with_payload_hash(&identity.dedupe_key, event_hash)
            .unwrap_or_else(|| identity.dedupe_key.clone());
    let mut provider_metadata = native.metadata.clone();
    let source_record_coordinates =
        take_firebender_source_record_coordinates(&mut provider_metadata)?;
    let verified_content_locators = provider_metadata
        .as_object_mut()
        .and_then(|metadata| metadata.remove(VERIFIED_CONTENT_LOCATORS_METADATA_KEY))
        .map(|value| {
            VerifiedContentLocatorsV1::from_metadata_value(&value).ok_or_else(|| {
                CaptureError::InvalidPayload(
                    "verified content locator annotation is malformed".to_owned(),
                )
            })
        })
        .transpose()?;
    let mut sync_metadata = json!({
        "provider_session_id": provider_session_id,
        "provider_event_index": native.provider_event_index,
        "provider_event_hash": event_hash,
        "provider_event_hash_authority": authority.as_str(),
        "cursor": native.cursor,
        "source_format": FIREBENDER_SQLITE_SOURCE_FORMAT,
        "source_trust": ProviderSourceTrust::ProviderNative,
        "fixture_line": line_number,
        "imported_at": context.imported_at,
        "event_idempotency_key": format!(
            "provider-event:{}:{}:{}",
            CaptureProvider::Firebender.as_str(),
            provider_session_id,
            native.provider_event_index,
        ),
        "source_record_ordinal": source_record_coordinates
            .as_ref()
            .map(|coordinates| coordinates.0),
        "source_record_subrecord_index": source_record_coordinates
            .as_ref()
            .map(|coordinates| coordinates.1),
        "metadata": provider_metadata,
    });
    if let (Some(metadata), Some(locators)) = (
        sync_metadata.as_object_mut(),
        verified_content_locators.as_ref(),
    ) {
        metadata.insert(
            VERIFIED_CONTENT_LOCATORS_METADATA_KEY.to_owned(),
            locators.to_metadata_value(),
        );
    }
    Ok(Event {
        id: identity.id,
        seq: identity.seq,
        history_record_id: options.history_record_id,
        session_id: Some(session_id),
        run_id: None,
        event_type: native.event_type,
        role: native.role,
        occurred_at: native.occurred_at,
        capture_source_id: Some(source_id),
        payload: json!({
            "provider": CaptureProvider::Firebender.as_str(),
            "provider_session_id": provider_session_id,
            "provider_event_index": native.provider_event_index,
            "provider_event_hash": event_hash,
            "cursor": native.cursor,
            "artifacts": [],
            "body": compact_provider_result_payload(native.event_type, &native.payload),
        }),
        payload_blob_id: None,
        dedupe_key: Some(dedupe_key),
        sync: provider_sync_metadata(Fidelity::Imported, sync_metadata),
    })
}

fn take_firebender_source_record_coordinates(metadata: &mut Value) -> Result<Option<(u64, u32)>> {
    let Some(object) = metadata.as_object_mut() else {
        return Ok(None);
    };
    let ordinal = object.remove("source_record_ordinal");
    let subrecord = object.remove("source_record_subrecord_index");
    if ordinal.is_none() && subrecord.is_none() {
        return Ok(None);
    }
    let ordinal = ordinal.and_then(|value| value.as_u64()).ok_or_else(|| {
        CaptureError::InvalidPayload("source record ordinal annotation is malformed".to_owned())
    })?;
    let subrecord = subrecord
        .and_then(|value| value.as_u64())
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "source record subrecord annotation is malformed".to_owned(),
            )
        })?;
    Ok(Some((ordinal, subrecord)))
}

fn attach_firebender_complete_content(
    event: &mut FirebenderNativeEvent,
    locator: &NativeLocator,
    values: &[NativeSqliteValue],
    complete_text: impl FnOnce() -> String,
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
    let complete_text = complete_text();
    let content_ref = ContentRef::from_bytes(complete_text.as_bytes()).ok_or(
        CaptureError::SystemInvariant("SQLite content length exceeds ContentRef bounds"),
    )?;
    let profile = verified_content_profile(
        CaptureProvider::Firebender,
        FIREBENDER_SQLITE_SOURCE_FORMAT,
        CompleteContentSourceFamily::Sqlite,
        VerifiedContentRole::MessageBody,
    )
    .ok_or(CaptureError::SystemInvariant(
        "supported SQLite message route must have a verified-content profile",
    ))?;
    let native_record_id = event
        .provider_event_hash
        .clone()
        .unwrap_or_else(|| event.cursor.clone());
    let persisted = VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        profile,
        content_ref,
        CompleteContentSourceFamily::Sqlite,
        locator.kind(),
        locator.value(),
        native_record_id,
        firebender_record_digest(values),
    )
    .ok_or(CaptureError::SystemInvariant(
        "SQLite complete-content locator exceeds the bounded canonical schema",
    ))?;
    attach_verified_content_locator(&mut event.metadata, persisted).ok_or(
        CaptureError::SystemInvariant("verified-content locator collection is malformed"),
    )?;
    Ok(())
}

fn firebender_record_digest(values: &[NativeSqliteValue]) -> CompleteContentBodyDigest {
    const DOMAIN: &[u8] = b"ctx-complete-content-sqlite-logical-row-v1\0";
    let mut digest = Sha256::new();
    digest.update(DOMAIN);
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
    CompleteContentBodyDigest::parse(format!("{:x}", digest.finalize()))
        .expect("SHA-256 formatter must return a valid digest")
}

fn core_event_count(page: &FirebenderPage) -> usize {
    let Some(row) = page.row.as_ref() else {
        return 0;
    };
    row.messages[page.message_start..page.message_end]
        .iter()
        .filter(|message| {
            if message.get("role").and_then(Value::as_str) != Some("tool") {
                return true;
            }
            let evidence = firebender_output_evidence(message);
            evidence.failure || evidence.timeout
        })
        .count()
}

fn build_page(
    conn: &Connection,
    expected: &FirebenderFrontier,
    output_lane: bool,
) -> Result<FirebenderPage> {
    expected.validate()?;
    if expected.terminal {
        return Ok(FirebenderPage {
            expected: expected.clone(),
            next: expected.clone(),
            row: None,
            message_start: 0,
            message_end: 0,
            rejection: None,
            retained_bytes: FIREBENDER_PAGE_OVERHEAD_BYTES,
        });
    }
    let Some(candidate) = fetch_candidate(conn, expected)? else {
        let mut next = expected.clone();
        next.terminal = true;
        return Ok(FirebenderPage {
            expected: expected.clone(),
            next,
            row: None,
            message_start: 0,
            message_end: 0,
            rejection: None,
            retained_bytes: FIREBENDER_PAGE_OVERHEAD_BYTES,
        });
    };
    let retained_bytes = candidate.retained_bytes()?;
    if retained_bytes > NATIVE_PATH_MAX_RETAINED_PAGE_BYTES {
        let oversize_authority = format!(
            "oversize:{}:{}:{}:{}",
            candidate.id_bytes,
            candidate.name_bytes,
            candidate.messages_bytes,
            candidate.metadata_bytes
        );
        let next = completed_row_frontier(
            conn,
            expected,
            candidate.rowid,
            candidate.updated_at,
            &oversize_authority,
        )?;
        return Ok(FirebenderPage {
            expected: expected.clone(),
            next,
            row: None,
            message_start: 0,
            message_end: 0,
            rejection: Some(format!(
                "Firebender session rowid {} exceeds the {NATIVE_PATH_MAX_RETAINED_PAGE_BYTES} byte NativePath page bound",
                candidate.rowid
            )),
            retained_bytes: FIREBENDER_PAGE_OVERHEAD_BYTES,
        });
    }
    let (id, name, created_at, messages_json, metadata_json): (
        String,
        String,
        i64,
        String,
        String,
    ) = conn.query_row(
        "select id, name, cast(created_at as integer), messages_json, metadata_json \
         from chat_sessions \
         where rowid = ?1 and cast(updated_at as integer) = ?2",
        params![candidate.rowid, candidate.updated_at],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        },
    )?;
    let parsed = serde_json::from_str::<Value>(&messages_json);
    let (messages, rejection) = match parsed {
        Ok(Value::Array(messages)) => (messages, None),
        Ok(_) => (
            Vec::new(),
            Some(format!(
                "Firebender session {} messages_json is not an array",
                id
            )),
        ),
        Err(error) => (
            Vec::new(),
            Some(format!(
                "Firebender session {} messages_json is invalid JSON: {error}",
                id
            )),
        ),
    };
    let start = if expected.next_message_index == 0 {
        0
    } else {
        if expected.rowid != candidate.rowid || expected.updated_at != candidate.updated_at {
            return Err(CaptureError::InvalidPayload(
                "Firebender NativePath cursor no longer addresses its active row".to_owned(),
            ));
        }
        usize::try_from(expected.next_message_index).map_err(|_| {
            CaptureError::InvalidPayload(
                "Firebender NativePath message cursor exceeds platform limits".to_owned(),
            )
        })?
    };
    if start > messages.len() {
        return Err(CaptureError::InvalidPayload(
            "Firebender NativePath message cursor exceeds its source row".to_owned(),
        ));
    }
    let page_messages = if output_lane {
        FIREBENDER_MAX_OUTPUTS_PER_PAGE
    } else {
        FIREBENDER_MAX_MESSAGES_PER_CORE_PAGE
    };
    let end = start.saturating_add(page_messages).min(messages.len());
    let row = FirebenderRow {
        rowid: candidate.rowid,
        row_ordinal: expected.row_ordinal,
        id,
        name,
        created_at,
        updated_at: candidate.updated_at,
        messages_json,
        metadata_json,
        messages,
    };
    let next = if rejection.is_some() || end == row.messages.len() {
        completed_row_frontier(
            conn,
            expected,
            row.rowid,
            row.updated_at,
            &row.messages_json,
        )?
    } else {
        active_row_frontier(expected, &row, end)?
    };
    Ok(FirebenderPage {
        expected: expected.clone(),
        next,
        row: Some(row),
        message_start: start,
        message_end: end,
        rejection,
        retained_bytes,
    })
}

fn fetch_candidate(
    conn: &Connection,
    frontier: &FirebenderFrontier,
) -> Result<Option<FirebenderRowCandidate>> {
    let columns = sqlite_table_columns(conn, "chat_sessions")?;
    let deleted_filter = if columns.contains("deleted_at") {
        "deleted_at is null and"
    } else {
        ""
    };
    let active = frontier.next_message_index != 0;
    let sql = format!(
        "select rowid, cast(updated_at as integer), length(cast(id as blob)), \
                length(cast(name as blob)), \
                length(cast(messages_json as blob)), length(cast(metadata_json as blob)) \
         from chat_sessions where {deleted_filter} \
              ((?1 = 1 and rowid = ?2 and cast(updated_at as integer) = ?3) or \
               (?1 = 0 and (?4 = 0 or cast(updated_at as integer) > ?3 or \
                (cast(updated_at as integer) = ?3 and rowid > ?2)))) \
         order by cast(updated_at as integer), rowid limit 1"
    );
    let has_after = i64::from(frontier.row_ordinal != 0);
    let _length_guard = SqliteLengthPreflightGuard::new(conn);
    conn.query_row(
        &sql,
        params![
            i64::from(active),
            frontier.rowid,
            frontier.updated_at,
            has_after
        ],
        |row| {
            Ok(FirebenderRowCandidate {
                rowid: row.get(0)?,
                updated_at: row.get(1)?,
                id_bytes: row.get(2)?,
                name_bytes: row.get(3)?,
                messages_bytes: row.get(4)?,
                metadata_bytes: row.get(5)?,
            })
        },
    )
    .optional()
    .map_err(CaptureError::from)
}

fn active_row_frontier(
    expected: &FirebenderFrontier,
    row: &FirebenderRow,
    end: usize,
) -> Result<FirebenderFrontier> {
    let mut hasher = prefix_hasher(expected);
    hash_processed_messages(&mut hasher, row, expected.next_message_index, end);
    Ok(FirebenderFrontier {
        version: FIREBENDER_NATIVE_FRONTIER_VERSION,
        row_ordinal: expected.row_ordinal,
        updated_at: row.updated_at,
        rowid: row.rowid,
        next_message_index: u64::try_from(end).map_err(|_| {
            CaptureError::SystemInvariant("Firebender message frontier exceeds u64")
        })?,
        prefix_sha256: hasher.finalize().into(),
        terminal: false,
    })
}

fn completed_row_frontier(
    conn: &Connection,
    expected: &FirebenderFrontier,
    rowid: i64,
    updated_at: i64,
    semantic_row: &str,
) -> Result<FirebenderFrontier> {
    let mut hasher = prefix_hasher(expected);
    hasher.update(rowid.to_le_bytes());
    hasher.update(updated_at.to_le_bytes());
    hasher.update((semantic_row.len() as u64).to_le_bytes());
    hasher.update(semantic_row.as_bytes());
    let row_ordinal = expected
        .row_ordinal
        .checked_add(1)
        .ok_or(CaptureError::SystemInvariant(
            "Firebender row ordinal exceeds u64",
        ))?;
    let mut next = FirebenderFrontier {
        version: FIREBENDER_NATIVE_FRONTIER_VERSION,
        row_ordinal,
        updated_at,
        rowid,
        next_message_index: 0,
        prefix_sha256: hasher.finalize().into(),
        terminal: false,
    };
    next.terminal = fetch_candidate(conn, &next)?.is_none();
    Ok(next)
}

fn prefix_hasher(frontier: &FirebenderFrontier) -> Sha256 {
    let mut hasher = Sha256::new();
    hasher.update(FIREBENDER_INITIAL_PREFIX_DOMAIN);
    hasher.update(frontier.prefix_sha256);
    hasher
}

fn hash_processed_messages(hasher: &mut Sha256, row: &FirebenderRow, prior_index: u64, end: usize) {
    let start = usize::try_from(prior_index).unwrap_or(usize::MAX);
    hasher.update(row.rowid.to_le_bytes());
    hasher.update(row.updated_at.to_le_bytes());
    hasher.update(prior_index.to_le_bytes());
    for message in row
        .messages
        .iter()
        .skip(start)
        .take(end.saturating_sub(start))
    {
        if let Ok(bytes) = serde_json::to_vec(message) {
            hasher.update((bytes.len() as u64).to_le_bytes());
            hasher.update(bytes);
        }
    }
}

fn decode_core_cursor_for_migration(
    stored: Option<&SyncCursor>,
) -> Result<Option<FirebenderNativeCursor>> {
    let Some(stored) = stored else {
        return Ok(None);
    };
    match decode_native_path_committed_cursor(&stored.cursor) {
        Ok(committed) => FirebenderNativeCursor::decode(committed.provider_cursor()).map(Some),
        Err(_) => {
            // Released pre-NativePath cursors are decoded only to distinguish a
            // valid migration reset from corrupt JSON. Their position is never
            // used as NativePath authority.
            match CertifiedProviderCursor::decode_if_certified(&stored.cursor)? {
                Some(_) | None => Ok(None),
            }
        }
    }
}

fn next_generation(
    prior: Option<&FirebenderNativeCursor>,
    route_identity: &str,
    source_revision: &str,
) -> Result<u64> {
    let Some(prior) = prior else {
        return Ok(0);
    };
    if prior.route_identity == route_identity && prior.source_revision == source_revision {
        return Ok(prior.generation);
    }
    prior
        .generation
        .checked_add(1)
        .ok_or(CaptureError::InvalidPayload(
            "Firebender NativePath generation is exhausted".to_owned(),
        ))
}

fn require_complete_matching_core(
    store: &Store,
    authority: &FirebenderSourceAuthority,
    context: &ProviderAdapterContext,
) -> Result<()> {
    let stored = store
        .get_sync_cursor(None, &context.machine_id, &authority.cursor_stream)?
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Firebender Pro replay requires committed NativePath Core".to_owned(),
            )
        })?;
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    let cursor = FirebenderNativeCursor::decode(committed.provider_cursor())?;
    if cursor.route_identity != authority.route_identity
        || cursor.source_revision != authority.source_revision
        || cursor.schema_fingerprint != authority.schema_fingerprint
        || !cursor.frontier.terminal
    {
        return Err(CaptureError::InvalidPayload(
            "Firebender Pro replay source no longer matches committed Core authority".to_owned(),
        ));
    }
    Ok(())
}

fn replay_output(
    conn: &Connection,
    snapshot: &ProviderSqliteSourceSnapshot,
    authority: &FirebenderSourceAuthority,
    sink: &dyn ProOutputSink,
) -> Result<bool> {
    replay_output_inner(conn, snapshot, authority, sink)
}

fn replay_output_inner(
    conn: &Connection,
    snapshot: &ProviderSqliteSourceSnapshot,
    authority: &FirebenderSourceAuthority,
    sink: &dyn ProOutputSink,
) -> Result<bool> {
    let source = OutputSourceIdentity {
        provider: CaptureProvider::Firebender.as_str().to_owned(),
        namespace_id: authority.canonical_source_identity.clone(),
        source_id: authority.route_identity.clone(),
    };
    let progress = match sink.observe_source(&source) {
        Ok(progress) => progress,
        Err(_) => {
            sink.mark_behind(ProOutputSinkError::new(
                "firebender_output_progress",
                "Firebender Pro output progress is unavailable",
            ));
            return Ok(true);
        }
    };
    let mut state = match output_state(progress, authority, sink) {
        Ok(state) => state,
        Err(_) => {
            sink.mark_behind(ProOutputSinkError::new(
                "firebender_output_progress",
                "Firebender Pro output progress is invalid",
            ));
            return Ok(true);
        }
    };
    if state.complete {
        return Ok(false);
    }
    loop {
        if !snapshot.revalidate(&authority.database_path)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let page = with_sqlite_read_snapshot(conn, || build_page(conn, &state.frontier, true))?;
        if !snapshot.revalidate(&authority.database_path)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let output_page = (|| {
            let observations = output_observations(&page)?;
            let expected = safe_frontier(&page.expected)?;
            let next = safe_frontier(&page.next)?;
            let output = NativeProOutputPage {
                inventory_generation: sink.inventory_generation(),
                source: source.clone(),
                source_epoch: state.source_epoch,
                observed_revision: authority.source_revision.clone(),
                parser_revision: FIREBENDER_OUTPUT_PARSER_REVISION.to_owned(),
                materializer_revision: sink.materializer_revision().to_owned(),
                disposition: state.disposition,
                expected_prior_source_epoch: state.expected_source_epoch,
                expected_prior_frontier: state.expected_sink_frontier.clone(),
                observations,
            };
            let replay = NativeProReplayPage::new_with_source_identity(
                NativeSourceIdentity::new(
                    CaptureProvider::Firebender.as_str(),
                    authority.route_identity.clone(),
                ),
                expected,
                next.clone(),
                page.next.terminal,
                NativePageAccounting {
                    logical_units: output.observations.len(),
                    conservative_serialized_bytes: NATIVE_PATH_MAX_RETAINED_PAGE_BYTES,
                },
                output,
            )
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
            Ok::<_, CaptureError>((replay, next))
        })();
        let (replay, next) = match output_page {
            Ok(page) => page,
            Err(_) => {
                sink.mark_behind(ProOutputSinkError::new(
                    "firebender_output_page",
                    "Firebender Pro output page is invalid",
                ));
                return Ok(true);
            }
        };
        if process_pro_replay_only(replay, sink).is_err() {
            return Ok(true);
        }
        state.frontier = page.next;
        state.expected_source_epoch = Some(state.source_epoch);
        state.expected_sink_frontier = Some(next);
        state.disposition = ProOutputSourceDisposition::AppendOrResume;
        if state.frontier.terminal {
            return Ok(false);
        }
    }
}

struct FirebenderOutputState {
    frontier: FirebenderFrontier,
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_sink_frontier: Option<NativeSafeFrontier>,
    disposition: ProOutputSourceDisposition,
    complete: bool,
}

fn output_state(
    progress: Option<ProOutputProgress>,
    authority: &FirebenderSourceAuthority,
    sink: &dyn ProOutputSink,
) -> Result<FirebenderOutputState> {
    let Some(progress) = progress else {
        return Ok(FirebenderOutputState {
            frontier: FirebenderFrontier::initial(),
            source_epoch: 0,
            expected_source_epoch: None,
            expected_sink_frontier: None,
            disposition: ProOutputSourceDisposition::NewSource,
            complete: false,
        });
    };
    let prior_frontier = progress
        .cursor
        .as_ref()
        .map(|cursor| {
            if cursor.version != FIREBENDER_NATIVE_FRONTIER_VERSION {
                return Err(CaptureError::InvalidPayload(
                    "Firebender output cursor has an unsupported version".to_owned(),
                ));
            }
            serde_json::from_slice::<FirebenderFrontier>(&cursor.payload)
                .map_err(CaptureError::from)
        })
        .transpose()?;
    let matching = progress.observed_revision == authority.source_revision
        && progress.parser_revision == FIREBENDER_OUTPUT_PARSER_REVISION
        && progress.materializer_revision == sink.materializer_revision()
        && prior_frontier.is_some();
    if matching {
        let frontier = prior_frontier.unwrap_or_else(FirebenderFrontier::initial);
        frontier.validate()?;
        return Ok(FirebenderOutputState {
            complete: progress.terminal && frontier.terminal,
            frontier,
            source_epoch: progress.source_epoch,
            expected_source_epoch: Some(progress.source_epoch),
            expected_sink_frontier: progress
                .cursor
                .map(|cursor| NativeSafeFrontier::new(cursor.version, cursor.payload))
                .transpose()
                .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?,
            disposition: ProOutputSourceDisposition::AppendOrResume,
        });
    }
    Ok(FirebenderOutputState {
        frontier: FirebenderFrontier::initial(),
        source_epoch: progress
            .source_epoch
            .checked_add(1)
            .ok_or(CaptureError::InvalidPayload(
                "Firebender output source epoch is exhausted".to_owned(),
            ))?,
        expected_source_epoch: Some(progress.source_epoch),
        expected_sink_frontier: progress
            .cursor
            .map(|cursor| NativeSafeFrontier::new(cursor.version, cursor.payload))
            .transpose()
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?,
        disposition: ProOutputSourceDisposition::Rewrite,
        complete: false,
    })
}

fn output_observations(page: &FirebenderPage) -> Result<Vec<ProOutputObservation>> {
    let Some(row) = page.row.as_ref() else {
        return Ok(Vec::new());
    };
    let mut observations = Vec::new();
    for (offset, message) in row.messages[page.message_start..page.message_end]
        .iter()
        .enumerate()
    {
        if message.get("role").and_then(Value::as_str) != Some("tool") {
            continue;
        }
        let index = page.message_start.saturating_add(offset);
        let provider_event_index = u64::try_from(index).map_err(|_| {
            CaptureError::InvalidPayload("Firebender output index exceeds u64".to_owned())
        })?;
        let subrecord = u32::try_from(index).map_err(|_| {
            CaptureError::InvalidPayload(
                "Firebender output index exceeds native coordinates".to_owned(),
            )
        })?;
        let fallback = provider_timestamp_millis(Some(row.created_at), DateTime::<Utc>::UNIX_EPOCH);
        let occurred_at = firebender_message_time(message, fallback);
        let evidence = firebender_output_evidence(message);
        let call_id = message
            .get("tool_call_id")
            .or_else(|| message.get("toolCallId"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let native_record_id = message
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| call_id.clone())
            .unwrap_or_else(|| format!("message:{provider_event_index}"));
        observations.push(ProOutputObservation {
            kind: OutputObservationKind::Tool,
            coordinate: OutputNativeCoordinate {
                unit_key: format!("firebender:{}:message:{index:010}:output", row.id),
                native_sequence: provider_event_index,
                native_record_id: Some(native_record_id),
                source_record_ordinal: Some(row.row_ordinal),
                source_record_subrecord_index: Some(subrecord),
                byte_start: None,
                byte_end_exclusive: None,
            },
            occurred_at_unix_ms: Some(occurred_at.timestamp_millis()),
            associations: OutputAssociations {
                direct_session_id: row.id.clone(),
                root_session_id: row.id.clone(),
                parent_session_id: None,
                provider_session_id: Some(row.id.clone()),
                agent_id: None,
                repository: None,
            },
            call_id,
            command: None,
            outcome: OutputOutcomeMetadata {
                outcome: if evidence.timeout {
                    OutputOutcome::Timeout
                } else if evidence.failure {
                    OutputOutcome::Failure
                } else if evidence.success {
                    OutputOutcome::Success
                } else {
                    OutputOutcome::Unknown
                },
                exit_code: evidence.exit_code,
                duration_ms: evidence.duration_ms,
            },
            locator: OutputSourceLocator {
                version: 1,
                kind: FIREBENDER_LOCATOR_KIND.to_owned(),
                payload: row.rowid.to_be_bytes().to_vec(),
            },
            content: firebender_result_content(message)
                .unwrap_or_default()
                .into_bytes(),
        });
    }
    Ok(observations)
}

fn safe_frontier(frontier: &FirebenderFrontier) -> Result<NativeSafeFrontier> {
    let encoded = serde_json::to_vec(frontier)?;
    NativeSafeFrontier::new(FIREBENDER_NATIVE_FRONTIER_VERSION, encoded)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

fn retire_missing_firebender_source(
    original_path: &Path,
    database_path: &Path,
    store: &mut Store,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    if options.inventory_observation_token.is_none() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: database_path.to_path_buf(),
            reason: "Firebender chat_history.db is missing",
        });
    }
    let route_identity = provider_path_identity(database_path)?;
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Firebender,
        FIREBENDER_SQLITE_SOURCE_FORMAT,
        &route_identity,
    );
    let Some(stored) = store.get_sync_cursor(None, &context.machine_id, &stream)? else {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: database_path.to_path_buf(),
            reason: "Firebender chat_history.db is missing and has no prior route authority",
        });
    };
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    let prior = FirebenderNativeCursor::decode(committed.provider_cursor())?;
    let direct_database_path =
        original_path.file_name().and_then(|name| name.to_str()) == Some("chat_history.db");
    let reason = if direct_database_path || original_path.exists() {
        ProviderSourceRouteRetirementReason::SourceMissing
    } else {
        ProviderSourceRouteRetirementReason::RootMissing
    };
    let retirement = ProviderSourceRouteRetirement {
        provider: CaptureProvider::Firebender,
        source_format: FIREBENDER_SQLITE_SOURCE_FORMAT.to_owned(),
        machine_id: context.machine_id.clone(),
        locator_identity: route_identity,
        cursor_stream: stream.clone(),
        expected_canonical_source_identity: prior.canonical_source_identity.clone(),
        expected_source_revision: prior.source_revision.clone(),
        retired_at_ms: context.imported_at.timestamp_millis(),
        reason,
    };
    let publication_id = retirement_publication_id(&retirement);
    if committed.publication_id() == publication_id {
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }
    let next = SyncCursor {
        id: Uuid::new_v4(),
        team_id: None,
        device_id: context.machine_id.clone(),
        stream,
        cursor: committed.provider_cursor().to_owned(),
        last_synced_at: Some(context.imported_at),
        timestamps: timestamps(context.imported_at),
    };
    let transition = NativePathCursorTransition::new(Some(stored.cursor), next);
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let admission = store.admit_event_search_bulk_group(&bulk_guard)?;
        let mut group = store.begin_native_path_publication_group(
            admission,
            NativePathGroupAccounting::new(0, 1, 0)?,
        )?;
        if matches!(
            group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
            NativePathCursorSetClassification::AllNextSameGroup { .. }
        ) {
            group.commit()?;
            let mut summary = ProviderImportSummary::default();
            summary.set_work_result(ProviderImportWorkResult::NoOp);
            return Ok(summary);
        }
        let disposition = group.retire_provider_source_route(&retirement)?;
        group.prepare_journal_checkpoint()?;
        group.publish_cursor_set()?;
        group.commit()?;
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(match disposition {
            ProviderSourceRouteRetirementDisposition::Retired => ProviderImportWorkResult::Changed,
            ProviderSourceRouteRetirementDisposition::AlreadyRetired => {
                ProviderImportWorkResult::NoOp
            }
        });
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

fn publication_id(
    authority: &FirebenderSourceAuthority,
    page: &FirebenderPage,
    next_cursor: &str,
) -> String {
    let mut digest = Sha256::new();
    digest.update(FIREBENDER_PUBLICATION_DOMAIN);
    digest.update(authority.route_identity.as_bytes());
    digest.update(authority.source_revision.as_bytes());
    digest.update(page.expected.prefix_sha256);
    digest.update(page.next.prefix_sha256);
    digest.update(next_cursor.as_bytes());
    format!("firebender-native:{}", hex(&digest.finalize()))
}

fn retirement_publication_id(retirement: &ProviderSourceRouteRetirement) -> String {
    let mut digest = Sha256::new();
    digest.update(FIREBENDER_RETIREMENT_DOMAIN);
    digest.update(retirement.machine_id.as_bytes());
    digest.update(retirement.locator_identity.as_bytes());
    digest.update(retirement.cursor_stream.as_bytes());
    digest.update(retirement.expected_canonical_source_identity.as_bytes());
    digest.update(retirement.expected_source_revision.as_bytes());
    format!("firebender-retirement:{}", hex(&digest.finalize()))
}

fn firebender_source_snapshot(path: &Path) -> Result<ProviderSqliteSourceSnapshot> {
    ProviderSqliteSourceSnapshot::read(
        path,
        "Firebender SQLite source must be a regular non-symlink file",
        "Firebender SQLite sidecar must be a regular non-symlink file",
    )
}

fn firebender_source_revision(
    snapshot: &ProviderSqliteSourceSnapshot,
    schema_fingerprint: &str,
) -> String {
    format!(
        "firebender-native-sqlite-v1:parser={FIREBENDER_NATIVE_PARSER_REVISION};policy={FIREBENDER_NATIVE_POLICY_REVISION};schema={schema_fingerprint};{}",
        snapshot.revision_component(),
    )
}

fn validate_schema(conn: &Connection, path: &Path) -> Result<()> {
    if !sqlite_table_exists(conn, "chat_sessions")? {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Firebender chat_history.db is missing required chat_sessions table",
        });
    }
    let columns = sqlite_table_columns(conn, "chat_sessions")?;
    ensure_sqlite_table_columns(
        &columns,
        "Firebender chat_sessions table",
        &[
            "id",
            "name",
            "created_at",
            "updated_at",
            "messages_json",
            "metadata_json",
        ],
    )
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
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

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    };

    use super::*;

    #[derive(Default)]
    struct RecordingOutputSink {
        fail_once: AtomicBool,
        behind: AtomicUsize,
        progress: Mutex<Option<crate::ProOutputProgress>>,
        contents: Mutex<Vec<Vec<u8>>>,
    }

    impl crate::ProOutputSink for RecordingOutputSink {
        fn inventory_generation(&self) -> u64 {
            1
        }

        fn materializer_revision(&self) -> &str {
            "firebender-nativepath-test-materializer-v1"
        }

        fn observe_source(
            &self,
            _source: &crate::OutputSourceIdentity,
        ) -> std::result::Result<Option<crate::ProOutputProgress>, crate::ProOutputSinkError>
        {
            Ok(self.progress.lock().unwrap().clone())
        }

        fn materialize_page(
            &self,
            page: crate::ProOutputMaterializationPage,
        ) -> std::result::Result<crate::ProOutputPageResult, crate::ProOutputSinkError> {
            if self.fail_once.swap(false, Ordering::SeqCst) {
                return Err(crate::ProOutputSinkError::new(
                    "firebender_test_failure",
                    "retry the output page",
                ));
            }
            self.contents.lock().unwrap().extend(
                page.observations
                    .iter()
                    .map(|observation| observation.content.clone()),
            );
            let committed_cursor = page.next_safe_cursor.clone();
            *self.progress.lock().unwrap() = Some(crate::ProOutputProgress {
                source_epoch: page.source_epoch,
                observed_revision: page.observed_revision.clone(),
                cursor: Some(committed_cursor.clone()),
                parser_revision: page.parser_revision.clone(),
                materializer_revision: page.materializer_revision.clone(),
                terminal: page.terminal,
            });
            Ok(crate::ProOutputPageResult {
                source_epoch: page.source_epoch,
                committed_cursor,
                accepted_outputs: u32::try_from(page.observations.len()).unwrap(),
                materialized_facts: 0,
                replayed: false,
            })
        }

        fn mark_behind(&self, _error: crate::ProOutputSinkError) {
            self.behind.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn native_cursor_round_trips() {
        let cursor = FirebenderNativeCursor {
            version: FIREBENDER_NATIVE_CURSOR_VERSION,
            parser_revision: FIREBENDER_NATIVE_PARSER_REVISION,
            policy_revision: FIREBENDER_NATIVE_POLICY_REVISION,
            route_identity: "route".to_owned(),
            canonical_source_identity: "source".to_owned(),
            source_revision: "revision".to_owned(),
            schema_fingerprint: "schema".to_owned(),
            generation: 2,
            rejected_records: 3,
            accepted_sessions: 4,
            accepted_events: 5,
            frontier: FirebenderFrontier::initial(),
        };
        let encoded = cursor.encode().expect("encode");
        assert_eq!(
            FirebenderNativeCursor::decode(&encoded).expect("decode"),
            cursor
        );
    }

    #[test]
    fn successful_output_is_not_core_eligible() {
        let message = json!({
            "role": "tool",
            "content": "SECRET_OUTPUT",
            "status": "success"
        });
        let evidence = firebender_output_evidence(&message);
        assert!(evidence.success);
        assert!(!evidence.failure);
        assert!(!evidence.timeout);
    }

    #[test]
    fn failure_output_keeps_only_sparse_outcome_authority() {
        let message = json!({
            "role": "tool",
            "content": "SECRET_OUTPUT",
            "status": "failed",
            "exit_code": 9
        });
        let event = super::super::firebender_native_event(
            "session",
            0,
            &message,
            DateTime::<Utc>::UNIX_EPOCH,
        );
        let evidence = firebender_output_evidence(&message);
        assert!(evidence.failure);
        assert_eq!(evidence.exit_code, Some(9));
        assert!(!event.payload.to_string().contains("SECRET_OUTPUT"));
    }

    #[test]
    fn output_failure_keeps_core_success_and_later_replay_catches_up() {
        const SECRET: &str = "firebender-private-output";

        let temp = crate::test_support_paths::tempdir().unwrap();
        let root = temp.path().join("project");
        let database = root
            .join(".idea")
            .join("firebender")
            .join("chat_history.db");
        fs::create_dir_all(database.parent().unwrap()).unwrap();
        let conn = Connection::open(&database).unwrap();
        conn.execute_batch(
            "create table chat_sessions (
                id text not null,
                name text not null,
                created_at integer not null,
                updated_at integer not null,
                messages_json text not null,
                metadata_json text not null
            );",
        )
        .unwrap();
        conn.execute(
            "insert into chat_sessions
             (id, name, created_at, updated_at, messages_json, metadata_json)
             values (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                "firebender-session",
                "test",
                1_785_000_000_i64,
                1_785_000_001_i64,
                json!([
                    {"role": "user", "content": "core message"},
                    {
                        "role": "tool",
                        "tool_call_id": "call-1",
                        "status": "success",
                        "content": SECRET
                    }
                ])
                .to_string(),
                "{}",
            ],
        )
        .unwrap();
        drop(conn);

        let mut store = Store::open(temp.path().join("ctx.sqlite")).unwrap();
        let sink = Arc::new(RecordingOutputSink::default());
        sink.fail_once.store(true, Ordering::SeqCst);
        let context = ProviderAdapterContext {
            machine_id: "firebender-nativepath-test".to_owned(),
            source_path: Some(root.clone()),
            source_root: Some(root.clone()),
            imported_at: "2026-07-25T12:00:00Z".parse().unwrap(),
        };
        let summary = import_firebender_nativepath(
            &root,
            &mut store,
            context.clone(),
            ProviderImportOptions {
                import_profile: crate::ImportProfile::CoreAndPro(sink.clone()),
                ..ProviderImportOptions::default()
            },
        )
        .unwrap();

        assert_eq!(summary.imported_events, 1);
        assert_eq!(summary.failed, 1);
        assert_eq!(
            summary.failures[0].error,
            "Firebender Pro output is behind committed Core"
        );
        assert!(sink.behind.load(Ordering::SeqCst) > 0);
        assert!(!serde_json::to_string(&store.export_archive().unwrap())
            .unwrap()
            .contains(SECRET));

        let replay = import_firebender_nativepath(
            &root,
            &mut store,
            context,
            ProviderImportOptions {
                import_profile: crate::ImportProfile::ProReplayOnly(sink.clone()),
                ..ProviderImportOptions::default()
            },
        )
        .unwrap();
        assert_eq!(replay.imported_events, 0);
        assert_eq!(replay.failed, 0);
        assert_eq!(
            sink.contents.lock().unwrap().as_slice(),
            [SECRET.as_bytes()]
        );
    }
}
