use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, SyncCursor};
use ctx_history_store::{
    NativePathCursorSetClassification, NativePathCursorTransition, NativePathGroupAccounting,
    ProviderSourceRouteRetirement, ProviderSourceRouteRetirementDisposition,
    ProviderSourceRouteRetirementReason, Store,
};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::{
    provider::{
        importer::{provider_path_identity, provider_source_cursor_stream_for_path, timestamps},
        native_ingestion::{
            process_pro_replay_only, NativePageAccounting, NativeProOutputPage,
            NativeProReplayPage, NativeSafeFrontier, NativeSourceIdentity,
        },
    },
    stable_capture_uuid, CaptureError, ImportProfile, OutputAssociations, OutputNativeCoordinate,
    OutputObservationKind, OutputOutcomeMetadata, OutputSourceIdentity, OutputSourceLocator,
    ProOutputObservation, ProOutputProgress, ProOutputSink, ProOutputSinkError,
    ProOutputSourceDisposition, ProviderImportSummary, ProviderImportWorkResult, Result,
    ANTIGRAVITY_CLI_SOURCE_FORMAT,
};

use super::{
    committed_direct_jsonl_replay_authority, decode_direct_jsonl_native_cursor,
    direct_jsonl_checkpoint_is_covered_by, encode_direct_jsonl_cursor,
    import_direct_native_jsonl_tree_core, open_direct_jsonl_pages,
    reader::direct_jsonl_source_revision, DirectJsonlCheckpoint, DirectJsonlOutput,
    DirectJsonlSourceChange, NativePathJsonlTreeImport,
};

const ANTIGRAVITY_OUTPUT_FRONTIER_VERSION: u32 = 1;
const ANTIGRAVITY_OUTPUT_PARSER_REVISION: &str = "antigravity-direct-native-jsonl-v1";

pub(crate) fn import_antigravity_nativepath_tree(
    store: &mut Store,
    request: NativePathJsonlTreeImport<'_>,
) -> Result<ProviderImportSummary> {
    let configured_source_root = request
        .source_root
        .clone()
        .or(request.source_path.clone())
        .unwrap_or_else(|| request.path.to_path_buf());
    let live_inventory = discover_live_transcripts(request.path)?;
    let known_routes =
        known_antigravity_routes(store, &request.machine_id, &configured_source_root)?;
    let sink = request.import_profile.sink().cloned();

    if request.import_profile.is_replay_only() {
        replay_outputs_or_mark_behind(
            store,
            &request.machine_id,
            &live_inventory.paths,
            &configured_source_root,
            request.imported_at,
            sink.as_deref(),
        );
        return Ok(ProviderImportSummary::default());
    }

    if live_inventory.paths.is_empty() {
        if known_routes.is_empty() {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: request.path.to_path_buf(),
                reason: super::super::dialect::native_jsonl_missing_reason(
                    CaptureProvider::Antigravity,
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
        CaptureProvider::Antigravity,
        ANTIGRAVITY_CLI_SOURCE_FORMAT,
    )?;
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
    );
    Ok(summary)
}

struct AntigravityInventory {
    paths: BTreeSet<PathBuf>,
    root_missing: bool,
}

fn discover_live_transcripts(root: &Path) -> Result<AntigravityInventory> {
    match std::fs::symlink_metadata(root) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(AntigravityInventory {
                paths: BTreeSet::new(),
                root_missing: true,
            });
        }
        Err(error) => return Err(error.into()),
    }
    let mut paths = BTreeSet::new();
    super::super::traversal::visit_jsonl_tree_files(
        root,
        &|path| {
            super::super::dialect::native_jsonl_file_is_selected(CaptureProvider::Antigravity, path)
        },
        &mut |path| {
            paths.insert(std::fs::canonicalize(path)?);
            Ok(())
        },
    )?;
    Ok(AntigravityInventory {
        paths,
        root_missing: false,
    })
}

#[derive(Clone)]
struct KnownAntigravityRoute {
    path: PathBuf,
    locator_identity: String,
    canonical_source_identity: String,
    source_revision: String,
    current_cursor: SyncCursor,
    checkpoint: DirectJsonlCheckpoint,
}

fn known_antigravity_routes(
    store: &Store,
    machine_id: &str,
    source_root: &Path,
) -> Result<Vec<KnownAntigravityRoute>> {
    let source_root = source_root.display().to_string();
    let mut routes = BTreeMap::<String, KnownAntigravityRoute>::new();
    for source in store.list_capture_sources()? {
        if source.descriptor.provider != CaptureProvider::Antigravity
            || source.descriptor.machine_id != machine_id
            || source.descriptor.source_format.as_deref() != Some(ANTIGRAVITY_CLI_SOURCE_FORMAT)
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
            CaptureProvider::Antigravity,
            ANTIGRAVITY_CLI_SOURCE_FORMAT,
            &locator_identity,
        );
        let Some(current_cursor) = store.get_sync_cursor(None, machine_id, &stream)? else {
            continue;
        };
        let Some(checkpoint) = decode_direct_jsonl_native_cursor(
            &current_cursor.cursor,
            CaptureProvider::Antigravity,
            ANTIGRAVITY_CLI_SOURCE_FORMAT,
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
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        let route = KnownAntigravityRoute {
            path,
            locator_identity: locator_identity.clone(),
            canonical_source_identity: canonical_source_identity.to_owned(),
            source_revision: source_revision.to_owned(),
            current_cursor,
            checkpoint,
        };
        if routes.insert(locator_identity, route).is_some() {
            return Err(CaptureError::SystemInvariant(
                "Antigravity persisted duplicate current routes for one transcript",
            ));
        }
    }
    Ok(routes.into_values().collect())
}

fn retire_missing_routes(
    store: &mut Store,
    machine_id: &str,
    retired_at: DateTime<Utc>,
    known_routes: &[KnownAntigravityRoute],
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
    route: &KnownAntigravityRoute,
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
        provider: CaptureProvider::Antigravity,
        source_format: ANTIGRAVITY_CLI_SOURCE_FORMAT.to_owned(),
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
) {
    let Some(sink) = sink else {
        return;
    };
    if let Err(error) =
        replay_antigravity_outputs(store, machine_id, paths, source_root, imported_at, sink)
    {
        sink.mark_behind(ProOutputSinkError::new(
            "antigravity_nativepath_output_replay",
            error.to_string(),
        ));
    }
}

fn replay_antigravity_outputs(
    store: &Store,
    machine_id: &str,
    paths: &BTreeSet<PathBuf>,
    source_root: &Path,
    imported_at: DateTime<Utc>,
    sink: &dyn ProOutputSink,
) -> Result<()> {
    for path in paths {
        let authority = committed_direct_jsonl_replay_authority(
            store,
            machine_id,
            CaptureProvider::Antigravity,
            ANTIGRAVITY_CLI_SOURCE_FORMAT,
            path,
        )?;
        let locator_identity = provider_path_identity(path)?;
        let source = OutputSourceIdentity {
            provider: CaptureProvider::Antigravity.as_str().to_owned(),
            namespace_id: source_root.display().to_string(),
            source_id: locator_identity.clone(),
        };
        let progress = match sink.observe_source(&source) {
            Ok(progress) => progress,
            Err(error) => {
                sink.mark_behind(error);
                continue;
            }
        };
        replay_antigravity_source(
            path,
            imported_at,
            sink,
            source,
            locator_identity,
            progress,
            &authority,
        )?;
    }
    Ok(())
}

fn replay_antigravity_source(
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
        .filter(|cursor| cursor.version == ANTIGRAVITY_OUTPUT_FRONTIER_VERSION)
        .and_then(|cursor| serde_json::from_slice::<DirectJsonlCheckpoint>(&cursor.payload).ok())
        .filter(|checkpoint| {
            checkpoint.is_supported_for(CaptureProvider::Antigravity, ANTIGRAVITY_CLI_SOURCE_FORMAT)
        });
    let can_resume = progress.as_ref().is_some_and(|progress| {
        progress.parser_revision == ANTIGRAVITY_OUTPUT_PARSER_REVISION
            && progress.materializer_revision == sink.materializer_revision()
            && progress_cursor.is_some()
    });
    let previous = if can_resume {
        progress_cursor.as_ref()
    } else {
        None
    };
    let mut reader = open_direct_jsonl_pages(
        CaptureProvider::Antigravity,
        ANTIGRAVITY_CLI_SOURCE_FORMAT,
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
    let mut output_state = AntigravityOutputState::new(
        output_source,
        progress,
        source_change,
        can_resume,
        sink.materializer_revision(),
    )?;

    while let Some(page) = reader.next_page()? {
        if !direct_jsonl_checkpoint_is_covered_by(authority, &page.next_checkpoint) {
            return Err(CaptureError::InvalidPayload(
                "Antigravity output replay advanced beyond committed Core authority".to_owned(),
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
            parser_revision: ANTIGRAVITY_OUTPUT_PARSER_REVISION.to_owned(),
            materializer_revision: sink.materializer_revision().to_owned(),
            disposition: output_state.disposition,
            expected_prior_source_epoch: output_state.expected_source_epoch,
            expected_prior_frontier: output_state.expected_sink_frontier.clone(),
            observations,
        };
        let replay = NativeProReplayPage::new_with_source_identity(
            NativeSourceIdentity::new(CaptureProvider::Antigravity.as_str(), &locator_identity),
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
        "Antigravity output replay reader completed without an outcome",
    ))?;
    if !outcome.checkpoint.terminal
        || !direct_jsonl_checkpoint_is_covered_by(authority, &outcome.checkpoint)
    {
        return Err(CaptureError::InvalidPayload(
            "Antigravity output replay outcome exceeded committed Core authority".to_owned(),
        ));
    }
    Ok(())
}

struct AntigravityOutputState {
    source: OutputSourceIdentity,
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_sink_frontier: Option<NativeSafeFrontier>,
    disposition: ProOutputSourceDisposition,
}

impl AntigravityOutputState {
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
                        "Antigravity output source epoch exhausted",
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
        ANTIGRAVITY_OUTPUT_FRONTIER_VERSION,
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
            native_record_id: output.call_id.clone(),
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
            kind: "antigravity-jsonl-range-v1".to_owned(),
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
                CaptureProvider::Antigravity.as_str(),
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
    digest.update(b"ctx-antigravity-nativepath-route-retirement-v1\0");
    digest.update(retirement.provider.as_str().as_bytes());
    digest.update(retirement.source_format.as_bytes());
    digest.update(retirement.machine_id.as_bytes());
    digest.update(retirement.locator_identity.as_bytes());
    digest.update(retirement.cursor_stream.as_bytes());
    digest.update(retirement.expected_canonical_source_identity.as_bytes());
    digest.update(retirement.expected_source_revision.as_bytes());
    digest.update(format!("{:?}", retirement.reason).as_bytes());
    format!(
        "antigravity-nativepath-retirement-v1:{:x}",
        digest.finalize()
    )
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::Write,
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc, Mutex,
        },
    };

    use ctx_history_core::EventType;
    use serde_json::{json, Value};

    use super::*;
    use crate::{
        test_support_paths::tempdir, AntigravityCliImportOptions, ProOutputMaterializationPage,
        ProOutputPageResult,
    };

    const MACHINE: &str = "antigravity-nativepath-test-machine";

    #[test]
    fn production_lifecycle_covers_all_source_changes_and_retires_disappearance() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("brain");
        let transcript = transcript_path(&root);
        write_transcript(
            &transcript,
            &[
                record(0, "USER_INPUT", "fresh-user"),
                record(1, "PLANNER_RESPONSE", "fresh-assistant"),
                tool_call(2, "write README"),
            ],
        );
        let store_path = temp.path().join("work.sqlite");
        let mut store = Store::open(&store_path).unwrap();

        let fresh = import(&root, &mut store, ImportProfile::CoreOnly);
        assert_eq!(fresh.work_result(), ProviderImportWorkResult::Changed);
        assert_eq!(fresh.imported_sessions, 1);
        assert_eq!(fresh.imported_events, 3);
        let session = store
            .list_sessions()
            .unwrap()
            .into_iter()
            .find(|session| session.provider == CaptureProvider::Antigravity)
            .unwrap();
        let original_events = store.events_for_session(session.id).unwrap();
        assert_eq!(original_events.len(), 3);
        assert!(original_events.iter().all(|event| !matches!(
            event.event_type,
            EventType::ToolOutput | EventType::CommandOutput
        )));
        let routed_event = original_events[0].id;
        assert!(store
            .authorized_source_route_for_event(routed_event)
            .is_ok());

        let previous = checkpoint(&store, &transcript);
        assert_eq!(
            classify(&transcript, &root, &previous),
            DirectJsonlSourceChange::Unchanged
        );
        let noop = import(&root, &mut store, ImportProfile::CoreOnly);
        assert_eq!(noop.work_result(), ProviderImportWorkResult::NoOp);

        drop(store);
        let mut store = Store::open(&store_path).unwrap();
        let restart = import(&root, &mut store, ImportProfile::CoreOnly);
        assert_eq!(restart.work_result(), ProviderImportWorkResult::NoOp);

        let previous = checkpoint(&store, &transcript);
        append_record(&transcript, &record(3, "PLANNER_RESPONSE", "append"));
        assert_eq!(
            classify(&transcript, &root, &previous),
            DirectJsonlSourceChange::Append
        );
        let append = import(&root, &mut store, ImportProfile::CoreOnly);
        assert_eq!(append.work_result(), ProviderImportWorkResult::Changed);
        assert_eq!(append.imported_events, 1);

        let previous = checkpoint(&store, &transcript);
        write_transcript(
            &transcript,
            &[
                record(0, "USER_INPUT", &"rewrite-user-content-".repeat(24)),
                record(
                    1,
                    "PLANNER_RESPONSE",
                    &"rewrite-assistant-content-".repeat(24),
                ),
            ],
        );
        assert_eq!(
            classify(&transcript, &root, &previous),
            DirectJsonlSourceChange::Rewrite
        );
        assert_eq!(
            import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
            ProviderImportWorkResult::Changed
        );

        let previous = checkpoint(&store, &transcript);
        write_transcript(&transcript, &[record(0, "USER_INPUT", "short")]);
        assert_eq!(
            classify(&transcript, &root, &previous),
            DirectJsonlSourceChange::Truncation
        );
        assert_eq!(
            import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
            ProviderImportWorkResult::Changed
        );

        let previous = checkpoint(&store, &transcript);
        let replacement = transcript.with_extension("replacement");
        write_transcript(
            &replacement,
            &[record(0, "USER_INPUT", "replacement-generation")],
        );
        fs::rename(&replacement, &transcript).unwrap();
        assert_eq!(
            classify(&transcript, &root, &previous),
            DirectJsonlSourceChange::Replacement
        );
        assert_eq!(
            import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
            ProviderImportWorkResult::Changed
        );

        fs::remove_dir_all(&root).unwrap();
        let disappeared = import(&root, &mut store, ImportProfile::CoreOnly);
        assert_eq!(disappeared.work_result(), ProviderImportWorkResult::Changed);
        assert!(store
            .authorized_source_route_for_event(routed_event)
            .is_err());
        let repeated = import(&root, &mut store, ImportProfile::CoreOnly);
        assert_eq!(repeated.work_result(), ProviderImportWorkResult::NoOp);
    }

    #[test]
    fn production_is_core_first_with_independent_pro_replay() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("brain");
        let transcript = transcript_path(&root);
        write_transcript(
            &transcript,
            &[
                record(0, "USER_INPUT", "core-first"),
                tool_call(1, "call-only-no-output-body"),
            ],
        );
        let store_path = temp.path().join("core.sqlite");
        let mut store = Store::open(&store_path).unwrap();
        let sink = Arc::new(RecordingSink::new(store_path.clone()));

        let fresh = import(&root, &mut store, ImportProfile::CoreAndPro(sink.clone()));
        assert_eq!(fresh.work_result(), ProviderImportWorkResult::Changed);
        assert!(sink.saw_core_before_page.load(Ordering::SeqCst));
        assert!(sink.pages.load(Ordering::SeqCst) > 0);
        assert_eq!(sink.outputs.load(Ordering::SeqCst), 0);
        let pages_after_fresh = sink.pages.load(Ordering::SeqCst);

        let noop = import(&root, &mut store, ImportProfile::CoreAndPro(sink.clone()));
        assert_eq!(noop.work_result(), ProviderImportWorkResult::NoOp);
        assert_eq!(sink.pages.load(Ordering::SeqCst), pages_after_fresh);

        let pro_only_path = temp.path().join("pro-only.sqlite");
        let mut pro_only_store = Store::open(&pro_only_path).unwrap();
        let pro_only_sink = Arc::new(RecordingSink::new(pro_only_path));
        let replay = import(
            &root,
            &mut pro_only_store,
            ImportProfile::ProReplayOnly(pro_only_sink.clone()),
        );
        assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
        assert!(pro_only_store.list_sessions().unwrap().is_empty());
        assert!(!pro_only_sink.saw_core_before_page.load(Ordering::SeqCst));
        assert_eq!(pro_only_sink.pages.load(Ordering::SeqCst), 0);
        assert_eq!(pro_only_sink.outputs.load(Ordering::SeqCst), 0);
    }

    struct RecordingSink {
        store_path: PathBuf,
        progress: Mutex<Option<ProOutputProgress>>,
        pages: AtomicUsize,
        outputs: AtomicUsize,
        saw_core_before_page: AtomicBool,
    }

    impl RecordingSink {
        fn new(store_path: PathBuf) -> Self {
            Self {
                store_path,
                progress: Mutex::new(None),
                pages: AtomicUsize::new(0),
                outputs: AtomicUsize::new(0),
                saw_core_before_page: AtomicBool::new(false),
            }
        }
    }

    impl ProOutputSink for RecordingSink {
        fn inventory_generation(&self) -> u64 {
            1
        }

        fn materializer_revision(&self) -> &str {
            "antigravity-nativepath-test-materializer-v1"
        }

        fn observe_source(
            &self,
            _source: &OutputSourceIdentity,
        ) -> std::result::Result<Option<ProOutputProgress>, ProOutputSinkError> {
            Ok(self.progress.lock().unwrap().clone())
        }

        fn materialize_page(
            &self,
            page: ProOutputMaterializationPage,
        ) -> std::result::Result<ProOutputPageResult, ProOutputSinkError> {
            let core = Store::open_read_only(&self.store_path)
                .map_err(|error| ProOutputSinkError::new("test_store", error.to_string()))?;
            if !core
                .list_sessions()
                .map_err(|error| ProOutputSinkError::new("test_sessions", error.to_string()))?
                .is_empty()
            {
                self.saw_core_before_page.store(true, Ordering::SeqCst);
            }
            self.pages.fetch_add(1, Ordering::SeqCst);
            self.outputs
                .fetch_add(page.observations.len(), Ordering::SeqCst);
            let committed_cursor = page.next_safe_cursor.clone();
            *self.progress.lock().unwrap() = Some(ProOutputProgress {
                source_epoch: page.source_epoch,
                observed_revision: page.observed_revision.clone(),
                cursor: Some(committed_cursor.clone()),
                parser_revision: page.parser_revision.clone(),
                materializer_revision: page.materializer_revision.clone(),
                terminal: page.terminal,
            });
            Ok(ProOutputPageResult {
                source_epoch: page.source_epoch,
                committed_cursor,
                accepted_outputs: u32::try_from(page.observations.len()).unwrap(),
                materialized_facts: 0,
                replayed: false,
            })
        }
    }

    fn import(
        root: &Path,
        store: &mut Store,
        import_profile: ImportProfile,
    ) -> ProviderImportSummary {
        crate::import_antigravity_cli_history(
            root,
            store,
            AntigravityCliImportOptions {
                machine_id: MACHINE.to_owned(),
                source_path: Some(root.to_path_buf()),
                imported_at: "2026-07-25T12:00:00Z".parse().unwrap(),
                import_profile,
                ..AntigravityCliImportOptions::default()
            },
        )
        .unwrap()
    }

    fn transcript_path(root: &Path) -> PathBuf {
        root.join("agy-life/.system_generated/logs/transcript_full.jsonl")
    }

    fn record(step: u64, kind: &str, content: &str) -> Value {
        json!({
            "step_index": step,
            "source": if kind == "USER_INPUT" { "user" } else { "planner" },
            "type": kind,
            "status": "ok",
            "created_at": format!("2026-07-25T12:00:{step:02}Z"),
            "content": content,
        })
    }

    fn tool_call(step: u64, content: &str) -> Value {
        json!({
            "step_index": step,
            "source": "planner",
            "type": "CODE_ACTION",
            "status": "ok",
            "created_at": format!("2026-07-25T12:00:{step:02}Z"),
            "content": content,
            "tool_calls": [{"name": "write_to_file", "args": {"TargetFile": "README.md"}}],
        })
    }

    fn write_transcript(path: &Path, records: &[Value]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut bytes = Vec::new();
        for record in records {
            serde_json::to_writer(&mut bytes, record).unwrap();
            bytes.push(b'\n');
        }
        fs::write(path, bytes).unwrap();
    }

    fn append_record(path: &Path, record: &Value) {
        let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
        serde_json::to_writer(&mut file, record).unwrap();
        file.write_all(b"\n").unwrap();
    }

    fn checkpoint(store: &Store, path: &Path) -> DirectJsonlCheckpoint {
        let canonical = fs::canonicalize(path).unwrap();
        let locator = provider_path_identity(&canonical).unwrap();
        let stream = provider_source_cursor_stream_for_path(
            CaptureProvider::Antigravity,
            ANTIGRAVITY_CLI_SOURCE_FORMAT,
            &locator,
        );
        let cursor = store
            .get_sync_cursor(None, MACHINE, &stream)
            .unwrap()
            .unwrap();
        decode_direct_jsonl_native_cursor(
            &cursor.cursor,
            CaptureProvider::Antigravity,
            ANTIGRAVITY_CLI_SOURCE_FORMAT,
        )
        .unwrap()
    }

    fn classify(
        path: &Path,
        root: &Path,
        previous: &DirectJsonlCheckpoint,
    ) -> DirectJsonlSourceChange {
        open_direct_jsonl_pages(
            CaptureProvider::Antigravity,
            ANTIGRAVITY_CLI_SOURCE_FORMAT,
            path,
            Some(root.to_path_buf()),
            "2026-07-25T12:01:00Z".parse().unwrap(),
            false,
            Some(previous),
        )
        .unwrap()
        .source_change()
    }
}
