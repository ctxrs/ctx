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
            provider_source_event_import_identity, provider_source_identity,
            provider_sync_metadata, timestamps, CertifiedProviderCursor,
            ExactLegacySourceEventCandidate, ProviderEventImportIdentity,
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
    openhands_result_content,
    source::{
        discover_openhands_event_paths, hex, openhands_legacy_filename_index_candidate,
        openhands_line_number, openhands_missing_event_files, openhands_physical_fingerprint,
        OpenHandsFileObservation, OpenHandsObservedFile,
    },
};

mod core;
mod output;
mod publication;
mod routes;
mod source_backed;

#[cfg(test)]
mod source_backed_tests;

#[allow(unused_imports)]
pub(crate) use source_backed::{
    project_openhands_source_backed_v1, OpenHandsHydratedRecordV1, OpenHandsLocatorResolverV1,
    OpenHandsRejectedEventV1, OpenHandsSourceBackedAdapterV1, OpenHandsSourceBackedErrorV1,
    OpenHandsSourceBackedProjectionV1, OpenHandsSourceBackedResultV1,
};

use self::{
    core::{
        event_file_identity_index_for_path, event_identity_index, event_identity_index_for_path,
        load_stored_core_cursor, prepare_core_page,
    },
    output::{
        apply_failure_diagnostic, bounded_failure, openhands_output_call_id,
        openhands_output_command_context, openhands_output_outcome, replay_outputs_or_mark_behind,
    },
    publication::{publish_core_page, record_unchanged_source},
    routes::{
        current_route_for_source, known_openhands_routes, provider_sync_cursor, publication_id,
        relocation_route_for_source, retire_missing_routes, route_hash,
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

// The decoded native cursor stays inline so cursor-state handling remains allocation-free.
#[allow(clippy::large_enum_variant)]
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
    cursor_revision: String,
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
    provider_event_hash: String,
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

#[derive(Clone)]
struct KnownOpenHandsRoute {
    source_id: Uuid,
    source_root: Option<String>,
    path: PathBuf,
    path_identity: String,
    identity_path: String,
    identity_raw_path: PathBuf,
    locator_identity: String,
    canonical_source_identity: String,
    locator_revision: String,
    cursor_revision: String,
    physical_fingerprint: Option<String>,
    current_cursor: SyncCursor,
    checkpoint: Option<OpenHandsNativeCursor>,
}

#[derive(Default)]
struct OpenHandsRelocationState {
    output_identity_paths: BTreeMap<PathBuf, String>,
    relocated_locators: BTreeSet<String>,
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
    let all_known_routes = known_openhands_routes(store, &context.machine_id)?;
    let configured_source_root_text = configured_source_root.display().to_string();
    let known_routes = all_known_routes
        .iter()
        .filter(|route| route.source_root.as_deref() == Some(configured_source_root_text.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let sink = options.import_profile.sink().cloned();
    let mut relocation_state = OpenHandsRelocationState::default();

    if options.import_profile.is_replay_only() {
        replay_outputs_or_mark_behind(
            store,
            &live_paths,
            &known_routes,
            &relocation_state,
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
            let current_route = current_route_for_source(&all_known_routes, &source)?;
            let relocation_route = if current_route.is_none() {
                relocation_route_for_source(&all_known_routes, &source)?
            } else {
                None
            };
            let physical_fingerprint = source.physical_fingerprint();
            let mut reconcile_current_route =
                current_route.is_some_and(|route| route.locator_revision != physical_fingerprint);
            relocation_state.output_identity_paths.insert(
                source.canonical_path.clone(),
                current_route.or(relocation_route).map_or_else(
                    || source.path_identity.clone(),
                    |route| route.identity_path.clone(),
                ),
            );
            loop {
                let page =
                    prepare_core_page(store, &source, &context, &options, reconcile_current_route)?;
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
                    current_route,
                    relocation_route,
                    page,
                )?;
                reconcile_current_route = false;
                if let Some(route) = relocation_route {
                    relocation_state
                        .relocated_locators
                        .insert(route.locator_identity.clone());
                }
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
            &relocation_state.relocated_locators,
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
            &relocation_state,
            &configured_source_root,
            &context,
            sink.as_deref(),
        );
    }
    Ok(summary)
}
