use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{AgentType, CaptureProvider, EventRole, EventType, SyncCursor};
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
        },
        normalization::{provider_output_event_is_failure, provider_role, provider_value_text},
    },
    stable_capture_uuid, CaptureError, ImportProfile, OutputAssociations, OutputNativeCoordinate,
    OutputObservationKind, OutputOutcome, OutputOutcomeMetadata, OutputSourceIdentity,
    OutputSourceLocator, ProOutputObservation, ProOutputProgress, ProOutputSink,
    ProOutputSinkError, ProOutputSourceDisposition, ProviderImportSummary,
    ProviderImportWorkResult, Result, FACTORY_DROID_SOURCE_FORMAT,
};

use super::super::result_content::{NativeJsonlResultExtractionError, NativeJsonlResultSubrecord};
use super::{
    decode_direct_jsonl_native_cursor, encode_direct_jsonl_cursor,
    import_direct_native_jsonl_tree_core, open_direct_jsonl_pages,
    reader::direct_jsonl_source_revision, DirectJsonlCheckpoint, DirectJsonlOutput,
    DirectJsonlSourceChange, NativePathJsonlTreeImport,
};

const FACTORY_DROID_OUTPUT_FRONTIER_VERSION: u32 = 1;
const FACTORY_DROID_OUTPUT_PARSER_REVISION: &str = "factory-droid-direct-native-jsonl-v1";
const FACTORY_DROID_MISSING_REASON: &str = "no Factory AI Droid session JSONL transcripts found";

pub(crate) fn import_factory_ai_droid_nativepath_tree(
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
        known_factory_droid_routes(store, &request.machine_id, &configured_source_root)?;
    let sink = request.import_profile.sink().cloned();

    if request.import_profile.is_replay_only() {
        replay_outputs_or_mark_behind(
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
                reason: FACTORY_DROID_MISSING_REASON,
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
        CaptureProvider::FactoryAiDroid,
        FACTORY_DROID_SOURCE_FORMAT,
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
        &live_inventory.paths,
        &configured_source_root,
        request.imported_at,
        sink.as_deref(),
    );
    Ok(summary)
}

struct FactoryDroidInventory {
    paths: BTreeSet<PathBuf>,
    root_missing: bool,
}

fn discover_live_transcripts(root: &Path) -> Result<FactoryDroidInventory> {
    match std::fs::symlink_metadata(root) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(FactoryDroidInventory {
                paths: BTreeSet::new(),
                root_missing: true,
            });
        }
        Err(error) => return Err(error.into()),
    }
    let mut paths = BTreeSet::new();
    super::super::traversal::visit_jsonl_tree_files(
        root,
        &factory_droid_file_is_selected,
        &mut |path| {
            paths.insert(std::fs::canonicalize(path)?);
            Ok(())
        },
    )?;
    Ok(FactoryDroidInventory {
        paths,
        root_missing: false,
    })
}

#[derive(Clone)]
struct KnownFactoryDroidRoute {
    path: PathBuf,
    locator_identity: String,
    canonical_source_identity: String,
    source_revision: String,
    current_cursor: SyncCursor,
    checkpoint: DirectJsonlCheckpoint,
}

fn known_factory_droid_routes(
    store: &Store,
    machine_id: &str,
    source_root: &Path,
) -> Result<Vec<KnownFactoryDroidRoute>> {
    let source_root = source_root.display().to_string();
    let mut routes = BTreeMap::<String, KnownFactoryDroidRoute>::new();
    for source in store.list_capture_sources()? {
        if source.descriptor.provider != CaptureProvider::FactoryAiDroid
            || source.descriptor.machine_id != machine_id
            || source.descriptor.source_format.as_deref() != Some(FACTORY_DROID_SOURCE_FORMAT)
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
            CaptureProvider::FactoryAiDroid,
            FACTORY_DROID_SOURCE_FORMAT,
            &locator_identity,
        );
        let Some(current_cursor) = store.get_sync_cursor(None, machine_id, &stream)? else {
            continue;
        };
        let Some(checkpoint) = decode_direct_jsonl_native_cursor(
            &current_cursor.cursor,
            CaptureProvider::FactoryAiDroid,
            FACTORY_DROID_SOURCE_FORMAT,
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
        let route = KnownFactoryDroidRoute {
            path,
            locator_identity: locator_identity.clone(),
            canonical_source_identity: canonical_source_identity.to_owned(),
            source_revision: source_revision.to_owned(),
            current_cursor,
            checkpoint,
        };
        if routes.insert(locator_identity, route).is_some() {
            return Err(CaptureError::SystemInvariant(
                "Factory Droid persisted duplicate current routes for one transcript",
            ));
        }
    }
    Ok(routes.into_values().collect())
}

fn retire_missing_routes(
    store: &mut Store,
    machine_id: &str,
    retired_at: DateTime<Utc>,
    known_routes: &[KnownFactoryDroidRoute],
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
    route: &KnownFactoryDroidRoute,
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
        provider: CaptureProvider::FactoryAiDroid,
        source_format: FACTORY_DROID_SOURCE_FORMAT.to_owned(),
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
    paths: &BTreeSet<PathBuf>,
    source_root: &Path,
    imported_at: DateTime<Utc>,
    sink: Option<&dyn ProOutputSink>,
) {
    let Some(sink) = sink else {
        return;
    };
    if let Err(error) = replay_factory_droid_outputs(paths, source_root, imported_at, sink) {
        sink.mark_behind(ProOutputSinkError::new(
            "factory_droid_nativepath_output_replay",
            error.to_string(),
        ));
    }
}

fn replay_factory_droid_outputs(
    paths: &BTreeSet<PathBuf>,
    source_root: &Path,
    imported_at: DateTime<Utc>,
    sink: &dyn ProOutputSink,
) -> Result<()> {
    for path in paths {
        let locator_identity = provider_path_identity(path)?;
        let source = OutputSourceIdentity {
            provider: CaptureProvider::FactoryAiDroid.as_str().to_owned(),
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
        replay_factory_droid_source(path, imported_at, sink, source, locator_identity, progress)?;
    }
    Ok(())
}

fn replay_factory_droid_source(
    path: &Path,
    imported_at: DateTime<Utc>,
    sink: &dyn ProOutputSink,
    output_source: OutputSourceIdentity,
    locator_identity: String,
    progress: Option<ProOutputProgress>,
) -> Result<()> {
    let progress_cursor = progress
        .as_ref()
        .and_then(|progress| progress.cursor.as_ref())
        .filter(|cursor| cursor.version == FACTORY_DROID_OUTPUT_FRONTIER_VERSION)
        .and_then(|cursor| serde_json::from_slice::<DirectJsonlCheckpoint>(&cursor.payload).ok())
        .filter(|checkpoint| {
            checkpoint
                .is_supported_for(CaptureProvider::FactoryAiDroid, FACTORY_DROID_SOURCE_FORMAT)
        });
    let can_resume = progress.as_ref().is_some_and(|progress| {
        progress.parser_revision == FACTORY_DROID_OUTPUT_PARSER_REVISION
            && progress.materializer_revision == sink.materializer_revision()
            && progress_cursor.is_some()
    });
    let previous = if can_resume {
        progress_cursor.as_ref()
    } else {
        None
    };
    let mut reader = open_direct_jsonl_pages(
        CaptureProvider::FactoryAiDroid,
        FACTORY_DROID_SOURCE_FORMAT,
        path,
        None,
        imported_at,
        true,
        previous,
    )?;
    let source_change = reader.source_change();
    let observed_revision = direct_jsonl_source_revision(reader.observation());
    let mut output_state = FactoryDroidOutputState::new(
        output_source,
        progress,
        source_change,
        can_resume,
        sink.materializer_revision(),
    )?;

    while let Some(page) = reader.next_page()? {
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
            parser_revision: FACTORY_DROID_OUTPUT_PARSER_REVISION.to_owned(),
            materializer_revision: sink.materializer_revision().to_owned(),
            disposition: output_state.disposition,
            expected_prior_source_epoch: output_state.expected_source_epoch,
            expected_prior_frontier: output_state.expected_sink_frontier.clone(),
            observations,
        };
        let replay = NativeProReplayPage::new_with_source_identity(
            NativeSourceIdentity::new(CaptureProvider::FactoryAiDroid.as_str(), &locator_identity),
            expected_frontier,
            next_safe_frontier.clone(),
            page.terminal,
            accounting,
            output,
        )
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        if process_pro_replay_only(replay, sink).is_err() {
            sink.mark_behind(ProOutputSinkError::new(
                "factory_droid_nativepath_output_page",
                "Factory Droid output materialization did not advance",
            ));
            break;
        }
        output_state.expected_source_epoch = Some(output_state.source_epoch);
        output_state.expected_sink_frontier = Some(next_safe_frontier);
        output_state.disposition = ProOutputSourceDisposition::AppendOrResume;
    }
    Ok(())
}

struct FactoryDroidOutputState {
    source: OutputSourceIdentity,
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_sink_frontier: Option<NativeSafeFrontier>,
    disposition: ProOutputSourceDisposition,
}

impl FactoryDroidOutputState {
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
                        "Factory Droid output source epoch exhausted",
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
        FACTORY_DROID_OUTPUT_FRONTIER_VERSION,
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
            kind: "factory-droid-jsonl-range-v1".to_owned(),
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
                CaptureProvider::FactoryAiDroid.as_str(),
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
    digest.update(b"ctx-factory-droid-nativepath-route-retirement-v1\0");
    digest.update(retirement.provider.as_str().as_bytes());
    digest.update(retirement.source_format.as_bytes());
    digest.update(retirement.machine_id.as_bytes());
    digest.update(retirement.locator_identity.as_bytes());
    digest.update(retirement.cursor_stream.as_bytes());
    digest.update(retirement.expected_canonical_source_identity.as_bytes());
    digest.update(retirement.expected_source_revision.as_bytes());
    digest.update(format!("{:?}", retirement.reason).as_bytes());
    format!(
        "factory-droid-nativepath-retirement-v1:{:x}",
        digest.finalize()
    )
}

pub(crate) fn factory_droid_file_is_selected(path: &Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()) == Some("jsonl")
}

pub(crate) fn factory_droid_event_identity(value: &Value) -> Option<&str> {
    value
        .get("id")
        .or_else(|| value.get("uuid"))
        .and_then(Value::as_str)
        .filter(|event_id| !event_id.trim().is_empty())
}

pub(crate) fn factory_droid_header_session_id(value: &Value) -> Option<String> {
    (value.get("type").and_then(Value::as_str) == Some("session_start"))
        .then(|| {
            value
                .get("sessionId")
                .or_else(|| value.get("id"))
                .and_then(Value::as_str)
        })
        .flatten()
        .filter(|session_id| !session_id.trim().is_empty())
        .map(str::to_owned)
}

pub(crate) fn factory_droid_header_cwd(value: &Value) -> Option<String> {
    value
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|cwd| !cwd.trim().is_empty())
        .map(str::to_owned)
}

pub(crate) fn factory_droid_session_relationships(
    header: &Value,
    native_session_id: &str,
) -> (String, Option<String>, Option<String>, AgentType) {
    let parent = header
        .get("parent")
        .or_else(|| header.get("callingSessionId"))
        .and_then(Value::as_str)
        .filter(|session_id| !session_id.trim().is_empty())
        .map(str::to_owned);
    let agent_type = if parent.is_some()
        || header.get("decompSessionType").and_then(Value::as_str) == Some("worker")
    {
        AgentType::Subagent
    } else {
        AgentType::Primary
    };
    (
        native_session_id.to_owned(),
        parent,
        header
            .get("decompMissionId")
            .and_then(Value::as_str)
            .filter(|mission_id| !mission_id.trim().is_empty())
            .map(str::to_owned),
        agent_type,
    )
}

pub(crate) fn factory_droid_event_type(value: &Value) -> EventType {
    match value.get("type").and_then(Value::as_str) {
        Some("message") if factory_droid_content_has(value, "tool_use") => EventType::ToolCall,
        Some("message") if factory_droid_content_has(value, "tool_result") => EventType::ToolOutput,
        Some("message") => EventType::Message,
        Some("compaction_state") => EventType::Summary,
        Some("todo_state" | "session_start") => EventType::Notice,
        _ => EventType::Notice,
    }
}

pub(crate) fn factory_droid_role(value: &Value) -> EventRole {
    provider_role(
        value
            .get("role")
            .or_else(|| value.pointer("/message/role"))
            .and_then(Value::as_str),
    )
}

pub(crate) fn factory_droid_event_text(value: &Value) -> String {
    value
        .get("content")
        .or_else(|| value.pointer("/message/content"))
        .and_then(provider_value_text)
        .or_else(|| {
            value
                .get("summary")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .or_else(|| value.get("items").and_then(provider_value_text))
        .unwrap_or_default()
}

pub(crate) fn factory_droid_model(value: &Value) -> Option<Value> {
    value
        .get("model")
        .cloned()
        .or_else(|| value.pointer("/message/model").cloned())
        .or_else(|| value.pointer("/metadata/model").cloned())
}

pub(crate) fn enumerate_factory_droid_results(
    value: &Value,
) -> std::result::Result<Vec<NativeJsonlResultSubrecord<'_>>, NativeJsonlResultExtractionError> {
    if value.get("type").and_then(Value::as_str) != Some("message") {
        return Ok(Vec::new());
    }
    let content = value
        .get("content")
        .or_else(|| value.pointer("/message/content"));
    if reject_redacted(value).is_err() {
        return placeholder_results(content);
    }
    enumerate_content_results(content, value)
}

fn factory_droid_content_has(value: &Value, expected: &str) -> bool {
    value
        .get("content")
        .or_else(|| value.pointer("/message/content"))
        .and_then(Value::as_array)
        .is_some_and(|blocks| {
            blocks
                .iter()
                .any(|block| block.get("type").and_then(Value::as_str) == Some(expected))
        })
}

fn placeholder_results(
    content: Option<&Value>,
) -> std::result::Result<Vec<NativeJsonlResultSubrecord<'_>>, NativeJsonlResultExtractionError> {
    let count = content
        .map(|content| {
            content
                .as_array()
                .ok_or(NativeJsonlResultExtractionError::InvalidShape)
                .map(|blocks| {
                    blocks
                        .iter()
                        .filter(|block| {
                            block.get("type").and_then(Value::as_str) == Some("tool_result")
                        })
                        .count()
                })
        })
        .transpose()?
        .unwrap_or(0);
    (0..count)
        .map(|index| {
            Ok(NativeJsonlResultSubrecord {
                subrecord_index: u32::try_from(index)
                    .map_err(|_| NativeJsonlResultExtractionError::InvalidShape)?,
                content: None,
                call_id: None,
                tool_name: None,
                outcome: unknown_result_outcome(),
            })
        })
        .collect()
}

fn enumerate_content_results<'a>(
    content: Option<&'a Value>,
    record: &'a Value,
) -> std::result::Result<Vec<NativeJsonlResultSubrecord<'a>>, NativeJsonlResultExtractionError> {
    let Some(content) = content else {
        return Ok(Vec::new());
    };
    content
        .as_array()
        .ok_or(NativeJsonlResultExtractionError::InvalidShape)?
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
        .enumerate()
        .map(|(index, block)| {
            let (content, redacted) =
                match extract_result_ref(Some(block), &["content", "output", "text"]) {
                    Ok(content) => (content, false),
                    Err(NativeJsonlResultExtractionError::Redacted) => (None, true),
                    Err(error) => return Err(error),
                };
            Ok(NativeJsonlResultSubrecord {
                subrecord_index: u32::try_from(index)
                    .map_err(|_| NativeJsonlResultExtractionError::InvalidShape)?,
                content,
                call_id: (!redacted).then(|| native_result_identity(block)).flatten(),
                tool_name: (!redacted)
                    .then(|| native_result_tool_name(block))
                    .flatten(),
                outcome: if redacted {
                    unknown_result_outcome()
                } else {
                    native_result_outcome_with_record(block, record)
                },
            })
        })
        .collect()
}

fn extract_result_ref<'a>(
    value: Option<&'a Value>,
    object_fields: &[&str],
) -> std::result::Result<Option<&'a str>, NativeJsonlResultExtractionError> {
    let Some(value) = value else {
        return Ok(None);
    };
    reject_redacted(value)?;
    match value {
        Value::String(text) => Ok(Some(text)),
        Value::Null => Ok(None),
        Value::Object(object) => {
            for field in object_fields {
                if let Some(selected) = object.get(*field) {
                    return match selected {
                        Value::String(text) => Ok(Some(text)),
                        Value::Null => Ok(None),
                        _ => Err(NativeJsonlResultExtractionError::InvalidShape),
                    };
                }
            }
            Ok(None)
        }
        Value::Array(_) | Value::Bool(_) | Value::Number(_) => {
            Err(NativeJsonlResultExtractionError::InvalidShape)
        }
    }
}

fn native_result_identity(value: &Value) -> Option<&str> {
    [
        "call_id",
        "callId",
        "tool_call_id",
        "toolCallId",
        "tool_use_id",
        "toolUseId",
        "id",
    ]
    .into_iter()
    .find_map(|key| value.get(key).and_then(Value::as_str))
}

fn native_result_tool_name(value: &Value) -> Option<&str> {
    ["tool_name", "toolName", "name", "tool"]
        .into_iter()
        .find_map(|key| value.get(key).and_then(Value::as_str))
}

fn native_result_outcome_with_record(subrecord: &Value, record: &Value) -> OutputOutcomeMetadata {
    let mut outcome = native_result_outcome(subrecord);
    if outcome.outcome == OutputOutcome::Unknown {
        outcome = native_result_outcome(record);
    }
    outcome
}

fn native_result_outcome(value: &Value) -> OutputOutcomeMetadata {
    let timeout = native_result_has_timeout(value);
    let failure = provider_output_event_is_failure(value);
    let success = native_result_has_success(value);
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
        exit_code: native_result_i64(value, &["exit_code", "exitCode"])
            .and_then(|code| i32::try_from(code).ok()),
        duration_ms: native_result_u64(value, &["duration_ms", "durationMs", "duration"]),
    }
}

fn unknown_result_outcome() -> OutputOutcomeMetadata {
    OutputOutcomeMetadata {
        outcome: OutputOutcome::Unknown,
        exit_code: None,
        duration_ms: None,
    }
}

fn native_result_has_timeout(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(native_result_has_timeout),
        Value::Object(values) => {
            values.iter().any(|(key, value)| {
                matches!(normalized_result_key(key).as_str(), "timeout" | "timedout")
                    && value.as_bool().unwrap_or(false)
            }) || values.values().any(native_result_has_timeout)
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

fn native_result_has_success(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(native_result_has_success),
        Value::Object(values) => {
            values.iter().any(|(key, value)| {
                let key = normalized_result_key(key);
                (matches!(key.as_str(), "success" | "ok") && value.as_bool() == Some(true))
                    || (key == "exitcode" && value.as_i64() == Some(0))
                    || (key == "statuscode"
                        && value
                            .as_i64()
                            .is_some_and(|code| (200..400).contains(&code)))
                    || (matches!(key.as_str(), "iserror" | "timedout" | "timeout")
                        && value.as_bool() == Some(false))
                    || (matches!(key.as_str(), "status" | "state" | "outcome")
                        && value.as_str().is_some_and(|status| {
                            matches!(
                                status.trim().to_ascii_lowercase().as_str(),
                                "success"
                                    | "succeeded"
                                    | "complete"
                                    | "completed"
                                    | "ok"
                                    | "passed"
                            )
                        }))
            }) || values.values().any(native_result_has_success)
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

fn native_result_i64(value: &Value, expected_keys: &[&str]) -> Option<i64> {
    match value {
        Value::Array(values) => values
            .iter()
            .find_map(|value| native_result_i64(value, expected_keys)),
        Value::Object(values) => values
            .iter()
            .find_map(|(key, value)| {
                expected_keys
                    .iter()
                    .any(|expected| key == expected)
                    .then(|| value.as_i64())
                    .flatten()
            })
            .or_else(|| {
                values
                    .values()
                    .find_map(|value| native_result_i64(value, expected_keys))
            }),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => None,
    }
}

fn native_result_u64(value: &Value, expected_keys: &[&str]) -> Option<u64> {
    match value {
        Value::Array(values) => values
            .iter()
            .find_map(|value| native_result_u64(value, expected_keys)),
        Value::Object(values) => values
            .iter()
            .find_map(|(key, value)| {
                expected_keys
                    .iter()
                    .any(|expected| key == expected)
                    .then(|| value.as_u64())
                    .flatten()
            })
            .or_else(|| {
                values
                    .values()
                    .find_map(|value| native_result_u64(value, expected_keys))
            }),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => None,
    }
}

fn normalized_result_key(key: &str) -> String {
    key.chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn reject_redacted(value: &Value) -> std::result::Result<(), NativeJsonlResultExtractionError> {
    let Some(object) = value.as_object() else {
        return Ok(());
    };
    let flag_is_redacted = ["redacted", "is_redacted", "isRedacted"]
        .iter()
        .filter_map(|field| object.get(*field))
        .any(|flag| flag.as_bool() != Some(false));
    let state_is_redacted = ["status", "state"]
        .iter()
        .filter_map(|field| object.get(*field).and_then(Value::as_str))
        .any(|state| matches!(state, "redacted" | "output-redacted"));
    if flag_is_redacted || state_is_redacted {
        Err(NativeJsonlResultExtractionError::Redacted)
    } else {
        Ok(())
    }
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

    use super::*;
    use crate::{
        test_support_paths::tempdir, CaptureWorkLimit, ProOutputMaterializationPage,
        ProOutputPageResult,
    };

    const MACHINE: &str = "factory-droid-nativepath-test-machine";
    const SUCCESS_BODY: &str = "FACTORY_DROID_SUCCESS_BODY_MUST_NOT_ENTER_CORE";

    #[test]
    fn production_lifecycle_covers_replay_append_all_rewrites_and_disappearance() {
        let temp = tempdir().unwrap();
        let root = temp.path().join(".factory/sessions");
        let transcript = transcript_path(&root);
        write_transcript(
            &transcript,
            &[
                header("droid-life"),
                message("fresh-user", "user", "fresh-user"),
                tool_call("fresh-call"),
                tool_result("fresh-result", SUCCESS_BODY),
            ],
        );
        let store_path = temp.path().join("work.sqlite");
        let mut store = Store::open(&store_path).unwrap();

        let fresh = import(&root, &mut store, ImportProfile::CoreOnly);
        assert_eq!(fresh.work_result(), ProviderImportWorkResult::Changed);
        assert_eq!(fresh.imported_sessions, 1);
        assert_eq!(fresh.imported_events, 3);
        let session = provider_session(&store, "droid-life");
        let original_events = store.events_for_session(session.id).unwrap();
        assert_eq!(original_events.len(), 3);
        assert!(original_events
            .iter()
            .all(|event| event.event_type != EventType::ToolOutput));
        assert!(!serde_json::to_string(&original_events)
            .unwrap()
            .contains(SUCCESS_BODY));
        let routed_event = original_events[0].id;
        assert!(store
            .authorized_source_route_for_event(routed_event)
            .is_ok());

        let previous = checkpoint(&store, &transcript);
        assert_eq!(
            classify(&transcript, &root, &previous),
            DirectJsonlSourceChange::Unchanged
        );
        assert_eq!(
            import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
            ProviderImportWorkResult::NoOp
        );

        drop(store);
        let mut store = Store::open(&store_path).unwrap();
        assert_eq!(
            import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
            ProviderImportWorkResult::NoOp
        );

        let previous = checkpoint(&store, &transcript);
        append_record(
            &transcript,
            &message("append", "assistant", "append-assistant"),
        );
        assert_eq!(
            classify(&transcript, &root, &previous),
            DirectJsonlSourceChange::Append
        );
        let appended = import(&root, &mut store, ImportProfile::CoreOnly);
        assert_eq!(appended.work_result(), ProviderImportWorkResult::Changed);
        assert_eq!(appended.imported_events, 1);

        let previous = checkpoint(&store, &transcript);
        write_transcript(
            &transcript,
            &[
                header("droid-life"),
                message("rewrite-user", "user", &"rewrite-user-content-".repeat(24)),
                message(
                    "rewrite-assistant",
                    "assistant",
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
        write_transcript(
            &transcript,
            &[header("droid-life"), message("short", "user", "short")],
        );
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
            &[
                header("droid-life"),
                message("replacement", "user", "replacement-generation"),
            ],
        );
        fs::remove_file(&transcript).unwrap();
        fs::rename(&replacement, &transcript).unwrap();
        assert_eq!(
            classify(&transcript, &root, &previous),
            DirectJsonlSourceChange::Replacement
        );
        assert_eq!(
            import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
            ProviderImportWorkResult::Changed
        );

        fs::remove_file(&transcript).unwrap();
        let missing_source = import(&root, &mut store, ImportProfile::CoreOnly);
        assert_eq!(
            missing_source.work_result(),
            ProviderImportWorkResult::Changed
        );
        assert!(store
            .authorized_source_route_for_event(routed_event)
            .is_err());
        assert_eq!(
            import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
            ProviderImportWorkResult::NoOp
        );

        write_transcript(
            &transcript,
            &[
                header("droid-life"),
                message("root-returned", "user", "root-returned"),
            ],
        );
        assert_eq!(
            import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
            ProviderImportWorkResult::Changed
        );
        fs::remove_dir_all(&root).unwrap();
        assert_eq!(
            import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
            ProviderImportWorkResult::Changed
        );
        assert_eq!(
            import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
            ProviderImportWorkResult::NoOp
        );
    }

    #[test]
    fn production_is_core_first_and_pro_failure_is_independent() {
        let temp = tempdir().unwrap();
        let root = temp.path().join(".factory/sessions");
        let transcript = transcript_path(&root);
        write_transcript(
            &transcript,
            &[
                header("droid-core-first"),
                message("core-first", "user", "core-first"),
                tool_call("call-with-output"),
                tool_result("result-with-output", SUCCESS_BODY),
            ],
        );

        let store_path = temp.path().join("core.sqlite");
        let mut store = Store::open(&store_path).unwrap();
        let sink = Arc::new(RecordingSink::new(store_path.clone(), false));
        let fresh = import(&root, &mut store, ImportProfile::CoreAndPro(sink.clone()));
        assert_eq!(fresh.work_result(), ProviderImportWorkResult::Changed);
        assert!(sink.saw_core_before_page.load(Ordering::SeqCst));
        assert_eq!(sink.outputs.load(Ordering::SeqCst), 1);
        let core_events = store
            .events_for_session(provider_session(&store, "droid-core-first").id)
            .unwrap();
        assert!(!serde_json::to_string(&core_events)
            .unwrap()
            .contains(SUCCESS_BODY));

        let pages_after_fresh = sink.pages.load(Ordering::SeqCst);
        assert_eq!(
            import(&root, &mut store, ImportProfile::CoreAndPro(sink.clone())).work_result(),
            ProviderImportWorkResult::NoOp
        );
        assert_eq!(sink.pages.load(Ordering::SeqCst), pages_after_fresh);

        let later_path = temp.path().join("later.sqlite");
        let mut later_store = Store::open(&later_path).unwrap();
        assert_eq!(
            import(&root, &mut later_store, ImportProfile::CoreOnly).work_result(),
            ProviderImportWorkResult::Changed
        );
        let later_sink = Arc::new(RecordingSink::new(later_path, false));
        let replay = import(
            &root,
            &mut later_store,
            ImportProfile::ProReplayOnly(later_sink.clone()),
        );
        assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
        assert_eq!(later_sink.outputs.load(Ordering::SeqCst), 1);

        let failure_path = temp.path().join("failure.sqlite");
        let mut failure_store = Store::open(&failure_path).unwrap();
        let failing_sink = Arc::new(RecordingSink::new(failure_path, true));
        let core_survives = import(
            &root,
            &mut failure_store,
            ImportProfile::CoreAndPro(failing_sink.clone()),
        );
        assert_eq!(
            core_survives.work_result(),
            ProviderImportWorkResult::Changed
        );
        assert!(!failure_store.list_sessions().unwrap().is_empty());
        assert!(failing_sink.behind.load(Ordering::SeqCst) > 0);
    }

    #[test]
    fn relationships_corruption_incomplete_tail_and_result_privacy_are_exact() {
        let temp = tempdir().unwrap();
        let root = temp.path().join(".factory/sessions");
        let parent = root.join("project/a-parent.jsonl");
        let child = root.join("project/b-child.jsonl");
        write_transcript(
            &parent,
            &[
                header("droid-parent"),
                message("parent-user", "user", "parent"),
            ],
        );
        write_transcript(
            &child,
            &[
                child_header("droid-child", "droid-parent"),
                message("child-user", "user", "child"),
            ],
        );
        let mut bytes = fs::read(&child).unwrap();
        bytes.extend_from_slice(b"{malformed-json}\n");
        let incomplete = serde_json::to_vec(&message(
            "incomplete",
            "assistant",
            "complete-only-after-newline",
        ))
        .unwrap();
        bytes.extend_from_slice(&incomplete);
        fs::write(&child, bytes).unwrap();

        let mut store = Store::open(temp.path().join("relationships.sqlite")).unwrap();
        let first = import(&root, &mut store, ImportProfile::CoreOnly);
        assert_eq!(first.work_result(), ProviderImportWorkResult::Changed);
        let parent_session = provider_session(&store, "droid-parent");
        let child_session = provider_session(&store, "droid-child");
        assert_eq!(child_session.parent_session_id, Some(parent_session.id));
        assert!(store
            .events_for_session(child_session.id)
            .unwrap()
            .iter()
            .all(|event| {
                !serde_json::to_string(event)
                    .unwrap()
                    .contains("complete-only-after-newline")
            }));

        append_raw(&child, b"\n");
        let resumed = import(&root, &mut store, ImportProfile::CoreOnly);
        assert_eq!(resumed.work_result(), ProviderImportWorkResult::Changed);
        assert!(store
            .events_for_session(child_session.id)
            .unwrap()
            .iter()
            .any(|event| {
                serde_json::to_string(event)
                    .unwrap()
                    .contains("complete-only-after-newline")
            }));

        let redacted = json!({
            "type": "message",
            "redacted": true,
            "message": {
                "role": "tool",
                "content": [{"type": "tool_result", "content": "secret"}]
            }
        });
        let results = enumerate_factory_droid_results(&redacted).unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].content.is_none());
        assert!(results[0].call_id.is_none());
    }

    struct RecordingSink {
        store_path: PathBuf,
        progress: Mutex<Option<ProOutputProgress>>,
        pages: AtomicUsize,
        outputs: AtomicUsize,
        behind: AtomicUsize,
        saw_core_before_page: AtomicBool,
        fail_pages: bool,
    }

    impl RecordingSink {
        fn new(store_path: PathBuf, fail_pages: bool) -> Self {
            Self {
                store_path,
                progress: Mutex::new(None),
                pages: AtomicUsize::new(0),
                outputs: AtomicUsize::new(0),
                behind: AtomicUsize::new(0),
                saw_core_before_page: AtomicBool::new(false),
                fail_pages,
            }
        }
    }

    impl ProOutputSink for RecordingSink {
        fn inventory_generation(&self) -> u64 {
            1
        }

        fn materializer_revision(&self) -> &str {
            "factory-droid-nativepath-test-materializer-v1"
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
            if self.fail_pages {
                return Err(ProOutputSinkError::new(
                    "factory_droid_test_failure",
                    "injected output materialization failure",
                ));
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

        fn mark_behind(&self, _error: ProOutputSinkError) {
            self.behind.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn import(
        root: &Path,
        store: &mut Store,
        import_profile: ImportProfile,
    ) -> ProviderImportSummary {
        import_factory_ai_droid_nativepath_tree(
            store,
            NativePathJsonlTreeImport {
                path: root,
                machine_id: MACHINE.to_owned(),
                source_path: Some(root.to_path_buf()),
                source_root: None,
                imported_at: "2026-07-25T12:00:00Z".parse().unwrap(),
                history_record_id: None,
                capture_work_limit: CaptureWorkLimit::Drain,
                inventory_observation_token: None,
                import_profile,
            },
        )
        .unwrap()
    }

    fn provider_session(store: &Store, provider_session_id: &str) -> ctx_history_core::Session {
        store
            .list_sessions()
            .unwrap()
            .into_iter()
            .find(|session| {
                session.provider == CaptureProvider::FactoryAiDroid
                    && session.external_session_id.as_deref() == Some(provider_session_id)
            })
            .unwrap()
    }

    fn transcript_path(root: &Path) -> PathBuf {
        root.join("project/droid-life.jsonl")
    }

    fn header(session_id: &str) -> Value {
        json!({
            "type": "session_start",
            "id": session_id,
            "timestamp": "2026-07-25T12:00:00Z",
            "cwd": "/workspace/factory",
            "model": "factory/droid",
        })
    }

    fn child_header(session_id: &str, parent: &str) -> Value {
        json!({
            "type": "session_start",
            "sessionId": session_id,
            "timestamp": "2026-07-25T12:00:00Z",
            "cwd": "/workspace/factory",
            "model": "factory/droid",
            "callingSessionId": parent,
            "decompSessionType": "worker",
            "decompMissionId": "mission-1",
        })
    }

    fn message(id: &str, role: &str, text: &str) -> Value {
        json!({
            "type": "message",
            "id": id,
            "timestamp": "2026-07-25T12:00:01Z",
            "message": {
                "role": role,
                "content": [{"type": "text", "text": text}],
            },
            "model": "factory/droid",
        })
    }

    fn tool_call(id: &str) -> Value {
        json!({
            "type": "message",
            "id": id,
            "timestamp": "2026-07-25T12:00:02Z",
            "message": {
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "call-1",
                    "name": "read_file",
                    "input": {"file_path": "README.md"},
                }],
            },
            "model": "factory/droid",
        })
    }

    fn tool_result(id: &str, result: &str) -> Value {
        json!({
            "type": "message",
            "id": id,
            "timestamp": "2026-07-25T12:00:03Z",
            "message": {
                "role": "tool",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "call-1",
                    "name": "read_file",
                    "content": result,
                    "is_error": false,
                }],
            },
            "model": "factory/droid",
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

    fn append_raw(path: &Path, bytes: &[u8]) {
        fs::OpenOptions::new()
            .append(true)
            .open(path)
            .unwrap()
            .write_all(bytes)
            .unwrap();
    }

    fn checkpoint(store: &Store, path: &Path) -> DirectJsonlCheckpoint {
        let canonical = fs::canonicalize(path).unwrap();
        let locator = provider_path_identity(&canonical).unwrap();
        let stream = provider_source_cursor_stream_for_path(
            CaptureProvider::FactoryAiDroid,
            FACTORY_DROID_SOURCE_FORMAT,
            &locator,
        );
        let cursor = store
            .get_sync_cursor(None, MACHINE, &stream)
            .unwrap()
            .unwrap();
        decode_direct_jsonl_native_cursor(
            &cursor.cursor,
            CaptureProvider::FactoryAiDroid,
            FACTORY_DROID_SOURCE_FORMAT,
        )
        .unwrap()
    }

    fn classify(
        path: &Path,
        root: &Path,
        previous: &DirectJsonlCheckpoint,
    ) -> DirectJsonlSourceChange {
        open_direct_jsonl_pages(
            CaptureProvider::FactoryAiDroid,
            FACTORY_DROID_SOURCE_FORMAT,
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
