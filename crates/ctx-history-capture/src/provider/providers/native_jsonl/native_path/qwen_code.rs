use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, EventRole, EventType, SyncCursor};
use ctx_history_store::{
    NativePathCursorSetClassification, NativePathCursorTransition, NativePathGroupAccounting,
    ProviderSourceRouteRetirement, ProviderSourceRouteRetirementDisposition,
    ProviderSourceRouteRetirementReason, Store,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{
    provider::{
        importer::{provider_path_identity, provider_source_cursor_stream_for_path, timestamps},
        native_ingestion::{
            process_pro_replay_only, NativePageAccounting, NativeProOutputPage,
            NativeProReplayPage, NativeSafeFrontier, NativeSourceIdentity,
            NATIVE_INGESTION_PAGE_MAX_BYTES,
        },
        normalization::{provider_output_event_is_failure, provider_role, provider_value_text},
    },
    stable_capture_uuid, CaptureError, ImportProfile, OutputAssociations, OutputNativeCoordinate,
    OutputObservationKind, OutputOutcome, OutputOutcomeMetadata, OutputSourceIdentity,
    OutputSourceLocator, ProOutputObservation, ProOutputProgress, ProOutputSink,
    ProOutputSinkError, ProOutputSourceDisposition, ProviderImportSummary,
    ProviderImportWorkResult, Result, QWEN_CODE_SOURCE_FORMAT,
};

use super::super::result_content::{NativeJsonlResultExtractionError, NativeJsonlResultSubrecord};
use super::{
    committed_direct_jsonl_replay_authority, decode_direct_jsonl_native_cursor,
    direct_jsonl_checkpoint_is_covered_by, encode_direct_jsonl_cursor, open_direct_jsonl_pages,
    reader::direct_jsonl_source_revision, DirectJsonlCheckpoint, DirectJsonlOutput,
    DirectJsonlSourceChange, NativePathJsonlTreeImport,
};

const QWEN_CODE_MISSING_REASON: &str =
    "no Qwen Code chat JSONL transcripts found under projects/*/chats";
const QWEN_CODE_OUTPUT_FRONTIER_VERSION: u32 = 1;
const QWEN_CODE_OUTPUT_PARSER_REVISION: &str = "qwen-code-direct-native-jsonl-v1";

pub(crate) const fn qwen_code_source_backed_adapter() -> super::DirectJsonlSourceAdapter {
    super::DirectJsonlSourceAdapter::new(
        CaptureProvider::QwenCode,
        QWEN_CODE_SOURCE_FORMAT,
        "qwen-code-direct-native-jsonl-v1",
    )
}

pub(crate) fn qwen_code_event_identity(value: &Value) -> Option<&str> {
    value
        .get("id")
        .or_else(|| value.get("uuid"))
        .and_then(Value::as_str)
        .filter(|event_id| !event_id.trim().is_empty())
}

pub(crate) fn import_qwen_code_nativepath_tree(
    store: &mut Store,
    request: NativePathJsonlTreeImport<'_>,
) -> Result<ProviderImportSummary> {
    let configured_source_root = request
        .source_root
        .clone()
        .or(request.source_path.clone())
        .unwrap_or_else(|| request.path.to_path_buf());
    let live_inventory = discover_live_transcripts(request.path)?;
    let known_routes = known_qwen_code_routes(store, &request.machine_id, &configured_source_root)?;
    let sink = request.import_profile.sink().cloned();

    if request.import_profile.is_replay_only() {
        replay_outputs_or_mark_behind(
            store,
            &request.machine_id,
            &live_inventory.paths,
            &configured_source_root,
            request.imported_at,
            sink.as_deref(),
        )?;
        replay_missing_outputs_or_mark_behind(
            &known_routes,
            &live_inventory.paths,
            &configured_source_root,
            if live_inventory.root_missing {
                ProviderSourceRouteRetirementReason::RootMissing
            } else {
                ProviderSourceRouteRetirementReason::SourceMissing
            },
            sink.as_deref(),
        )?;
        return Ok(ProviderImportSummary::default());
    }

    if live_inventory.paths.is_empty() {
        if known_routes.is_empty() {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: request.path.to_path_buf(),
                reason: QWEN_CODE_MISSING_REASON,
            });
        }
        let reason = if live_inventory.root_missing {
            ProviderSourceRouteRetirementReason::RootMissing
        } else {
            ProviderSourceRouteRetirementReason::SourceMissing
        };
        let summary = retire_missing_routes(
            store,
            &request.machine_id,
            request.imported_at,
            &known_routes,
            &live_inventory.paths,
            reason,
        )?;
        replay_missing_outputs_or_mark_behind(
            &known_routes,
            &live_inventory.paths,
            &configured_source_root,
            reason,
            sink.as_deref(),
        )?;
        return Ok(summary);
    }

    let mut summary = import_qwen_code_core(
        store,
        NativePathJsonlTreeImport {
            path: request.path,
            machine_id: request.machine_id.clone(),
            source_path: request.source_path,
            source_root: request.source_root,
            imported_at: request.imported_at,
            history_record_id: request.history_record_id,
            capture_work_limit: request.capture_work_limit,
            inventory_observation_token: request.inventory_observation_token,
            import_profile: ImportProfile::CoreOnly,
        },
    )?;
    if summary.work_remaining {
        return Ok(summary);
    }
    summary.merge_from(retire_missing_routes(
        store,
        &request.machine_id,
        request.imported_at,
        &known_routes,
        &live_inventory.paths,
        ProviderSourceRouteRetirementReason::SourceMissing,
    )?);
    replay_outputs_or_mark_behind(
        store,
        &request.machine_id,
        &live_inventory.paths,
        &configured_source_root,
        request.imported_at,
        sink.as_deref(),
    )?;
    replay_missing_outputs_or_mark_behind(
        &known_routes,
        &live_inventory.paths,
        &configured_source_root,
        ProviderSourceRouteRetirementReason::SourceMissing,
        sink.as_deref(),
    )?;
    Ok(summary)
}

struct QwenCodeInventory {
    paths: BTreeSet<PathBuf>,
    root_missing: bool,
}

fn discover_live_transcripts(root: &Path) -> Result<QwenCodeInventory> {
    if super::super::traversal::native_jsonl_root_kind(root)?.is_none() {
        return Ok(QwenCodeInventory {
            paths: BTreeSet::new(),
            root_missing: true,
        });
    }
    let mut paths = BTreeSet::new();
    super::super::traversal::visit_jsonl_tree_files(
        CaptureProvider::QwenCode,
        root,
        &mut |source_file| {
            paths.insert(source_file.path().to_path_buf());
            Ok(())
        },
    )?;
    Ok(QwenCodeInventory {
        paths,
        root_missing: false,
    })
}

pub(crate) fn qwen_code_file_is_selected(path: &Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()) == Some("jsonl")
        && path
            .components()
            .any(|component| component.as_os_str() == "chats")
}

fn import_qwen_code_core(
    store: &mut Store,
    request: NativePathJsonlTreeImport<'_>,
) -> Result<ProviderImportSummary> {
    super::import_direct_native_jsonl_tree_core(
        store,
        request,
        CaptureProvider::QwenCode,
        QWEN_CODE_SOURCE_FORMAT,
    )
}

#[derive(Clone)]
struct KnownQwenCodeRoute {
    path: PathBuf,
    locator_identity: String,
    canonical_source_identity: String,
    source_revision: String,
    current_cursor: SyncCursor,
    checkpoint: DirectJsonlCheckpoint,
}

fn known_qwen_code_routes(
    store: &Store,
    machine_id: &str,
    source_root: &Path,
) -> Result<Vec<KnownQwenCodeRoute>> {
    let source_root = source_root.display().to_string();
    let mut routes = BTreeMap::<String, KnownQwenCodeRoute>::new();
    for source in store.list_capture_sources()? {
        if source.descriptor.provider != CaptureProvider::QwenCode
            || source.descriptor.machine_id != machine_id
            || source.descriptor.source_format.as_deref() != Some(QWEN_CODE_SOURCE_FORMAT)
            || source.descriptor.source_root.as_deref() != Some(source_root.as_str())
        {
            continue;
        }
        let (Some(raw_source_path), Some(canonical_source_identity)) = (
            source.descriptor.raw_source_path.as_deref(),
            source.descriptor.source_identity.as_deref(),
        ) else {
            continue;
        };
        let path = PathBuf::from(raw_source_path);
        let locator_identity = provider_path_identity(&path)?;
        let stream = provider_source_cursor_stream_for_path(
            CaptureProvider::QwenCode,
            QWEN_CODE_SOURCE_FORMAT,
            &locator_identity,
        );
        let Some(current_cursor) = store.get_sync_cursor(None, machine_id, &stream)? else {
            continue;
        };
        let Some(checkpoint) = decode_direct_jsonl_native_cursor(
            &current_cursor.cursor,
            CaptureProvider::QwenCode,
            QWEN_CODE_SOURCE_FORMAT,
        ) else {
            continue;
        };
        let checkpoint_session = checkpoint
            .session
            .as_ref()
            .map(|session| session.provider_session_id.as_str());
        if checkpoint.source_path != path
            || source.descriptor.external_session_id.as_deref() != checkpoint_session
        {
            continue;
        }
        let Some(source_revision) = source
            .sync
            .metadata
            .get("source_revision")
            .and_then(Value::as_str)
        else {
            continue;
        };
        let route = KnownQwenCodeRoute {
            path,
            locator_identity: locator_identity.clone(),
            canonical_source_identity: canonical_source_identity.to_owned(),
            source_revision: source_revision.to_owned(),
            current_cursor,
            checkpoint,
        };
        if routes.insert(locator_identity, route).is_some() {
            return Err(CaptureError::SystemInvariant(
                "Qwen Code persisted duplicate current routes for one transcript",
            ));
        }
    }
    Ok(routes.into_values().collect())
}

fn retire_missing_routes(
    store: &mut Store,
    machine_id: &str,
    retired_at: DateTime<Utc>,
    known_routes: &[KnownQwenCodeRoute],
    live_paths: &BTreeSet<PathBuf>,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<ProviderImportSummary> {
    let missing = known_routes
        .iter()
        .filter(|route| !live_paths.contains(&route.path))
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(ProviderImportSummary::default());
    }
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let mut summary = ProviderImportSummary::default();
        for route in missing {
            if retire_route(store, &bulk_guard, machine_id, retired_at, route, reason)? {
                summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
                summary.skipped = summary.skipped.saturating_add(1);
                summary.set_work_result(ProviderImportWorkResult::Changed);
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

fn retire_route(
    store: &Store,
    bulk_guard: &ctx_history_store::EventSearchBulkGuard,
    machine_id: &str,
    retired_at: DateTime<Utc>,
    route: &KnownQwenCodeRoute,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<bool> {
    let stream = route.current_cursor.stream.clone();
    let provider_cursor = encode_direct_jsonl_cursor(&route.checkpoint)?;
    let transition = NativePathCursorTransition::new(
        Some(route.current_cursor.cursor.clone()),
        provider_sync_cursor(
            machine_id,
            stream.clone(),
            provider_cursor.clone(),
            retired_at,
        ),
    );
    let retirement = ProviderSourceRouteRetirement {
        provider: CaptureProvider::QwenCode,
        source_format: QWEN_CODE_SOURCE_FORMAT.to_owned(),
        machine_id: machine_id.to_owned(),
        locator_identity: route.locator_identity.clone(),
        cursor_stream: stream,
        expected_canonical_source_identity: route.canonical_source_identity.clone(),
        expected_source_revision: route.source_revision.clone(),
        retired_at_ms: retired_at.timestamp_millis(),
        reason,
    };
    let publication_id = retirement_publication_id(&retirement);
    if super::direct_jsonl_cursor_matches_publication(
        &route.current_cursor.cursor,
        &publication_id,
        &provider_cursor,
    ) {
        return Ok(false);
    }
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

fn replay_outputs_or_mark_behind(
    store: &Store,
    machine_id: &str,
    paths: &BTreeSet<PathBuf>,
    source_root: &Path,
    imported_at: DateTime<Utc>,
    sink: Option<&dyn ProOutputSink>,
) -> Result<()> {
    let Some(sink) = sink else {
        return Ok(());
    };
    replay_qwen_code_outputs(store, machine_id, paths, source_root, imported_at, sink)
}

fn replay_qwen_code_outputs(
    store: &Store,
    machine_id: &str,
    paths: &BTreeSet<PathBuf>,
    source_root: &Path,
    imported_at: DateTime<Utc>,
    sink: &dyn ProOutputSink,
) -> Result<()> {
    for path in paths {
        let replay = (|| {
            let authority = committed_direct_jsonl_replay_authority(
                store,
                machine_id,
                CaptureProvider::QwenCode,
                QWEN_CODE_SOURCE_FORMAT,
                path,
            )?;
            let locator_identity = provider_path_identity(path)?;
            let source = OutputSourceIdentity {
                provider: CaptureProvider::QwenCode.as_str().to_owned(),
                namespace_id: source_root.display().to_string(),
                source_id: locator_identity.clone(),
            };
            let progress = match sink.observe_source(&source) {
                Ok(progress) => progress,
                Err(error) => {
                    sink.mark_behind(error);
                    return Ok(());
                }
            };
            replay_qwen_code_source(
                path,
                imported_at,
                sink,
                source,
                locator_identity,
                progress,
                &authority,
            )
        })();
        if let Err(error) = replay {
            if super::driver::capture_error_is_systemic(&error) {
                return Err(error);
            }
            sink.mark_behind(ProOutputSinkError::new(
                "qwen_code_nativepath_output_source",
                bounded_output_diagnostic(path, &error),
            ));
        }
    }
    Ok(())
}

fn replay_qwen_code_source(
    path: &Path,
    imported_at: DateTime<Utc>,
    sink: &dyn ProOutputSink,
    output_source: OutputSourceIdentity,
    locator_identity: String,
    progress: Option<ProOutputProgress>,
    authority: &DirectJsonlCheckpoint,
) -> Result<()> {
    let progress_cursor = progress
        .as_ref()
        .and_then(|progress| progress.cursor.as_ref())
        .filter(|cursor| cursor.version == QWEN_CODE_OUTPUT_FRONTIER_VERSION)
        .and_then(|cursor| serde_json::from_slice::<DirectJsonlCheckpoint>(&cursor.payload).ok())
        .filter(|checkpoint| {
            checkpoint.is_supported_for(CaptureProvider::QwenCode, QWEN_CODE_SOURCE_FORMAT)
        });
    let can_resume = progress.as_ref().is_some_and(|progress| {
        progress.parser_revision == QWEN_CODE_OUTPUT_PARSER_REVISION
            && progress.materializer_revision == sink.materializer_revision()
            && progress_cursor.is_some()
    });
    let previous = if can_resume {
        progress_cursor.as_ref()
    } else {
        None
    };
    let mut reader = open_direct_jsonl_pages(
        CaptureProvider::QwenCode,
        QWEN_CODE_SOURCE_FORMAT,
        path,
        None,
        imported_at,
        true,
        previous,
    )?;
    if reader.observation() != &authority.source_observation {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let source_change = reader.source_change();
    let observed_revision = direct_jsonl_source_revision(&authority.source_observation);
    let mut output_state = QwenCodeOutputState::new(
        output_source,
        progress,
        source_change,
        can_resume,
        &observed_revision,
        sink.materializer_revision(),
    )?;

    let mut emitted_page = false;
    while let Some(page) = reader.next_page()? {
        emitted_page = true;
        if !direct_jsonl_checkpoint_is_covered_by(authority, &page.next_checkpoint) {
            return Err(CaptureError::InvalidPayload(
                "Qwen Code output replay advanced beyond committed Core authority".to_owned(),
            ));
        }
        let expected_frontier = safe_frontier(&page.expected_checkpoint)?;
        let next_safe_frontier = safe_frontier(&page.next_checkpoint)?;
        let observations = page
            .outputs
            .into_iter()
            .map(|output| output_observation(&page.next_checkpoint, path, output))
            .collect::<Vec<_>>();
        let accounting = NativePageAccounting {
            logical_units: page.logical_units.max(1),
            // Replay adds the source-root namespace and exact path locator to
            // the reader-owned payload. These pages are dispatched singly, so
            // certify the full bounded replay allowance rather than reuse the
            // smaller Core projection claim.
            conservative_serialized_bytes: NATIVE_INGESTION_PAGE_MAX_BYTES,
        };
        let output = NativeProOutputPage {
            inventory_generation: sink.inventory_generation(),
            source: output_state.source.clone(),
            source_epoch: output_state.source_epoch,
            observed_revision: observed_revision.clone(),
            parser_revision: QWEN_CODE_OUTPUT_PARSER_REVISION.to_owned(),
            materializer_revision: sink.materializer_revision().to_owned(),
            disposition: output_state.disposition,
            expected_prior_source_epoch: output_state.expected_source_epoch,
            expected_prior_frontier: output_state.expected_sink_frontier.clone(),
            observations,
        };
        let replay = NativeProReplayPage::new_with_source_identity(
            NativeSourceIdentity::new(CaptureProvider::QwenCode.as_str(), &locator_identity),
            expected_frontier,
            next_safe_frontier.clone(),
            page.terminal,
            accounting,
            output,
        )
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        if process_pro_replay_only(replay, sink).is_err() {
            return Ok(());
        }
        output_state.expected_source_epoch = Some(output_state.source_epoch);
        output_state.expected_sink_frontier = Some(next_safe_frontier);
        output_state.disposition = ProOutputSourceDisposition::AppendOrResume;
    }
    let outcome = reader.outcome().ok_or(CaptureError::SystemInvariant(
        "Qwen Code output replay reader completed without an outcome",
    ))?;
    if !outcome.checkpoint.terminal
        || !direct_jsonl_checkpoint_is_covered_by(authority, &outcome.checkpoint)
    {
        return Err(CaptureError::InvalidPayload(
            "Qwen Code output replay outcome exceeded committed Core authority".to_owned(),
        ));
    }
    if !emitted_page {
        let next_safe_frontier = safe_frontier(&outcome.checkpoint)?;
        if output_state.prior_terminal
            && output_state.disposition == ProOutputSourceDisposition::AppendOrResume
            && output_state.expected_sink_frontier.as_ref() == Some(&next_safe_frontier)
        {
            return Ok(());
        }
        let expected_frontier = next_safe_frontier.clone();
        let output = NativeProOutputPage {
            inventory_generation: sink.inventory_generation(),
            source: output_state.source,
            source_epoch: output_state.source_epoch,
            observed_revision,
            parser_revision: QWEN_CODE_OUTPUT_PARSER_REVISION.to_owned(),
            materializer_revision: sink.materializer_revision().to_owned(),
            disposition: output_state.disposition,
            expected_prior_source_epoch: output_state.expected_source_epoch,
            expected_prior_frontier: output_state.expected_sink_frontier,
            observations: Vec::new(),
        };
        let replay = NativeProReplayPage::new_with_source_identity(
            NativeSourceIdentity::new(CaptureProvider::QwenCode.as_str(), &locator_identity),
            expected_frontier,
            next_safe_frontier,
            true,
            NativePageAccounting {
                logical_units: 1,
                conservative_serialized_bytes: 64 * 1024,
            },
            output,
        )
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        let _ = process_pro_replay_only(replay, sink);
    }
    Ok(())
}

struct QwenCodeOutputState {
    source: OutputSourceIdentity,
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_sink_frontier: Option<NativeSafeFrontier>,
    disposition: ProOutputSourceDisposition,
    prior_terminal: bool,
}

impl QwenCodeOutputState {
    fn new(
        source: OutputSourceIdentity,
        progress: Option<ProOutputProgress>,
        source_change: DirectJsonlSourceChange,
        can_resume: bool,
        observed_revision: &str,
        materializer_revision: &str,
    ) -> Result<Self> {
        let Some(progress) = progress else {
            return Ok(Self {
                source,
                source_epoch: 0,
                expected_source_epoch: None,
                expected_sink_frontier: None,
                disposition: ProOutputSourceDisposition::NewSource,
                prior_terminal: false,
            });
        };
        let prior_frontier = progress
            .cursor
            .as_ref()
            .map(|cursor| NativeSafeFrontier::new(cursor.version, cursor.payload.clone()))
            .transpose()
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        let rewrite = !can_resume
            || progress.observed_revision != observed_revision
            || progress.materializer_revision != materializer_revision
            || matches!(
                source_change,
                DirectJsonlSourceChange::Fresh
                    | DirectJsonlSourceChange::Rewrite
                    | DirectJsonlSourceChange::Truncation
                    | DirectJsonlSourceChange::Replacement
            );
        Ok(Self {
            source,
            source_epoch: if rewrite {
                progress
                    .source_epoch
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "Qwen Code output source epoch exhausted",
                    ))?
            } else {
                progress.source_epoch
            },
            expected_source_epoch: Some(progress.source_epoch),
            expected_sink_frontier: prior_frontier,
            disposition: if rewrite {
                ProOutputSourceDisposition::Rewrite
            } else {
                ProOutputSourceDisposition::AppendOrResume
            },
            prior_terminal: progress.terminal,
        })
    }
}

fn replay_missing_outputs_or_mark_behind(
    known_routes: &[KnownQwenCodeRoute],
    live_paths: &BTreeSet<PathBuf>,
    source_root: &Path,
    reason: ProviderSourceRouteRetirementReason,
    sink: Option<&dyn ProOutputSink>,
) -> Result<()> {
    let Some(sink) = sink else {
        return Ok(());
    };
    for route in known_routes
        .iter()
        .filter(|route| !live_paths.contains(&route.path))
    {
        let source = OutputSourceIdentity {
            provider: CaptureProvider::QwenCode.as_str().to_owned(),
            namespace_id: source_root.display().to_string(),
            source_id: route.locator_identity.clone(),
        };
        let progress = match sink.observe_source(&source) {
            Ok(progress) => progress,
            Err(error) => {
                sink.mark_behind(error);
                continue;
            }
        };
        if let Err(error) = replay_missing_output_source(route, reason, source, progress, sink) {
            sink.mark_behind(ProOutputSinkError::new(
                "qwen_code_nativepath_missing_output_source",
                bounded_output_diagnostic(&route.path, &error),
            ));
            if super::driver::capture_error_is_systemic(&error) {
                return Err(error);
            }
        }
    }
    Ok(())
}

fn replay_missing_output_source(
    route: &KnownQwenCodeRoute,
    reason: ProviderSourceRouteRetirementReason,
    source: OutputSourceIdentity,
    progress: Option<ProOutputProgress>,
    sink: &dyn ProOutputSink,
) -> Result<()> {
    let observed_revision = format!(
        "qwen-code-source-unavailable-v1:{}:{}",
        route.source_revision,
        qwen_missing_reason(reason)
    );
    let next_frontier = NativeSafeFrontier::new(
        QWEN_CODE_OUTPUT_FRONTIER_VERSION,
        serde_json::to_vec(&json!({
            "kind": "qwen-code-source-unavailable-v1",
            "locator_identity": route.locator_identity,
            "reason": qwen_missing_reason(reason),
        }))?,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let prior_frontier = progress
        .as_ref()
        .and_then(|progress| progress.cursor.as_ref())
        .map(|cursor| NativeSafeFrontier::new(cursor.version, cursor.payload.clone()))
        .transpose()
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    if progress.as_ref().is_some_and(|progress| {
        progress.terminal
            && progress.observed_revision == observed_revision
            && progress.parser_revision == QWEN_CODE_OUTPUT_PARSER_REVISION
            && progress.materializer_revision == sink.materializer_revision()
            && prior_frontier.as_ref() == Some(&next_frontier)
    }) {
        return Ok(());
    }
    let (source_epoch, expected_source_epoch, disposition) = match progress.as_ref() {
        Some(progress) => (
            progress
                .source_epoch
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "Qwen Code missing output source epoch exhausted",
                ))?,
            Some(progress.source_epoch),
            ProOutputSourceDisposition::Rewrite,
        ),
        None => (0, None, ProOutputSourceDisposition::NewSource),
    };
    let replay = NativeProReplayPage::new_with_source_identity(
        NativeSourceIdentity::new(CaptureProvider::QwenCode.as_str(), &route.locator_identity),
        prior_frontier
            .clone()
            .unwrap_or_else(|| next_frontier.clone()),
        next_frontier,
        true,
        NativePageAccounting {
            logical_units: 1,
            conservative_serialized_bytes: 64 * 1024,
        },
        NativeProOutputPage {
            inventory_generation: sink.inventory_generation(),
            source,
            source_epoch,
            observed_revision,
            parser_revision: QWEN_CODE_OUTPUT_PARSER_REVISION.to_owned(),
            materializer_revision: sink.materializer_revision().to_owned(),
            disposition,
            expected_prior_source_epoch: expected_source_epoch,
            expected_prior_frontier: prior_frontier,
            observations: Vec::new(),
        },
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let _ = process_pro_replay_only(replay, sink);
    Ok(())
}

fn qwen_missing_reason(reason: ProviderSourceRouteRetirementReason) -> &'static str {
    match reason {
        ProviderSourceRouteRetirementReason::SourceMissing => "source_missing",
        ProviderSourceRouteRetirementReason::RootMissing => "root_missing",
        ProviderSourceRouteRetirementReason::Replaced => "replaced",
    }
}

fn bounded_output_diagnostic(path: &Path, error: &CaptureError) -> String {
    format!("{}: {error}", path.display())
        .chars()
        .take(1_024)
        .collect()
}

fn safe_frontier(checkpoint: &DirectJsonlCheckpoint) -> Result<NativeSafeFrontier> {
    NativeSafeFrontier::new(
        QWEN_CODE_OUTPUT_FRONTIER_VERSION,
        serde_json::to_vec(checkpoint)?,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

fn output_observation(
    checkpoint: &DirectJsonlCheckpoint,
    path: &Path,
    output: DirectJsonlOutput,
) -> ProOutputObservation {
    let session = checkpoint.session.as_ref();
    let direct_session_id = session
        .map(|session| session.provider_session_id.clone())
        .unwrap_or_else(|| "unknown-session".to_owned());
    ProOutputObservation {
        kind: OutputObservationKind::Tool,
        coordinate: OutputNativeCoordinate {
            unit_key: format!("{}:{}", output.raw_ordinal, output.sub_ordinal),
            native_sequence: output.raw_ordinal,
            native_record_id: output.native_record_id.clone(),
            source_record_ordinal: Some(output.raw_ordinal),
            source_record_subrecord_index: Some(output.sub_ordinal),
            byte_start: Some(output.byte_start),
            byte_end_exclusive: Some(output.byte_end_exclusive),
        },
        occurred_at_unix_ms: None,
        associations: OutputAssociations {
            direct_session_id: direct_session_id.clone(),
            root_session_id: session
                .and_then(|session| session.root_provider_session_id.clone())
                .unwrap_or_else(|| direct_session_id.clone()),
            parent_session_id: session
                .and_then(|session| session.parent_provider_session_id.clone()),
            provider_session_id: Some(direct_session_id),
            agent_id: session.and_then(|session| session.external_agent_id.clone()),
            repository: None,
        },
        call_id: output.call_id,
        command: None,
        outcome: OutputOutcomeMetadata {
            outcome: output.outcome,
            exit_code: output.exit_code,
            duration_ms: output.duration_ms,
        },
        locator: OutputSourceLocator {
            version: 1,
            kind: "qwen-code-jsonl-range-v1".to_owned(),
            payload: serde_json::to_vec(&json!({
                "path": path,
                "byte_start": output.byte_start,
                "byte_end_exclusive": output.byte_end_exclusive,
            }))
            .unwrap_or_default(),
        },
        content: output.content,
    }
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
                CaptureProvider::QwenCode.as_str(),
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

fn retirement_publication_id(retirement: &ProviderSourceRouteRetirement) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-qwen-code-nativepath-route-retirement-v1\0");
    digest.update(retirement.provider.as_str().as_bytes());
    digest.update(retirement.source_format.as_bytes());
    digest.update(retirement.machine_id.as_bytes());
    digest.update(retirement.locator_identity.as_bytes());
    digest.update(retirement.cursor_stream.as_bytes());
    digest.update(retirement.expected_canonical_source_identity.as_bytes());
    digest.update(retirement.expected_source_revision.as_bytes());
    digest.update(format!("{:?}", retirement.reason).as_bytes());
    format!("qwen-code-nativepath-retirement-v1:{:x}", digest.finalize())
}
#[path = "qwen_code_records.rs"]
mod records;
pub(super) use records::{
    enumerate_qwen_code_results, qwen_code_event_text, qwen_code_event_type, qwen_code_header_cwd,
    qwen_code_header_session_id, qwen_code_model, qwen_code_role,
};

#[cfg(test)]
#[path = "qwen_code_tests.rs"]
mod tests;
