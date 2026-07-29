//! Production Mux NativePath ingestion.
//!
//! Mux owns discovery, source certification, parsing, identity, privacy, cursor,
//! and lifecycle policy here. Only certified Core mutations cross the typed
//! NativePath Store surface; successful output bytes are emitted solely through
//! the independent Pro replay lane.

use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    Confidence, ContentRef, Event, Fidelity, FileChangeKind, FileTouched, Session, SessionEdge,
    SessionEdgeType, SessionStatus, SyncCursor,
};
use ctx_history_store::{
    decode_native_path_committed_cursor, CanonicalActor, EventSearchBulkGuard,
    NativePathCursorSetClassification, NativePathCursorTransition, NativePathGroupAccounting,
    ProviderEventHashAuthority, ProviderSourceLocatorObservation, ProviderSourceRouteRetirement,
    ProviderSourceRouteRetirementReason, Store, StoreError,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    complete_content::{
        jsonl::{mux_record_locator, MUX_LOCATOR_KIND},
        verified_content_profile, CompleteContentBodyDigest, CompleteContentSourceFamily,
        CompleteContentSourceLocator, VerifiedContentLocatorV1, VerifiedContentLocatorsV1,
        VerifiedContentRole, VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
    },
    provider::{
        file_touches::{
            event_type_supports_structured_file_touches,
            visit_provider_file_touch_drafts_with_limit, MAX_PACKED_PROVIDER_EVENT_INDEX,
            PROVIDER_FILE_TOUCH_LIMIT_REJECTION,
        },
        importer::{
            avoid_provider_source_event_seq_collision, compact_provider_result_payload,
            provider_file_touch_import_id, provider_path_identity, provider_scoped_source_uuid,
            provider_source_cursor_stream_for_path, provider_source_event_import_identity,
            provider_source_identity, provider_source_root, provider_source_session_uuid,
            provider_sync_metadata, timestamps,
        },
        native_ingestion::{
            process_pro_replay_only, NativePageAccounting, NativeProOutputPage,
            NativeProReplayPage, NativeSafeFrontier,
        },
        providers::native_jsonl::native_jsonl_missing_reason,
    },
    stable_capture_uuid, CaptureError, CaptureWorkLimit, OutputAssociations,
    OutputNativeCoordinate, OutputObservationKind, OutputOutcome, OutputOutcomeMetadata,
    OutputSourceIdentity, OutputSourceLocator, ProOutputObservation, ProOutputSink,
    ProOutputSourceDisposition, ProviderAdapterContext, ProviderImportFailure,
    ProviderImportOptions, ProviderImportSummary, ProviderImportWorkResult, Result,
    MAX_PROVIDER_JSONL_LINE_BYTES, MUX_SOURCE_FORMAT,
};

use super::{
    metadata::{
        bounded_mux_failure, bounded_mux_id, mux_bounded_session_metadata,
        MuxBoundedSessionMetadata,
    },
    normalization::{
        apply_mux_core_output_diagnostic, mux_core_event, mux_event_id, mux_event_text,
        mux_event_type, mux_history_sequence, mux_message_model, mux_message_timestamp_opt,
        mux_output_projection, mux_partial_event_index, mux_result_content, MuxCoreEvent,
        MuxMessageRow, MuxOutputOutcome,
    },
    source::{visit_mux_session_sources, MuxFileObservation, MuxSessionSource},
    MUX_CAPTURE_REVISION, MUX_POLICY_REVISION,
};

mod core;
mod lifecycle;
mod model;
mod output;
mod parse;
mod projection;
mod publication;
mod source;
#[cfg_attr(not(test), allow(dead_code))]
mod source_backed;

use core::*;
use lifecycle::*;
use model::*;
use output::*;
use parse::*;
use projection::*;
use publication::*;
use source::*;

pub(crate) use source_backed::{
    discover_mux_source_backed_sources, revalidate_mux_source_backed, scan_mux_source_backed,
    MuxBoundedProjection, MuxReplacementEvidence, MuxReplacementReason, MuxSourceBackedCandidate,
    MuxSourceBackedDisposition, MuxSourceBackedError, MuxSourceBackedPage, MuxSourceBackedRecord,
    MuxSourceBackedResolverV0, MuxSourceBackedResult, MuxSourceBackedScanReceipt,
    MuxUnaddressableReason, MuxUnaddressableRecord,
};

pub(crate) fn import_mux_native_path(
    path: &Path,
    store: &mut Store,
    mut context: ProviderAdapterContext,
    options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    if context.source_path.is_none() {
        context.source_path = Some(path.to_path_buf());
    }
    ensure_active_journal(store)?;
    let configured_root = context
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let prior_manifest = load_root_manifest(store, &context.machine_id, &configured_root)?;
    let mut sessions = discover_sessions(path)?;
    sessions.sort_by(|left, right| left.session_dir.cmp(&right.session_dir));
    if sessions.is_empty() && prior_manifest.is_none() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: native_jsonl_missing_reason(CaptureProvider::Mux),
        });
    }

    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let mut summary = ProviderImportSummary::default();
        let replay_only = options.import_profile.is_replay_only();
        let mut manifest_sources = Vec::new();
        let mut changed_groups = 0_usize;

        for session in sessions {
            let legacy_bridge = mux_legacy_bridge(store, &context, &session)?;
            for (kind, source_path) in [
                (MuxStreamKind::Chat, session.chat_path.clone()),
                (MuxStreamKind::Partial, session.partial_path.clone()),
            ] {
                let Some(source_path) = source_path else {
                    continue;
                };
                let plan = plan_source(
                    store,
                    &configured_root,
                    session.clone(),
                    source_path,
                    kind,
                    &context,
                    legacy_bridge.clone(),
                )?;
                manifest_sources.push(plan.manifest_source());
                let core_output_ready = if replay_only {
                    verify_terminal_core(store, &context.machine_id, &plan)?;
                    true
                } else {
                    let source_summary = import_core_source(
                        store,
                        &bulk_guard,
                        &configured_root,
                        &context,
                        &options,
                        &plan,
                    )?;
                    let core_output_ready = !source_summary.work_remaining;
                    if source_summary.work_result() == ProviderImportWorkResult::Changed {
                        changed_groups = changed_groups.saturating_add(1);
                    }
                    summary.merge_from(source_summary);
                    core_output_ready
                };
                if core_output_ready {
                    if let Some(sink) = options.import_profile.sink() {
                        if replay_source_outputs(&plan, &context, sink.as_ref())? {
                            summary.record_failure(ProviderImportFailure {
                                line: 0,
                                error: "Mux Pro output replay is behind Core".to_owned(),
                            });
                        }
                    }
                }
                if !replay_only
                    && options.capture_work_limit == CaptureWorkLimit::OneSafeGroup
                    && changed_groups != 0
                {
                    summary.work_remaining = true;
                    return Ok(summary);
                }
            }
        }

        if !replay_only {
            manifest_sources.sort_by(|left, right| {
                left.path
                    .cmp(&right.path)
                    .then_with(|| stream_kind_rank(left.kind).cmp(&stream_kind_rank(right.kind)))
            });
            if let Some(prior) = prior_manifest.as_ref() {
                retire_missing_sources(
                    store,
                    &bulk_guard,
                    &context,
                    prior,
                    &manifest_sources,
                    &mut summary,
                )?;
            }
            let manifest = MuxRootManifest {
                version: MUX_ROOT_MANIFEST_VERSION,
                configured_root,
                sources: manifest_sources,
            };
            summary.merge_from(publish_root_manifest(
                store,
                &bulk_guard,
                &context,
                manifest,
            )?);
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
