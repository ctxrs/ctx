use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    Confidence, ContentRef, Event, EventRole, EventType, Fidelity, FileChangeKind, FileTouched,
    Session, SessionStatus, SyncCursor,
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

use crate::{
    complete_content::{
        attach_verified_content_locator, structured::STRUCTURED_COMPLETE_CONTENT_LOCATOR_KIND,
        verified_content_profile, CompleteContentBodyDigest, CompleteContentSourceFamily,
        VerifiedContentLocatorV1, VerifiedContentRole, VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
    },
    provider::{
        file_touches::{
            visit_provider_file_touch_drafts_with_limit, MAX_PACKED_PROVIDER_EVENT_INDEX,
            MAX_PROVIDER_FILE_TOUCHES_PER_EVENT, PROVIDER_FILE_TOUCH_LIMIT_REJECTION,
        },
        importer::{
            compact_provider_result_payload,
            provider_event_import_identity_with_exact_legacy_source, provider_file_touch_import_id,
            provider_import_session_uuid, provider_path_identity,
            provider_scoped_source_identity_key, provider_scoped_source_uuid,
            provider_session_uuid, provider_source_cursor_stream_for_path,
            provider_source_identity, provider_sync_metadata, timestamps, CertifiedProviderCursor,
            ExactLegacySourceEventCandidate,
        },
        native_ingestion::{
            process_pro_replay_only, NativePageAccounting, NativeProOutputPage,
            NativeProReplayPage, NativeSafeFrontier, NativeSourceIdentity,
            NATIVE_INGESTION_PAGE_MAX_BYTES,
        },
        normalization::{
            provider_capped_json, provider_local_preview, provider_policy_body,
            provider_policy_event_text, provider_result_identifier_evidence,
            provider_result_outcome_evidence,
        },
        tool_input,
    },
    stable_capture_uuid, CaptureError, CaptureWorkLimit, OutputAssociations, OutputCommandContext,
    OutputNativeCoordinate, OutputObservationKind, OutputOutcome, OutputOutcomeMetadata,
    OutputSourceIdentity, OutputSourceLocator, ProOutputObservation, ProOutputProgress,
    ProOutputSink, ProOutputSinkError, ProOutputSourceDisposition, ProviderAdapterContext,
    ProviderImportFailure, ProviderImportOptions, ProviderImportSummary, ProviderImportWorkResult,
    Result, MAX_PROVIDER_JSONL_LINE_BYTES, OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
    PROVIDER_MAX_PREVIEW_CHARS, PROVIDER_MAX_TEXT_CHARS,
};

use super::{
    event::{decode_openhands_event, OpenHandsDecodedEvent},
    source::{
        discover_openhands_event_paths, hex, openhands_legacy_filename_index_candidate,
        openhands_line_number, openhands_missing_event_files, OpenHandsFileObservation,
        OpenHandsObservedFile,
    },
};

const OPENHANDS_NATIVE_CURSOR_VERSION: u32 = 1;
const OPENHANDS_NATIVE_PARSER_REVISION: u32 = 1;
const OPENHANDS_NATIVE_POLICY_REVISION: u32 = 1;
const OPENHANDS_OUTPUT_FRONTIER_VERSION: u32 = 1;
const OPENHANDS_OUTPUT_PARSER_REVISION: &str = "openhands-nativepath-output-v1";
const OPENHANDS_NATIVE_PUBLICATION_DOMAIN: &[u8] = b"ctx-openhands-nativepath-publication-v1\0";
const OPENHANDS_NATIVE_RETIREMENT_DOMAIN: &[u8] = b"ctx-openhands-nativepath-retirement-v1\0";
const OPENHANDS_NATIVE_PAGE_TOUCHES: usize = 48;
const OPENHANDS_NATIVE_PAGE_MAX_BYTES: usize = 6 * 1024 * 1024;
const OPENHANDS_MAX_FAILURE_BYTES: usize = 4 * 1024;
const OPENHANDS_LOCATOR_KIND: &str = "openhands-event-path-v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenHandsNativeCursor {
    version: u32,
    parser_revision: u32,
    policy_revision: u32,
    route_sha256: [u8; 32],
    locator_identity: String,
    legacy_source_layout: bool,
    source_revision: String,
    observation: Option<OpenHandsFileObservation>,
    content_sha256: Option<[u8; 32]>,
    generation: u64,
    next_touch: u64,
    accepted_event: bool,
    accepted_file_touches: u64,
    rejected_records: u64,
    terminal: bool,
    deleted: bool,
}

impl OpenHandsNativeCursor {
    fn for_source(
        source: &OpenHandsObservedFile,
        source_revision: String,
        generation: u64,
    ) -> Self {
        Self {
            version: OPENHANDS_NATIVE_CURSOR_VERSION,
            parser_revision: OPENHANDS_NATIVE_PARSER_REVISION,
            policy_revision: OPENHANDS_NATIVE_POLICY_REVISION,
            route_sha256: source.route_sha256,
            locator_identity: source.path_identity.clone(),
            legacy_source_layout: false,
            source_revision,
            observation: Some(source.observation.clone()),
            content_sha256: source.content_sha256,
            generation,
            next_touch: 0,
            accepted_event: false,
            accepted_file_touches: 0,
            rejected_records: 0,
            terminal: false,
            deleted: false,
        }
    }

    fn route_supported_for(&self, source: &OpenHandsObservedFile) -> bool {
        self.version == OPENHANDS_NATIVE_CURSOR_VERSION
            && self.parser_revision == OPENHANDS_NATIVE_PARSER_REVISION
            && self.policy_revision == OPENHANDS_NATIVE_POLICY_REVISION
            && self.route_sha256 == source.route_sha256
            && !self.locator_identity.is_empty()
    }

    fn supported_for(&self, source: &OpenHandsObservedFile) -> bool {
        self.route_supported_for(source) && !self.deleted
    }

    fn encode(&self) -> Result<String> {
        serde_json::to_string(self).map_err(CaptureError::from)
    }
}

enum StoredCoreCursor {
    Fresh,
    Native {
        stored: SyncCursor,
        cursor: OpenHandsNativeCursor,
    },
    Migrated {
        stored: SyncCursor,
    },
}

impl StoredCoreCursor {
    fn expected_encoded(&self) -> Option<String> {
        match self {
            Self::Fresh => None,
            Self::Native { stored, .. } | Self::Migrated { stored } => Some(stored.cursor.clone()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OpenHandsSourceChange {
    Fresh,
    Unchanged,
    Append,
    Rewrite,
    Truncation,
    Replacement,
    Migrated,
}

struct PreparedCorePage {
    source_revision: String,
    expected_cursor: Option<String>,
    next_cursor: OpenHandsNativeCursor,
    event: Option<OpenHandsEventFact>,
    touches: Vec<(usize, OpenHandsTouchFact)>,
    rejection: Option<ProviderImportFailure>,
    conservative_serialized_bytes: usize,
    source_change: OpenHandsSourceChange,
}

#[derive(Serialize)]
struct OpenHandsEventFact {
    provider_event_index: u64,
    provider_event_hash: String,
    cursor: String,
    event_type: EventType,
    role: EventRole,
    occurred_at: DateTime<Utc>,
    payload: Value,
    metadata: Value,
}

#[derive(Serialize)]
struct OpenHandsTouchFact {
    provider_session_id: String,
    provider_touch_index: u64,
    provider_event_index: Option<u64>,
    raw_source_path: String,
    source_root: Option<String>,
    path: String,
    change_kind: Option<FileChangeKind>,
    old_path: Option<String>,
    line_count_delta: Option<i64>,
    confidence: Confidence,
    occurred_at: DateTime<Utc>,
    metadata: Value,
}

impl PreparedCorePage {
    fn terminal(&self) -> bool {
        self.next_cursor.terminal
    }
}

pub(crate) fn import_openhands_nativepath(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let configured_source_root = context
        .source_root
        .clone()
        .or(context.source_path.clone())
        .unwrap_or_else(|| path.to_path_buf());
    let inventory = discover_openhands_event_paths(path)?;
    let live_paths = inventory.paths.iter().cloned().collect::<BTreeSet<_>>();
    let known_routes = known_openhands_routes(store, &context.machine_id, &configured_source_root)?;
    let sink = options.import_profile.sink().cloned();

    if options.import_profile.is_replay_only() {
        replay_outputs_or_mark_behind(
            store,
            &live_paths,
            &known_routes,
            &configured_source_root,
            &context,
            sink.as_deref(),
        );
        return Ok(ProviderImportSummary::default());
    }

    if live_paths.is_empty() && known_routes.is_empty() {
        return Err(openhands_missing_event_files(path));
    }

    let committed_store = Store::open_read_only(store.path())?;
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let mut summary = ProviderImportSummary::default();
        let mut changed_groups = 0_usize;
        let mut stopped = false;
        for event_path in &live_paths {
            if stopped {
                break;
            }
            let source = OpenHandsObservedFile::open(event_path)?;
            loop {
                let page = prepare_core_page(store, &source, &context, &options)?;
                let Some(page) = page else {
                    record_unchanged_source(store, &source, &context, &mut summary)?;
                    break;
                };
                let terminal = page.terminal();
                let page_summary = publish_core_page(
                    store,
                    &committed_store,
                    &bulk_guard,
                    &configured_source_root,
                    &context,
                    &options,
                    &source,
                    page,
                )?;
                if page_summary.work_result() == ProviderImportWorkResult::Changed {
                    changed_groups = changed_groups.saturating_add(1);
                }
                summary.merge_from(page_summary);
                if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup
                    && changed_groups != 0
                {
                    stopped = true;
                    break;
                }
                if terminal {
                    break;
                }
            }
        }
        if stopped {
            summary.work_remaining = true;
            return Ok(summary);
        }
        summary.merge_from(retire_missing_routes(
            store,
            &bulk_guard,
            &context,
            &known_routes,
            &live_paths,
            if inventory.root_missing {
                ProviderSourceRouteRetirementReason::RootMissing
            } else {
                ProviderSourceRouteRetirementReason::SourceMissing
            },
        )?);
        Ok(summary)
    })();
    let finish = store
        .finish_event_search_bulk_mode(&bulk_guard)
        .map_err(CaptureError::from);
    let summary = match (operation, finish) {
        (Ok(summary), Ok(())) => summary,
        (_, Err(error)) => return Err(error),
        (Err(error), Ok(())) => return Err(error),
    };
    if !summary.work_remaining {
        replay_outputs_or_mark_behind(
            store,
            &live_paths,
            &known_routes,
            &configured_source_root,
            &context,
            sink.as_deref(),
        );
    }
    Ok(summary)
}

fn load_stored_core_cursor(
    store: &Store,
    source: &OpenHandsObservedFile,
    machine_id: &str,
) -> Result<StoredCoreCursor> {
    let Some(stored) = store.get_sync_cursor(None, machine_id, &source.cursor_stream)? else {
        return Ok(StoredCoreCursor::Fresh);
    };
    if let Ok(committed) = decode_native_path_committed_cursor(&stored.cursor) {
        let cursor: OpenHandsNativeCursor = serde_json::from_str(committed.provider_cursor())
            .map_err(|_| {
                CaptureError::InvalidPayload(
                    "OpenHands NativePath cursor payload is malformed".to_owned(),
                )
            })?;
        return Ok(StoredCoreCursor::Native { stored, cursor });
    }
    match CertifiedProviderCursor::decode_if_certified(&stored.cursor)? {
        Some(_) => Ok(StoredCoreCursor::Migrated { stored }),
        None => Err(CaptureError::InvalidPayload(
            "OpenHands cursor is neither NativePath nor a released migration cursor".to_owned(),
        )),
    }
}

fn prepare_core_page(
    store: &Store,
    source: &OpenHandsObservedFile,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
) -> Result<Option<PreparedCorePage>> {
    let source_revision = source.source_revision(options.inventory_observation_token.as_deref());
    let stored = load_stored_core_cursor(store, source, &context.machine_id)?;
    let expected_cursor = stored.expected_encoded();
    let (mut cursor, source_change) = match &stored {
        StoredCoreCursor::Fresh => (
            OpenHandsNativeCursor::for_source(source, source_revision.clone(), 0),
            OpenHandsSourceChange::Fresh,
        ),
        StoredCoreCursor::Migrated { .. } => {
            let mut cursor = OpenHandsNativeCursor::for_source(source, source_revision.clone(), 0);
            cursor.legacy_source_layout = legacy_source_layout_required(store, source)?;
            (cursor, OpenHandsSourceChange::Migrated)
        }
        StoredCoreCursor::Native { cursor, .. } => {
            if !cursor.route_supported_for(source) {
                return Err(CaptureError::InvalidPayload(
                    "OpenHands NativePath cursor route or revision is inconsistent".to_owned(),
                ));
            }
            if cursor.deleted {
                let generation =
                    cursor
                        .generation
                        .checked_add(1)
                        .ok_or(CaptureError::SystemInvariant(
                            "OpenHands NativePath generation exhausted",
                        ))?;
                let mut next =
                    OpenHandsNativeCursor::for_source(source, source_revision.clone(), generation);
                next.locator_identity = reactivated_locator_identity(source, generation);
                next.legacy_source_layout = cursor.legacy_source_layout;
                (next, OpenHandsSourceChange::Replacement)
            } else if cursor.source_revision == source_revision {
                if cursor.terminal {
                    return Ok(None);
                }
                (cursor.clone(), OpenHandsSourceChange::Unchanged)
            } else {
                let generation =
                    cursor
                        .generation
                        .checked_add(1)
                        .ok_or(CaptureError::SystemInvariant(
                            "OpenHands NativePath generation exhausted",
                        ))?;
                let source_change = classify_source_change(cursor, source);
                let mut next =
                    OpenHandsNativeCursor::for_source(source, source_revision.clone(), generation);
                next.legacy_source_layout = cursor.legacy_source_layout;
                (next, source_change)
            }
        }
    };

    let mut rejection = None;
    let mut event = None;
    let mut touches = Vec::new();
    if source_change == OpenHandsSourceChange::Migrated {
        cursor.terminal = false;
        return finish_prepared_page(
            source_revision,
            expected_cursor,
            cursor,
            event,
            touches,
            rejection,
            source_change,
        )
        .map(Some);
    }
    let raw_bytes = match source.raw_bytes.as_deref() {
        Some(bytes) => bytes,
        None => {
            rejection = Some(ProviderImportFailure {
                line: openhands_line_number(&source.canonical_path),
                error: format!(
                    "provider record exceeds the {MAX_PROVIDER_JSONL_LINE_BYTES} byte limit (observed {} bytes)",
                    source.observation.length
                ),
            });
            cursor.rejected_records = cursor.rejected_records.saturating_add(1);
            cursor.terminal = true;
            cursor.next_touch = 0;
            return finish_prepared_page(
                source_revision,
                expected_cursor,
                cursor,
                event,
                touches,
                rejection,
                source_change,
            )
            .map(Some);
        }
    };
    let decoded = match decode_openhands_event(&source.canonical_path, raw_bytes) {
        Ok(decoded) => decoded,
        Err(error) => {
            rejection = Some(ProviderImportFailure {
                line: openhands_line_number(&source.canonical_path),
                error: bounded_failure(error.to_string()),
            });
            cursor.rejected_records = cursor.rejected_records.saturating_add(1);
            cursor.terminal = true;
            cursor.next_touch = 0;
            return finish_prepared_page(
                source_revision,
                expected_cursor,
                cursor,
                event,
                touches,
                rejection,
                source_change,
            )
            .map(Some);
        }
    };

    if cursor.next_touch == 0 && !cursor.accepted_event {
        let retained = retained_core_event(source, &decoded, raw_bytes)?;
        if retained
            .as_ref()
            .map(|event| serde_json::to_vec(event))
            .transpose()?
            .is_some_and(|bytes| bytes.len() > OPENHANDS_NATIVE_PAGE_MAX_BYTES)
        {
            rejection = Some(ProviderImportFailure {
                line: openhands_line_number(&source.canonical_path),
                error: "OpenHands normalized event exceeds the bounded NativePath Core page"
                    .to_owned(),
            });
            cursor.rejected_records = cursor.rejected_records.saturating_add(1);
            cursor.terminal = true;
        } else {
            event = retained;
            cursor.accepted_event = true;
        }
    }

    if !cursor.terminal {
        let touch_page = collect_touch_page(
            source,
            &decoded,
            usize::try_from(cursor.next_touch).map_err(|_| {
                CaptureError::SystemInvariant(
                    "OpenHands NativePath touch frontier exceeds platform limits",
                )
            })?,
            context,
        )?;
        touches = touch_page.touches;
        cursor.next_touch = cursor
            .next_touch
            .checked_add(u64::try_from(touches.len()).map_err(|_| {
                CaptureError::SystemInvariant("OpenHands touch page count exceeds u64")
            })?)
            .ok_or(CaptureError::SystemInvariant(
                "OpenHands touch frontier overflowed",
            ))?;
        cursor.accepted_file_touches = cursor.next_touch;
        cursor.terminal = !touch_page.has_more;
        if touch_page.limit_exceeded {
            rejection = Some(ProviderImportFailure {
                line: openhands_line_number(&source.canonical_path),
                error: PROVIDER_FILE_TOUCH_LIMIT_REJECTION.to_owned(),
            });
            cursor.rejected_records = cursor.rejected_records.saturating_add(1);
            cursor.terminal = true;
        }
    }

    finish_prepared_page(
        source_revision,
        expected_cursor,
        cursor,
        event,
        touches,
        rejection,
        source_change,
    )
    .map(Some)
}

fn finish_prepared_page(
    source_revision: String,
    expected_cursor: Option<String>,
    next_cursor: OpenHandsNativeCursor,
    event: Option<OpenHandsEventFact>,
    touches: Vec<(usize, OpenHandsTouchFact)>,
    rejection: Option<ProviderImportFailure>,
    source_change: OpenHandsSourceChange,
) -> Result<PreparedCorePage> {
    let conservative_serialized_bytes = 4 * 1024
        + event
            .as_ref()
            .map(serde_json::to_vec)
            .transpose()?
            .map_or(0, |bytes| bytes.len())
        + serde_json::to_vec(&touches)?.len()
        + rejection
            .as_ref()
            .map_or(0, |failure| failure.error.len().saturating_add(64));
    if conservative_serialized_bytes > OPENHANDS_NATIVE_PAGE_MAX_BYTES {
        return Err(CaptureError::InvalidPayload(
            "OpenHands NativePath Core page exceeds its retained-byte bound".to_owned(),
        ));
    }
    Ok(PreparedCorePage {
        source_revision,
        expected_cursor,
        next_cursor,
        event,
        touches,
        rejection,
        conservative_serialized_bytes,
        source_change,
    })
}

fn classify_source_change(
    previous: &OpenHandsNativeCursor,
    source: &OpenHandsObservedFile,
) -> OpenHandsSourceChange {
    let Some(previous_observation) = previous.observation.as_ref() else {
        return OpenHandsSourceChange::Rewrite;
    };
    if previous_observation.physical_identity() != source.observation.physical_identity() {
        return OpenHandsSourceChange::Replacement;
    }
    if source.observation.length < previous_observation.length {
        return OpenHandsSourceChange::Truncation;
    }
    if source.observation.length > previous_observation.length
        && previous
            .content_sha256
            .is_some_and(|hash| source.current_prefix_matches(previous_observation.length, hash))
    {
        return OpenHandsSourceChange::Append;
    }
    OpenHandsSourceChange::Rewrite
}

struct TouchPage {
    touches: Vec<(usize, OpenHandsTouchFact)>,
    has_more: bool,
    limit_exceeded: bool,
}

fn collect_touch_page(
    source: &OpenHandsObservedFile,
    decoded: &OpenHandsDecodedEvent,
    skip: usize,
    context: &ProviderAdapterContext,
) -> Result<TouchPage> {
    #[derive(Debug)]
    enum Stop {
        PageFull,
    }

    let provider_event_index = event_identity_index(source, decoded.event_id());
    let include_structured_touches = matches!(
        decoded.event_type(),
        EventType::ToolCall | EventType::FileTouched
    );
    let mut touches = Vec::new();
    let source_root = context.source_root_display();
    let line_number = openhands_line_number(&source.canonical_path);
    let outcome = visit_provider_file_touch_drafts_with_limit(
        decoded.value(),
        include_structured_touches,
        MAX_PROVIDER_FILE_TOUCHES_PER_EVENT,
        |(touch_ordinal, draft)| {
            let touch_ordinal = usize::try_from(touch_ordinal).unwrap_or(usize::MAX);
            if touch_ordinal < skip {
                return Ok(());
            }
            if touches.len() == OPENHANDS_NATIVE_PAGE_TOUCHES {
                return Err(Stop::PageFull);
            }
            let provider_touch_index = if provider_event_index > MAX_PACKED_PROVIDER_EVENT_INDEX {
                u64::try_from(touch_ordinal).unwrap_or(u64::MAX)
            } else {
                (provider_event_index << 16) | u64::try_from(touch_ordinal).unwrap_or(u64::MAX)
            };
            touches.push((
                line_number,
                OpenHandsTouchFact {
                    provider_session_id: source.session_id.clone(),
                    provider_touch_index,
                    provider_event_index: Some(provider_event_index),
                    raw_source_path: source.canonical_path_text.clone(),
                    source_root: source_root.clone(),
                    path: draft.path,
                    change_kind: draft.change_kind,
                    old_path: draft.old_path,
                    line_count_delta: None,
                    confidence: draft.confidence,
                    occurred_at: decoded.timestamp(),
                    metadata: draft.metadata,
                },
            ));
            return Ok(());
        },
    );
    match outcome {
        Ok(outcome) => Ok(TouchPage {
            touches,
            has_more: false,
            limit_exceeded: outcome.limit_exceeded(),
        }),
        Err(Stop::PageFull) => Ok(TouchPage {
            touches,
            has_more: true,
            limit_exceeded: false,
        }),
    }
}

fn retained_core_event(
    source: &OpenHandsObservedFile,
    decoded: &OpenHandsDecodedEvent,
    raw_bytes: &[u8],
) -> Result<Option<OpenHandsEventFact>> {
    let is_output = matches!(
        decoded.event_type(),
        EventType::ToolOutput | EventType::CommandOutput
    );
    let outcome = openhands_output_outcome(decoded);
    let retained_failure = is_output
        && matches!(
            outcome.outcome,
            OutputOutcome::Failure | OutputOutcome::Timeout
        )
        && (decoded.event_type() != EventType::CommandOutput
            || openhands_output_command_context(decoded).is_some());
    if is_output && !retained_failure {
        return Ok(None);
    }
    let mut event = openhands_event_fact(source, decoded);
    if retained_failure {
        apply_failure_diagnostic(
            &mut event,
            super::openhands_result_content(decoded).as_deref(),
            &outcome,
            openhands_output_call_id(decoded.value()).as_deref(),
            openhands_output_command_context(decoded).as_ref(),
        )?;
    } else {
        attach_openhands_complete_content_locator(
            &mut event,
            0,
            0,
            decoded.event_id(),
            raw_bytes,
            decoded.text(),
        )?;
    }
    Ok(Some(event))
}

fn openhands_event_fact(
    source: &OpenHandsObservedFile,
    decoded: &OpenHandsDecodedEvent,
) -> OpenHandsEventFact {
    let identity = event_identity_index(source, decoded.event_id());
    let legacy_source_event_candidate = openhands_legacy_filename_index_candidate(
        &source.canonical_path,
    )
    .map(|provider_event_index| {
        json!({
            "raw_source_path": source.conversation_dir.display().to_string(),
            "provider_event_index": provider_event_index,
        })
    });
    let event_type = decoded.event_type();
    let text = decoded.text();
    let body = decoded.value();
    let retained_text = provider_policy_event_text(event_type, text, body);
    let retained_body = provider_policy_body(event_type, body);
    OpenHandsEventFact {
        provider_event_index: identity,
        provider_event_hash: decoded.event_id().to_owned(),
        cursor: format!("{}:{}", source.canonical_path.display(), decoded.event_id()),
        event_type,
        role: decoded.role(),
        occurred_at: decoded.timestamp(),
        payload: json!({
            "text": retained_text.text,
            "text_retention": retained_text.retention.as_json(),
            "result_evidence": provider_result_identifier_evidence(event_type, text, body),
            "result_outcome": provider_result_outcome_evidence(event_type, body),
            "source_format": OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
            "body": provider_capped_json(&retained_body, PROVIDER_MAX_PREVIEW_CHARS),
        }),
        metadata: json!({
            "source": OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
            "source_format": OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
            "event_id": decoded.event_id(),
            "entry_type": decoded.entry_type(),
            "event_path": source.canonical_path_text,
            "conversation_id": source.session_id,
            "provider_event_identity_index": identity,
            "event_file_identity": format!("{identity:016x}"),
            "legacy_source_event_candidate_v1": legacy_source_event_candidate,
            "tool_name": decoded.value().get("tool_name").and_then(Value::as_str),
            "tool_call_id": decoded.value().get("tool_call_id").and_then(Value::as_str),
            "action_id": decoded.value().get("action_id").and_then(Value::as_str),
        }),
    }
}

fn attach_openhands_complete_content_locator(
    event: &mut OpenHandsEventFact,
    source_record_ordinal: u64,
    source_record_subrecord_index: u32,
    native_record_id: &str,
    record_bytes: &[u8],
    complete_text: &str,
) -> Result<()> {
    if event.event_type != EventType::Message
        || complete_text.chars().count() <= PROVIDER_MAX_TEXT_CHARS
    {
        return Ok(());
    }
    if native_record_id.is_empty()
        || native_record_id.len() > 1_024
        || native_record_id.chars().any(char::is_control)
    {
        return Err(CaptureError::InvalidPayload(
            "OpenHands complete-content native record identity is invalid".to_owned(),
        ));
    }
    let locator_value = openhands_structured_locator(
        source_record_ordinal,
        source_record_subrecord_index,
        native_record_id,
    )?;
    let content_ref = ContentRef::from_bytes(complete_text.as_bytes()).ok_or(
        CaptureError::SystemInvariant("OpenHands complete content exceeds ContentRef bounds"),
    )?;
    let profile = verified_content_profile(
        CaptureProvider::OpenHands,
        OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
        CompleteContentSourceFamily::Structured,
        VerifiedContentRole::MessageBody,
    )
    .ok_or(CaptureError::SystemInvariant(
        "OpenHands complete-content profile is not registered",
    ))?;
    let locator = VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        profile,
        content_ref,
        CompleteContentSourceFamily::Structured,
        STRUCTURED_COMPLETE_CONTENT_LOCATOR_KIND,
        &locator_value,
        native_record_id,
        CompleteContentBodyDigest::from_bytes(record_bytes),
    )
    .ok_or(CaptureError::SystemInvariant(
        "OpenHands complete-content locator exceeds its typed bounds",
    ))?;
    attach_verified_content_locator(&mut event.metadata, locator).ok_or(
        CaptureError::SystemInvariant("OpenHands complete-content locator metadata is malformed"),
    )?;
    Ok(())
}

fn openhands_structured_locator(
    source_record_ordinal: u64,
    source_record_subrecord_index: u32,
    native_record_id: &str,
) -> Result<Vec<u8>> {
    let provider = CaptureProvider::OpenHands.as_str().as_bytes();
    let provider_len = u8::try_from(provider.len())
        .map_err(|_| CaptureError::SystemInvariant("OpenHands provider identity is too long"))?;
    let native_id = native_record_id.as_bytes();
    let native_len = u16::try_from(native_id.len()).map_err(|_| {
        CaptureError::InvalidPayload(
            "OpenHands complete-content native record identity is too long".to_owned(),
        )
    })?;
    let mut value = Vec::with_capacity(4 + 1 + provider.len() + 8 + 4 + 2 + native_id.len());
    value.extend_from_slice(b"SC\0\x01");
    value.push(provider_len);
    value.extend_from_slice(provider);
    value.extend_from_slice(&source_record_ordinal.to_be_bytes());
    value.extend_from_slice(&source_record_subrecord_index.to_be_bytes());
    value.extend_from_slice(&native_len.to_be_bytes());
    value.extend_from_slice(native_id);
    Ok(value)
}

fn event_identity_index(source: &OpenHandsObservedFile, event_id: &str) -> u64 {
    let identity = serde_json::to_string(&(
        "openhands-native-event-v1",
        source.path_identity.as_str(),
        event_id,
    ))
    .expect("OpenHands event identity should serialize");
    crate::fnv1a64(identity.as_bytes())
}

fn event_file_identity_index(source: &OpenHandsObservedFile) -> u64 {
    crate::fnv1a64(source.path_identity.as_bytes())
}

fn reactivated_locator_identity(source: &OpenHandsObservedFile, generation: u64) -> String {
    serde_json::to_string(&(
        "openhands-native-route-incarnation-v1",
        source.path_identity.as_str(),
        generation,
    ))
    .expect("OpenHands reactivated locator identity should serialize")
}

fn legacy_source_layout_required(store: &Store, source: &OpenHandsObservedFile) -> Result<bool> {
    let source_id = provider_scoped_source_uuid(
        CaptureProvider::OpenHands,
        &source.session_id,
        OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
        Some(&source.canonical_path_text),
    );
    match store.get_capture_source(source_id) {
        Ok(_) => Ok(false),
        Err(StoreError::NotFound(_)) => Ok(true),
        Err(error) => Err(error.into()),
    }
}

#[allow(clippy::too_many_arguments)]
fn publish_core_page(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    configured_source_root: &Path,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    source: &OpenHandsObservedFile,
    page: PreparedCorePage,
) -> Result<ProviderImportSummary> {
    if !source.revalidate()? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let next = provider_sync_cursor(
        &context.machine_id,
        source.cursor_stream.clone(),
        page.next_cursor.encode()?,
        context.imported_at,
    );
    let transition = NativePathCursorTransition::new(page.expected_cursor.clone(), next);
    let publication_id = publication_id(source, &page, &transition);
    let accounting = NativePathGroupAccounting::new(1, 1, page.conservative_serialized_bytes)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
        NativePathCursorSetClassification::AllNextSameGroup { .. } => {
            group.commit()?;
            let mut summary = ProviderImportSummary::default();
            summary.set_work_result(ProviderImportWorkResult::NoOp);
            return Ok(summary);
        }
        NativePathCursorSetClassification::AllExpected => {}
    }

    let mut summary = ProviderImportSummary::default();
    let resolved = resolve_source(
        committed_store,
        &mut group,
        configured_source_root,
        context,
        options,
        source,
        &page.source_revision,
        &page.next_cursor.locator_identity,
        page.next_cursor.legacy_source_layout,
        &mut summary,
    )?;
    let published_event = page
        .event
        .as_ref()
        .map(|event| {
            publish_event(
                committed_store,
                &mut group,
                context,
                options,
                source,
                &resolved,
                event,
                &mut summary,
            )
        })
        .transpose()?;
    for (touch_ordinal, touch) in &page.touches {
        if resolved.legacy_source_layout && published_event.is_some_and(|(_, inserted)| !inserted) {
            continue;
        }
        publish_touch(
            committed_store,
            &mut group,
            options,
            source,
            &resolved,
            published_event.map(|(event_id, _)| event_id),
            *touch_ordinal,
            touch,
        )?;
        summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
    }
    if let Some(rejection) = page.rejection {
        summary.record_failure(rejection);
    }
    if !source.revalidate()? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

struct ResolvedSource {
    source_id: Uuid,
    session: Session,
    legacy_source_layout: bool,
}

#[allow(clippy::too_many_arguments)]
fn resolve_source(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    configured_source_root: &Path,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    source: &OpenHandsObservedFile,
    source_revision: &str,
    locator_identity: &str,
    legacy_source_layout: bool,
    summary: &mut ProviderImportSummary,
) -> Result<ResolvedSource> {
    let raw_source_path = source.canonical_path_text.clone();
    let source_root = configured_source_root.display().to_string();
    let proposed_source_identity = provider_source_identity(
        CaptureProvider::OpenHands,
        OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
        Some(&source_root),
        Some(&raw_source_path),
        None,
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "OpenHands NativePath source has no canonical identity",
    ))?;
    let resolution =
        group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::OpenHands,
            source_format: OPENHANDS_FILE_EVENTS_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity: locator_identity.to_owned(),
            cursor_stream: source.cursor_stream.clone(),
            proposed_source_identity,
            raw_source_path: Some(raw_source_path.clone()),
            source_revision: source_revision.to_owned(),
            observed_at_ms: context.imported_at.timestamp_millis(),
        })?;
    let default_source_id = provider_scoped_source_uuid(
        CaptureProvider::OpenHands,
        &source.session_id,
        OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
        Some(&raw_source_path),
    );
    let (source_id, legacy_source_layout) = if resolution.relocated {
        (
            committed_store
                .capture_source_by_canonical_identity_session(
                    CaptureProvider::OpenHands,
                    OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
                    &context.machine_id,
                    &resolution.canonical_source_identity,
                    &source.session_id,
                )?
                .map_or(default_source_id, |source| source.id),
            false,
        )
    } else if legacy_source_layout {
        (
            provider_scoped_source_uuid(
                CaptureProvider::OpenHands,
                &source.session_id,
                OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
                Some(&source.conversation_dir.display().to_string()),
            ),
            true,
        )
    } else {
        (default_source_id, false)
    };
    let started_at = source_event_timestamp(source).unwrap_or(context.imported_at);
    group.upsert_capture_source(&CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::OpenHands,
            machine_id: context.machine_id.clone(),
            process_id: None,
            cwd: None,
            raw_source_path: Some(raw_source_path.clone()),
            source_format: Some(OPENHANDS_FILE_EVENTS_SOURCE_FORMAT.to_owned()),
            source_root: Some(source_root.clone()),
            source_identity: Some(resolution.canonical_source_identity.clone()),
            external_session_id: Some(source.session_id.clone()),
        },
        started_at,
        ended_at: None,
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": source.session_id,
                "source_format": OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "source_identity": resolution.canonical_source_identity,
                "source_root": source_root,
                "source_revision": source_revision,
                "source_identity_key": provider_scoped_source_identity_key(
                    CaptureProvider::OpenHands,
                    &source.session_id,
                    OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
                    Some(&raw_source_path),
                ),
                "source_metadata": {
                    "adapter": OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
                    "storage": "filesystem_event_service",
                    "conversation_dir": source.conversation_dir,
                    "event_path": source.canonical_path,
                    "event_file_identity": format!("{:016x}", event_file_identity_index(source)),
                    "native_locator_identity": locator_identity,
                    "nativepath_publication": OPENHANDS_NATIVE_CURSOR_VERSION,
                },
            }),
        ),
    })?;
    group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;
    let session_id = provider_import_session_uuid(
        committed_store,
        CaptureProvider::OpenHands,
        &source.session_id,
        source_id,
        Some(&resolution.canonical_source_identity),
    )?;
    let proposed_session = Session {
        id: session_id,
        history_record_id: options.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::OpenHands,
        external_session_id: Some(source.session_id.clone()),
        external_agent_id: source.user_id.clone(),
        agent_type: AgentType::Primary,
        role_hint: Some("primary".to_owned()),
        is_primary: true,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at,
        ended_at: Some(started_at),
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": source.session_id,
                "source_format": OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "metadata": {
                    "source_format": OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
                    "provider": "openhands",
                    "conversation_id": source.session_id,
                    "user_id": source.user_id,
                    "nativepath_publication": OPENHANDS_NATIVE_CURSOR_VERSION,
                },
            }),
        ),
    };
    let (session, existed) = match committed_store.get_session(session_id) {
        Ok(existing) => (existing, true),
        Err(StoreError::NotFound(_)) => (proposed_session, false),
        Err(error) => return Err(error.into()),
    };
    group.upsert_session(&session)?;
    if existed {
        summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    } else {
        summary.imported_sessions = summary.imported_sessions.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    }
    Ok(ResolvedSource {
        source_id,
        session,
        legacy_source_layout,
    })
}

fn source_event_timestamp(source: &OpenHandsObservedFile) -> Option<DateTime<Utc>> {
    source
        .raw_bytes
        .as_deref()
        .and_then(|bytes| decode_openhands_event(&source.canonical_path, bytes).ok())
        .map(|event| event.timestamp())
}

#[allow(clippy::too_many_arguments)]
fn publish_event(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    source: &OpenHandsObservedFile,
    resolved: &ResolvedSource,
    event: &OpenHandsEventFact,
    summary: &mut ProviderImportSummary,
) -> Result<(Uuid, bool)> {
    let provider_event_index = event.provider_event_index;
    let event_hash = event.provider_event_hash.as_str();
    let exact_legacy_source = openhands_legacy_filename_index_candidate(&source.canonical_path)
        .map(|provider_event_index| ExactLegacySourceEventCandidate {
            source_id: provider_scoped_source_uuid(
                CaptureProvider::OpenHands,
                &source.session_id,
                OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
                Some(&source.conversation_dir.display().to_string()),
            ),
            provider_event_index,
        });
    let identity = provider_event_import_identity_with_exact_legacy_source(
        committed_store,
        CaptureProvider::OpenHands,
        &source.session_id,
        resolved.source_id,
        provider_event_index,
        provider_event_index,
        event_hash,
        exact_legacy_source,
        openhands_legacy_filename_index_candidate(&source.canonical_path),
        resolved.session.id
            == provider_session_uuid(CaptureProvider::OpenHands, &source.session_id),
    )?;
    let dedupe_key =
        Store::provider_event_dedupe_key_with_payload_hash(&identity.dedupe_key, event_hash)
            .unwrap_or(identity.dedupe_key);
    let mut provider_metadata = event.metadata.clone();
    let verified_locators = provider_metadata
        .as_object_mut()
        .and_then(|metadata| metadata.remove(VERIFIED_CONTENT_LOCATORS_METADATA_KEY));
    let mut sync_metadata = json!({
        "provider_session_id": source.session_id,
        "provider_event_index": provider_event_index,
        "provider_event_hash": event_hash,
        "provider_event_hash_authority": "provider_supplied",
        "cursor": event.cursor,
        "source_format": OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
        "source_trust": "provider_native",
        "fixture_line": openhands_line_number(&source.canonical_path),
        "imported_at": context.imported_at,
        "source_record_ordinal": 0,
        "source_record_subrecord_index": 0,
        "metadata": provider_metadata,
    });
    if let (Some(metadata), Some(locators)) = (sync_metadata.as_object_mut(), verified_locators) {
        metadata.insert(VERIFIED_CONTENT_LOCATORS_METADATA_KEY.to_owned(), locators);
    }
    let normalized = Event {
        id: identity.id,
        seq: identity.seq,
        history_record_id: options.history_record_id,
        session_id: Some(resolved.session.id),
        run_id: None,
        event_type: event.event_type,
        role: Some(event.role),
        occurred_at: event.occurred_at,
        capture_source_id: Some(resolved.source_id),
        payload: json!({
            "provider": CaptureProvider::OpenHands.as_str(),
            "provider_session_id": source.session_id,
            "provider_event_index": provider_event_index,
            "provider_event_hash": event_hash,
            "cursor": event.cursor,
            "artifacts": [],
            "body": compact_provider_result_payload(event.event_type, &event.payload),
        }),
        payload_blob_id: None,
        dedupe_key: Some(dedupe_key),
        sync: provider_sync_metadata(Fidelity::Imported, sync_metadata),
    };
    let inserted = group
        .reconcile_provider_event(&normalized, ProviderEventHashAuthority::ProviderSupplied)?;
    if inserted {
        summary.imported_events = summary.imported_events.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    } else {
        summary.skipped_events = summary.skipped_events.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    }
    summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
    Ok((normalized.id, inserted))
}

fn publish_touch(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    options: &ProviderImportOptions,
    source: &OpenHandsObservedFile,
    resolved: &ResolvedSource,
    event_id: Option<Uuid>,
    touch_ordinal: usize,
    touch: &OpenHandsTouchFact,
) -> Result<()> {
    let (provider_event_index, provider_touch_index) = if resolved.legacy_source_layout {
        let legacy_event_index =
            openhands_legacy_filename_index_candidate(&source.canonical_path).unwrap_or(0);
        let touch_ordinal = u64::try_from(touch_ordinal).map_err(|_| {
            CaptureError::SystemInvariant("OpenHands legacy touch ordinal exceeds u64")
        })?;
        let provider_touch_index = legacy_event_index
            .checked_mul(u64::from(u16::MAX) + 1)
            .and_then(|base| base.checked_add(touch_ordinal))
            .ok_or(CaptureError::SystemInvariant(
                "OpenHands legacy touch identity overflowed",
            ))?;
        (Some(legacy_event_index), provider_touch_index)
    } else {
        (touch.provider_event_index, touch.provider_touch_index)
    };
    let id = provider_file_touch_import_id(
        committed_store,
        CaptureProvider::OpenHands,
        &source.session_id,
        resolved.source_id,
        provider_event_index,
        provider_touch_index,
        resolved.session.id
            == provider_session_uuid(CaptureProvider::OpenHands, &source.session_id),
    )?;
    group.upsert_file_touched(&FileTouched {
        id,
        history_record_id: options.history_record_id,
        run_id: None,
        event_id,
        vcs_workspace_id: None,
        path: touch.path.clone(),
        change_kind: touch.change_kind,
        old_path: touch.old_path.clone(),
        line_count_delta: touch.line_count_delta,
        confidence: touch.confidence,
        timestamps: timestamps(touch.occurred_at),
        source_id: Some(resolved.source_id),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider": CaptureProvider::OpenHands.as_str(),
                "provider_session_id": source.session_id,
                "provider_touch_index": provider_touch_index,
                "provider_event_index": provider_event_index,
                "source_format": OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
                "session_id": resolved.session.id,
                "metadata": touch.metadata,
            }),
        ),
    })?;
    Ok(())
}

fn record_unchanged_source(
    store: &Store,
    source: &OpenHandsObservedFile,
    context: &ProviderAdapterContext,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let stored = load_stored_core_cursor(store, source, &context.machine_id)?;
    let StoredCoreCursor::Native { cursor, .. } = stored else {
        return Err(CaptureError::SystemInvariant(
            "OpenHands unchanged source lost its NativePath cursor",
        ));
    };
    let sessions = usize::from(cursor.accepted_event || cursor.accepted_file_touches != 0);
    let events = usize::from(cursor.accepted_event);
    let touches = usize::try_from(cursor.accepted_file_touches).unwrap_or(usize::MAX);
    summary.skipped_sessions = summary.skipped_sessions.saturating_add(sessions);
    summary.skipped_events = summary.skipped_events.saturating_add(events);
    summary.skipped = summary
        .skipped
        .saturating_add(sessions)
        .saturating_add(events)
        .saturating_add(touches);
    summary.accepted_content_records = summary
        .accepted_content_records
        .saturating_add(events)
        .saturating_add(touches);
    if cursor.rejected_records != 0 {
        summary.failed = summary
            .failed
            .saturating_add(usize::try_from(cursor.rejected_records).unwrap_or(usize::MAX));
    }
    Ok(())
}

#[derive(Clone)]
struct KnownOpenHandsRoute {
    path: PathBuf,
    path_identity: String,
    locator_identity: String,
    canonical_source_identity: String,
    source_revision: String,
    current_cursor: SyncCursor,
    checkpoint: Option<OpenHandsNativeCursor>,
}

fn known_openhands_routes(
    store: &Store,
    machine_id: &str,
    configured_source_root: &Path,
) -> Result<Vec<KnownOpenHandsRoute>> {
    let source_root = configured_source_root.display().to_string();
    let mut routes = BTreeMap::<String, KnownOpenHandsRoute>::new();
    for source in store.list_capture_sources()? {
        if source.descriptor.provider != CaptureProvider::OpenHands
            || source.descriptor.machine_id != machine_id
            || source.descriptor.source_format.as_deref()
                != Some(OPENHANDS_FILE_EVENTS_SOURCE_FORMAT)
            || source.descriptor.source_root.as_deref() != Some(source_root.as_str())
        {
            continue;
        }
        let (Some(raw_source_path), Some(canonical_source_identity), Some(source_revision)) = (
            source.descriptor.raw_source_path.as_deref(),
            source.descriptor.source_identity.as_deref(),
            source
                .sync
                .metadata
                .get("source_revision")
                .and_then(Value::as_str),
        ) else {
            continue;
        };
        let path = PathBuf::from(raw_source_path);
        let locator_identity = source
            .sync
            .metadata
            .pointer("/source_metadata/native_locator_identity")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or(provider_path_identity(&path)?);
        let path_identity = provider_path_identity(&path)?;
        let stream = provider_source_cursor_stream_for_path(
            CaptureProvider::OpenHands,
            OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
            &path_identity,
        );
        let Some(current_cursor) = store.get_sync_cursor(None, machine_id, &stream)? else {
            continue;
        };
        let checkpoint = decode_native_path_committed_cursor(&current_cursor.cursor)
            .ok()
            .and_then(|committed| serde_json::from_str(committed.provider_cursor()).ok());
        let route = KnownOpenHandsRoute {
            path,
            path_identity,
            locator_identity: locator_identity.clone(),
            canonical_source_identity: canonical_source_identity.to_owned(),
            source_revision: source_revision.to_owned(),
            current_cursor,
            checkpoint,
        };
        if routes.insert(locator_identity, route).is_some() {
            return Err(CaptureError::SystemInvariant(
                "OpenHands persisted duplicate current routes for one event file",
            ));
        }
    }
    Ok(routes.into_values().collect())
}

fn retire_missing_routes(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    known_routes: &[KnownOpenHandsRoute],
    live_paths: &BTreeSet<PathBuf>,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<ProviderImportSummary> {
    let mut summary = ProviderImportSummary::default();
    for route in known_routes
        .iter()
        .filter(|route| !live_paths.contains(&route.path))
    {
        if retire_route(store, bulk_guard, context, route, reason)? {
            summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
            summary.set_work_result(ProviderImportWorkResult::Changed);
        }
    }
    Ok(summary)
}

fn retire_route(
    store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    route: &KnownOpenHandsRoute,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<bool> {
    if route
        .checkpoint
        .as_ref()
        .is_some_and(|checkpoint| checkpoint.deleted)
    {
        return Ok(false);
    }
    let generation = route.checkpoint.as_ref().map_or(0, |cursor| {
        if cursor.deleted {
            cursor.generation
        } else {
            cursor.generation.saturating_add(1)
        }
    });
    let route_sha256 = route.checkpoint.as_ref().map_or_else(
        || route_hash(&route.locator_identity),
        |cursor| cursor.route_sha256,
    );
    let tombstone = OpenHandsNativeCursor {
        version: OPENHANDS_NATIVE_CURSOR_VERSION,
        parser_revision: OPENHANDS_NATIVE_PARSER_REVISION,
        policy_revision: OPENHANDS_NATIVE_POLICY_REVISION,
        route_sha256,
        locator_identity: route.locator_identity.clone(),
        legacy_source_layout: route
            .checkpoint
            .as_ref()
            .is_some_and(|cursor| cursor.legacy_source_layout),
        source_revision: route.source_revision.clone(),
        observation: None,
        content_sha256: None,
        generation,
        next_touch: 0,
        accepted_event: false,
        accepted_file_touches: 0,
        rejected_records: 0,
        terminal: true,
        deleted: true,
    };
    let transition = NativePathCursorTransition::new(
        Some(route.current_cursor.cursor.clone()),
        provider_sync_cursor(
            &context.machine_id,
            route.current_cursor.stream.clone(),
            tombstone.encode()?,
            context.imported_at,
        ),
    );
    let retirement = ProviderSourceRouteRetirement {
        provider: CaptureProvider::OpenHands,
        source_format: OPENHANDS_FILE_EVENTS_SOURCE_FORMAT.to_owned(),
        machine_id: context.machine_id.clone(),
        locator_identity: route.locator_identity.clone(),
        cursor_stream: route.current_cursor.stream.clone(),
        expected_canonical_source_identity: route.canonical_source_identity.clone(),
        expected_source_revision: route.source_revision.clone(),
        retired_at_ms: context.imported_at.timestamp_millis(),
        reason,
    };
    let publication_id = retirement_publication_id(&retirement);
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store
        .begin_native_path_publication_group(admission, NativePathGroupAccounting::new(0, 1, 0)?)?;
    let changed =
        match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
            NativePathCursorSetClassification::AllExpected => {
                let disposition = group.retire_provider_source_route(&retirement)?;
                group.prepare_journal_checkpoint()?;
                group.publish_cursor_set()?;
                matches!(
                    disposition,
                    ProviderSourceRouteRetirementDisposition::Retired
                )
            }
            NativePathCursorSetClassification::AllNextSameGroup { .. } => false,
        };
    group.commit()?;
    Ok(changed)
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
                CaptureProvider::OpenHands.as_str(),
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

fn publication_id(
    source: &OpenHandsObservedFile,
    page: &PreparedCorePage,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(OPENHANDS_NATIVE_PUBLICATION_DOMAIN);
    digest.update(source.route_sha256);
    digest.update(page.source_revision.as_bytes());
    digest.update(page.next_cursor.generation.to_be_bytes());
    digest.update(page.next_cursor.next_touch.to_be_bytes());
    digest.update([u8::from(page.next_cursor.terminal)]);
    digest.update([source_change_code(page.source_change)]);
    digest.update(transition.next().cursor.as_bytes());
    format!("openhands-nativepath-v1:{}", hex(&digest.finalize()))
}

const fn source_change_code(change: OpenHandsSourceChange) -> u8 {
    match change {
        OpenHandsSourceChange::Fresh => 0,
        OpenHandsSourceChange::Unchanged => 1,
        OpenHandsSourceChange::Append => 2,
        OpenHandsSourceChange::Rewrite => 3,
        OpenHandsSourceChange::Truncation => 4,
        OpenHandsSourceChange::Replacement => 5,
        OpenHandsSourceChange::Migrated => 6,
    }
}

fn retirement_publication_id(retirement: &ProviderSourceRouteRetirement) -> String {
    let mut digest = Sha256::new();
    digest.update(OPENHANDS_NATIVE_RETIREMENT_DOMAIN);
    digest.update(retirement.provider.as_str().as_bytes());
    digest.update(retirement.source_format.as_bytes());
    digest.update(retirement.machine_id.as_bytes());
    digest.update(retirement.locator_identity.as_bytes());
    digest.update(retirement.cursor_stream.as_bytes());
    digest.update(retirement.expected_canonical_source_identity.as_bytes());
    digest.update(retirement.expected_source_revision.as_bytes());
    digest.update(format!("{:?}", retirement.reason).as_bytes());
    format!(
        "openhands-nativepath-retirement-v1:{}",
        hex(&digest.finalize())
    )
}

fn route_hash(locator_identity: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"ctx-openhands-nativepath-route-v1\0");
    digest.update((locator_identity.len() as u64).to_be_bytes());
    digest.update(locator_identity.as_bytes());
    digest.finalize().into()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenHandsOutputFrontier {
    version: u32,
    parser_revision: u32,
    policy_revision: u32,
    route_sha256: [u8; 32],
    content_sha256: Option<[u8; 32]>,
    terminal: bool,
    deleted: bool,
}

impl OpenHandsOutputFrontier {
    fn initial(route_sha256: [u8; 32]) -> Self {
        Self {
            version: OPENHANDS_OUTPUT_FRONTIER_VERSION,
            parser_revision: OPENHANDS_NATIVE_PARSER_REVISION,
            policy_revision: OPENHANDS_NATIVE_POLICY_REVISION,
            route_sha256,
            content_sha256: None,
            terminal: false,
            deleted: false,
        }
    }

    fn terminal(source: &OpenHandsObservedFile) -> Self {
        Self {
            version: OPENHANDS_OUTPUT_FRONTIER_VERSION,
            parser_revision: OPENHANDS_NATIVE_PARSER_REVISION,
            policy_revision: OPENHANDS_NATIVE_POLICY_REVISION,
            route_sha256: source.route_sha256,
            content_sha256: source.content_sha256,
            terminal: true,
            deleted: false,
        }
    }

    fn deleted(route_sha256: [u8; 32]) -> Self {
        Self {
            version: OPENHANDS_OUTPUT_FRONTIER_VERSION,
            parser_revision: OPENHANDS_NATIVE_PARSER_REVISION,
            policy_revision: OPENHANDS_NATIVE_POLICY_REVISION,
            route_sha256,
            content_sha256: None,
            terminal: true,
            deleted: true,
        }
    }

    fn safe(&self) -> Result<NativeSafeFrontier> {
        NativeSafeFrontier::new(OPENHANDS_OUTPUT_FRONTIER_VERSION, serde_json::to_vec(self)?)
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
    }
}

fn replay_outputs_or_mark_behind(
    store: &Store,
    live_paths: &BTreeSet<PathBuf>,
    known_routes: &[KnownOpenHandsRoute],
    source_root: &Path,
    context: &ProviderAdapterContext,
    sink: Option<&dyn ProOutputSink>,
) {
    let Some(sink) = sink else {
        return;
    };
    if let Err(error) = replay_outputs(store, live_paths, known_routes, source_root, context, sink)
    {
        sink.mark_behind(ProOutputSinkError::new(
            "openhands_nativepath_output_replay",
            error.to_string(),
        ));
    }
}

fn replay_outputs(
    store: &Store,
    live_paths: &BTreeSet<PathBuf>,
    known_routes: &[KnownOpenHandsRoute],
    source_root: &Path,
    context: &ProviderAdapterContext,
    sink: &dyn ProOutputSink,
) -> Result<()> {
    for path in live_paths {
        let source = OpenHandsObservedFile::open(path)?;
        if !core_source_is_committed(store, &source, context)? {
            sink.mark_behind(ProOutputSinkError::new(
                "openhands_core_not_committed",
                "OpenHands output replay requires the exact terminal NativePath Core source",
            ));
            continue;
        }
        replay_live_output(&source, source_root, context, sink)?;
    }
    for route in known_routes
        .iter()
        .filter(|route| !live_paths.contains(&route.path))
    {
        replay_deleted_output(route, source_root, sink)?;
    }
    Ok(())
}

fn core_source_is_committed(
    store: &Store,
    source: &OpenHandsObservedFile,
    context: &ProviderAdapterContext,
) -> Result<bool> {
    let Some(stored) = store.get_sync_cursor(None, &context.machine_id, &source.cursor_stream)?
    else {
        return Ok(false);
    };
    let Ok(committed) = decode_native_path_committed_cursor(&stored.cursor) else {
        return Ok(false);
    };
    let Ok(cursor) = serde_json::from_str::<OpenHandsNativeCursor>(committed.provider_cursor())
    else {
        return Ok(false);
    };
    Ok(cursor.supported_for(source)
        && cursor.terminal
        && cursor.content_sha256 == source.content_sha256)
}

fn replay_live_output(
    source: &OpenHandsObservedFile,
    source_root: &Path,
    context: &ProviderAdapterContext,
    sink: &dyn ProOutputSink,
) -> Result<()> {
    let output_source = OutputSourceIdentity {
        provider: CaptureProvider::OpenHands.as_str().to_owned(),
        namespace_id: source_root.display().to_string(),
        source_id: source.path_identity.clone(),
    };
    let progress = match sink.observe_source(&output_source) {
        Ok(progress) => progress,
        Err(error) => {
            sink.mark_behind(error);
            return Ok(());
        }
    };
    let next = OpenHandsOutputFrontier::terminal(source);
    let prior = progress
        .as_ref()
        .and_then(|progress| progress.cursor.as_ref())
        .filter(|cursor| cursor.version == OPENHANDS_OUTPUT_FRONTIER_VERSION)
        .and_then(|cursor| serde_json::from_slice::<OpenHandsOutputFrontier>(&cursor.payload).ok());
    let exact = progress.as_ref().is_some_and(|progress| {
        progress.parser_revision == OPENHANDS_OUTPUT_PARSER_REVISION
            && progress.materializer_revision == sink.materializer_revision()
            && progress.terminal
            && prior.as_ref() == Some(&next)
    });
    if exact {
        return Ok(());
    }
    let expected_frontier = OpenHandsOutputFrontier::initial(source.route_sha256).safe()?;
    let next_safe_frontier = next.safe()?;
    let mut observations = Vec::new();
    if let Some(raw_bytes) = source.raw_bytes.as_deref() {
        if let Ok(decoded) = decode_openhands_event(&source.canonical_path, raw_bytes) {
            if matches!(
                decoded.event_type(),
                EventType::ToolOutput | EventType::CommandOutput
            ) {
                if let Some(content) = super::openhands_result_content(&decoded) {
                    observations.push(output_observation(source, &decoded, content));
                }
            }
        }
    }
    let can_resume = prior.as_ref().is_some_and(|prior| {
        prior.version == OPENHANDS_OUTPUT_FRONTIER_VERSION
            && prior.parser_revision == OPENHANDS_NATIVE_PARSER_REVISION
            && prior.policy_revision == OPENHANDS_NATIVE_POLICY_REVISION
            && prior.route_sha256 == source.route_sha256
            && !prior.deleted
            && prior.content_sha256 == source.content_sha256
    });
    let state = output_state(progress, can_resume, sink.materializer_revision())?;
    let output = NativeProOutputPage {
        inventory_generation: sink.inventory_generation(),
        source: output_source,
        source_epoch: state.source_epoch,
        observed_revision: source.source_revision(None),
        parser_revision: OPENHANDS_OUTPUT_PARSER_REVISION.to_owned(),
        materializer_revision: sink.materializer_revision().to_owned(),
        disposition: state.disposition,
        expected_prior_source_epoch: state.expected_source_epoch,
        expected_prior_frontier: state.expected_sink_frontier,
        observations,
    };
    let replay = NativeProReplayPage::new_with_source_identity(
        NativeSourceIdentity::new(CaptureProvider::OpenHands.as_str(), &source.path_identity),
        expected_frontier,
        next_safe_frontier,
        true,
        NativePageAccounting {
            logical_units: 1,
            conservative_serialized_bytes: NATIVE_INGESTION_PAGE_MAX_BYTES,
        },
        output,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    if let Err(failure) = process_pro_replay_only(replay, sink) {
        sink.mark_behind(ProOutputSinkError::new(
            "openhands_output_page",
            format!("{:?}", failure.output_error),
        ));
    }
    let _ = context;
    Ok(())
}

fn replay_deleted_output(
    route: &KnownOpenHandsRoute,
    source_root: &Path,
    sink: &dyn ProOutputSink,
) -> Result<()> {
    let output_source = OutputSourceIdentity {
        provider: CaptureProvider::OpenHands.as_str().to_owned(),
        namespace_id: source_root.display().to_string(),
        source_id: route.path_identity.clone(),
    };
    let progress = match sink.observe_source(&output_source) {
        Ok(Some(progress)) => progress,
        Ok(None) => return Ok(()),
        Err(error) => {
            sink.mark_behind(error);
            return Ok(());
        }
    };
    let route_sha256 = route.checkpoint.as_ref().map_or_else(
        || route_hash(&route.path_identity),
        |cursor| cursor.route_sha256,
    );
    let next = OpenHandsOutputFrontier::deleted(route_sha256);
    let prior = progress
        .cursor
        .as_ref()
        .and_then(|cursor| serde_json::from_slice::<OpenHandsOutputFrontier>(&cursor.payload).ok());
    if prior.as_ref() == Some(&next)
        && progress.parser_revision == OPENHANDS_OUTPUT_PARSER_REVISION
        && progress.materializer_revision == sink.materializer_revision()
    {
        return Ok(());
    }
    let source_epoch =
        progress
            .source_epoch
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "OpenHands deleted output source epoch exhausted",
            ))?;
    let expected_prior_frontier = progress
        .cursor
        .as_ref()
        .map(|cursor| NativeSafeFrontier::new(cursor.version, cursor.payload.clone()))
        .transpose()
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let replay = NativeProReplayPage::new_with_source_identity(
        NativeSourceIdentity::new(CaptureProvider::OpenHands.as_str(), &route.path_identity),
        OpenHandsOutputFrontier::initial(route_sha256).safe()?,
        next.safe()?,
        true,
        NativePageAccounting {
            logical_units: 1,
            conservative_serialized_bytes: NATIVE_INGESTION_PAGE_MAX_BYTES,
        },
        NativeProOutputPage {
            inventory_generation: sink.inventory_generation(),
            source: output_source,
            source_epoch,
            observed_revision: "openhands-nativepath-source-deleted-v1".to_owned(),
            parser_revision: OPENHANDS_OUTPUT_PARSER_REVISION.to_owned(),
            materializer_revision: sink.materializer_revision().to_owned(),
            disposition: ProOutputSourceDisposition::Rewrite,
            expected_prior_source_epoch: Some(progress.source_epoch),
            expected_prior_frontier,
            observations: Vec::new(),
        },
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    if let Err(failure) = process_pro_replay_only(replay, sink) {
        sink.mark_behind(ProOutputSinkError::new(
            "openhands_output_delete",
            format!("{:?}", failure.output_error),
        ));
    }
    Ok(())
}

struct OutputState {
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_sink_frontier: Option<NativeSafeFrontier>,
    disposition: ProOutputSourceDisposition,
}

fn output_state(
    progress: Option<ProOutputProgress>,
    can_resume_source: bool,
    materializer_revision: &str,
) -> Result<OutputState> {
    let Some(progress) = progress else {
        return Ok(OutputState {
            source_epoch: 0,
            expected_source_epoch: None,
            expected_sink_frontier: None,
            disposition: ProOutputSourceDisposition::NewSource,
        });
    };
    let can_resume = progress.parser_revision == OPENHANDS_OUTPUT_PARSER_REVISION
        && progress.materializer_revision == materializer_revision
        && can_resume_source;
    let expected_sink_frontier = progress
        .cursor
        .as_ref()
        .map(|cursor| NativeSafeFrontier::new(cursor.version, cursor.payload.clone()))
        .transpose()
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    Ok(OutputState {
        source_epoch: if can_resume {
            progress.source_epoch
        } else {
            progress
                .source_epoch
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "OpenHands output source epoch exhausted",
                ))?
        },
        expected_source_epoch: Some(progress.source_epoch),
        expected_sink_frontier,
        disposition: if can_resume {
            ProOutputSourceDisposition::AppendOrResume
        } else {
            ProOutputSourceDisposition::Rewrite
        },
    })
}

fn output_observation(
    source: &OpenHandsObservedFile,
    decoded: &OpenHandsDecodedEvent,
    content: String,
) -> ProOutputObservation {
    let outcome = openhands_output_outcome(decoded);
    ProOutputObservation {
        kind: if decoded.event_type() == EventType::CommandOutput {
            OutputObservationKind::Command
        } else {
            OutputObservationKind::Tool
        },
        coordinate: OutputNativeCoordinate {
            unit_key: decoded.event_id().to_owned(),
            native_sequence: event_identity_index(source, decoded.event_id()),
            native_record_id: Some(decoded.event_id().to_owned()),
            source_record_ordinal: Some(0),
            source_record_subrecord_index: Some(0),
            byte_start: None,
            byte_end_exclusive: None,
        },
        occurred_at_unix_ms: Some(decoded.timestamp().timestamp_millis()),
        associations: OutputAssociations {
            direct_session_id: source.session_id.clone(),
            root_session_id: source.session_id.clone(),
            parent_session_id: None,
            provider_session_id: Some(source.session_id.clone()),
            agent_id: source.user_id.clone(),
            repository: None,
        },
        call_id: openhands_output_call_id(decoded.value()),
        command: openhands_output_command_context(decoded),
        outcome,
        locator: OutputSourceLocator {
            version: 1,
            kind: OPENHANDS_LOCATOR_KIND.to_owned(),
            payload: source.canonical_path_text.as_bytes().to_vec(),
        },
        content: content.into_bytes(),
    }
}

fn openhands_output_outcome(decoded: &OpenHandsDecodedEvent) -> OutputOutcomeMetadata {
    let value = decoded.value();
    let exit_code = [
        "/observation/exit_code",
        "/observation/metadata/exit_code",
        "/exit_code",
    ]
    .into_iter()
    .find_map(|pointer| value.pointer(pointer).and_then(Value::as_i64))
    .and_then(|value| i32::try_from(value).ok());
    let duration_ms = [
        "/observation/duration_ms",
        "/observation/metadata/duration_ms",
        "/duration_ms",
    ]
    .into_iter()
    .find_map(|pointer| value.pointer(pointer).and_then(Value::as_u64));
    let timed_out = openhands_value_indicates_timeout(value);
    let classification = provider_result_outcome_evidence(decoded.event_type(), value);
    let outcome = if timed_out {
        OutputOutcome::Timeout
    } else {
        match classification.as_str() {
            Some("success") => OutputOutcome::Success,
            Some("failure") => OutputOutcome::Failure,
            _ => OutputOutcome::Unknown,
        }
    };
    OutputOutcomeMetadata {
        outcome,
        exit_code,
        duration_ms,
    }
}

fn openhands_value_indicates_timeout(value: &Value) -> bool {
    const MAX_NODES: usize = 4_096;

    fn visit(value: &Value, remaining: &mut usize) -> bool {
        if *remaining == 0 {
            return false;
        }
        *remaining -= 1;
        match value {
            Value::Array(values) => values.iter().any(|value| visit(value, remaining)),
            Value::Object(values) => values.iter().any(|(key, value)| {
                let normalized = key
                    .chars()
                    .filter(|ch| ch.is_ascii_alphanumeric())
                    .flat_map(char::to_lowercase)
                    .collect::<String>();
                let direct = matches!(normalized.as_str(), "timeout" | "timedout" | "istimeout")
                    && (value.as_bool().unwrap_or(false)
                        || value.as_str().is_some_and(|value| {
                            matches!(
                                value.trim().to_ascii_lowercase().as_str(),
                                "timeout" | "timed_out" | "timedout"
                            )
                        }));
                direct || visit(value, remaining)
            }),
            Value::String(value) => matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "timeout" | "timed_out" | "timedout"
            ),
            Value::Null | Value::Bool(_) | Value::Number(_) => false,
        }
    }

    let mut remaining = MAX_NODES;
    visit(value, &mut remaining)
}

fn openhands_output_call_id(value: &Value) -> Option<String> {
    [
        "/tool_call_id",
        "/action_id",
        "/observation/tool_call_id",
        "/observation/action_id",
        "/observation/command_id",
    ]
    .into_iter()
    .find_map(|pointer| value.pointer(pointer).and_then(Value::as_str))
    .filter(|value| valid_output_token(value, 384))
    .map(str::to_owned)
}

fn openhands_output_command_context(
    decoded: &OpenHandsDecodedEvent,
) -> Option<OutputCommandContext> {
    if decoded.event_type() != EventType::CommandOutput {
        return None;
    }
    let observation = decoded.value().get("observation")?;
    let tool_name = observation
        .get("kind")
        .and_then(Value::as_str)
        .filter(|value| valid_output_token(value, 256))
        .unwrap_or("command");
    Some(OutputCommandContext {
        tool_name: tool_name.to_owned(),
        command: tool_input::command(observation)?,
        working_directory: tool_input::working_directory(observation),
    })
}

fn apply_failure_diagnostic(
    event: &mut OpenHandsEventFact,
    content: Option<&str>,
    outcome: &OutputOutcomeMetadata,
    call_id: Option<&str>,
    command: Option<&OutputCommandContext>,
) -> Result<()> {
    let payload = event
        .payload
        .as_object_mut()
        .ok_or(CaptureError::SystemInvariant(
            "OpenHands failure event payload must be an object",
        ))?;
    payload.insert("result_outcome".to_owned(), json!("failure"));
    payload.insert(
        "timed_out".to_owned(),
        json!(outcome.outcome == OutputOutcome::Timeout),
    );
    if let Some(exit_code) = outcome.exit_code {
        payload.insert("exit_code".to_owned(), json!(exit_code));
    }
    if let Some(duration_ms) = outcome.duration_ms {
        payload.insert("duration_ms".to_owned(), json!(duration_ms));
    }
    if let Some(call_id) = call_id {
        payload.insert("call_id".to_owned(), Value::String(call_id.to_owned()));
    }
    if let Some(command) = command {
        payload.insert("command".to_owned(), Value::String(command.command.clone()));
        if let Some(working_directory) = command.working_directory.as_ref() {
            payload.insert("cwd".to_owned(), Value::String(working_directory.clone()));
        }
    }
    if let Some(content) = content {
        payload.insert("output_bytes".to_owned(), json!(content.len()));
        let (preview, _) = provider_local_preview(content, PROVIDER_MAX_PREVIEW_CHARS);
        if !preview.trim().is_empty() {
            payload.insert("output_preview".to_owned(), Value::String(preview));
        }
    }
    Ok(())
}

fn valid_output_token(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/')
        })
}

fn bounded_failure(mut failure: String) -> String {
    if failure.len() <= OPENHANDS_MAX_FAILURE_BYTES {
        return failure;
    }
    let mut boundary = OPENHANDS_MAX_FAILURE_BYTES;
    while !failure.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    failure.truncate(boundary);
    failure
}
