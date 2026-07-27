use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    ContentRef, Event, Fidelity, Session, SessionStatus, SyncCursor,
};
use ctx_history_store::{
    decode_native_path_committed_cursor, EventSearchBulkGuard, NativePathCursorSetClassification,
    NativePathCursorTransition, NativePathGroupAccounting, ProviderEventHashAuthority,
    ProviderSourceLocatorObservation, ProviderSourceRouteRetirement,
    ProviderSourceRouteRetirementDisposition, ProviderSourceRouteRetirementReason, Store,
    StoreError,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::complete_content::{
    attach_verified_content_locator, verified_content_profile, CompleteContentBodyDigest,
    CompleteContentSourceFamily, VerifiedContentLocatorV1, VerifiedContentLocatorsV1,
    VerifiedContentRole, VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
};
use crate::native_source::{NativeLocator, NativeSqliteValue};
use crate::provider::importer::{
    compact_provider_result_payload, provider_event_import_identity_with_exact_legacy_source,
    provider_import_session_uuid, provider_path_identity, provider_scoped_source_identity_key,
    provider_scoped_source_uuid, provider_session_uuid, provider_source_cursor_stream_for_path,
    provider_source_identity, provider_sync_metadata, timestamps, CertifiedProviderCursor,
};
use crate::provider::native_ingestion::{
    process_pro_replay_only, NativePageAccounting, NativeProOutputPage, NativeProReplayPage,
    NativeSafeFrontier, NativeSourceIdentity,
};
use crate::provider::normalization::{provider_nonnegative_i64_to_u64, provider_timestamp_millis};
use crate::provider::sqlite::{open_provider_sqlite_readonly, sqlite_schema_fingerprint};
use crate::{
    stable_capture_uuid, CaptureError, CaptureWorkLimit, OutputSourceIdentity, ProOutputProgress,
    ProOutputSink, ProOutputSinkError, ProOutputSourceDisposition, ProviderAdapterContext,
    ProviderImportFailure, ProviderImportOptions, ProviderImportSummary, ProviderImportWorkResult,
    Result, NANOCLAW_SOURCE_FORMAT,
};

use super::position::NanoClawFrontier;
use super::project::{nanoclaw_project_root, NanoClawProjectSnapshot};
use super::projection::{nanoclaw_core_event, NanoClawCoreEvent};
use super::rows::{nanoclaw_message_digest_values, NanoClawSessionRow};
use super::source::{NanoClawNativePage, NanoClawNativeScanner, NanoClawNativeUnit};
use super::{NANOCLAW_CAPTURE_REVISION, NANOCLAW_POLICY_REVISION};

const NANOCLAW_NATIVE_CURSOR_VERSION: u32 = 1;
const NANOCLAW_NATIVE_CURSOR_PREFIX: &str = "nanoclaw-nativepath-v1:";
const NANOCLAW_NATIVE_PUBLICATION_DOMAIN: &[u8] = b"ctx-nanoclaw-nativepath-publication-v1\0";
const NANOCLAW_NATIVE_RETIREMENT_DOMAIN: &[u8] = b"ctx-nanoclaw-nativepath-retirement-v1\0";
const NANOCLAW_NATIVE_PUBLICATION_REVISION: &str = "nanoclaw-nativepath-v1";
const NANOCLAW_OUTPUT_FRONTIER_VERSION: u32 = 1;
const NANOCLAW_OUTPUT_PARSER_REVISION: &str = "nanoclaw-nativepath-output-v1";
const NANOCLAW_PROJECT_EXTERNAL_SESSION: &str = "__nanoclaw_project__";
const NANOCLAW_OUTPUT_PAGE_BYTES: usize = 4 * 1024;
const NANOCLAW_LEGACY_POSITION_KIND: &str = "nanoclaw-project-keyset-v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NanoClawNativeCursor {
    version: u32,
    capture_revision: u32,
    policy_revision: u32,
    generation: u64,
    anchor_source_id: Uuid,
    source_revision: String,
    frontier: NanoClawFrontier,
    prefix_digest: String,
    terminal: bool,
    retained_sessions: u64,
    retained_events: u64,
    rejected_records: u64,
}

impl NanoClawNativeCursor {
    fn initial(anchor_source_id: Uuid, source_revision: String, generation: u64) -> Self {
        Self {
            version: NANOCLAW_NATIVE_CURSOR_VERSION,
            capture_revision: NANOCLAW_CAPTURE_REVISION,
            policy_revision: NANOCLAW_POLICY_REVISION,
            generation,
            anchor_source_id,
            source_revision,
            frontier: NanoClawFrontier::initial(),
            prefix_digest: initial_prefix_digest(),
            terminal: false,
            retained_sessions: 0,
            retained_events: 0,
            rejected_records: 0,
        }
    }

    fn validate(&self, expected_anchor_source_id: Uuid) -> Result<()> {
        if self.version != NANOCLAW_NATIVE_CURSOR_VERSION
            || self.capture_revision != NANOCLAW_CAPTURE_REVISION
            || self.policy_revision != NANOCLAW_POLICY_REVISION
            || self.anchor_source_id != expected_anchor_source_id
            || self.source_revision.is_empty()
            || self.prefix_digest.len() != 64
            || !self
                .prefix_digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(CaptureError::InvalidPayload(
                "NanoClaw NativePath cursor is inconsistent".to_owned(),
            ));
        }
        self.frontier.validate()?;
        Ok(())
    }

    fn encode(&self) -> Result<String> {
        let wire = serde_json::to_string(self)?;
        Ok(format!("{NANOCLAW_NATIVE_CURSOR_PREFIX}{wire}"))
    }

    fn decode(encoded: &str, expected_anchor_source_id: Uuid) -> Result<Self> {
        let wire = encoded
            .strip_prefix(NANOCLAW_NATIVE_CURSOR_PREFIX)
            .ok_or_else(|| {
                CaptureError::InvalidPayload(
                    "NanoClaw Store cursor has an unknown NativePath provider payload".to_owned(),
                )
            })?;
        let cursor: Self = serde_json::from_str(wire)?;
        cursor.validate(expected_anchor_source_id)?;
        if cursor.encode()? != encoded {
            return Err(CaptureError::InvalidPayload(
                "NanoClaw NativePath cursor is not canonical".to_owned(),
            ));
        }
        Ok(cursor)
    }
}

enum PriorCursor {
    None,
    Legacy(SyncCursor),
    Native {
        stored: SyncCursor,
        cursor: NanoClawNativeCursor,
        retired: bool,
    },
}

struct NanoClawLiveProject {
    root: PathBuf,
    central_path: PathBuf,
    machine_id: String,
    cursor_stream: String,
    locator_identity: String,
    proposed_source_identity: String,
    raw_source_path: String,
    source_root: String,
    source_revision: String,
    user_version: i64,
    schema_fingerprint: String,
    anchor_source_id: Uuid,
}

struct CoreOutcome {
    summary: ProviderImportSummary,
    terminal_cursor: Option<NanoClawNativeCursor>,
}

pub(super) fn import_nanoclaw_project(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let requested_root = requested_project_root(path)?;
    let central_path = requested_root.join("data").join("v2.db");
    if !requested_root.is_dir() || !central_path.is_file() {
        if options.import_profile.is_replay_only() {
            mark_output_behind(
                options.import_profile.sink().map(AsRef::as_ref),
                "NanoClaw source is unavailable for output replay",
            );
            return Ok(ProviderImportSummary::default());
        }
        return retire_missing_project(
            store,
            &requested_root,
            &context,
            if !requested_root.exists() {
                ProviderSourceRouteRetirementReason::RootMissing
            } else {
                ProviderSourceRouteRetirementReason::SourceMissing
            },
        );
    }

    let root = fs::canonicalize(nanoclaw_project_root(path)?)?;
    let central_path = root.join("data").join("v2.db");
    let snapshot = NanoClawProjectSnapshot::read(&root, &central_path)?;
    let central = open_provider_sqlite_readonly(&central_path)?;
    let user_version = central.query_row("pragma user_version", [], |row| row.get(0))?;
    let schema_fingerprint = sqlite_schema_fingerprint(&central)?;
    let source_revision = snapshot.source_revision(user_version, &schema_fingerprint);
    let live = live_project(
        &root,
        &central_path,
        &source_revision,
        user_version,
        &schema_fingerprint,
        &context,
    )?;

    if options.import_profile.is_replay_only() {
        replay_outputs_or_mark_behind(
            store,
            &live,
            &snapshot,
            options.import_profile.sink().map(AsRef::as_ref),
        );
        return Ok(ProviderImportSummary::default());
    }

    ensure_active_journal(store)?;
    let committed_store = Store::open_read_only(store.path())?;
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = import_core(
        store,
        &committed_store,
        &bulk_guard,
        &central,
        &snapshot,
        &live,
        &context,
        &options,
    );
    let finish = store
        .finish_event_search_bulk_mode(&bulk_guard)
        .map_err(CaptureError::from);
    let outcome = match (operation, finish) {
        (Ok(outcome), Ok(())) => outcome,
        (_, Err(error)) => return Err(error),
        (Err(error), Ok(())) => return Err(error),
    };
    if outcome.terminal_cursor.is_some() {
        replay_outputs_or_mark_behind(
            store,
            &live,
            &snapshot,
            options.import_profile.sink().map(AsRef::as_ref),
        );
    }
    Ok(outcome.summary)
}

// Core publication deliberately keeps mutable and committed stores, source
// authority, and import policy separate so their transaction roles stay clear.
#[allow(clippy::too_many_arguments)]
fn import_core(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    central: &rusqlite::Connection,
    snapshot: &NanoClawProjectSnapshot,
    live: &NanoClawLiveProject,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
) -> Result<CoreOutcome> {
    let stored = committed_store.get_sync_cursor(None, &context.machine_id, &live.cursor_stream)?;
    let prior = decode_prior_cursor(stored, live.anchor_source_id)?;
    if let PriorCursor::Native {
        cursor, retired, ..
    } = &prior
    {
        if !retired && cursor.source_revision == live.source_revision && cursor.terminal {
            if !snapshot.revalidate()? {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            let mut summary = ProviderImportSummary::default();
            summary.set_work_result(ProviderImportWorkResult::NoOp);
            return Ok(CoreOutcome {
                summary,
                terminal_cursor: Some(cursor.clone()),
            });
        }
    }

    let (mut scanner, mut cursor, mut expected_store_cursor) =
        resume_scanner(central, snapshot, live, prior)?;
    let mut summary = ProviderImportSummary::default();
    let mut committed_groups = 0usize;
    loop {
        let page = scanner.next_page()?;
        let next_cursor = cursor_after_page(&cursor, &page, &live.source_revision)?;
        let page_summary = publish_page(
            store,
            committed_store,
            bulk_guard,
            snapshot,
            live,
            context,
            options,
            &page,
            &next_cursor,
            expected_store_cursor.as_ref(),
        )?;
        summary.merge_from(page_summary);
        expected_store_cursor =
            store.get_sync_cursor(None, &context.machine_id, &live.cursor_stream)?;
        if expected_store_cursor.is_none() {
            return Err(CaptureError::SystemInvariant(
                "NanoClaw NativePath commit did not publish its cursor",
            ));
        }
        cursor = next_cursor;
        committed_groups = committed_groups.saturating_add(1);
        if cursor.terminal {
            summary.work_remaining = false;
            return Ok(CoreOutcome {
                summary,
                terminal_cursor: Some(cursor),
            });
        }
        if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup && committed_groups == 1 {
            summary.work_remaining = true;
            return Ok(CoreOutcome {
                summary,
                terminal_cursor: None,
            });
        }
    }
}

fn resume_scanner<'connection, 'snapshot>(
    central: &'connection rusqlite::Connection,
    snapshot: &'snapshot NanoClawProjectSnapshot,
    live: &NanoClawLiveProject,
    prior: PriorCursor,
) -> Result<(
    NanoClawNativeScanner<'connection, 'snapshot>,
    NanoClawNativeCursor,
    Option<SyncCursor>,
)> {
    let mut scanner = NanoClawNativeScanner::new(central, snapshot)?;
    match prior {
        PriorCursor::None => Ok((
            scanner,
            NanoClawNativeCursor::initial(live.anchor_source_id, live.source_revision.clone(), 0),
            None,
        )),
        PriorCursor::Legacy(stored) => Ok((
            scanner,
            NanoClawNativeCursor::initial(live.anchor_source_id, live.source_revision.clone(), 1),
            Some(stored),
        )),
        PriorCursor::Native {
            stored,
            cursor,
            retired: _,
        } => {
            let prefix_matches = scanner.seek(cursor.frontier, &cursor.prefix_digest)?;
            if !prefix_matches {
                if cursor.source_revision == live.source_revision {
                    return Err(CaptureError::InvalidPayload(
                        "NanoClaw NativePath cursor does not prove the current source prefix"
                            .to_owned(),
                    ));
                }
                let generation =
                    cursor
                        .generation
                        .checked_add(1)
                        .ok_or(CaptureError::SystemInvariant(
                            "NanoClaw NativePath generation exhausted",
                        ))?;
                return Ok((
                    NanoClawNativeScanner::new(central, snapshot)?,
                    NanoClawNativeCursor::initial(
                        live.anchor_source_id,
                        live.source_revision.clone(),
                        generation,
                    ),
                    Some(stored),
                ));
            }
            let mut next = cursor;
            if next.source_revision != live.source_revision {
                next.generation =
                    next.generation
                        .checked_add(1)
                        .ok_or(CaptureError::SystemInvariant(
                            "NanoClaw NativePath generation exhausted",
                        ))?;
                next.source_revision = live.source_revision.clone();
                next.terminal = false;
            }
            Ok((scanner, next, Some(stored)))
        }
    }
}

fn cursor_after_page(
    prior: &NanoClawNativeCursor,
    page: &NanoClawNativePage,
    source_revision: &str,
) -> Result<NanoClawNativeCursor> {
    if page.expected_frontier != prior.frontier {
        return Err(CaptureError::SystemInvariant(
            "NanoClaw scanner page does not begin at the committed frontier",
        ));
    }
    let mut next = prior.clone();
    next.source_revision = source_revision.to_owned();
    next.frontier = page.next_frontier;
    next.prefix_digest = page.prefix_digest.clone();
    next.terminal = page.terminal;
    for unit in &page.units {
        match unit {
            NanoClawNativeUnit::Session { .. } => {
                next.retained_sessions = next.retained_sessions.saturating_add(1)
            }
            NanoClawNativeUnit::Message { .. } => {
                next.retained_events = next.retained_events.saturating_add(1)
            }
            NanoClawNativeUnit::Rejection { .. } => {
                next.rejected_records = next.rejected_records.saturating_add(1)
            }
        }
    }
    Ok(next)
}

#[allow(clippy::too_many_arguments)]
fn publish_page(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    snapshot: &NanoClawProjectSnapshot,
    live: &NanoClawLiveProject,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    page: &NanoClawNativePage,
    cursor: &NanoClawNativeCursor,
    expected_store_cursor: Option<&SyncCursor>,
) -> Result<ProviderImportSummary> {
    let next_sync_cursor = provider_sync_cursor(
        &context.machine_id,
        &live.cursor_stream,
        cursor.encode()?,
        context.imported_at,
    );
    let transition = NativePathCursorTransition::new(
        expected_store_cursor.map(|cursor| cursor.cursor.clone()),
        next_sync_cursor,
    );
    let publication_id = publication_id(live, page, cursor)?;
    let accounting = NativePathGroupAccounting::new(
        1,
        1,
        page.conservative_serialized_bytes
            .saturating_add(transition.next().cursor.len()),
    )?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }

    let resolution =
        group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::NanoClaw,
            source_format: NANOCLAW_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity: live.locator_identity.clone(),
            cursor_stream: live.cursor_stream.clone(),
            proposed_source_identity: live.proposed_source_identity.clone(),
            raw_source_path: Some(live.root.display().to_string()),
            source_revision: live.source_revision.clone(),
            observed_at_ms: context.imported_at.timestamp_millis(),
        })?;
    let route_binding = resolution.route_binding();
    group.upsert_capture_source(&project_capture_source(
        live,
        context,
        &resolution.canonical_source_identity,
    ))?;
    group.bind_capture_source_provider_route(live.anchor_source_id, &route_binding)?;

    let mut summary = ProviderImportSummary::default();
    let mut resolved_sessions = BTreeMap::<String, (Uuid, Uuid)>::new();
    for unit in &page.units {
        let session = match unit {
            NanoClawNativeUnit::Session { session, .. }
            | NanoClawNativeUnit::Message { session, .. } => session,
            NanoClawNativeUnit::Rejection { ordinal, reason } => {
                summary.record_failure(ProviderImportFailure {
                    line: line_number(*ordinal),
                    error: reason.clone(),
                });
                continue;
            }
        };
        let provider_session_id = provider_session_id(session);
        if resolved_sessions.contains_key(&provider_session_id) {
            continue;
        }
        let existing_source = committed_store.capture_source_by_canonical_identity_session(
            CaptureProvider::NanoClaw,
            NANOCLAW_SOURCE_FORMAT,
            &context.machine_id,
            &resolution.canonical_source_identity,
            &provider_session_id,
        )?;
        let source_id = existing_source
            .as_ref()
            .map(|source| source.id)
            .unwrap_or_else(|| {
                provider_scoped_source_uuid(
                    CaptureProvider::NanoClaw,
                    &provider_session_id,
                    NANOCLAW_SOURCE_FORMAT,
                    Some(&live.raw_source_path),
                )
            });
        let session_id = provider_import_session_uuid(
            committed_store,
            CaptureProvider::NanoClaw,
            &provider_session_id,
            source_id,
            Some(&resolution.canonical_source_identity),
        )?;
        let existed = match committed_store.get_session(session_id) {
            Ok(_) => true,
            Err(StoreError::NotFound(_)) => false,
            Err(error) => return Err(error.into()),
        };
        group.upsert_capture_source(&session_capture_source(
            live,
            context,
            session,
            source_id,
            &resolution.canonical_source_identity,
        ))?;
        group.bind_capture_source_provider_route(source_id, &route_binding)?;
        group.upsert_session(&native_session(
            live, context, options, session, source_id, session_id,
        ))?;
        if existed {
            summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
        } else {
            summary.imported_sessions = summary.imported_sessions.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        }
        resolved_sessions.insert(provider_session_id, (source_id, session_id));
    }

    for unit in &page.units {
        let NanoClawNativeUnit::Message {
            ordinal,
            session,
            message,
            locator,
            ..
        } = unit
        else {
            continue;
        };
        let seq = match message
            .seq
            .map(|seq| provider_nonnegative_i64_to_u64(seq, "NanoClaw message seq"))
            .transpose()
        {
            Ok(seq) => seq,
            Err(error) => {
                summary.record_failure(ProviderImportFailure {
                    line: line_number(*ordinal),
                    error: error.to_string(),
                });
                continue;
            }
        };
        let provider_session_id = provider_session_id(session);
        let (source_id, session_id) = resolved_sessions.get(&provider_session_id).copied().ok_or(
            CaptureError::SystemInvariant("NanoClaw message lost its page-local session"),
        )?;
        let (mut event, complete_text) =
            nanoclaw_core_event(session, message, seq, context.imported_at);
        event.metadata["source_record_ordinal"] = json!(ordinal);
        event.metadata["source_record_subrecord_index"] = json!(0);
        attach_nanoclaw_complete_content_locator(
            &mut event,
            locator,
            &nanoclaw_message_digest_values(message),
            &complete_text,
        )?;
        let event_hash = event.provider_event_hash.as_str();
        let identity = provider_event_import_identity_with_exact_legacy_source(
            committed_store,
            CaptureProvider::NanoClaw,
            &provider_session_id,
            source_id,
            event.provider_event_index,
            event.provider_event_index,
            event_hash,
            None,
            None,
            session_id == provider_session_uuid(CaptureProvider::NanoClaw, &provider_session_id),
        )?;
        let normalized = nanoclaw_canonical_event(
            &provider_session_id,
            source_id,
            session_id,
            line_number(*ordinal),
            &event,
            event_hash,
            &identity,
            context,
            options,
        )?;
        if group
            .reconcile_provider_event(&normalized, ProviderEventHashAuthority::ProviderSupplied)?
        {
            summary.imported_events = summary.imported_events.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        } else {
            summary.skipped_events = summary.skipped_events.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
        }
    }
    if !snapshot.revalidate_before_commit()? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

#[allow(clippy::too_many_arguments)]
fn nanoclaw_canonical_event(
    provider_session_id: &str,
    source_id: Uuid,
    session_id: Uuid,
    line_number: usize,
    event: &NanoClawCoreEvent,
    event_hash: &str,
    identity: &crate::provider::importer::ProviderEventImportIdentity,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
) -> Result<Event> {
    let mut provider_metadata = event.metadata.clone();
    let source_record_ordinal = provider_metadata
        .as_object_mut()
        .and_then(|metadata| metadata.remove("source_record_ordinal"))
        .and_then(|value| value.as_u64())
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "NanoClaw source record ordinal annotation is malformed".to_owned(),
            )
        })?;
    let source_record_subrecord_index = provider_metadata
        .as_object_mut()
        .and_then(|metadata| metadata.remove("source_record_subrecord_index"))
        .and_then(|value| value.as_u64())
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "NanoClaw source record subrecord annotation is malformed".to_owned(),
            )
        })?;
    let verified_content_locators = provider_metadata
        .as_object_mut()
        .and_then(|metadata| metadata.remove(VERIFIED_CONTENT_LOCATORS_METADATA_KEY))
        .map(|value| {
            VerifiedContentLocatorsV1::from_metadata_value(&value)
                .map(|locators| locators.to_metadata_value())
                .ok_or_else(|| {
                    CaptureError::InvalidPayload(
                        "NanoClaw verified content locator annotation is malformed".to_owned(),
                    )
                })
        })
        .transpose()?;
    let dedupe_key =
        Store::provider_event_dedupe_key_with_payload_hash(&identity.dedupe_key, event_hash)
            .unwrap_or_else(|| identity.dedupe_key.clone());
    let mut sync_metadata = json!({
        "provider_session_id": provider_session_id,
        "provider_event_index": event.provider_event_index,
        "provider_event_hash": event_hash,
        "provider_event_hash_authority": ProviderEventHashAuthority::ProviderSupplied.as_str(),
        "cursor": event.cursor,
        "source_format": NANOCLAW_SOURCE_FORMAT,
        "source_trust": "provider_native",
        "fixture_line": line_number,
        "imported_at": context.imported_at,
        "event_idempotency_key": format!(
            "provider-event:{}:{}:{}",
            CaptureProvider::NanoClaw.as_str(),
            provider_session_id,
            event.provider_event_index,
        ),
        "source_record_ordinal": source_record_ordinal,
        "source_record_subrecord_index": source_record_subrecord_index,
        "metadata": provider_metadata,
    });
    if let (Some(metadata), Some(locators)) =
        (sync_metadata.as_object_mut(), verified_content_locators)
    {
        metadata.insert(VERIFIED_CONTENT_LOCATORS_METADATA_KEY.to_owned(), locators);
    }
    Ok(Event {
        id: identity.id,
        seq: identity.seq,
        history_record_id: options.history_record_id,
        session_id: Some(session_id),
        run_id: None,
        event_type: event.event_type,
        role: event.role,
        occurred_at: event.occurred_at,
        capture_source_id: Some(source_id),
        payload: json!({
            "provider": CaptureProvider::NanoClaw.as_str(),
            "provider_session_id": provider_session_id,
            "provider_event_index": event.provider_event_index,
            "provider_event_hash": event_hash,
            "cursor": event.cursor,
            "artifacts": [],
            "body": compact_provider_result_payload(event.event_type, &event.payload),
        }),
        payload_blob_id: None,
        dedupe_key: Some(dedupe_key),
        sync: provider_sync_metadata(Fidelity::Imported, sync_metadata),
    })
}

fn attach_nanoclaw_complete_content_locator(
    event: &mut NanoClawCoreEvent,
    locator: &NativeLocator,
    values: &[NativeSqliteValue],
    complete_text: &str,
) -> Result<()> {
    if event.event_type != ctx_history_core::EventType::Message
        || event
            .payload
            .pointer("/text_retention/truncated")
            .and_then(Value::as_bool)
            != Some(true)
    {
        return Ok(());
    }
    let content_ref = ContentRef::from_bytes(complete_text.as_bytes()).ok_or(
        CaptureError::SystemInvariant("NanoClaw content length exceeds ContentRef bounds"),
    )?;
    let profile = verified_content_profile(
        CaptureProvider::NanoClaw,
        NANOCLAW_SOURCE_FORMAT,
        CompleteContentSourceFamily::Sqlite,
        VerifiedContentRole::MessageBody,
    )
    .ok_or(CaptureError::SystemInvariant(
        "NanoClaw message route must have a verified-content profile",
    ))?;
    let persisted = VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        profile,
        content_ref,
        CompleteContentSourceFamily::Sqlite,
        locator.kind(),
        locator.value(),
        event.provider_event_hash.clone(),
        nanoclaw_logical_record_digest(values)?,
    )
    .ok_or(CaptureError::SystemInvariant(
        "NanoClaw complete-content locator exceeds the bounded canonical schema",
    ))?;
    attach_verified_content_locator(&mut event.metadata, persisted).ok_or(
        CaptureError::SystemInvariant("NanoClaw verified-content locator collection is malformed"),
    )?;
    Ok(())
}

fn nanoclaw_logical_record_digest(
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
        CaptureError::SystemInvariant("NanoClaw SHA-256 formatting produced an invalid digest"),
    )
}

fn project_capture_source(
    live: &NanoClawLiveProject,
    context: &ProviderAdapterContext,
    canonical_source_identity: &str,
) -> CaptureSource {
    CaptureSource {
        id: live.anchor_source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::NanoClaw,
            machine_id: context.machine_id.clone(),
            process_id: None,
            cwd: Some(live.root.display().to_string()),
            raw_source_path: Some(live.raw_source_path.clone()),
            source_format: Some(NANOCLAW_SOURCE_FORMAT.to_owned()),
            source_root: Some(live.source_root.clone()),
            source_identity: Some(canonical_source_identity.to_owned()),
            external_session_id: Some(NANOCLAW_PROJECT_EXTERNAL_SESSION.to_owned()),
        },
        started_at: context.imported_at,
        ended_at: None,
        sync: provider_sync_metadata(
            Fidelity::Partial,
            json!({
                "source_format": NANOCLAW_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "source_identity": canonical_source_identity,
                "source_root": live.source_root,
                "source_revision": live.source_revision,
                "nativepath_publication": NANOCLAW_NATIVE_PUBLICATION_REVISION,
                "project_anchor": true,
            }),
        ),
    }
}

fn session_capture_source(
    live: &NanoClawLiveProject,
    context: &ProviderAdapterContext,
    session: &NanoClawSessionRow,
    source_id: Uuid,
    canonical_source_identity: &str,
) -> CaptureSource {
    let provider_session_id = provider_session_id(session);
    let started_at = provider_timestamp_millis(session.created_at, context.imported_at);
    CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::NanoClaw,
            machine_id: context.machine_id.clone(),
            process_id: None,
            cwd: session.agent_group_folder.clone(),
            raw_source_path: Some(live.raw_source_path.clone()),
            source_format: Some(NANOCLAW_SOURCE_FORMAT.to_owned()),
            source_root: Some(live.source_root.clone()),
            source_identity: Some(canonical_source_identity.to_owned()),
            external_session_id: Some(provider_session_id.clone()),
        },
        started_at,
        ended_at: session
            .last_active
            .map(|timestamp| provider_timestamp_millis(Some(timestamp), context.imported_at)),
        sync: provider_sync_metadata(
            Fidelity::Partial,
            json!({
                "provider_session_id": provider_session_id,
                "source_format": NANOCLAW_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "source_identity": canonical_source_identity,
                "source_root": live.source_root,
                "source_revision": live.source_revision,
                "source_identity_key": provider_scoped_source_identity_key(
                    CaptureProvider::NanoClaw,
                    &provider_session_id,
                    NANOCLAW_SOURCE_FORMAT,
                    Some(&live.raw_source_path),
                ),
                "adapter": NANOCLAW_SOURCE_FORMAT,
                "central_db": live.central_path,
                "sqlite_user_version": live.user_version,
                "schema_fingerprint": live.schema_fingerprint,
                "support_level": "explicit",
                "nativepath_publication": NANOCLAW_NATIVE_PUBLICATION_REVISION,
            }),
        ),
    }
}

fn native_session(
    live: &NanoClawLiveProject,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    row: &NanoClawSessionRow,
    source_id: Uuid,
    session_id: Uuid,
) -> Session {
    let provider_session_id = provider_session_id(row);
    let started_at = provider_timestamp_millis(row.created_at, context.imported_at);
    let ended_at = row
        .last_active
        .map(|timestamp| provider_timestamp_millis(Some(timestamp), context.imported_at));
    Session {
        id: session_id,
        history_record_id: options.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::NanoClaw,
        external_session_id: Some(provider_session_id.clone()),
        external_agent_id: row.agent_provider.clone(),
        agent_type: AgentType::Primary,
        role_hint: Some("container-session".to_owned()),
        is_primary: true,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at,
        ended_at,
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Partial,
            json!({
                "provider_session_id": provider_session_id,
                "source_format": NANOCLAW_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "session_idempotency_key": format!(
                    "provider-session:{}:{}",
                    CaptureProvider::NanoClaw.as_str(),
                    provider_session_id,
                ),
                "metadata": {
                    "source_format": NANOCLAW_SOURCE_FORMAT,
                    "session_id": row.id,
                    "agent_group_id": row.agent_group_id,
                    "agent_group_name": row.agent_group_name,
                    "agent_provider": row.agent_provider,
                    "status": row.status,
                    "container_status": row.container_status,
                    "messaging_group_id": row.messaging_group_id,
                    "messaging": {
                        "channel_type": row.messaging_channel_type,
                        "platform_id": row.messaging_platform_id,
                        "instance": row.messaging_instance,
                        "name": row.messaging_name,
                        "thread_id": row.thread_id,
                    },
                    "central_db": live.central_path,
                    "sqlite_user_version": live.user_version,
                    "schema_fingerprint": live.schema_fingerprint,
                    "nativepath_publication": NANOCLAW_NATIVE_PUBLICATION_REVISION,
                },
            }),
        ),
    }
}

fn retire_missing_project(
    store: &mut Store,
    requested_root: &Path,
    context: &ProviderAdapterContext,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<ProviderImportSummary> {
    let cursor_path_identity = provider_path_identity(requested_root)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::NanoClaw,
        NANOCLAW_SOURCE_FORMAT,
        &cursor_path_identity,
    );
    let Some(stored) = store.get_sync_cursor(None, &context.machine_id, &cursor_stream)? else {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: requested_root.to_path_buf(),
            reason: "NanoClaw project root or data/v2.db does not exist",
        });
    };
    let committed = match decode_native_path_committed_cursor(&stored.cursor) {
        Ok(committed) => committed,
        Err(_) => {
            if !is_released_nanoclaw_legacy_cursor(&stored.cursor)? {
                return Err(CaptureError::InvalidPayload(
                    "NanoClaw cursor is neither a released legacy cursor nor a NativePath cursor"
                        .to_owned(),
                ));
            }
            let mut summary = ProviderImportSummary::default();
            summary.set_work_result(ProviderImportWorkResult::NoOp);
            return Ok(summary);
        }
    };
    let anchor_source_id = project_anchor_source_id(&cursor_path_identity);
    let cursor = NanoClawNativeCursor::decode(committed.provider_cursor(), anchor_source_id)?;
    let anchor = store.get_capture_source(cursor.anchor_source_id)?;
    let canonical_source_identity =
        anchor
            .descriptor
            .source_identity
            .ok_or(CaptureError::SystemInvariant(
                "NanoClaw project anchor has no canonical source identity",
            ))?;
    let transition = NativePathCursorTransition::new(
        Some(stored.cursor.clone()),
        provider_sync_cursor(
            &context.machine_id,
            &cursor_stream,
            cursor.encode()?,
            context.imported_at,
        ),
    );
    let retirement = ProviderSourceRouteRetirement {
        provider: CaptureProvider::NanoClaw,
        source_format: NANOCLAW_SOURCE_FORMAT.to_owned(),
        machine_id: context.machine_id.clone(),
        locator_identity: cursor_path_identity,
        cursor_stream,
        expected_canonical_source_identity: canonical_source_identity,
        expected_source_revision: cursor.source_revision,
        retired_at_ms: context.imported_at.timestamp_millis(),
        reason,
    };
    let publication_id = retirement_publication_id(&retirement, &transition);
    if committed.publication_id() == publication_id {
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }
    ensure_active_journal(store)?;
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let admission = store.admit_event_search_bulk_group(&bulk_guard)?;
        let mut group = store.begin_native_path_publication_group(
            admission,
            NativePathGroupAccounting::new(0, 1, transition.next().cursor.len())?,
        )?;
        let disposition = if matches!(
            group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
            NativePathCursorSetClassification::AllNextSameGroup { .. }
        ) {
            ProviderSourceRouteRetirementDisposition::AlreadyRetired
        } else {
            let disposition = group.retire_provider_source_route(&retirement)?;
            group.prepare_journal_checkpoint()?;
            group.publish_cursor_set()?;
            disposition
        };
        group.commit()?;
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(
            if disposition == ProviderSourceRouteRetirementDisposition::Retired {
                ProviderImportWorkResult::Changed
            } else {
                ProviderImportWorkResult::NoOp
            },
        );
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

fn replay_outputs_or_mark_behind(
    store: &Store,
    live: &NanoClawLiveProject,
    snapshot: &NanoClawProjectSnapshot,
    sink: Option<&dyn ProOutputSink>,
) {
    let Some(sink) = sink else {
        return;
    };
    if let Err(error) = replay_outputs(store, live, snapshot, sink) {
        sink.mark_behind(ProOutputSinkError::new(
            "nanoclaw_nativepath_output_replay",
            error.to_string(),
        ));
    }
}

fn replay_outputs(
    store: &Store,
    live: &NanoClawLiveProject,
    snapshot: &NanoClawProjectSnapshot,
    sink: &dyn ProOutputSink,
) -> Result<()> {
    if !snapshot.revalidate()? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let Some(stored) = store.get_sync_cursor(None, &live.machine_id, &live.cursor_stream)? else {
        return Ok(());
    };
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    let cursor = NanoClawNativeCursor::decode(committed.provider_cursor(), live.anchor_source_id)?;
    if !cursor.terminal || cursor.source_revision != live.source_revision {
        return Err(CaptureError::InvalidPayload(
            "NanoClaw output replay requires the terminal current Core frontier".to_owned(),
        ));
    }
    let anchor = store.get_capture_source(cursor.anchor_source_id)?;
    let canonical_source_identity =
        anchor
            .descriptor
            .source_identity
            .ok_or(CaptureError::SystemInvariant(
                "NanoClaw project anchor has no canonical source identity",
            ))?;
    let output_source = OutputSourceIdentity {
        provider: CaptureProvider::NanoClaw.as_str().to_owned(),
        namespace_id: live.source_root.clone(),
        source_id: canonical_source_identity.clone(),
    };
    let progress = match sink.observe_source(&output_source) {
        Ok(progress) => progress,
        Err(error) => {
            sink.mark_behind(error);
            return Ok(());
        }
    };
    let final_frontier = output_frontier(&cursor)?;
    if progress.as_ref().is_some_and(|progress| {
        progress.observed_revision == cursor.source_revision
            && progress.parser_revision == NANOCLAW_OUTPUT_PARSER_REVISION
            && progress.materializer_revision == sink.materializer_revision()
            && progress.terminal
            && progress.cursor.as_ref().is_some_and(|prior| {
                prior.version == final_frontier.version && prior.payload == final_frontier.bytes
            })
    }) {
        return Ok(());
    }
    let state = output_state(progress, &cursor, sink.materializer_revision())?;
    let expected_frontier = NativeSafeFrontier::new(
        NANOCLAW_OUTPUT_FRONTIER_VERSION,
        serde_json::to_vec(&NanoClawFrontier::initial())?,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let output = NativeProOutputPage {
        inventory_generation: sink.inventory_generation(),
        source: output_source,
        source_epoch: state.source_epoch,
        observed_revision: cursor.source_revision.clone(),
        parser_revision: NANOCLAW_OUTPUT_PARSER_REVISION.to_owned(),
        materializer_revision: sink.materializer_revision().to_owned(),
        disposition: state.disposition,
        expected_prior_source_epoch: state.expected_source_epoch,
        expected_prior_frontier: state.expected_frontier,
        observations: Vec::new(),
    };
    let page = NativeProReplayPage::new_with_source_identity(
        NativeSourceIdentity::new(
            CaptureProvider::NanoClaw.as_str(),
            canonical_source_identity,
        ),
        expected_frontier,
        final_frontier,
        true,
        NativePageAccounting {
            logical_units: 1,
            conservative_serialized_bytes: NANOCLAW_OUTPUT_PAGE_BYTES,
        },
        output,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    if let Err(failure) = process_pro_replay_only(page, sink) {
        sink.mark_behind(ProOutputSinkError::new(
            "nanoclaw_nativepath_output_replay",
            format!("{:?}", failure.output_error),
        ));
    }
    Ok(())
}

struct NanoClawOutputState {
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_frontier: Option<NativeSafeFrontier>,
    disposition: ProOutputSourceDisposition,
}

fn output_state(
    progress: Option<ProOutputProgress>,
    cursor: &NanoClawNativeCursor,
    materializer_revision: &str,
) -> Result<NanoClawOutputState> {
    let Some(progress) = progress else {
        return Ok(NanoClawOutputState {
            source_epoch: 0,
            expected_source_epoch: None,
            expected_frontier: None,
            disposition: ProOutputSourceDisposition::NewSource,
        });
    };
    let expected_frontier = progress
        .cursor
        .as_ref()
        .map(|cursor| NativeSafeFrontier::new(cursor.version, cursor.payload.clone()))
        .transpose()
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let rewrite = progress.observed_revision != cursor.source_revision
        || progress.parser_revision != NANOCLAW_OUTPUT_PARSER_REVISION
        || progress.materializer_revision != materializer_revision
        || progress.source_epoch != cursor.generation;
    Ok(NanoClawOutputState {
        source_epoch: if rewrite {
            progress
                .source_epoch
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "NanoClaw output source epoch exhausted",
                ))?
        } else {
            progress.source_epoch
        },
        expected_source_epoch: Some(progress.source_epoch),
        expected_frontier,
        disposition: if rewrite {
            ProOutputSourceDisposition::Rewrite
        } else {
            ProOutputSourceDisposition::AppendOrResume
        },
    })
}

fn output_frontier(cursor: &NanoClawNativeCursor) -> Result<NativeSafeFrontier> {
    NativeSafeFrontier::new(
        NANOCLAW_OUTPUT_FRONTIER_VERSION,
        serde_json::to_vec(&json!({
            "generation": cursor.generation,
            "source_revision": cursor.source_revision,
            "frontier": cursor.frontier,
            "prefix_digest": cursor.prefix_digest,
            "terminal": cursor.terminal,
        }))?,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

fn live_project(
    root: &Path,
    central_path: &Path,
    source_revision: &str,
    user_version: i64,
    schema_fingerprint: &str,
    context: &ProviderAdapterContext,
) -> Result<NanoClawLiveProject> {
    let cursor_path_identity = provider_path_identity(root)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::NanoClaw,
        NANOCLAW_SOURCE_FORMAT,
        &cursor_path_identity,
    );
    let raw_source_path = context
        .source_path
        .as_deref()
        .unwrap_or(root)
        .display()
        .to_string();
    let source_root = context
        .source_root_display()
        .unwrap_or_else(|| raw_source_path.clone());
    let proposed_source_identity = provider_source_identity(
        CaptureProvider::NanoClaw,
        NANOCLAW_SOURCE_FORMAT,
        Some(&source_root),
        Some(&raw_source_path),
        None,
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "NanoClaw project has no canonical source identity",
    ))?;
    Ok(NanoClawLiveProject {
        root: root.to_path_buf(),
        central_path: central_path.to_path_buf(),
        machine_id: context.machine_id.clone(),
        locator_identity: cursor_path_identity.clone(),
        cursor_stream,
        proposed_source_identity,
        raw_source_path,
        source_root,
        source_revision: source_revision.to_owned(),
        user_version,
        schema_fingerprint: schema_fingerprint.to_owned(),
        anchor_source_id: project_anchor_source_id(&cursor_path_identity),
    })
}

fn requested_project_root(path: &Path) -> Result<PathBuf> {
    let root = if path.file_name().and_then(|name| name.to_str()) == Some("v2.db") {
        path.parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "NanoClaw data/v2.db has no project root",
            })?
    } else {
        path.to_path_buf()
    };
    if root.is_absolute() {
        Ok(root)
    } else {
        Ok(std::env::current_dir()?.join(root))
    }
}

fn decode_prior_cursor(stored: Option<SyncCursor>, anchor_source_id: Uuid) -> Result<PriorCursor> {
    let Some(stored) = stored else {
        return Ok(PriorCursor::None);
    };
    match decode_native_path_committed_cursor(&stored.cursor) {
        Ok(committed) => {
            let retired = committed
                .publication_id()
                .starts_with("nanoclaw-nativepath-retire:");
            Ok(PriorCursor::Native {
                cursor: NanoClawNativeCursor::decode(
                    committed.provider_cursor(),
                    anchor_source_id,
                )?,
                stored,
                retired,
            })
        }
        Err(_) => {
            if is_released_nanoclaw_legacy_cursor(&stored.cursor)? {
                Ok(PriorCursor::Legacy(stored))
            } else {
                Err(CaptureError::InvalidPayload(
                    "NanoClaw cursor is neither a released legacy cursor nor a NativePath cursor"
                        .to_owned(),
                ))
            }
        }
    }
}

fn is_released_nanoclaw_legacy_cursor(encoded: &str) -> Result<bool> {
    let Some(cursor) = CertifiedProviderCursor::decode_if_certified(encoded)? else {
        return Ok(false);
    };
    if cursor.parser_revision() != NANOCLAW_CAPTURE_REVISION
        || cursor.policy_revision() != NANOCLAW_POLICY_REVISION
        || cursor.native_position().kind() != NANOCLAW_LEGACY_POSITION_KIND
    {
        return Ok(false);
    }
    let _: () = cursor.parser_checkpoint().deserialize()?;
    let value = cursor.native_position().value();
    if value == [0] {
        return Ok(true);
    }
    if value.len() != 27 || value[0] != 1 || !matches!(value[9], 1 | 2) || value[18] > 2 {
        return Ok(false);
    }
    let session_rowid = decode_legacy_ordered_i64(&value[10..18])?;
    let message_rowid = decode_legacy_ordered_i64(&value[19..27])?;
    let valid = session_rowid > 0
        && !(value[9] == 1 && (value[18] != 0 || message_rowid != 0))
        && !(value[18] == 0 && message_rowid != 0);
    Ok(valid)
}

fn decode_legacy_ordered_i64(bytes: &[u8]) -> Result<i64> {
    let bytes: [u8; 8] = bytes.try_into().map_err(|_| {
        CaptureError::InvalidPayload(
            "NanoClaw released cursor integer has an invalid width".to_owned(),
        )
    })?;
    Ok((u64::from_be_bytes(bytes) ^ (1_u64 << 63)) as i64)
}

fn provider_sync_cursor(
    machine_id: &str,
    stream: &str,
    cursor: String,
    at: DateTime<Utc>,
) -> SyncCursor {
    SyncCursor {
        id: stable_capture_uuid(
            &format!("provider-cursor:{machine_id}:{stream}"),
            "provider-sync-cursor",
        ),
        team_id: None,
        device_id: machine_id.to_owned(),
        stream: stream.to_owned(),
        cursor,
        last_synced_at: Some(at),
        timestamps: timestamps(at),
    }
}

fn publication_id(
    live: &NanoClawLiveProject,
    page: &NanoClawNativePage,
    cursor: &NanoClawNativeCursor,
) -> Result<String> {
    let mut digest = Sha256::new();
    digest.update(NANOCLAW_NATIVE_PUBLICATION_DOMAIN);
    hash_field(&mut digest, live.cursor_stream.as_bytes());
    hash_field(&mut digest, live.source_revision.as_bytes());
    hash_field(&mut digest, &serde_json::to_vec(&page.expected_frontier)?);
    hash_field(&mut digest, &serde_json::to_vec(&page.next_frontier)?);
    hash_field(&mut digest, cursor.prefix_digest.as_bytes());
    digest.update([u8::from(page.terminal)]);
    for unit in &page.units {
        hash_field(&mut digest, &serde_json::to_vec(unit)?);
    }
    Ok(format!("nanoclaw-nativepath:{}", hex(&digest.finalize())))
}

fn retirement_publication_id(
    retirement: &ProviderSourceRouteRetirement,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(NANOCLAW_NATIVE_RETIREMENT_DOMAIN);
    hash_field(&mut digest, retirement.machine_id.as_bytes());
    hash_field(&mut digest, retirement.locator_identity.as_bytes());
    hash_field(
        &mut digest,
        retirement.expected_canonical_source_identity.as_bytes(),
    );
    hash_field(&mut digest, retirement.expected_source_revision.as_bytes());
    hash_field(&mut digest, transition.next().cursor.as_bytes());
    format!("nanoclaw-nativepath-retire:{}", hex(&digest.finalize()))
}

fn project_anchor_source_id(cursor_path_identity: &str) -> Uuid {
    stable_capture_uuid(
        &format!("nanoclaw-nativepath-project:{cursor_path_identity}"),
        "source",
    )
}

fn provider_session_id(session: &NanoClawSessionRow) -> String {
    format!("{}/{}", session.agent_group_id, session.id)
}

fn initial_prefix_digest() -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-nanoclaw-nativepath-prefix-v1\0");
    hex(&digest.finalize())
}

fn line_number(ordinal: u64) -> usize {
    ordinal.min(usize::MAX as u64) as usize
}

fn hash_field(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_le_bytes());
    digest.update(value);
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        encoded.push(char::from(DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn ensure_active_journal(store: &Store) -> Result<()> {
    match store.projection_journal_snapshot(None) {
        Ok(_) => Ok(()),
        Err(StoreError::ProjectionJournalInactive) => {
            store.activate_projection_journal(ctx_pro_host_protocol::PROTOCOL_FINGERPRINT)?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

fn mark_output_behind(sink: Option<&dyn ProOutputSink>, message: &str) {
    if let Some(sink) = sink {
        sink.mark_behind(ProOutputSinkError::new(
            "nanoclaw_nativepath_output_replay",
            message,
        ));
    }
}
