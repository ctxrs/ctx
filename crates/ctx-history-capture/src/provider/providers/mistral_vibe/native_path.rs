use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, Metadata},
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    Confidence, ContentRef, Event, EventRole, EventType, Fidelity, FileChangeKind, FileTouched,
    Session, SessionEdge, SessionEdgeType, SessionStatus, SyncCursor,
};
use ctx_history_store::{
    decode_native_path_committed_cursor, CanonicalActor, EventSearchBulkGuard,
    NativePathCursorSetClassification, NativePathCursorTransition, NativePathGroupAccounting,
    ProviderEventHashAuthority, ProviderSourceLocatorObservation, ProviderSourceRouteRetirement,
    ProviderSourceRouteRetirementDisposition, ProviderSourceRouteRetirementReason, Store,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    complete_content::{
        attach_verified_content_locator, verified_content_profile, CompleteContentBodyDigest,
        CompleteContentSourceFamily, VerifiedContentLocatorV1, VerifiedContentRole,
        COMPLETE_CONTENT_MAX_BODY_BYTES, VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
    },
    provider::{
        file_touches::visit_all_file_touch_drafts,
        importer::{
            compact_provider_result_payload,
            provider_event_import_identity_with_exact_legacy_source, provider_file_touch_import_id,
            provider_import_session_uuid, provider_path_identity,
            provider_scoped_source_identity_key, provider_source_cursor_stream_for_path,
            provider_source_identity, provider_sync_metadata, timestamps, CertifiedProviderCursor,
        },
        native_ingestion::{
            process_pro_replay_only, NativePageAccounting, NativeProOutputPage,
            NativeProReplayPage, NativeSafeFrontier, NativeSourceIdentity,
        },
        normalization::{
            provider_capped_json, provider_capped_json_value, provider_local_preview,
            provider_output_event_is_failure, provider_policy_body, provider_policy_event_text,
            provider_result_identifier_evidence, provider_result_outcome_evidence, provider_role,
        },
        providers::native_jsonl::native_jsonl_timestamp,
        tool_input,
    },
    stable_capture_uuid,
    summaries::MAX_RETAINED_PROVIDER_FAILURES,
    CaptureError, CaptureWorkLimit, ImportProfile, OutputAssociations, OutputCommandContext,
    OutputNativeCoordinate, OutputNativeCursor, OutputObservationKind, OutputOutcome,
    OutputOutcomeMetadata, OutputSourceIdentity, OutputSourceLocator, ProOutputObservation,
    ProOutputProgress, ProOutputSinkError, ProOutputSourceDisposition, ProviderAdapterContext,
    ProviderImportFailure, ProviderImportOptions, ProviderImportSummary, ProviderImportWorkResult,
    Result, MAX_PROVIDER_JSONL_LINE_BYTES, MISTRAL_VIBE_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS,
    PROVIDER_MAX_TEXT_CHARS,
};

use super::{
    schema::{
        mistral_vibe_bounded_metadata, mistral_vibe_event_id, mistral_vibe_event_text,
        mistral_vibe_event_type, mistral_vibe_metadata_pointer_string,
        mistral_vibe_metadata_string, mistral_vibe_metadata_timestamp, mistral_vibe_result_content,
    },
    source::{visit_mistral_vibe_session_sources, MistralVibeSessionSource},
    MISTRAL_VIBE_CAPTURE_REVISION, MISTRAL_VIBE_POLICY_REVISION,
};

mod lifecycle;
mod model;
mod projection;
mod publication;
mod reader;
pub(super) mod source_backed;
#[cfg(test)]
mod tests;

use self::{lifecycle::*, model::*, projection::*, publication::*, reader::*};
pub(super) use model::source_cursor_stream;

struct MistralVibeCorePublication<'a> {
    source: &'a MistralVibeSessionSource,
    observation: &'a SourceObservation,
    page: Page,
}

pub(crate) fn import_mistral_vibe_nativepath(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let root_missing = match fs::symlink_metadata(path) {
        Ok(_) => false,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => return Err(error.into()),
    };
    let configured_root = configured_source_root(path, &context, root_missing)?;
    let known_routes = load_known_routes(store, &context.machine_id, &configured_root)?;
    let mut sources = Vec::new();
    let discovered = match visit_mistral_vibe_session_sources(path, &mut |source| {
        sources.push(source);
        Ok(())
    }) {
        Ok(count) => count,
        Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => return Err(error),
    };
    sources.sort_by(|left, right| left.messages_path.cmp(&right.messages_path));
    if discovered == 0 && known_routes.is_empty() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Mistral Vibe history root contains no complete session directories",
        });
    }
    if options.import_profile.is_replay_only() && discovered == 0 {
        if let Some(sink) = options.import_profile.sink() {
            sink.mark_behind(ProOutputSinkError::new(
                if root_missing {
                    "mistral_vibe_root_missing"
                } else {
                    "mistral_vibe_source_missing"
                },
                "Mistral Vibe source is unavailable for output replay",
            ));
        }
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }

    let mut prepared_sources = Vec::with_capacity(sources.len());
    let mut session_ids = BTreeSet::new();
    for source in sources {
        let file_context = ProviderAdapterContext {
            machine_id: context.machine_id.clone(),
            source_path: Some(source.messages_path.clone()),
            source_root: Some(configured_root.clone()),
            imported_at: context.imported_at,
        };
        let stream = source_cursor_stream(&source.messages_path)?;
        let observation = SourceObservation::read(&source)?;
        let source_revision =
            observation.source_revision(options.inventory_observation_token.as_deref());
        let (session, metadata_failure) = SessionFact::from_source(&source, context.imported_at)?;
        if !session_ids.insert(session.provider_session_id.clone()) {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: configured_root,
                reason: "Mistral Vibe history root contains duplicate session IDs",
            });
        }
        let proposed_source_identity =
            proposed_source_identity(&file_context, &source.messages_path)?;
        let canonical_source_identity = known_routes
            .iter()
            .find(|route| route.cursor_stream == stream)
            .map(|route| route.canonical_source_identity.clone())
            .unwrap_or(proposed_source_identity);
        prepared_sources.push(PreparedSource {
            source,
            observation,
            file_context,
            stream,
            source_revision,
            canonical_source_identity,
            session,
            metadata_failure,
        });
    }
    let live_streams = prepared_sources
        .iter()
        .map(|source| source.stream.clone())
        .collect::<BTreeSet<_>>();

    let committed_store = Store::open_read_only(store.path())?;
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let mut summary = ProviderImportSummary::default();
        let mut changed_groups = 0_usize;

        for prepared in prepared_sources {
            let PreparedSource {
                source,
                observation,
                file_context,
                stream,
                source_revision,
                canonical_source_identity,
                session,
                metadata_failure,
            } = prepared;
            let stored = store.get_sync_cursor(None, &context.machine_id, &stream)?;
            let mut opened = open_source(
                source,
                observation,
                &context.machine_id,
                source_revision,
                canonical_source_identity,
                session,
                metadata_failure,
                stored.as_ref(),
            )?;

            if options.import_profile.is_replay_only() {
                let Some(committed) = stored.as_ref() else {
                    if let Some(sink) = options.import_profile.sink() {
                        sink.mark_behind(ProOutputSinkError::new(
                            "mistral_vibe_core_missing",
                            "Mistral Vibe output replay requires committed NativePath Core",
                        ));
                    }
                    continue;
                };
                let Some(checkpoint) = decode_native_checkpoint(&committed.cursor)? else {
                    if let Some(sink) = options.import_profile.sink() {
                        sink.mark_behind(ProOutputSinkError::new(
                            "mistral_vibe_core_upgrade_required",
                            "Mistral Vibe output replay requires a NativePath Core cursor",
                        ));
                    }
                    continue;
                };
                replay_outputs_or_mark_behind(
                    &opened.source,
                    &opened.observation,
                    &checkpoint,
                    &opened.target_source_identity,
                    &options.import_profile,
                );
                continue;
            }

            if opened.lifecycle == SourceLifecycle::NoOp {
                let mut skipped = summary_from_checkpoint(&opened.checkpoint);
                skipped.set_work_result(ProviderImportWorkResult::NoOp);
                summary.merge_from(skipped);
            } else {
                loop {
                    let Some(page) = next_core_page(&mut opened)? else {
                        break;
                    };
                    let (page_summary, committed_checkpoint, reconciled_source_identity) =
                        publish_core_page(
                            store,
                            &committed_store,
                            &bulk_guard,
                            &file_context,
                            &options,
                            MistralVibeCorePublication {
                                source: &opened.source,
                                observation: &opened.observation,
                                page,
                            },
                        )?;
                    let terminal = committed_checkpoint.terminal
                        && committed_checkpoint.canonical_source_identity
                            == reconciled_source_identity;
                    opened.target_source_identity = reconciled_source_identity;
                    opened.checkpoint = committed_checkpoint;
                    if page_summary.work_result() == ProviderImportWorkResult::Changed {
                        changed_groups = changed_groups.saturating_add(1);
                    }
                    summary.merge_from(page_summary);
                    if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup
                        && changed_groups != 0
                        && !terminal
                    {
                        summary.work_remaining = true;
                        return Ok(summary);
                    }
                    if terminal {
                        break;
                    }
                }
            }

            let stored = store
                .get_sync_cursor(None, &context.machine_id, &stream)?
                .ok_or(CaptureError::SystemInvariant(
                    "Mistral Vibe Core publication lost its cursor",
                ))?;
            let checkpoint =
                decode_native_checkpoint(&stored.cursor)?.ok_or(CaptureError::SystemInvariant(
                    "Mistral Vibe Core publication stored a non-NativePath cursor",
                ))?;
            if options.import_profile.sink().is_some() {
                replay_outputs_or_mark_behind(
                    &opened.source,
                    &opened.observation,
                    &checkpoint,
                    &checkpoint.canonical_source_identity,
                    &options.import_profile,
                );
            }
        }

        if options.import_profile.is_replay_only() {
            summary.set_work_result(ProviderImportWorkResult::NoOp);
            return Ok(summary);
        }

        if !known_routes.is_empty() {
            let current_routes = load_known_routes(store, &context.machine_id, &configured_root)?;
            let current_sources = current_routes
                .iter()
                .filter(|entry| live_streams.contains(&entry.cursor_stream))
                .map(|entry| entry.canonical_source_identity.as_str())
                .collect::<BTreeSet<_>>();
            for missing in known_routes.iter().filter(|entry| {
                !live_streams.contains(&entry.cursor_stream)
                    && !current_sources.contains(entry.canonical_source_identity.as_str())
            }) {
                summary.merge_from(retire_missing_source(
                    store,
                    &bulk_guard,
                    &context,
                    missing,
                    if root_missing {
                        ProviderSourceRouteRetirementReason::RootMissing
                    } else {
                        ProviderSourceRouteRetirementReason::SourceMissing
                    },
                )?);
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
