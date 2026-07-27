use std::{
    fs,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    compute_payload_hash, AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor,
    CaptureSourceKind, ContentRef, Event, EventRole, EventType, Fidelity, ProviderSourceTrust,
    Session, SessionStatus, SyncCursor,
};
use ctx_history_store::{
    decode_native_path_committed_cursor, EventSearchBulkGuard, NativePathCursorSetClassification,
    NativePathCursorTransition, NativePathGroupAccounting, NativePathRetainedSourceEntities,
    NativePathSourceEntityFrontier, NativePathSourceEntityKind, NativePathSourceGenerationKey,
    ProviderEventHashAuthority, ProviderSourceLocatorObservation, ProviderSourceRouteRetirement,
    ProviderSourceRouteRetirementReason, Store, NATIVE_PATH_MAX_RETAINED_PAGE_BYTES,
};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    complete_content::{
        attach_verified_content_locator, verified_content_profile, CompleteContentSourceFamily,
        VerifiedContentLocatorV1, VerifiedContentLocatorsV1, VerifiedContentRole,
        VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
    },
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
        normalization::{
            provider_capped_json, provider_policy_body, provider_policy_event_text,
            provider_result_identifier_evidence, provider_result_outcome_evidence,
        },
        sqlite::{
            open_provider_sqlite_readonly, sqlite_schema_fingerprint, with_sqlite_read_snapshot,
            ProviderSqliteSourceSnapshot,
        },
    },
    CaptureError, CaptureWorkLimit, OutputAssociations, OutputNativeCoordinate,
    OutputObservationKind, OutputOutcome, OutputOutcomeMetadata, OutputSourceIdentity,
    OutputSourceLocator, ProOutputObservation, ProOutputProgress, ProOutputSink,
    ProOutputSinkError, ProOutputSourceDisposition, ProviderAdapterContext, ProviderImportFailure,
    ProviderImportOptions, ProviderImportSummary, ProviderImportWorkResult, Result,
    DEEPAGENTS_SQLITE_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS,
};

use super::{
    complete_content::{deepagents_write_record_digest, DeepAgentsContentAddress},
    message::{deepagents_messages_from_blob, DeepAgentsMessage},
    source::{
        deepagents_checkpoint_time, deepagents_hydrate_write, deepagents_next_thread_candidate,
        deepagents_next_write_candidate, deepagents_source_snapshot, deepagents_thread_summary,
        deepagents_validate_schema, deepagents_write_candidate_at, DeepAgentsThread,
        DeepAgentsThreadSummary, DeepAgentsWriteCandidate, DeepAgentsWriteKey,
    },
    DEEPAGENTS_CONTENT_LOCATOR_KIND,
};

const DEEPAGENTS_NATIVE_CURSOR_VERSION: u32 = 1;
const DEEPAGENTS_OUTPUT_FRONTIER_VERSION: u32 = 1;
const DEEPAGENTS_NATIVE_PARSER_REVISION: &str = "deepagents-nativepath-sqlite-v1";
const DEEPAGENTS_NATIVE_POLICY_REVISION: &str = "deepagents-core-private-output-v1";
const DEEPAGENTS_OUTPUT_PARSER_REVISION: &str = "deepagents-native-output-v1";
const DEEPAGENTS_PAGE_UNITS: usize = 48;
const DEEPAGENTS_RETIREMENT_UNITS: usize = 48;
const DEEPAGENTS_PAGE_OVERHEAD_BYTES: usize = 256 * 1024;
const DEEPAGENTS_PUBLICATION_DOMAIN: &[u8] = b"ctx-deepagents-native-publication-v1\0";

#[derive(Debug)]
struct DeepAgentsSourceAuthority {
    configured_source_root: PathBuf,
    database_path: PathBuf,
    canonical_database_path: PathBuf,
    route_identity: String,
    cursor_stream: String,
    proposed_source_identity: String,
    canonical_source_identity: String,
    source_revision: String,
    schema_fingerprint: String,
    sqlite_user_version: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "phase")]
enum DeepAgentsCorePhase {
    Threads {
        after_rowid: Option<i64>,
    },
    Writes {
        after_rowid: Option<i64>,
        active_rowid: Option<i64>,
        next_message_offset: u32,
        current_thread_id: Option<String>,
        next_event_index: u64,
    },
    StageSources {
        next_source: usize,
    },
    Retire {
        after: Option<SerializableRetirementFrontier>,
    },
    Complete,
    MissingStage {
        next_source: usize,
    },
    MissingRetire {
        after: Option<SerializableRetirementFrontier>,
    },
    MissingComplete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SerializableRetirementFrontier {
    kind: String,
    id: Uuid,
}

impl SerializableRetirementFrontier {
    fn from_store(value: NativePathSourceEntityFrontier) -> Self {
        Self {
            kind: value.kind.as_str().to_owned(),
            id: value.id,
        }
    }

    fn to_store(&self) -> Result<NativePathSourceEntityFrontier> {
        let kind = match self.kind.as_str() {
            "session" => NativePathSourceEntityKind::Session,
            "session_edge" => NativePathSourceEntityKind::SessionEdge,
            "run" => NativePathSourceEntityKind::Run,
            "event" => NativePathSourceEntityKind::Event,
            "file_touch" => NativePathSourceEntityKind::FileTouch,
            _ => {
                return Err(CaptureError::InvalidPayload(
                    "Deep Agents retirement cursor has an unsupported entity kind".to_owned(),
                ));
            }
        };
        Ok(NativePathSourceEntityFrontier { kind, id: self.id })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeepAgentsNativeCursor {
    version: u32,
    parser_revision: String,
    policy_revision: String,
    route_identity: String,
    canonical_source_identity: String,
    source_revision: String,
    schema_fingerprint: String,
    generation: u64,
    generation_staged: bool,
    accepted_sessions: u64,
    accepted_events: u64,
    rejected_records: u64,
    phase: DeepAgentsCorePhase,
}

impl DeepAgentsNativeCursor {
    fn encode(&self) -> Result<String> {
        serde_json::to_string(self).map_err(CaptureError::from)
    }

    fn decode(encoded: &str) -> Result<Self> {
        let cursor: Self = serde_json::from_str(encoded)?;
        if cursor.version != DEEPAGENTS_NATIVE_CURSOR_VERSION
            || cursor.parser_revision != DEEPAGENTS_NATIVE_PARSER_REVISION
            || cursor.policy_revision != DEEPAGENTS_NATIVE_POLICY_REVISION
            || cursor.route_identity.is_empty()
            || cursor.canonical_source_identity.is_empty()
            || cursor.source_revision.is_empty()
            || cursor.schema_fingerprint.is_empty()
        {
            return Err(CaptureError::InvalidPayload(
                "Deep Agents NativePath cursor is unsupported or incomplete".to_owned(),
            ));
        }
        Ok(cursor)
    }

    fn is_complete(&self) -> bool {
        matches!(
            self.phase,
            DeepAgentsCorePhase::Complete | DeepAgentsCorePhase::MissingComplete
        )
    }
}

#[derive(Debug)]
struct DeepAgentsThreadPage {
    entries: Vec<DeepAgentsThreadEntry>,
    next_after_rowid: Option<i64>,
    terminal: bool,
    retained_bytes: usize,
}

#[derive(Debug)]
struct DeepAgentsThreadEntry {
    rowid: i64,
    summary: Option<DeepAgentsThreadSummary>,
    rejection: Option<String>,
}

#[derive(Debug)]
struct DeepAgentsWritePage {
    key: Option<DeepAgentsWriteKey>,
    rowid: Option<i64>,
    messages: Vec<DeepAgentsParsedMessage>,
    value_type: Option<String>,
    value: Vec<u8>,
    occurred_at: Option<DateTime<Utc>>,
    rejection: Option<String>,
    next_phase: DeepAgentsCorePhase,
    retained_bytes: usize,
}

#[derive(Debug, Clone)]
pub(super) struct DeepAgentsParsedMessage {
    pub(super) offset: usize,
    pub(super) provider_event_index: u64,
    pub(super) message: DeepAgentsMessage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeepAgentsOutputFrontier {
    version: u32,
    after_rowid: Option<i64>,
    active_rowid: Option<i64>,
    next_message_offset: u32,
    terminal: bool,
}

impl DeepAgentsOutputFrontier {
    fn initial() -> Self {
        Self {
            version: DEEPAGENTS_OUTPUT_FRONTIER_VERSION,
            after_rowid: None,
            active_rowid: None,
            next_message_offset: 0,
            terminal: false,
        }
    }
}

pub(super) fn import_deepagents_sqlite_nativepath(
    path: &Path,
    store: &mut Store,
    mut context: ProviderAdapterContext,
    options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    if context.source_path.is_none() {
        context.source_path = Some(path.to_path_buf());
    }
    let database_path = absolute_path(path)?;
    if !database_path.exists() {
        return retire_missing_source(path, &database_path, store, &context, &options);
    }

    let canonical_database_path = fs::canonicalize(&database_path)?;
    let snapshot = deepagents_source_snapshot(&database_path)?;
    let conn = open_provider_sqlite_readonly(&database_path)?;
    if !snapshot.revalidate(&database_path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    deepagents_validate_schema(&conn, &database_path)?;
    let sqlite_user_version =
        conn.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))?;
    let schema_fingerprint = sqlite_schema_fingerprint(&conn)?;
    let source_revision = source_revision(&snapshot, &schema_fingerprint);
    let configured_source_root = context
        .source_root
        .clone()
        .or_else(|| context.source_path.clone())
        .unwrap_or_else(|| database_path.clone());
    let route_identity = provider_path_identity(&canonical_database_path)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::DeepAgents,
        DEEPAGENTS_SQLITE_SOURCE_FORMAT,
        &route_identity,
    );
    let raw_source_path = canonical_database_path.display().to_string();
    let source_root = configured_source_root.display().to_string();
    let proposed_source_identity = provider_source_identity(
        CaptureProvider::DeepAgents,
        DEEPAGENTS_SQLITE_SOURCE_FORMAT,
        Some(&source_root),
        Some(&raw_source_path),
        None,
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Deep Agents NativePath source has no canonical identity",
    ))?;
    let stored = store.get_sync_cursor(None, &context.machine_id, &cursor_stream)?;
    let prior = decode_core_cursor_for_migration(stored.as_ref())?;
    let same_generation = prior.as_ref().is_some_and(|cursor| {
        cursor.route_identity == route_identity
            && cursor.source_revision == source_revision
            && cursor.schema_fingerprint == schema_fingerprint
            && !matches!(
                cursor.phase,
                DeepAgentsCorePhase::MissingStage { .. }
                    | DeepAgentsCorePhase::MissingRetire { .. }
                    | DeepAgentsCorePhase::MissingComplete
            )
    });
    let generation = if same_generation {
        prior.as_ref().map_or(0, |cursor| cursor.generation)
    } else {
        prior
            .as_ref()
            .map_or(0, |cursor| cursor.generation.saturating_add(1))
    };
    if generation == u64::MAX
        && prior
            .as_ref()
            .is_some_and(|cursor| cursor.generation == u64::MAX)
    {
        return Err(CaptureError::SystemInvariant(
            "Deep Agents source generation is exhausted",
        ));
    }
    let canonical_source_identity = prior
        .as_ref()
        .map(|cursor| cursor.canonical_source_identity.clone())
        .unwrap_or_else(|| proposed_source_identity.clone());
    let mut authority = DeepAgentsSourceAuthority {
        configured_source_root,
        database_path,
        canonical_database_path,
        route_identity,
        cursor_stream,
        proposed_source_identity,
        canonical_source_identity,
        source_revision,
        schema_fingerprint,
        sqlite_user_version,
    };

    if options.import_profile.is_replay_only() {
        require_complete_matching_core(store, &authority, &context)?;
        let mut summary = ProviderImportSummary::default();
        if let Some(sink) = options.import_profile.sink() {
            if replay_outputs(&conn, &snapshot, &authority, &context, sink.as_ref())? {
                record_output_behind(&mut summary);
            }
        }
        return Ok(summary);
    }

    let cursor = prior
        .filter(|_| same_generation)
        .unwrap_or(DeepAgentsNativeCursor {
            version: DEEPAGENTS_NATIVE_CURSOR_VERSION,
            parser_revision: DEEPAGENTS_NATIVE_PARSER_REVISION.to_owned(),
            policy_revision: DEEPAGENTS_NATIVE_POLICY_REVISION.to_owned(),
            route_identity: authority.route_identity.clone(),
            canonical_source_identity: authority.canonical_source_identity.clone(),
            source_revision: authority.source_revision.clone(),
            schema_fingerprint: authority.schema_fingerprint.clone(),
            generation,
            generation_staged: false,
            accepted_sessions: 0,
            accepted_events: 0,
            rejected_records: 0,
            phase: DeepAgentsCorePhase::Threads { after_rowid: None },
        });
    if cursor.is_complete() {
        let mut summary = ProviderImportSummary::default();
        summary.skipped_sessions = usize::try_from(cursor.accepted_sessions).unwrap_or(usize::MAX);
        summary.skipped_events = usize::try_from(cursor.accepted_events).unwrap_or(usize::MAX);
        summary.skipped = summary
            .skipped_sessions
            .saturating_add(summary.skipped_events);
        summary.failed = usize::try_from(cursor.rejected_records).unwrap_or(usize::MAX);
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        if let Some(sink) = options.import_profile.sink() {
            if replay_outputs(&conn, &snapshot, &authority, &context, sink.as_ref())? {
                record_output_behind(&mut summary);
            }
        }
        return Ok(summary);
    }

    let mut summary = import_core(
        store,
        &conn,
        &snapshot,
        &mut authority,
        &context,
        &options,
        cursor,
    )?;
    if !summary.work_remaining {
        if let Some(sink) = options.import_profile.sink() {
            if replay_outputs(&conn, &snapshot, &authority, &context, sink.as_ref())? {
                record_output_behind(&mut summary);
            }
        }
    }
    if summary.work_result.is_none() {
        summary.set_work_result(ProviderImportWorkResult::NoOp);
    }
    Ok(summary)
}

#[allow(clippy::too_many_arguments)]
fn import_core(
    store: &mut Store,
    conn: &Connection,
    snapshot: &ProviderSqliteSourceSnapshot,
    authority: &mut DeepAgentsSourceAuthority,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    mut cursor: DeepAgentsNativeCursor,
) -> Result<ProviderImportSummary> {
    let committed_store = Store::open_read_only(store.path())?;
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let mut summary = ProviderImportSummary::default();
        loop {
            if !snapshot.revalidate(&authority.database_path)? {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            let next = match cursor.phase.clone() {
                DeepAgentsCorePhase::Threads { after_rowid } => {
                    let page = with_sqlite_read_snapshot(conn, || {
                        build_thread_page(conn, context, after_rowid)
                    })?;
                    publish_thread_page(
                        store,
                        &committed_store,
                        &bulk_guard,
                        snapshot,
                        authority,
                        context,
                        options,
                        &cursor,
                        page,
                        &mut summary,
                    )?
                }
                DeepAgentsCorePhase::Writes {
                    after_rowid,
                    active_rowid,
                    next_message_offset,
                    current_thread_id,
                    next_event_index,
                } => {
                    let page = with_sqlite_read_snapshot(conn, || {
                        build_write_page(
                            conn,
                            context,
                            after_rowid,
                            active_rowid,
                            next_message_offset,
                            current_thread_id,
                            next_event_index,
                        )
                    })?;
                    publish_write_page(
                        store,
                        &committed_store,
                        &bulk_guard,
                        snapshot,
                        authority,
                        context,
                        options,
                        &cursor,
                        page,
                        &mut summary,
                    )?
                }
                DeepAgentsCorePhase::StageSources { next_source } => publish_source_stage_page(
                    store,
                    &bulk_guard,
                    Some(snapshot),
                    authority,
                    context,
                    &cursor,
                    next_source,
                    false,
                )?,
                DeepAgentsCorePhase::Retire { after } => publish_retirement_page(
                    store,
                    &bulk_guard,
                    Some(snapshot),
                    authority,
                    context,
                    &cursor,
                    after,
                    false,
                )?,
                DeepAgentsCorePhase::Complete => break,
                DeepAgentsCorePhase::MissingStage { .. }
                | DeepAgentsCorePhase::MissingRetire { .. }
                | DeepAgentsCorePhase::MissingComplete => {
                    return Err(CaptureError::InvalidPayload(
                        "Deep Agents live source resumed a disappearance cursor".to_owned(),
                    ));
                }
            };
            authority.canonical_source_identity = next.canonical_source_identity.clone();
            cursor = next;
            summary.set_work_result(ProviderImportWorkResult::Changed);
            if cursor.is_complete() {
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

fn build_thread_page(
    conn: &Connection,
    context: &ProviderAdapterContext,
    after_rowid: Option<i64>,
) -> Result<DeepAgentsThreadPage> {
    let mut entries = Vec::new();
    let mut after = after_rowid;
    let mut retained_bytes = DEEPAGENTS_PAGE_OVERHEAD_BYTES;
    let mut terminal = false;
    while entries.len() < DEEPAGENTS_PAGE_UNITS {
        let Some(candidate) = deepagents_next_thread_candidate(conn, after)? else {
            terminal = true;
            break;
        };
        after = Some(candidate.rowid);
        let summary = candidate
            .thread_id
            .as_deref()
            .map(|thread_id| deepagents_thread_summary(conn, context, thread_id, None))
            .transpose()?
            .flatten();
        let rejection = candidate.rejection_reason.or_else(|| {
            summary
                .is_none()
                .then(|| "Deep Agents thread has no valid bounded checkpoint metadata".to_owned())
        });
        retained_bytes = retained_bytes.saturating_add(
            summary
                .as_ref()
                .map(|summary| {
                    summary
                        .thread
                        .thread_id
                        .len()
                        .saturating_add(summary.thread.agent_name.as_ref().map_or(0, String::len))
                        .saturating_add(summary.thread.cwd.as_ref().map_or(0, String::len))
                })
                .unwrap_or_default(),
        );
        entries.push(DeepAgentsThreadEntry {
            rowid: candidate.rowid,
            summary,
            rejection,
        });
    }
    ensure_retained_bound(retained_bytes)?;
    Ok(DeepAgentsThreadPage {
        entries,
        next_after_rowid: after,
        terminal,
        retained_bytes,
    })
}

#[allow(clippy::too_many_arguments)]
fn build_write_page(
    conn: &Connection,
    context: &ProviderAdapterContext,
    after_rowid: Option<i64>,
    active_rowid: Option<i64>,
    next_message_offset: u32,
    current_thread_id: Option<String>,
    next_event_index: u64,
) -> Result<DeepAgentsWritePage> {
    let candidate = match active_rowid {
        Some(rowid) => deepagents_write_candidate_at(conn, rowid)?
            .ok_or(CaptureError::SourceChangedDuringCapture)?,
        None => match deepagents_next_write_candidate(conn, after_rowid)? {
            Some(candidate) => candidate,
            None => {
                return Ok(DeepAgentsWritePage {
                    key: None,
                    rowid: None,
                    messages: Vec::new(),
                    value_type: None,
                    value: Vec::new(),
                    occurred_at: None,
                    rejection: None,
                    next_phase: DeepAgentsCorePhase::StageSources { next_source: 0 },
                    retained_bytes: DEEPAGENTS_PAGE_OVERHEAD_BYTES,
                });
            }
        },
    };
    let rowid = candidate.rowid;
    let Some(key) = candidate.key.clone() else {
        let rejection = candidate.rejection_reason.clone().unwrap_or_else(|| {
            format!(
                "Deep Agents write exceeds the bounded record limit ({} bytes)",
                candidate.observed_bytes().unwrap_or(u64::MAX)
            )
        });
        return Ok(rejected_write_page(
            candidate,
            current_thread_id,
            next_event_index,
            rejection,
        ));
    };
    let occurred_at =
        deepagents_checkpoint_time(conn, context, &key.thread_id, &key.checkpoint_id)?;
    let Some(occurred_at) = occurred_at else {
        return Ok(rejected_write_page(
            candidate,
            current_thread_id,
            next_event_index,
            format!(
                "Deep Agents writes row references unknown thread_id {}",
                key.thread_id
            ),
        ));
    };
    let (value_type, value) = deepagents_hydrate_write(conn, rowid)?;
    let decoded = match deepagents_messages_from_blob(value_type.as_deref(), &value) {
        Ok(messages) => messages,
        Err(error) => {
            return Ok(rejected_write_page(
                candidate,
                current_thread_id,
                next_event_index,
                error.to_string(),
            ));
        }
    };
    let start = usize::try_from(next_message_offset).map_err(|_| {
        CaptureError::InvalidPayload(
            "Deep Agents write message frontier exceeds platform limits".to_owned(),
        )
    })?;
    if start > decoded.len() {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let end = start
        .saturating_add(DEEPAGENTS_PAGE_UNITS)
        .min(decoded.len());
    let mut index =
        if active_rowid.is_some() || current_thread_id.as_deref() == Some(key.thread_id.as_str()) {
            next_event_index
        } else {
            1
        };
    let mut messages = Vec::with_capacity(end.saturating_sub(start));
    for (offset, message) in decoded[start..end].iter().cloned().enumerate() {
        messages.push(DeepAgentsParsedMessage {
            offset: start.saturating_add(offset),
            provider_event_index: index,
            message,
        });
        index = index.checked_add(1).ok_or(CaptureError::SystemInvariant(
            "Deep Agents event index overflowed",
        ))?;
    }
    let row_complete = end == decoded.len();
    let next_phase = if row_complete {
        DeepAgentsCorePhase::Writes {
            after_rowid: Some(rowid),
            active_rowid: None,
            next_message_offset: 0,
            current_thread_id: Some(key.thread_id.clone()),
            next_event_index: index,
        }
    } else {
        DeepAgentsCorePhase::Writes {
            after_rowid,
            active_rowid: Some(rowid),
            next_message_offset: u32::try_from(end).map_err(|_| {
                CaptureError::InvalidPayload(
                    "Deep Agents write contains too many decoded messages".to_owned(),
                )
            })?,
            current_thread_id: Some(key.thread_id.clone()),
            next_event_index: index,
        }
    };
    let retained_bytes = DEEPAGENTS_PAGE_OVERHEAD_BYTES.saturating_add(value.len());
    ensure_retained_bound(retained_bytes)?;
    Ok(DeepAgentsWritePage {
        key: Some(key),
        rowid: Some(rowid),
        messages,
        value_type,
        value,
        occurred_at: Some(occurred_at),
        rejection: None,
        next_phase,
        retained_bytes,
    })
}

fn rejected_write_page(
    candidate: DeepAgentsWriteCandidate,
    current_thread_id: Option<String>,
    next_event_index: u64,
    rejection: String,
) -> DeepAgentsWritePage {
    DeepAgentsWritePage {
        key: candidate.key,
        rowid: Some(candidate.rowid),
        messages: Vec::new(),
        value_type: None,
        value: Vec::new(),
        occurred_at: None,
        rejection: Some(rejection),
        next_phase: DeepAgentsCorePhase::Writes {
            after_rowid: Some(candidate.rowid),
            active_rowid: None,
            next_message_offset: 0,
            current_thread_id,
            next_event_index,
        },
        retained_bytes: DEEPAGENTS_PAGE_OVERHEAD_BYTES,
    }
}

#[allow(clippy::too_many_arguments)]
fn publish_thread_page(
    store: &Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    snapshot: &ProviderSqliteSourceSnapshot,
    authority: &DeepAgentsSourceAuthority,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    cursor: &DeepAgentsNativeCursor,
    page: DeepAgentsThreadPage,
    summary: &mut ProviderImportSummary,
) -> Result<DeepAgentsNativeCursor> {
    let mut next = cursor.clone();
    next.phase = if page.terminal {
        DeepAgentsCorePhase::Writes {
            after_rowid: None,
            active_rowid: None,
            next_message_offset: 0,
            current_thread_id: None,
            next_event_index: 1,
        }
    } else {
        DeepAgentsCorePhase::Threads {
            after_rowid: page.next_after_rowid,
        }
    };
    next.accepted_sessions = next.accepted_sessions.saturating_add(
        u64::try_from(
            page.entries
                .iter()
                .filter(|entry| entry.summary.is_some())
                .count(),
        )
        .unwrap_or(u64::MAX),
    );
    next.rejected_records = next.rejected_records.saturating_add(
        u64::try_from(
            page.entries
                .iter()
                .filter(|entry| entry.rejection.is_some())
                .count(),
        )
        .unwrap_or(u64::MAX),
    );
    next.generation_staged |= page.entries.iter().any(|entry| entry.summary.is_some());
    let stored = store.get_sync_cursor(None, &context.machine_id, &authority.cursor_stream)?;
    let transition = cursor_transition(context, authority, stored.as_ref(), &next)?;
    let publication_id =
        publication_id(authority, cursor, &next, transition.next().cursor.as_str());
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(
        admission,
        NativePathGroupAccounting::new(1, 1, page.retained_bytes)?,
    )?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        return Ok(next);
    }
    let resolution = reconcile_locator(&mut group, authority, context)?;
    let mut retained = NativePathRetainedSourceEntities::default();
    for entry in &page.entries {
        if let Some(failure) = &entry.rejection {
            summary.record_failure(ProviderImportFailure {
                line: usize::try_from(entry.rowid).unwrap_or(usize::MAX),
                error: failure.clone(),
            });
            continue;
        }
        let thread = &entry
            .summary
            .as_ref()
            .ok_or(CaptureError::SystemInvariant(
                "accepted Deep Agents thread has no summary",
            ))?
            .thread;
        let raw_source_path = authority.canonical_database_path.display().to_string();
        let source_id = resolve_source_id(
            committed_store,
            thread,
            &context.machine_id,
            &resolution.canonical_source_identity,
            &raw_source_path,
        )?;
        let source = capture_source(
            source_id,
            thread,
            authority,
            context,
            &raw_source_path,
            &resolution.canonical_source_identity,
        );
        group.upsert_capture_source(&source)?;
        group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;
        let session = canonical_session(
            committed_store,
            source_id,
            thread,
            context,
            options,
            &resolution.canonical_source_identity,
        )?;
        let existed = committed_store.get_session(session.id).is_ok();
        group.upsert_session(&session)?;
        if existed {
            summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
        } else {
            summary.imported_sessions = summary.imported_sessions.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        }
        retained.capture_source_ids.push(source_id);
        retained.session_ids.push(session.id);
    }
    if !retained.capture_source_ids.is_empty() {
        let key = generation_key(
            authority,
            context,
            &resolution.canonical_source_identity,
            cursor.generation,
        );
        group.stage_source_generation_page(&key, &retained)?;
        next.generation_staged = true;
    }
    if !snapshot.revalidate(&authority.database_path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    next.canonical_source_identity = resolution.canonical_source_identity;
    Ok(next)
}

#[allow(clippy::too_many_arguments)]
fn publish_write_page(
    store: &Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    snapshot: &ProviderSqliteSourceSnapshot,
    authority: &DeepAgentsSourceAuthority,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    cursor: &DeepAgentsNativeCursor,
    page: DeepAgentsWritePage,
    summary: &mut ProviderImportSummary,
) -> Result<DeepAgentsNativeCursor> {
    let core_event_count = page
        .messages
        .iter()
        .filter(|message| core_eligible(&message.message))
        .count();
    let mut next = cursor.clone();
    next.phase = page.next_phase.clone();
    next.accepted_events = next
        .accepted_events
        .saturating_add(u64::try_from(core_event_count).unwrap_or(u64::MAX));
    next.rejected_records = next
        .rejected_records
        .saturating_add(u64::from(page.rejection.is_some()));
    let stored = store.get_sync_cursor(None, &context.machine_id, &authority.cursor_stream)?;
    let transition = cursor_transition(context, authority, stored.as_ref(), &next)?;
    let publication_id =
        publication_id(authority, cursor, &next, transition.next().cursor.as_str());
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(
        admission,
        NativePathGroupAccounting::new(1, 1, page.retained_bytes)?,
    )?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        return Ok(next);
    }
    let resolution = reconcile_locator(&mut group, authority, context)?;
    if let Some(failure) = &page.rejection {
        summary.record_failure(ProviderImportFailure {
            line: page
                .rowid
                .and_then(|rowid| usize::try_from(rowid).ok())
                .unwrap_or(usize::MAX),
            error: failure.clone(),
        });
    }
    let mut retained = NativePathRetainedSourceEntities::default();
    if let Some(key) = page.key.as_ref() {
        if let Some((source, session)) =
            committed_source_and_session(committed_store, key, authority, context)?
        {
            group.bind_capture_source_provider_route(source.id, &resolution.route_binding())?;
            retained.capture_source_ids.push(source.id);
            retained.session_ids.push(session.id);
            if page.rejection.is_none() {
                publish_core_messages(
                    committed_store,
                    &mut group,
                    &source,
                    &session,
                    key,
                    &page,
                    context,
                    options,
                    summary,
                    &mut retained,
                )?;
            }
        } else if page.rejection.is_none() {
            summary.record_failure(ProviderImportFailure {
                line: page
                    .rowid
                    .and_then(|rowid| usize::try_from(rowid).ok())
                    .unwrap_or(usize::MAX),
                error: format!(
                    "Deep Agents write references uncommitted thread {}",
                    key.thread_id
                ),
            });
            next.rejected_records = next.rejected_records.saturating_add(1);
        }
    }
    if !retained.capture_source_ids.is_empty() {
        let key = generation_key(
            authority,
            context,
            &resolution.canonical_source_identity,
            cursor.generation,
        );
        group.stage_source_generation_page(&key, &retained)?;
        next.generation_staged = true;
    }
    if !snapshot.revalidate(&authority.database_path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    next.canonical_source_identity = resolution.canonical_source_identity;
    Ok(next)
}

#[allow(clippy::too_many_arguments)]
fn publish_source_stage_page(
    store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    snapshot: Option<&ProviderSqliteSourceSnapshot>,
    authority: &DeepAgentsSourceAuthority,
    context: &ProviderAdapterContext,
    cursor: &DeepAgentsNativeCursor,
    next_source: usize,
    missing: bool,
) -> Result<DeepAgentsNativeCursor> {
    let sources = known_capture_sources(store, authority, context)?;
    if next_source > sources.len() {
        return Err(CaptureError::InvalidPayload(
            "Deep Agents source-stage cursor exceeds the current source set".to_owned(),
        ));
    }
    let end = next_source
        .saturating_add(DEEPAGENTS_PAGE_UNITS)
        .min(sources.len());
    let page = &sources[next_source..end];
    let terminal = end == sources.len();
    let mut next = cursor.clone();
    next.generation_staged |= !page.is_empty();
    next.phase = if terminal {
        if next.generation_staged {
            if missing {
                DeepAgentsCorePhase::MissingRetire { after: None }
            } else {
                DeepAgentsCorePhase::Retire { after: None }
            }
        } else if missing {
            DeepAgentsCorePhase::MissingComplete
        } else {
            DeepAgentsCorePhase::Complete
        }
    } else if missing {
        DeepAgentsCorePhase::MissingStage { next_source: end }
    } else {
        DeepAgentsCorePhase::StageSources { next_source: end }
    };
    let retained_bytes =
        page.iter()
            .try_fold(DEEPAGENTS_PAGE_OVERHEAD_BYTES, |total, source| {
                total.checked_add(serde_json::to_vec(source)?.len()).ok_or(
                    CaptureError::SystemInvariant(
                        "Deep Agents source-stage retained bytes overflowed",
                    ),
                )
            })?;
    ensure_retained_bound(retained_bytes)?;
    let stored = store.get_sync_cursor(None, &context.machine_id, &authority.cursor_stream)?;
    let transition = cursor_transition(context, authority, stored.as_ref(), &next)?;
    let publication_id =
        publication_id(authority, cursor, &next, transition.next().cursor.as_str());
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(
        admission,
        NativePathGroupAccounting::new(1, 1, retained_bytes)?,
    )?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        return Ok(next);
    }
    let resolution = reconcile_locator(&mut group, authority, context)?;
    if !page.is_empty() {
        let mut retained = NativePathRetainedSourceEntities::default();
        for source in page {
            let mut source = source.clone();
            source.descriptor.source_identity = Some(resolution.canonical_source_identity.clone());
            source.descriptor.raw_source_path =
                Some(authority.canonical_database_path.display().to_string());
            source.descriptor.source_root =
                Some(authority.configured_source_root.display().to_string());
            source.sync.deleted_at = None;
            if let Some(metadata) = source.sync.metadata.as_object_mut() {
                metadata.insert(
                    "source_identity".to_owned(),
                    json!(resolution.canonical_source_identity),
                );
                metadata.insert(
                    "source_revision".to_owned(),
                    json!(authority.source_revision),
                );
                metadata.insert(
                    "nativepath_publication".to_owned(),
                    json!(DEEPAGENTS_NATIVE_PARSER_REVISION),
                );
            }
            group.upsert_capture_source(&source)?;
            group.bind_capture_source_provider_route(source.id, &resolution.route_binding())?;
            retained.capture_source_ids.push(source.id);
        }
        let key = generation_key(
            authority,
            context,
            &resolution.canonical_source_identity,
            cursor.generation,
        );
        group.stage_source_generation_page(&key, &retained)?;
    }
    revalidate_optional(snapshot, &authority.database_path)?;
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    next.canonical_source_identity = resolution.canonical_source_identity;
    Ok(next)
}

#[allow(clippy::too_many_arguments)]
fn publish_retirement_page(
    store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    snapshot: Option<&ProviderSqliteSourceSnapshot>,
    authority: &DeepAgentsSourceAuthority,
    context: &ProviderAdapterContext,
    cursor: &DeepAgentsNativeCursor,
    after: Option<SerializableRetirementFrontier>,
    missing: bool,
) -> Result<DeepAgentsNativeCursor> {
    let store_after = after
        .as_ref()
        .map(SerializableRetirementFrontier::to_store)
        .transpose()?;
    let predicted = predict_retirement_page(
        store,
        authority,
        context,
        store_after.as_ref(),
        DEEPAGENTS_RETIREMENT_UNITS,
    )?;
    let mut next = cursor.clone();
    next.phase = if predicted.done {
        if missing {
            DeepAgentsCorePhase::MissingComplete
        } else {
            DeepAgentsCorePhase::Complete
        }
    } else {
        let next_after = predicted
            .next_after
            .clone()
            .map(SerializableRetirementFrontier::from_store);
        if missing {
            DeepAgentsCorePhase::MissingRetire { after: next_after }
        } else {
            DeepAgentsCorePhase::Retire { after: next_after }
        }
    };
    let stored = store.get_sync_cursor(None, &context.machine_id, &authority.cursor_stream)?;
    let transition = cursor_transition(context, authority, stored.as_ref(), &next)?;
    let publication_id =
        publication_id(authority, cursor, &next, transition.next().cursor.as_str());
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(
        admission,
        NativePathGroupAccounting::new(1, 1, DEEPAGENTS_PAGE_OVERHEAD_BYTES)?,
    )?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        return Ok(next);
    }
    let key = generation_key(
        authority,
        context,
        &cursor.canonical_source_identity,
        cursor.generation,
    );
    let actual = group.retire_source_generation_page(
        &key,
        store_after.as_ref(),
        DEEPAGENTS_RETIREMENT_UNITS,
        context.imported_at.timestamp_millis(),
    )?;
    if actual.next_after != predicted.next_after || actual.done != predicted.done {
        return Err(CaptureError::SystemInvariant(
            "Deep Agents retirement frontier diverged from typed Store authority",
        ));
    }
    if missing && actual.done {
        let retirement = ProviderSourceRouteRetirement {
            provider: CaptureProvider::DeepAgents,
            source_format: DEEPAGENTS_SQLITE_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity: authority.route_identity.clone(),
            cursor_stream: authority.cursor_stream.clone(),
            expected_canonical_source_identity: cursor.canonical_source_identity.clone(),
            expected_source_revision: authority.source_revision.clone(),
            retired_at_ms: context.imported_at.timestamp_millis(),
            reason: missing_retirement_reason(
                &authority.configured_source_root,
                &authority.database_path,
            ),
        };
        let _ = group.retire_provider_source_route(&retirement)?;
    }
    revalidate_optional(snapshot, &authority.database_path)?;
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    Ok(next)
}

#[derive(Debug)]
struct PredictedRetirementPage {
    next_after: Option<NativePathSourceEntityFrontier>,
    done: bool,
}

fn predict_retirement_page(
    store: &Store,
    authority: &DeepAgentsSourceAuthority,
    context: &ProviderAdapterContext,
    after: Option<&NativePathSourceEntityFrontier>,
    limit: usize,
) -> Result<PredictedRetirementPage> {
    let sources = known_capture_sources(store, authority, context)?;
    let mut candidates = Vec::<NativePathSourceEntityFrontier>::new();
    for source in sources {
        let Some(provider_session_id) = source.descriptor.external_session_id.as_deref() else {
            continue;
        };
        let Some(session) = store.session_by_capture_source_and_external_session(
            source.id,
            CaptureProvider::DeepAgents,
            provider_session_id,
        )?
        else {
            continue;
        };
        if session.sync.deleted_at.is_none() {
            candidates.push(NativePathSourceEntityFrontier {
                kind: NativePathSourceEntityKind::Session,
                id: session.id,
            });
        }
        for run in store.runs_for_session(session.id)? {
            if run.sync.deleted_at.is_none() {
                candidates.push(NativePathSourceEntityFrontier {
                    kind: NativePathSourceEntityKind::Run,
                    id: run.id,
                });
            }
        }
        for event in store.events_for_session(session.id)? {
            if event.sync.deleted_at.is_none() {
                candidates.push(NativePathSourceEntityFrontier {
                    kind: NativePathSourceEntityKind::Event,
                    id: event.id,
                });
            }
        }
    }
    candidates.sort_by_key(|candidate| (retirement_kind_order(candidate.kind), candidate.id));
    candidates.dedup();
    let after_key = after.map(|value| (retirement_kind_order(value.kind), value.id));
    let remaining = candidates
        .into_iter()
        .filter(|candidate| {
            after_key
                .is_none_or(|after| (retirement_kind_order(candidate.kind), candidate.id) > after)
        })
        .collect::<Vec<_>>();
    let done = remaining.len() <= limit;
    let next_after = remaining.into_iter().take(limit).next_back();
    Ok(PredictedRetirementPage { next_after, done })
}

fn retirement_kind_order(kind: NativePathSourceEntityKind) -> u8 {
    match kind {
        NativePathSourceEntityKind::SessionEdge => 0,
        NativePathSourceEntityKind::Run => 1,
        NativePathSourceEntityKind::Event => 2,
        NativePathSourceEntityKind::FileTouch => 3,
        NativePathSourceEntityKind::Session => 4,
    }
}

fn known_capture_sources(
    store: &Store,
    authority: &DeepAgentsSourceAuthority,
    context: &ProviderAdapterContext,
) -> Result<Vec<CaptureSource>> {
    let canonical_path = authority.canonical_database_path.display().to_string();
    let configured_path = authority.database_path.display().to_string();
    let configured_root = authority.configured_source_root.display().to_string();
    let mut sources = store
        .list_capture_sources()?
        .into_iter()
        .filter(|source| {
            source.descriptor.provider == CaptureProvider::DeepAgents
                && source.descriptor.machine_id == context.machine_id
                && source.descriptor.source_format.as_deref()
                    == Some(DEEPAGENTS_SQLITE_SOURCE_FORMAT)
                && (source.descriptor.source_identity.as_deref()
                    == Some(authority.canonical_source_identity.as_str())
                    || source.descriptor.raw_source_path.as_deref()
                        == Some(canonical_path.as_str())
                    || source.descriptor.raw_source_path.as_deref()
                        == Some(configured_path.as_str())
                    || source.descriptor.source_root.as_deref() == Some(configured_root.as_str()))
        })
        .collect::<Vec<_>>();
    sources.sort_by_key(|source| source.id);
    sources.dedup_by_key(|source| source.id);
    Ok(sources)
}

fn resolve_source_id(
    store: &Store,
    thread: &DeepAgentsThread,
    machine_id: &str,
    canonical_source_identity: &str,
    raw_source_path: &str,
) -> Result<Uuid> {
    Ok(store
        .capture_source_by_canonical_identity_session(
            CaptureProvider::DeepAgents,
            DEEPAGENTS_SQLITE_SOURCE_FORMAT,
            machine_id,
            canonical_source_identity,
            &thread.thread_id,
        )?
        .map(|source| source.id)
        .unwrap_or_else(|| {
            provider_scoped_source_uuid(
                CaptureProvider::DeepAgents,
                &thread.thread_id,
                DEEPAGENTS_SQLITE_SOURCE_FORMAT,
                Some(raw_source_path),
            )
        }))
}

fn capture_source(
    source_id: Uuid,
    thread: &DeepAgentsThread,
    authority: &DeepAgentsSourceAuthority,
    context: &ProviderAdapterContext,
    raw_source_path: &str,
    canonical_source_identity: &str,
) -> CaptureSource {
    let source_root = authority.configured_source_root.display().to_string();
    CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::DeepAgents,
            machine_id: context.machine_id.clone(),
            process_id: None,
            cwd: thread.cwd.clone(),
            raw_source_path: Some(raw_source_path.to_owned()),
            source_format: Some(DEEPAGENTS_SQLITE_SOURCE_FORMAT.to_owned()),
            source_root: Some(source_root.clone()),
            source_identity: Some(canonical_source_identity.to_owned()),
            external_session_id: Some(thread.thread_id.clone()),
        },
        started_at: thread.created_at,
        ended_at: Some(thread.updated_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": thread.thread_id,
                "source_format": DEEPAGENTS_SQLITE_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "source_identity": canonical_source_identity,
                "source_root": source_root,
                "source_revision": authority.source_revision,
                "source_identity_key": provider_scoped_source_identity_key(
                    CaptureProvider::DeepAgents,
                    &thread.thread_id,
                    DEEPAGENTS_SQLITE_SOURCE_FORMAT,
                    Some(raw_source_path),
                ),
                "schema_fingerprint": authority.schema_fingerprint,
                "source_metadata": {
                    "adapter": DEEPAGENTS_SQLITE_SOURCE_FORMAT,
                    "sqlite_user_version": authority.sqlite_user_version,
                    "schema_fingerprint": authority.schema_fingerprint,
                    "source_observation_revision": authority.source_revision,
                    "message_import_policy":
                        "root writes.messages only; checkpoint state blobs are not indexed",
                },
                "nativepath_publication": DEEPAGENTS_NATIVE_PARSER_REVISION,
            }),
        ),
    }
}

fn canonical_session(
    store: &Store,
    source_id: Uuid,
    thread: &DeepAgentsThread,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    canonical_source_identity: &str,
) -> Result<Session> {
    Ok(Session {
        id: provider_import_session_uuid(
            store,
            CaptureProvider::DeepAgents,
            &thread.thread_id,
            source_id,
            Some(canonical_source_identity),
        )?,
        history_record_id: options.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::DeepAgents,
        external_session_id: Some(thread.thread_id.clone()),
        external_agent_id: thread.agent_name.clone(),
        agent_type: AgentType::Primary,
        role_hint: thread
            .agent_name
            .clone()
            .or_else(|| Some("agent".to_owned())),
        is_primary: true,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: thread.created_at,
        ended_at: Some(thread.updated_at),
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": thread.thread_id,
                "source_format": DEEPAGENTS_SQLITE_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "session_idempotency_key":
                    format!("provider-session:deepagents:{}", thread.thread_id),
                "metadata": {
                    "source_format": DEEPAGENTS_SQLITE_SOURCE_FORMAT,
                    "agent_name": thread.agent_name,
                    "git_branch": thread.git_branch,
                    "latest_checkpoint_id": thread.latest_checkpoint_id,
                    "storage": "LangGraph AsyncSqliteSaver checkpoints/writes",
                    "nativepath_publication": DEEPAGENTS_NATIVE_PARSER_REVISION,
                },
            }),
        ),
    })
}

fn committed_source_and_session(
    store: &Store,
    key: &DeepAgentsWriteKey,
    authority: &DeepAgentsSourceAuthority,
    context: &ProviderAdapterContext,
) -> Result<Option<(CaptureSource, Session)>> {
    let raw_source_path = authority.canonical_database_path.display().to_string();
    let source_id = store
        .capture_source_by_canonical_identity_session(
            CaptureProvider::DeepAgents,
            DEEPAGENTS_SQLITE_SOURCE_FORMAT,
            &context.machine_id,
            &authority.canonical_source_identity,
            &key.thread_id,
        )?
        .map(|source| source.id)
        .unwrap_or_else(|| {
            provider_scoped_source_uuid(
                CaptureProvider::DeepAgents,
                &key.thread_id,
                DEEPAGENTS_SQLITE_SOURCE_FORMAT,
                Some(&raw_source_path),
            )
        });
    let source = match store.get_capture_source(source_id) {
        Ok(source) => source,
        Err(ctx_history_store::StoreError::Sql(rusqlite::Error::QueryReturnedNoRows)) => {
            return Ok(None);
        }
        Err(error) => return Err(CaptureError::Store(error)),
    };
    let session = store.session_by_capture_source_and_external_session(
        source_id,
        CaptureProvider::DeepAgents,
        &key.thread_id,
    )?;
    Ok(session.map(|session| (source, session)))
}

#[allow(clippy::too_many_arguments)]
fn publish_core_messages(
    store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    source: &CaptureSource,
    session: &Session,
    key: &DeepAgentsWriteKey,
    page: &DeepAgentsWritePage,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    summary: &mut ProviderImportSummary,
    retained: &mut NativePathRetainedSourceEntities,
) -> Result<()> {
    let occurred_at = page.occurred_at.ok_or(CaptureError::SystemInvariant(
        "accepted Deep Agents write has no occurrence time",
    ))?;
    let record_digest =
        deepagents_write_record_digest(key, page.value_type.as_deref(), &page.value);
    for parsed in &page.messages {
        if !core_eligible(&parsed.message) {
            continue;
        }
        let identity = parsed
            .message
            .message_id
            .as_deref()
            .map(|message_id| deepagents_message_identity(&key.thread_id, message_id));
        let cursor = format!(
            "thread:{}:checkpoint:{}:task:{}:write:{}:message:{}",
            key.thread_id, key.checkpoint_id, key.task_id, key.idx, parsed.offset
        );
        let event_hash = identity
            .as_ref()
            .map(|identity| identity.payload_hash.clone())
            .unwrap_or_else(|| cursor.clone());
        let mut native = deepagents_native_event(
            key,
            parsed,
            occurred_at,
            &event_hash,
            identity.as_ref().map(|identity| identity.provider_index),
            Some(record_digest.clone()),
        );
        let (event_hash, authority) = native.provider_event_hash.as_ref().map_or_else(
            || {
                compute_payload_hash(&native.payload)
                    .map(|hash| (hash, ProviderEventHashAuthority::NormalizedPayloadFallback))
            },
            |hash| Ok((hash.clone(), ProviderEventHashAuthority::ProviderSupplied)),
        )?;
        let provider_identity_index = identity
            .as_ref()
            .map_or(parsed.provider_event_index, |identity| {
                identity.provider_index
            });
        let import_identity = provider_event_import_identity_with_exact_legacy_source(
            store,
            CaptureProvider::DeepAgents,
            &key.thread_id,
            source.id,
            provider_identity_index,
            parsed.provider_event_index,
            &event_hash,
            None,
            Some(provider_identity_index),
            session.id == provider_session_uuid(CaptureProvider::DeepAgents, &key.thread_id),
        )?;
        if let Some(metadata) = native.metadata.as_object_mut() {
            metadata.insert(
                "source_record_ordinal".to_owned(),
                json!(page.rowid.unwrap_or_default()),
            );
            metadata.insert(
                "source_record_subrecord_index".to_owned(),
                json!(parsed.offset),
            );
        }
        let line = page
            .rowid
            .and_then(|rowid| usize::try_from(rowid).ok())
            .unwrap_or(usize::MAX);
        let event = deepagents_core_event(
            context,
            options,
            &key.thread_id,
            source.id,
            session.id,
            line,
            &native,
            &event_hash,
            authority,
            &import_identity,
        )?;
        if group.reconcile_provider_event(&event, authority)? {
            summary.imported_events = summary.imported_events.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        } else {
            summary.skipped_events = summary.skipped_events.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
        }
        retained.event_ids.push(event.id);
        summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn deepagents_core_event(
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    provider_session_id: &str,
    source_id: Uuid,
    session_id: Uuid,
    line_number: usize,
    native: &DeepAgentsNativeEvent,
    event_hash: &str,
    authority: ProviderEventHashAuthority,
    identity: &ProviderEventImportIdentity,
) -> Result<Event> {
    let dedupe_key =
        Store::provider_event_dedupe_key_with_payload_hash(&identity.dedupe_key, event_hash)
            .unwrap_or_else(|| identity.dedupe_key.clone());
    let mut provider_metadata = native.metadata.clone();
    let source_record_coordinates =
        take_deepagents_source_record_coordinates(&mut provider_metadata)?;
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
        "source_format": DEEPAGENTS_SQLITE_SOURCE_FORMAT,
        "source_trust": ProviderSourceTrust::ProviderNative,
        "fixture_line": line_number,
        "imported_at": context.imported_at,
        "event_idempotency_key": format!(
            "provider-event:{}:{}:{}",
            CaptureProvider::DeepAgents.as_str(),
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
            "provider": CaptureProvider::DeepAgents.as_str(),
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

fn take_deepagents_source_record_coordinates(metadata: &mut Value) -> Result<Option<(u64, u32)>> {
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

fn core_eligible(message: &DeepAgentsMessage) -> bool {
    if message.role != EventRole::Tool {
        return true;
    }
    matches!(
        deepagents_output_outcome(message).outcome,
        OutputOutcome::Failure | OutputOutcome::Timeout
    )
}

#[derive(Debug, Clone)]
pub(super) struct DeepAgentsMessageIdentity {
    pub(super) provider_index: u64,
    pub(super) payload_hash: String,
}

pub(super) fn deepagents_message_identity(
    thread_id: &str,
    message_id: &str,
) -> DeepAgentsMessageIdentity {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for component in [
        b"ctx-deepagents-message-v1".as_slice(),
        thread_id.as_bytes(),
        message_id.as_bytes(),
    ] {
        for byte in component {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(PRIME);
        }
        hash ^= 0xff;
        hash = hash.wrapping_mul(PRIME);
    }
    DeepAgentsMessageIdentity {
        provider_index: hash,
        payload_hash: format!("fnv1a64:{hash:016x}"),
    }
}

#[derive(Debug)]
pub(crate) struct DeepAgentsNativeEvent {
    pub(crate) provider_event_index: u64,
    pub(crate) provider_event_hash: Option<String>,
    pub(crate) cursor: String,
    pub(crate) event_type: EventType,
    pub(crate) role: Option<EventRole>,
    pub(crate) occurred_at: DateTime<Utc>,
    pub(crate) payload: Value,
    pub(crate) metadata: Value,
}

pub(super) fn deepagents_native_event(
    key: &DeepAgentsWriteKey,
    parsed: &DeepAgentsParsedMessage,
    occurred_at: DateTime<Utc>,
    provider_event_hash: &str,
    provider_event_identity_index: Option<u64>,
    record_digest: Option<crate::complete_content::CompleteContentBodyDigest>,
) -> DeepAgentsNativeEvent {
    let event_type = if parsed.message.role == EventRole::Tool {
        EventType::ToolOutput
    } else {
        EventType::Message
    };
    let cursor = format!(
        "thread:{}:checkpoint:{}:task:{}:write:{}:message:{}",
        key.thread_id, key.checkpoint_id, key.task_id, key.idx, parsed.offset
    );
    let body = json!({
        "message_type": parsed.message.message_type,
        "message_class": parsed.message.message_class,
        "message_id": parsed.message.message_id,
        "tool_call_id": parsed.message.tool_call_id,
        "status": parsed.message.status,
        "exit_code": parsed.message.exit_code,
        "duration_ms": parsed.message.duration_ms,
        "timed_out": parsed.message.timed_out,
        "is_error": parsed.message.is_error,
        "success": parsed.message.success,
        "checkpoint_id": key.checkpoint_id,
        "task_id": key.task_id,
        "write_idx": key.idx,
        "message_offset": parsed.offset,
    });
    let retained_text = provider_policy_event_text(event_type, &parsed.message.text, &body);
    let retained_body = provider_policy_body(event_type, &body);
    let result_evidence =
        provider_result_identifier_evidence(event_type, &parsed.message.text, &body);
    let result_outcome = provider_result_outcome_evidence(event_type, &body);
    let mut event = DeepAgentsNativeEvent {
        provider_event_index: parsed.provider_event_index,
        provider_event_hash: Some(provider_event_hash.to_owned()),
        cursor,
        event_type,
        role: Some(parsed.message.role),
        occurred_at,
        payload: json!({
            "text": retained_text.text,
            "text_retention": retained_text.retention.as_json(),
            "result_evidence": result_evidence,
            "result_outcome": result_outcome,
            "source_format": DEEPAGENTS_SQLITE_SOURCE_FORMAT,
            "body": provider_capped_json(&retained_body, PROVIDER_MAX_PREVIEW_CHARS),
        }),
        metadata: json!({
            "source": DEEPAGENTS_SQLITE_SOURCE_FORMAT,
            "source_format": DEEPAGENTS_SQLITE_SOURCE_FORMAT,
            "checkpoint_id": key.checkpoint_id,
            "task_id": key.task_id,
            "write_idx": key.idx,
            "message_offset": parsed.offset,
            "message_type": parsed.message.message_type,
            "message_class": parsed.message.message_class,
            "message_id": parsed.message.message_id,
            "provider_event_identity_index": provider_event_identity_index,
            "privacy": "decoded from writes.messages only",
        }),
    };
    if event_type != EventType::ToolOutput {
        attach_native_message_content_locator(
            &mut event,
            key,
            parsed.offset,
            &parsed.message.text,
            record_digest,
        );
    }
    event
}

fn attach_native_message_content_locator(
    event: &mut DeepAgentsNativeEvent,
    key: &DeepAgentsWriteKey,
    message_offset: usize,
    text: &str,
    record_digest: Option<crate::complete_content::CompleteContentBodyDigest>,
) {
    let Some(locator) = deepagents_content_locator(
        &event.payload,
        key,
        message_offset,
        text,
        record_digest,
        event
            .provider_event_hash
            .clone()
            .unwrap_or_else(|| event.cursor.clone()),
    ) else {
        return;
    };
    let _ = attach_verified_content_locator(&mut event.metadata, locator);
}

fn deepagents_content_locator(
    payload: &Value,
    key: &DeepAgentsWriteKey,
    message_offset: usize,
    text: &str,
    record_digest: Option<crate::complete_content::CompleteContentBodyDigest>,
    native_record_id: String,
) -> Option<VerifiedContentLocatorV1> {
    if payload
        .pointer("/text_retention/truncated")
        .and_then(Value::as_bool)
        != Some(true)
    {
        return None;
    }
    let record_digest = record_digest?;
    let address = DeepAgentsContentAddress::from_write(key, message_offset)?;
    let locator_value = address.encode()?;
    let content_ref = ContentRef::from_bytes(text.as_bytes())?;
    let profile = verified_content_profile(
        CaptureProvider::DeepAgents,
        DEEPAGENTS_SQLITE_SOURCE_FORMAT,
        CompleteContentSourceFamily::Sqlite,
        VerifiedContentRole::MessageBody,
    )?;
    VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        profile,
        content_ref,
        CompleteContentSourceFamily::Sqlite,
        DEEPAGENTS_CONTENT_LOCATOR_KIND,
        &locator_value,
        native_record_id,
        record_digest,
    )
}

fn deepagents_output_outcome(message: &DeepAgentsMessage) -> OutputOutcomeMetadata {
    let status = message
        .status
        .as_deref()
        .map(str::trim)
        .map(str::to_ascii_lowercase);
    let timeout = message.timed_out
        || status
            .as_deref()
            .is_some_and(|status| matches!(status, "timeout" | "timed_out" | "timedout"));
    let failure = message.is_error == Some(true)
        || message.success == Some(false)
        || message.exit_code.is_some_and(|code| code != 0)
        || status.as_deref().is_some_and(|status| {
            matches!(
                status,
                "failed" | "failure" | "error" | "errored" | "cancelled" | "canceled"
            )
        });
    let success = message.success == Some(true)
        || message.exit_code == Some(0)
        || status.as_deref().is_some_and(|status| {
            matches!(
                status,
                "success" | "succeeded" | "complete" | "completed" | "ok" | "passed"
            )
        });
    OutputOutcomeMetadata {
        outcome: if timeout {
            OutputOutcome::Timeout
        } else if failure {
            OutputOutcome::Failure
        } else if success {
            OutputOutcome::Success
        } else {
            OutputOutcome::Unknown
        },
        exit_code: message.exit_code,
        duration_ms: message.duration_ms,
    }
}

fn reconcile_locator(
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    authority: &DeepAgentsSourceAuthority,
    context: &ProviderAdapterContext,
) -> Result<ctx_history_store::ProviderSourceLocatorResolution> {
    group
        .reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::DeepAgents,
            source_format: DEEPAGENTS_SQLITE_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity: authority.route_identity.clone(),
            cursor_stream: authority.cursor_stream.clone(),
            proposed_source_identity: authority.proposed_source_identity.clone(),
            raw_source_path: Some(authority.canonical_database_path.display().to_string()),
            source_revision: authority.source_revision.clone(),
            observed_at_ms: context.imported_at.timestamp_millis(),
        })
        .map_err(CaptureError::from)
}

fn generation_key(
    authority: &DeepAgentsSourceAuthority,
    context: &ProviderAdapterContext,
    canonical_source_identity: &str,
    generation: u64,
) -> NativePathSourceGenerationKey {
    NativePathSourceGenerationKey {
        provider: CaptureProvider::DeepAgents,
        source_format: DEEPAGENTS_SQLITE_SOURCE_FORMAT.to_owned(),
        machine_id: context.machine_id.clone(),
        canonical_source_identity: canonical_source_identity.to_owned(),
        locator_identity: authority.route_identity.clone(),
        cursor_stream: authority.cursor_stream.clone(),
        source_revision: authority.source_revision.clone(),
        generation_id: format!("deepagents-native-generation-{generation}"),
    }
}

fn cursor_transition(
    context: &ProviderAdapterContext,
    authority: &DeepAgentsSourceAuthority,
    stored: Option<&SyncCursor>,
    next_cursor: &DeepAgentsNativeCursor,
) -> Result<NativePathCursorTransition> {
    Ok(NativePathCursorTransition::new(
        stored.map(|cursor| cursor.cursor.clone()),
        SyncCursor {
            id: Uuid::new_v4(),
            team_id: None,
            device_id: context.machine_id.clone(),
            stream: authority.cursor_stream.clone(),
            cursor: next_cursor.encode()?,
            last_synced_at: Some(context.imported_at),
            timestamps: timestamps(context.imported_at),
        },
    ))
}

fn publication_id(
    authority: &DeepAgentsSourceAuthority,
    current: &DeepAgentsNativeCursor,
    next: &DeepAgentsNativeCursor,
    encoded_next_cursor: &str,
) -> String {
    let mut digest = Sha256::new();
    digest.update(DEEPAGENTS_PUBLICATION_DOMAIN);
    digest.update(authority.route_identity.as_bytes());
    digest.update(authority.source_revision.as_bytes());
    digest.update(current.generation.to_be_bytes());
    digest.update(serde_json::to_vec(&current.phase).unwrap_or_default());
    digest.update(serde_json::to_vec(&next.phase).unwrap_or_default());
    digest.update(encoded_next_cursor.as_bytes());
    format!("deepagents-native:{}", hex(&digest.finalize()))
}

fn source_revision(snapshot: &ProviderSqliteSourceSnapshot, schema_fingerprint: &str) -> String {
    format!(
        "deepagents-native-sqlite-v1:parser={DEEPAGENTS_NATIVE_PARSER_REVISION};policy={DEEPAGENTS_NATIVE_POLICY_REVISION};schema={schema_fingerprint};{}",
        snapshot.revision_component(),
    )
}

fn decode_core_cursor_for_migration(
    stored: Option<&SyncCursor>,
) -> Result<Option<DeepAgentsNativeCursor>> {
    let Some(stored) = stored else {
        return Ok(None);
    };
    if let Ok(committed) = decode_native_path_committed_cursor(&stored.cursor) {
        return DeepAgentsNativeCursor::decode(committed.provider_cursor()).map(Some);
    }
    if CertifiedProviderCursor::decode_if_certified(&stored.cursor)?.is_some() {
        return Ok(None);
    }
    Err(CaptureError::InvalidPayload(
        "Deep Agents cursor is neither NativePath nor a released migration cursor".to_owned(),
    ))
}

fn require_complete_matching_core(
    store: &Store,
    authority: &DeepAgentsSourceAuthority,
    context: &ProviderAdapterContext,
) -> Result<()> {
    let stored = store
        .get_sync_cursor(None, &context.machine_id, &authority.cursor_stream)?
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Deep Agents Pro replay requires committed NativePath Core".to_owned(),
            )
        })?;
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    let cursor = DeepAgentsNativeCursor::decode(committed.provider_cursor())?;
    if cursor.route_identity != authority.route_identity
        || cursor.source_revision != authority.source_revision
        || cursor.schema_fingerprint != authority.schema_fingerprint
        || !matches!(cursor.phase, DeepAgentsCorePhase::Complete)
    {
        return Err(CaptureError::InvalidPayload(
            "Deep Agents Pro replay source no longer matches committed Core authority".to_owned(),
        ));
    }
    Ok(())
}

fn ensure_retained_bound(retained_bytes: usize) -> Result<()> {
    if retained_bytes > NATIVE_PATH_MAX_RETAINED_PAGE_BYTES {
        return Err(CaptureError::InvalidPayload(
            "Deep Agents NativePath page exceeds the retained-byte bound".to_owned(),
        ));
    }
    Ok(())
}

fn revalidate_optional(snapshot: Option<&ProviderSqliteSourceSnapshot>, path: &Path) -> Result<()> {
    if let Some(snapshot) = snapshot {
        if !snapshot.revalidate(path)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
    }
    Ok(())
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

#[derive(Debug)]
struct DeepAgentsOutputPage {
    expected: DeepAgentsOutputFrontier,
    next: DeepAgentsOutputFrontier,
    key: Option<DeepAgentsWriteKey>,
    rowid: Option<i64>,
    messages: Vec<(usize, DeepAgentsMessage)>,
    occurred_at: Option<DateTime<Utc>>,
    retained_bytes: usize,
}

fn replay_outputs(
    conn: &Connection,
    snapshot: &ProviderSqliteSourceSnapshot,
    authority: &DeepAgentsSourceAuthority,
    context: &ProviderAdapterContext,
    sink: &dyn ProOutputSink,
) -> Result<bool> {
    replay_outputs_inner(conn, snapshot, authority, context, sink)
}

fn replay_outputs_inner(
    conn: &Connection,
    snapshot: &ProviderSqliteSourceSnapshot,
    authority: &DeepAgentsSourceAuthority,
    context: &ProviderAdapterContext,
    sink: &dyn ProOutputSink,
) -> Result<bool> {
    let source = OutputSourceIdentity {
        provider: CaptureProvider::DeepAgents.as_str().to_owned(),
        namespace_id: authority.canonical_source_identity.clone(),
        source_id: authority.route_identity.clone(),
    };
    let progress = match sink.observe_source(&source) {
        Ok(progress) => progress,
        Err(_) => {
            sink.mark_behind(ProOutputSinkError::new(
                "deepagents_output_progress",
                "Deep Agents Pro output progress is unavailable",
            ));
            return Ok(true);
        }
    };
    let mut state = match output_state(progress, authority, sink) {
        Ok(state) => state,
        Err(_) => {
            sink.mark_behind(ProOutputSinkError::new(
                "deepagents_output_progress",
                "Deep Agents Pro output progress is invalid",
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
        let page =
            with_sqlite_read_snapshot(conn, || build_output_page(conn, context, &state.frontier))?;
        if !snapshot.revalidate(&authority.database_path)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let output_page = (|| {
            let observations = output_observations(&page)?;
            let expected = output_safe_frontier(&page.expected)?;
            let next = output_safe_frontier(&page.next)?;
            let output = NativeProOutputPage {
                inventory_generation: sink.inventory_generation(),
                source: source.clone(),
                source_epoch: state.source_epoch,
                observed_revision: authority.source_revision.clone(),
                parser_revision: DEEPAGENTS_OUTPUT_PARSER_REVISION.to_owned(),
                materializer_revision: sink.materializer_revision().to_owned(),
                disposition: state.disposition,
                expected_prior_source_epoch: state.expected_source_epoch,
                expected_prior_frontier: state.expected_sink_frontier.clone(),
                observations,
            };
            let replay = NativeProReplayPage::new_with_source_identity(
                NativeSourceIdentity::new(
                    CaptureProvider::DeepAgents.as_str(),
                    authority.route_identity.clone(),
                ),
                expected,
                next.clone(),
                page.next.terminal,
                NativePageAccounting {
                    logical_units: output.observations.len().max(1),
                    conservative_serialized_bytes: page.retained_bytes,
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
                    "deepagents_output_page",
                    "Deep Agents Pro output page is invalid",
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

fn record_output_behind(summary: &mut ProviderImportSummary) {
    summary.record_failure(ProviderImportFailure {
        line: 0,
        error: "Deep Agents Pro output is behind committed Core".to_owned(),
    });
}

struct DeepAgentsOutputState {
    frontier: DeepAgentsOutputFrontier,
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_sink_frontier: Option<NativeSafeFrontier>,
    disposition: ProOutputSourceDisposition,
    complete: bool,
}

fn output_state(
    progress: Option<ProOutputProgress>,
    authority: &DeepAgentsSourceAuthority,
    sink: &dyn ProOutputSink,
) -> Result<DeepAgentsOutputState> {
    let Some(progress) = progress else {
        return Ok(DeepAgentsOutputState {
            frontier: DeepAgentsOutputFrontier::initial(),
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
            if cursor.version != DEEPAGENTS_OUTPUT_FRONTIER_VERSION {
                return Err(CaptureError::InvalidPayload(
                    "Deep Agents output cursor has an unsupported version".to_owned(),
                ));
            }
            serde_json::from_slice::<DeepAgentsOutputFrontier>(&cursor.payload)
                .map_err(CaptureError::from)
        })
        .transpose()?;
    let matching = progress.observed_revision == authority.source_revision
        && progress.parser_revision == DEEPAGENTS_OUTPUT_PARSER_REVISION
        && progress.materializer_revision == sink.materializer_revision()
        && prior_frontier.is_some();
    if matching {
        let frontier = prior_frontier.unwrap_or_else(DeepAgentsOutputFrontier::initial);
        return Ok(DeepAgentsOutputState {
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
    Ok(DeepAgentsOutputState {
        frontier: DeepAgentsOutputFrontier::initial(),
        source_epoch: progress
            .source_epoch
            .checked_add(1)
            .ok_or(CaptureError::InvalidPayload(
                "Deep Agents output source epoch is exhausted".to_owned(),
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

fn build_output_page(
    conn: &Connection,
    context: &ProviderAdapterContext,
    expected: &DeepAgentsOutputFrontier,
) -> Result<DeepAgentsOutputPage> {
    if expected.terminal {
        return Err(CaptureError::InvalidPayload(
            "Deep Agents output replay advanced a terminal frontier".to_owned(),
        ));
    }
    let candidate = match expected.active_rowid {
        Some(rowid) => deepagents_write_candidate_at(conn, rowid)?
            .ok_or(CaptureError::SourceChangedDuringCapture)?,
        None => match deepagents_next_write_candidate(conn, expected.after_rowid)? {
            Some(candidate) => candidate,
            None => {
                let mut next = expected.clone();
                next.terminal = true;
                return Ok(DeepAgentsOutputPage {
                    expected: expected.clone(),
                    next,
                    key: None,
                    rowid: None,
                    messages: Vec::new(),
                    occurred_at: None,
                    retained_bytes: DEEPAGENTS_PAGE_OVERHEAD_BYTES,
                });
            }
        },
    };
    let rowid = candidate.rowid;
    let Some(key) = candidate.key else {
        let mut next = expected.clone();
        next.after_rowid = Some(rowid);
        next.active_rowid = None;
        next.next_message_offset = 0;
        return Ok(DeepAgentsOutputPage {
            expected: expected.clone(),
            next,
            key: None,
            rowid: Some(rowid),
            messages: Vec::new(),
            occurred_at: None,
            retained_bytes: DEEPAGENTS_PAGE_OVERHEAD_BYTES,
        });
    };
    let (value_type, value) = deepagents_hydrate_write(conn, rowid)?;
    let messages = match deepagents_messages_from_blob(value_type.as_deref(), &value) {
        Ok(messages) => messages,
        Err(_) => {
            let mut next = expected.clone();
            next.after_rowid = Some(rowid);
            next.active_rowid = None;
            next.next_message_offset = 0;
            return Ok(DeepAgentsOutputPage {
                expected: expected.clone(),
                next,
                key: None,
                rowid: Some(rowid),
                messages: Vec::new(),
                occurred_at: None,
                retained_bytes: DEEPAGENTS_PAGE_OVERHEAD_BYTES,
            });
        }
    };
    let start = usize::try_from(expected.next_message_offset).map_err(|_| {
        CaptureError::InvalidPayload(
            "Deep Agents output message frontier exceeds platform limits".to_owned(),
        )
    })?;
    if start > messages.len() {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let end = start
        .saturating_add(NATIVE_INGESTION_PAGE_MAX_UNITS)
        .min(messages.len());
    let selected = messages[start..end]
        .iter()
        .cloned()
        .enumerate()
        .map(|(offset, message)| (start.saturating_add(offset), message))
        .collect::<Vec<_>>();
    let mut next = expected.clone();
    if end == messages.len() {
        next.after_rowid = Some(rowid);
        next.active_rowid = None;
        next.next_message_offset = 0;
    } else {
        next.active_rowid = Some(rowid);
        next.next_message_offset = u32::try_from(end).map_err(|_| {
            CaptureError::InvalidPayload(
                "Deep Agents output row contains too many messages".to_owned(),
            )
        })?;
    }
    let retained_bytes = DEEPAGENTS_PAGE_OVERHEAD_BYTES.saturating_add(value.len());
    ensure_retained_bound(retained_bytes)?;
    let occurred_at =
        deepagents_checkpoint_time(conn, context, &key.thread_id, &key.checkpoint_id)?;
    Ok(DeepAgentsOutputPage {
        expected: expected.clone(),
        next,
        key: Some(key),
        rowid: Some(rowid),
        messages: selected,
        occurred_at,
        retained_bytes,
    })
}

fn output_observations(page: &DeepAgentsOutputPage) -> Result<Vec<ProOutputObservation>> {
    let Some(key) = page.key.as_ref() else {
        return Ok(Vec::new());
    };
    let occurred_at = page.occurred_at.unwrap_or(DateTime::<Utc>::UNIX_EPOCH);
    let row_ordinal = page.rowid.and_then(|rowid| u64::try_from(rowid).ok());
    let mut observations = Vec::new();
    for (offset, message) in &page.messages {
        if message.role != EventRole::Tool {
            continue;
        }
        let subrecord = u32::try_from(*offset).map_err(|_| {
            CaptureError::InvalidPayload(
                "Deep Agents output offset exceeds native coordinates".to_owned(),
            )
        })?;
        let address = DeepAgentsContentAddress::from_write(key, *offset).ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Deep Agents output offset exceeds locator bounds".to_owned(),
            )
        })?;
        let locator_payload = address.encode().ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Deep Agents output locator exceeds coordinate bounds".to_owned(),
            )
        })?;
        let native_sequence = message.message_id.as_deref().map_or_else(
            || coordinate_hash(key, *offset),
            |message_id| deepagents_message_identity(&key.thread_id, message_id).provider_index,
        );
        let stable_record_id = message.message_id.clone().unwrap_or_else(|| {
            format!(
                "{}:{}:{}:{}:{offset}",
                key.thread_id, key.checkpoint_id, key.task_id, key.idx
            )
        });
        observations.push(ProOutputObservation {
            kind: OutputObservationKind::Tool,
            coordinate: OutputNativeCoordinate {
                unit_key: format!("deepagents:{}:output:{stable_record_id}", key.thread_id),
                native_sequence,
                native_record_id: Some(stable_record_id),
                source_record_ordinal: row_ordinal,
                source_record_subrecord_index: Some(subrecord),
                byte_start: None,
                byte_end_exclusive: None,
            },
            occurred_at_unix_ms: Some(occurred_at.timestamp_millis()),
            associations: OutputAssociations {
                direct_session_id: key.thread_id.clone(),
                root_session_id: key.thread_id.clone(),
                parent_session_id: None,
                provider_session_id: Some(key.thread_id.clone()),
                agent_id: None,
                repository: None,
            },
            call_id: message.tool_call_id.clone(),
            command: None,
            outcome: deepagents_output_outcome(message),
            locator: OutputSourceLocator {
                version: 1,
                kind: DEEPAGENTS_CONTENT_LOCATOR_KIND.to_owned(),
                payload: locator_payload,
            },
            content: message.text.as_bytes().to_vec(),
        });
    }
    Ok(observations)
}

fn coordinate_hash(key: &DeepAgentsWriteKey, offset: usize) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for component in [
        key.thread_id.as_bytes(),
        key.checkpoint_id.as_bytes(),
        key.task_id.as_bytes(),
        &key.idx.to_be_bytes(),
        &u64::try_from(offset).unwrap_or(u64::MAX).to_be_bytes(),
    ] {
        for byte in component {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(PRIME);
        }
        hash ^= 0xff;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

fn output_safe_frontier(frontier: &DeepAgentsOutputFrontier) -> Result<NativeSafeFrontier> {
    NativeSafeFrontier::new(
        DEEPAGENTS_OUTPUT_FRONTIER_VERSION,
        serde_json::to_vec(frontier)?,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

fn retire_missing_source(
    original_path: &Path,
    database_path: &Path,
    store: &mut Store,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    if options.inventory_observation_token.is_none() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: database_path.to_path_buf(),
            reason: "Deep Agents sessions.db is missing",
        });
    }
    let route_identity = provider_path_identity(database_path)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::DeepAgents,
        DEEPAGENTS_SQLITE_SOURCE_FORMAT,
        &route_identity,
    );
    let Some(stored) = store.get_sync_cursor(None, &context.machine_id, &cursor_stream)? else {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: database_path.to_path_buf(),
            reason: "Deep Agents sessions.db is missing and has no prior route authority",
        });
    };
    let committed = decode_native_path_committed_cursor(&stored.cursor).map_err(|_| {
        CaptureError::InvalidPayload(
            "Deep Agents missing-source retirement requires a NativePath cursor; released cursors are migration-only while the source is readable".to_owned(),
        )
    })?;
    let prior = DeepAgentsNativeCursor::decode(committed.provider_cursor())?;
    if matches!(prior.phase, DeepAgentsCorePhase::MissingComplete) {
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }
    let cursor = if matches!(
        prior.phase,
        DeepAgentsCorePhase::MissingStage { .. } | DeepAgentsCorePhase::MissingRetire { .. }
    ) {
        prior
    } else {
        let generation = prior
            .generation
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "Deep Agents source generation is exhausted",
            ))?;
        DeepAgentsNativeCursor {
            generation,
            generation_staged: false,
            phase: DeepAgentsCorePhase::MissingStage { next_source: 0 },
            ..prior
        }
    };
    let configured_source_root = context
        .source_root
        .clone()
        .or_else(|| context.source_path.clone())
        .unwrap_or_else(|| original_path.to_path_buf());
    let authority = DeepAgentsSourceAuthority {
        configured_source_root,
        database_path: database_path.to_path_buf(),
        canonical_database_path: database_path.to_path_buf(),
        route_identity,
        cursor_stream,
        proposed_source_identity: cursor.canonical_source_identity.clone(),
        canonical_source_identity: cursor.canonical_source_identity.clone(),
        source_revision: cursor.source_revision.clone(),
        schema_fingerprint: cursor.schema_fingerprint.clone(),
        sqlite_user_version: 0,
    };
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let mut cursor = cursor;
        let mut summary = ProviderImportSummary::default();
        loop {
            cursor = match cursor.phase.clone() {
                DeepAgentsCorePhase::MissingStage { next_source } => publish_source_stage_page(
                    store,
                    &bulk_guard,
                    None,
                    &authority,
                    context,
                    &cursor,
                    next_source,
                    true,
                )?,
                DeepAgentsCorePhase::MissingRetire { after } => publish_retirement_page(
                    store,
                    &bulk_guard,
                    None,
                    &authority,
                    context,
                    &cursor,
                    after,
                    true,
                )?,
                DeepAgentsCorePhase::MissingComplete => break,
                _ => {
                    return Err(CaptureError::InvalidPayload(
                        "Deep Agents disappearance cursor has an invalid phase".to_owned(),
                    ));
                }
            };
            summary.set_work_result(ProviderImportWorkResult::Changed);
            if matches!(cursor.phase, DeepAgentsCorePhase::MissingComplete) {
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

fn missing_retirement_reason(
    configured_source_root: &Path,
    database_path: &Path,
) -> ProviderSourceRouteRetirementReason {
    if configured_source_root == database_path || configured_source_root.exists() {
        ProviderSourceRouteRetirementReason::SourceMissing
    } else {
        ProviderSourceRouteRetirementReason::RootMissing
    }
}
