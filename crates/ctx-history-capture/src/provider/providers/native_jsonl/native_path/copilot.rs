use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, SyncCursor};
use ctx_history_store::{
    decode_native_path_committed_cursor, NativePathCursorSetClassification,
    NativePathCursorTransition, NativePathGroupAccounting, ProviderSourceRouteRetirement,
    ProviderSourceRouteRetirementDisposition, ProviderSourceRouteRetirementReason, Store,
    StoreError,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{
    provider::{
        importer::{
            provider_path_identity, provider_source_cursor_stream_for_path, timestamps,
            CertifiedProviderCursor,
        },
        native_ingestion::{
            process_pro_replay_only, NativePageAccounting, NativeProOutputPage,
            NativeProReplayPage, NativeSafeFrontier, NativeSourceIdentity,
        },
    },
    stable_capture_uuid, CaptureError, ImportProfile, OutputAssociations, OutputNativeCoordinate,
    OutputObservationKind, OutputOutcomeMetadata, OutputSourceIdentity, OutputSourceLocator,
    ProOutputObservation, ProOutputProgress, ProOutputSink, ProOutputSourceDisposition,
    ProviderImportSummary, ProviderImportWorkResult, Result, COPILOT_CLI_SOURCE_FORMAT,
};

#[cfg(test)]
use crate::ProOutputSinkError;

use super::{
    committed_direct_jsonl_replay_authority, decode_direct_jsonl_native_cursor,
    direct_jsonl_checkpoint_is_covered_by, import_direct_native_jsonl_tree_core,
    open_direct_jsonl_pages, reader::direct_jsonl_source_revision, DirectJsonlCheckpoint,
    DirectJsonlOutput, DirectJsonlSourceChange, NativePathJsonlTreeImport,
};

const COPILOT_OUTPUT_FRONTIER_VERSION: u32 = 1;
const COPILOT_OUTPUT_PARSER_REVISION: &str = "copilot-cli-direct-native-jsonl-v1";

pub(crate) const fn copilot_source_backed_adapter() -> super::DirectJsonlSourceAdapter {
    super::DirectJsonlSourceAdapter::new(
        CaptureProvider::CopilotCli,
        COPILOT_CLI_SOURCE_FORMAT,
        "copilot-cli-direct-native-jsonl-v1",
    )
}

pub(super) fn copilot_event_identity(value: &Value) -> Option<&str> {
    value
        .get("id")
        .and_then(Value::as_str)
        .filter(|event_id| !event_id.trim().is_empty())
}

pub(crate) fn import_copilot_nativepath_tree(
    store: &mut Store,
    request: NativePathJsonlTreeImport<'_>,
) -> Result<ProviderImportSummary> {
    let configured_source_root = request
        .source_root
        .clone()
        .or(request.source_path.clone())
        .unwrap_or_else(|| request.path.to_path_buf());
    let live_inventory = discover_live_transcripts(request.path)?;
    let (known_routes, had_persisted_routes) =
        known_copilot_routes(store, &request.machine_id, &configured_source_root)?;
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
        return Ok(ProviderImportSummary::default());
    }

    if live_inventory.paths.is_empty() {
        if known_routes.is_empty() {
            if had_persisted_routes {
                return Ok(ProviderImportSummary::default());
            }
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: request.path.to_path_buf(),
                reason: super::super::dialect::native_jsonl_missing_reason(
                    CaptureProvider::CopilotCli,
                ),
            });
        }
        return retire_missing_routes(
            store,
            &request.machine_id,
            request.imported_at,
            &known_routes,
            &live_inventory.paths,
            if live_inventory.root_missing {
                ProviderSourceRouteRetirementReason::RootMissing
            } else {
                ProviderSourceRouteRetirementReason::SourceMissing
            },
        );
    }

    let mut summary = import_direct_native_jsonl_tree_core(
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
        CaptureProvider::CopilotCli,
        COPILOT_CLI_SOURCE_FORMAT,
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
    Ok(summary)
}

struct CopilotInventory {
    paths: BTreeSet<PathBuf>,
    root_missing: bool,
}

fn discover_live_transcripts(root: &Path) -> Result<CopilotInventory> {
    if super::super::traversal::native_jsonl_root_kind(root)?.is_none() {
        return Ok(CopilotInventory {
            paths: BTreeSet::new(),
            root_missing: true,
        });
    }
    let mut paths = BTreeSet::new();
    super::super::traversal::visit_jsonl_tree_files(
        CaptureProvider::CopilotCli,
        root,
        &mut |source_file| {
            paths.insert(source_file.path().to_path_buf());
            Ok(())
        },
    )?;
    Ok(CopilotInventory {
        paths,
        root_missing: false,
    })
}

#[derive(Clone)]
struct KnownCopilotRoute {
    path: PathBuf,
    locator_identity: String,
    canonical_source_identity: String,
    source_revision: String,
    current_cursor: SyncCursor,
    provider_cursor: String,
}

fn known_copilot_routes(
    store: &Store,
    machine_id: &str,
    source_root: &Path,
) -> Result<(Vec<KnownCopilotRoute>, bool)> {
    let source_root = source_root.display().to_string();
    let mut routes = BTreeMap::<String, KnownCopilotRoute>::new();
    let mut had_persisted_routes = false;
    for source in store.list_capture_sources()? {
        if source.descriptor.provider != CaptureProvider::CopilotCli
            || source.descriptor.machine_id != machine_id
            || source.descriptor.source_format.as_deref() != Some(COPILOT_CLI_SOURCE_FORMAT)
            || source.descriptor.source_root.as_deref() != Some(source_root.as_str())
        {
            continue;
        }
        had_persisted_routes = true;
        let (Some(raw_source_path), Some(canonical_source_identity)) = (
            source.descriptor.raw_source_path.as_deref(),
            source.descriptor.source_identity.as_deref(),
        ) else {
            continue;
        };
        if !capture_source_route_is_current(store, source.id)? {
            continue;
        }
        let path = PathBuf::from(raw_source_path);
        let locator_identity = provider_path_identity(&path)?;
        let stream = provider_source_cursor_stream_for_path(
            CaptureProvider::CopilotCli,
            COPILOT_CLI_SOURCE_FORMAT,
            &locator_identity,
        );
        let Some(current_cursor) = store.get_sync_cursor(None, machine_id, &stream)? else {
            continue;
        };
        let provider_cursor = decode_native_path_committed_cursor(&current_cursor.cursor)
            .map(|cursor| cursor.provider_cursor().to_owned())
            .unwrap_or_else(|_| current_cursor.cursor.clone());
        if let Some(checkpoint) = decode_direct_jsonl_native_cursor(
            &provider_cursor,
            CaptureProvider::CopilotCli,
            COPILOT_CLI_SOURCE_FORMAT,
        ) {
            let checkpoint_session = checkpoint
                .session
                .as_ref()
                .map(|session| session.provider_session_id.as_str());
            if checkpoint.source_path != path
                || source.descriptor.external_session_id.as_deref() != checkpoint_session
            {
                continue;
            }
        } else if CertifiedProviderCursor::decode_if_certified(&provider_cursor)?.is_none() {
            continue;
        }
        let Some(source_revision) = source
            .sync
            .metadata
            .get("source_revision")
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        let route = KnownCopilotRoute {
            path,
            locator_identity: locator_identity.clone(),
            canonical_source_identity: canonical_source_identity.to_owned(),
            source_revision: source_revision.to_owned(),
            current_cursor,
            provider_cursor,
        };
        if routes.insert(locator_identity, route).is_some() {
            return Err(CaptureError::SystemInvariant(
                "Copilot persisted duplicate current routes for one transcript",
            ));
        }
    }
    Ok((routes.into_values().collect(), had_persisted_routes))
}

fn capture_source_route_is_current(store: &Store, capture_source_id: uuid::Uuid) -> Result<bool> {
    for session in store
        .list_sessions()?
        .into_iter()
        .filter(|session| session.capture_source_id == Some(capture_source_id))
    {
        for event in store.events_for_session(session.id)? {
            match store.authorized_source_route_for_event(event.id) {
                Ok(_) => return Ok(true),
                Err(StoreError::AuthorizedSourceRouteUnavailable { .. }) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(false)
}

fn retire_missing_routes(
    store: &mut Store,
    machine_id: &str,
    retired_at: DateTime<Utc>,
    known_routes: &[KnownCopilotRoute],
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
    route: &KnownCopilotRoute,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<bool> {
    let stream = route.current_cursor.stream.clone();
    let provider_cursor = route.provider_cursor.clone();
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
        provider: CaptureProvider::CopilotCli,
        source_format: COPILOT_CLI_SOURCE_FORMAT.to_owned(),
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
    replay_copilot_outputs(store, machine_id, paths, source_root, imported_at, sink)
}

fn replay_copilot_outputs(
    store: &Store,
    machine_id: &str,
    paths: &BTreeSet<PathBuf>,
    source_root: &Path,
    imported_at: DateTime<Utc>,
    sink: &dyn ProOutputSink,
) -> Result<()> {
    super::driver::replay_selected_output_sources(
        paths,
        sink,
        "copilot_nativepath_output_source",
        |path| {
            let authority = committed_direct_jsonl_replay_authority(
                store,
                machine_id,
                CaptureProvider::CopilotCli,
                COPILOT_CLI_SOURCE_FORMAT,
                path,
            )?;
            let locator_identity = provider_path_identity(path)?;
            let source = OutputSourceIdentity {
                provider: CaptureProvider::CopilotCli.as_str().to_owned(),
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
            replay_copilot_source(
                path,
                imported_at,
                sink,
                source,
                locator_identity,
                progress,
                &authority,
            )
        },
    )
}

fn replay_copilot_source(
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
        .filter(|cursor| cursor.version == COPILOT_OUTPUT_FRONTIER_VERSION)
        .and_then(|cursor| serde_json::from_slice::<DirectJsonlCheckpoint>(&cursor.payload).ok())
        .filter(|checkpoint| {
            checkpoint.is_supported_for(CaptureProvider::CopilotCli, COPILOT_CLI_SOURCE_FORMAT)
        });
    let can_resume = progress.as_ref().is_some_and(|progress| {
        progress.parser_revision == COPILOT_OUTPUT_PARSER_REVISION
            && progress.materializer_revision == sink.materializer_revision()
            && progress_cursor.is_some()
    });
    let previous = if can_resume {
        progress_cursor.as_ref()
    } else {
        None
    };
    let mut reader = open_direct_jsonl_pages(
        CaptureProvider::CopilotCli,
        COPILOT_CLI_SOURCE_FORMAT,
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
    let mut output_state = CopilotOutputState::new(
        output_source,
        progress,
        source_change,
        can_resume,
        sink.materializer_revision(),
    )?;

    while let Some(page) = reader.next_page()? {
        if !direct_jsonl_checkpoint_is_covered_by(authority, &page.next_checkpoint) {
            return Err(CaptureError::InvalidPayload(
                "Copilot CLI output replay advanced beyond committed Core authority".to_owned(),
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
            conservative_serialized_bytes: page.conservative_serialized_bytes,
        };
        let output = NativeProOutputPage {
            inventory_generation: sink.inventory_generation(),
            source: output_state.source.clone(),
            source_epoch: output_state.source_epoch,
            observed_revision: observed_revision.clone(),
            parser_revision: COPILOT_OUTPUT_PARSER_REVISION.to_owned(),
            materializer_revision: sink.materializer_revision().to_owned(),
            disposition: output_state.disposition,
            expected_prior_source_epoch: output_state.expected_source_epoch,
            expected_prior_frontier: output_state.expected_sink_frontier.clone(),
            observations,
        };
        let replay = NativeProReplayPage::new_with_source_identity(
            NativeSourceIdentity::new(CaptureProvider::CopilotCli.as_str(), &locator_identity),
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
        "Copilot CLI output replay reader completed without an outcome",
    ))?;
    if !outcome.checkpoint.terminal
        || !direct_jsonl_checkpoint_is_covered_by(authority, &outcome.checkpoint)
    {
        return Err(CaptureError::InvalidPayload(
            "Copilot CLI output replay outcome exceeded committed Core authority".to_owned(),
        ));
    }
    Ok(())
}

struct CopilotOutputState {
    source: OutputSourceIdentity,
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_sink_frontier: Option<NativeSafeFrontier>,
    disposition: ProOutputSourceDisposition,
}

impl CopilotOutputState {
    fn new(
        source: OutputSourceIdentity,
        progress: Option<ProOutputProgress>,
        source_change: DirectJsonlSourceChange,
        can_resume: bool,
        materializer_revision: &str,
    ) -> Result<Self> {
        let Some(progress) = progress else {
            return Ok(Self {
                source,
                source_epoch: 0,
                expected_source_epoch: None,
                expected_sink_frontier: None,
                disposition: ProOutputSourceDisposition::NewSource,
            });
        };
        let prior_frontier = progress
            .cursor
            .as_ref()
            .map(|cursor| NativeSafeFrontier::new(cursor.version, cursor.payload.clone()))
            .transpose()
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        let rewrite = !can_resume
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
                        "Copilot output source epoch exhausted",
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
        })
    }
}

fn safe_frontier(checkpoint: &DirectJsonlCheckpoint) -> Result<NativeSafeFrontier> {
    NativeSafeFrontier::new(
        COPILOT_OUTPUT_FRONTIER_VERSION,
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
            kind: "copilot-cli-jsonl-range-v1".to_owned(),
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
                CaptureProvider::CopilotCli.as_str(),
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
    digest.update(b"ctx-copilot-nativepath-route-retirement-v1\0");
    digest.update(retirement.provider.as_str().as_bytes());
    digest.update(retirement.source_format.as_bytes());
    digest.update(retirement.machine_id.as_bytes());
    digest.update(retirement.locator_identity.as_bytes());
    digest.update(retirement.cursor_stream.as_bytes());
    digest.update(retirement.expected_canonical_source_identity.as_bytes());
    digest.update(retirement.expected_source_revision.as_bytes());
    digest.update(format!("{:?}", retirement.reason).as_bytes());
    format!("copilot-nativepath-retirement-v1:{:x}", digest.finalize())
}
#[cfg(test)]
#[path = "copilot_tests.rs"]
mod tests;
