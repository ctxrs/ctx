#[cfg(test)]
use std::{env, time::Duration as StdDuration};
use std::{path::Path, process, time::Instant};

use anyhow::Result;
#[cfg(test)]
use anyhow::{anyhow, Context};
#[cfg(test)]
use ctx_history_capture::SourceBackedResolverRegistry;
#[cfg(test)]
use ctx_history_core::database_path;
use ctx_history_core::{
    utc_now, AgentType, BatchHydrationRequest, BatchHydrationResult, CaptureProvider,
    ContentSourceResolver, EventHydrationRequest, EventRole, EventType, HydratedProviderRecord,
    HydrationFailure, HydrationFailureKind,
};
use ctx_history_index::{EventRecord, VerifiedIndex};
use ctx_history_store::CanonicalSemanticProjectionVersion;
#[cfg(test)]
use ctx_history_store::Store;
use serde_json::{json, Value};

use crate::{DaemonRunArgs, DaemonTriggerCommandArg};

use super::{
    daemon::DaemonRuntime,
    daemon_retry::{
        annotate_semantic_failure, classify_semantic_failure, DaemonRetryBackoff,
        SemanticFailureClass,
    },
    daemon_scheduler::{daemon_deadline_has_min_budget, daemon_run_start_mode},
    health_search::{
        semantic_embed_policy_status_json, semantic_model_acquisition_integrity_error,
        semantic_model_acquisition_status_json, semantic_model_cache_available,
        semantic_worker_cache_dir,
    },
    model_contract::{semantic_model_key, SemanticModelLoadDeferred},
    model_runtime::{
        SemanticDaemonCpuFallbackRequired, SemanticDaemonModelAcquisition, SharedSemanticRuntime,
    },
    paths_status::{
        semantic_worker_lock_path, semantic_worker_status_path, write_daemon_status,
        write_semantic_worker_status,
    },
    reports::SemanticWorkerReport,
    resource_policy::{
        semantic_background_resource_deferred, semantic_resource_deferral_releases_runtime,
        SemanticBackgroundOperation, SemanticResourceDeferred,
    },
    runtime_limits::{
        DAEMON_MIN_REMAINING_FOR_JOB_SECS, DAEMON_SEMANTIC_RESERVE_GRACE_SECS,
        SEMANTIC_MODEL_INIT_MIN_REMAINING_SECS, SEMANTIC_SOURCE_MAX_CHARS,
    },
    source_backed_refresh_coordinator::{pin_published_generation, PinnedSourceBackedGeneration},
    vector_store::{
        semantic_hydrated_source_is_control, source_backed_semantic_vector_path,
        SemanticChunkDocument, SemanticVectorStore, SourceBackedSemanticEmbedder,
        SourceBackedSemanticOutcome, SourceBackedSemanticResolver,
    },
    SemanticEventDocument,
};

#[cfg(test)]
use super::document::semantic_event_document_from_store_projection;

#[cfg(test)]
use super::{
    daemon::daemon_test_job,
    daemon_scheduler::{daemon_deadline_remaining, refresh_semantic_document_count_cache},
    health_search::{env_usize, json_string, json_usize},
    indexing::{backfill_semantic_embeddings, semantic_document_hash, semantic_source_text},
    model_contract::SEMANTIC_MODEL_ID,
    paths_status::{
        read_semantic_worker_status, semantic_status_file_model_matches, SemanticWorkerLock,
    },
    runtime_limits::{
        SEMANTIC_DIRTY_QUEUE_RECENT_LIMIT, SEMANTIC_WORKER_BATCH_DEFAULT,
        SEMANTIC_WORKER_BATCH_MAX, SEMANTIC_WORKER_MAX_SECONDS_CAP,
        SEMANTIC_WORKER_MAX_SECONDS_DEFAULT,
    },
    vector_store::SemanticSidecarStats,
};

use crate::output::compact_json;

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct SemanticReconciliationSweepState {
    pub(super) target_version: Option<CanonicalSemanticProjectionVersion>,
    pub(super) committed_store_complete: bool,
    pub(super) pruning_complete: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[cfg(test)]
pub(super) struct SemanticReconciliationOutcome {
    pub(super) committed_documents_scanned: usize,
    pub(super) committed_documents_queued: usize,
    pub(super) pruned_events_scanned: usize,
    pub(super) deleted_chunks: usize,
    pub(super) queued_stale_events: usize,
    pub(super) work_remaining: bool,
}

#[derive(Debug, Clone)]
#[cfg(test)]
pub(super) struct SemanticWorkerArgs {
    pub(super) max_chunks: Option<usize>,
    pub(super) max_seconds: Option<u64>,
}

#[derive(Debug)]
pub(super) enum DaemonSemanticModelStartup {
    Loaded,
    Finished(Value),
}

fn daemon_semantic_model_acquisition_error(
    data_root: &Path,
    last_run_at_ms: i64,
    error: anyhow::Error,
) -> DaemonSemanticModelStartup {
    if let Some(deferred) = error.downcast_ref::<SemanticModelLoadDeferred>() {
        let _ = write_semantic_model_load_deferred_status(data_root, deferred);
        return DaemonSemanticModelStartup::Finished(daemon_semantic_model_load_deferred_job(
            last_run_at_ms,
            deferred,
        ));
    }
    let message = format!("{error:#}");
    let failure_class = classify_semantic_failure(&error);
    let integrity_failure = semantic_model_acquisition_integrity_error(&error);
    let failure_code = if integrity_failure {
        "model_integrity_failed"
    } else {
        "model_acquisition_failed"
    };
    let _ = write_semantic_model_acquisition_status(
        data_root,
        failure_code,
        Some(message.clone()),
        Some(failure_class),
    );
    DaemonSemanticModelStartup::Finished(annotate_semantic_failure(
        daemon_semantic_job_json(
            "skipped",
            Some(failure_code),
            last_run_at_ms,
            None,
            Some(message),
        ),
        failure_class,
    ))
}

pub(super) fn run_daemon_semantic_model_startup_with<Acquire, AcquireCpuFallback, Load>(
    data_root: &Path,
    last_run_at_ms: i64,
    acquire: Acquire,
    acquire_cpu_fallback: AcquireCpuFallback,
    mut load: Load,
) -> Result<DaemonSemanticModelStartup>
where
    Acquire: FnOnce() -> Result<SemanticDaemonModelAcquisition>,
    AcquireCpuFallback: FnOnce(&'static str) -> Result<SemanticDaemonModelAcquisition>,
    Load: FnMut(SemanticDaemonModelAcquisition) -> Result<(Option<Value>, Value)>,
{
    let _ = write_semantic_model_acquisition_status(data_root, "acquiring_model", None, None);
    let mut acquisition = match acquire() {
        Ok(acquisition) => acquisition,
        Err(error) => {
            return Ok(daemon_semantic_model_acquisition_error(
                data_root,
                last_run_at_ms,
                error,
            ))
        }
    };
    let mut acquire_cpu_fallback = Some(acquire_cpu_fallback);

    loop {
        let _ = write_semantic_model_load_status(data_root, "loading_model", None, None);
        match load(acquisition) {
            Ok((embedding_runtime, embed_policy)) => {
                let _ =
                    write_semantic_model_loaded_status(data_root, embedding_runtime, embed_policy);
                return Ok(DaemonSemanticModelStartup::Loaded);
            }
            Err(error)
                if error
                    .downcast_ref::<SemanticDaemonCpuFallbackRequired>()
                    .is_some() =>
            {
                let fallback = error
                    .downcast_ref::<SemanticDaemonCpuFallbackRequired>()
                    .expect("matched daemon CPU fallback");
                let reason = fallback.reason();
                let Some(acquire_cpu_fallback) = acquire_cpu_fallback.take() else {
                    return Err(error.context("daemon CPU fallback was requested twice"));
                };
                let _ = write_semantic_model_acquisition_status(
                    data_root,
                    "acquiring_model",
                    None,
                    None,
                );
                acquisition = match acquire_cpu_fallback(reason) {
                    Ok(acquisition) => acquisition,
                    Err(error) => {
                        return Ok(daemon_semantic_model_acquisition_error(
                            data_root,
                            last_run_at_ms,
                            error,
                        ))
                    }
                };
            }
            Err(error) if error.downcast_ref::<SemanticModelLoadDeferred>().is_some() => {
                let deferred = error
                    .downcast_ref::<SemanticModelLoadDeferred>()
                    .expect("matched semantic model load deferral");
                let _ = write_semantic_model_load_deferred_status(data_root, deferred);
                return Ok(DaemonSemanticModelStartup::Finished(
                    daemon_semantic_model_load_deferred_job(last_run_at_ms, deferred),
                ));
            }
            Err(error) => {
                let message = format!("{error:#}");
                let failure_class = classify_semantic_failure(&error);
                let failure_code = "model_load_failed";
                let _ = write_semantic_model_load_status(
                    data_root,
                    failure_code,
                    Some(message.clone()),
                    Some(failure_class),
                );
                return Ok(DaemonSemanticModelStartup::Finished(
                    annotate_semantic_failure(
                        daemon_semantic_job_json(
                            "skipped",
                            Some(failure_code),
                            last_run_at_ms,
                            None,
                            Some(message),
                        ),
                        failure_class,
                    ),
                ));
            }
        }
    }
}

pub(super) fn run_daemon_semantic_job(
    _args: &DaemonRunArgs,
    data_root: &Path,
    runtime: &mut DaemonRuntime,
    deadline: Option<Instant>,
    semantic_enabled: bool,
) -> Result<Value> {
    let last_run_at_ms = utc_now().timestamp_millis();
    if !semantic_enabled {
        return Ok(daemon_semantic_job_json(
            "disabled",
            Some("semantic_disabled"),
            last_run_at_ms,
            None,
            None,
        ));
    }

    #[cfg(test)]
    if let Some(value) = daemon_test_job("semantic_index") {
        return Ok(value);
    }

    let Some(source_generation) = pin_published_generation(data_root)? else {
        return Ok(daemon_semantic_job_json(
            "skipped",
            Some("source_generation_missing"),
            last_run_at_ms,
            None,
            None,
        ));
    };
    if !daemon_deadline_has_min_budget(deadline, DAEMON_MIN_REMAINING_FOR_JOB_SECS) {
        return Ok(daemon_semantic_job_json(
            "skipped",
            Some("daemon_deadline"),
            last_run_at_ms,
            None,
            None,
        ));
    }

    let admission_operation = if runtime.semantic_runtime.is_loaded() {
        SemanticBackgroundOperation::IndexBatch
    } else {
        SemanticBackgroundOperation::ModelLoad
    };
    if let Some(deferred) = semantic_background_resource_deferred(data_root, admission_operation) {
        if semantic_resource_deferral_releases_runtime(deferred.reason) {
            let _ = runtime.semantic_runtime.release_if_idle();
        }
        let _ = write_semantic_resource_deferred_status(data_root, deferred);
        return Ok(daemon_semantic_resource_deferred_job(
            last_run_at_ms,
            deferred,
        ));
    }

    let vector_path = source_backed_semantic_vector_path(data_root);
    let mut vector_store = SemanticVectorStore::open(&vector_path)?;
    let source_eligible_events = source_generation.semantic_eligible_event_count()?;
    let source_pending = !vector_store.source_backed_generation_ready_exact(
        source_generation.generation_id(),
        source_eligible_events,
    )?;
    if !source_pending {
        return Ok(daemon_semantic_job_json(
            "ready",
            None,
            last_run_at_ms,
            None,
            None,
        ));
    }
    let min_remaining_secs = if runtime.semantic_runtime.is_loaded() {
        DAEMON_MIN_REMAINING_FOR_JOB_SECS
    } else {
        SEMANTIC_MODEL_INIT_MIN_REMAINING_SECS
    }
    .saturating_add(DAEMON_SEMANTIC_RESERVE_GRACE_SECS);
    if !daemon_deadline_has_min_budget(deadline, min_remaining_secs) {
        return Ok(daemon_semantic_job_json(
            "skipped",
            Some("daemon_deadline"),
            last_run_at_ms,
            None,
            None,
        ));
    }
    let source_model_load_needed =
        source_eligible_events > 0 && !runtime.semantic_runtime.is_loaded();
    if source_model_load_needed {
        let cache_dir = semantic_worker_cache_dir(data_root);
        match run_daemon_semantic_model_startup_with(
            data_root,
            last_run_at_ms,
            || runtime.semantic_runtime.acquire_for_daemon(&cache_dir),
            |fallback| {
                runtime
                    .semantic_runtime
                    .acquire_cpu_fallback_for_daemon(&cache_dir, fallback)
            },
            |acquisition| {
                runtime
                    .semantic_runtime
                    .ensure_loaded_after_daemon_acquisition(&cache_dir, acquisition)?;
                Ok((
                    runtime.semantic_runtime.runtime_status_json()?,
                    runtime.semantic_runtime.policy_status_json()?,
                ))
            },
        )? {
            DaemonSemanticModelStartup::Loaded => {}
            DaemonSemanticModelStartup::Finished(job) => return Ok(job),
        }
    }
    let cache_dir = semantic_worker_cache_dir(data_root);
    let (outcome, indexed_chunks) = reconcile_source_backed_semantic_page(
        data_root,
        source_generation,
        &mut vector_store,
        &runtime.semantic_runtime,
        &cache_dir,
        deadline,
    )?;
    let (status, reason, last_error) = if let Some(unavailable) = outcome.unavailable {
        (
            "failed",
            Some("source_hydration_unavailable"),
            Some(format!(
                "source-backed semantic hydration failed: {unavailable:?}"
            )),
        )
    } else if outcome.ready {
        ("ready", None, None)
    } else {
        ("budget_exhausted", None, None)
    };
    let mut job = daemon_semantic_job_json(
        status,
        reason,
        last_run_at_ms,
        (indexed_chunks > 0).then_some(indexed_chunks),
        last_error,
    );
    annotate_source_backed_semantic_progress(&mut job, &outcome);
    Ok(job)
}

fn reconcile_source_backed_semantic_page(
    data_root: &Path,
    generation: PinnedSourceBackedGeneration,
    vector_store: &mut SemanticVectorStore,
    runtime: &SharedSemanticRuntime,
    cache_dir: &Path,
    deadline: Option<Instant>,
) -> Result<(SourceBackedSemanticOutcome, usize)> {
    let index = generation.into_index();
    let mut resolver = ProviderSourceSemanticResolver {
        index: &index,
        sources: DaemonGenerationSourceResolver {
            index: &index,
            data_root,
        },
    };
    let mut embedder = RuntimeSourceSemanticEmbedder {
        runtime,
        cache_dir,
        deadline,
        indexed_chunks: 0,
    };
    let outcome =
        vector_store.reconcile_source_backed_index(&index, &mut resolver, &mut embedder)?;
    Ok((outcome, embedder.indexed_chunks))
}

struct DaemonGenerationSourceResolver<'a> {
    index: &'a VerifiedIndex,
    data_root: &'a Path,
}

impl ContentSourceResolver for DaemonGenerationSourceResolver<'_> {
    fn hydrate_event(
        &self,
        request: &EventHydrationRequest,
    ) -> std::result::Result<HydratedProviderRecord, HydrationFailure> {
        let event = self
            .index
            .event_by_id(request.event_id().as_uuid())
            .map_err(|error| {
                source_hydration_failure(
                    HydrationFailureKind::TemporarilyUnavailable,
                    format!(
                        "read source-backed semantic event {}: {error}",
                        request.event_id()
                    ),
                )
            })?
            .ok_or_else(|| {
                source_hydration_failure(
                    HydrationFailureKind::MissingRecord,
                    format!(
                        "source-backed generation omitted semantic event {}",
                        request.event_id()
                    ),
                )
            })?;
        validate_source_semantic_request(&event, request)?;
        let mut hydrated = PinnedSourceBackedGeneration::hydrate_source_complete_events(
            self.index,
            self.data_root,
            &[&event],
        )
        .map_err(daemon_source_hydration_failure)?;
        let text = hydrated
            .remove(&request.event_id().as_uuid())
            .filter(|text| !text.is_empty())
            .ok_or_else(|| {
                source_hydration_failure(
                    HydrationFailureKind::MissingRecord,
                    format!(
                        "daemon source hydration omitted semantic event {}",
                        request.event_id()
                    ),
                )
            })?;
        Ok(HydratedProviderRecord {
            event_id: request.event_id(),
            provider_bytes: text.into_bytes(),
        })
    }

    fn hydrate_batch(
        &self,
        request: &BatchHydrationRequest,
    ) -> std::result::Result<BatchHydrationResult, HydrationFailure> {
        let mut events = Vec::with_capacity(request.events().len());
        for event_request in request.events() {
            let event = self
                .index
                .event_by_id(event_request.event_id().as_uuid())
                .map_err(|error| {
                    source_hydration_failure(
                        HydrationFailureKind::TemporarilyUnavailable,
                        format!(
                            "read source-backed semantic session event {}: {error}",
                            event_request.event_id()
                        ),
                    )
                })?
                .ok_or_else(|| {
                    source_hydration_failure(
                        HydrationFailureKind::MissingRecord,
                        format!(
                            "source-backed generation omitted semantic session event {}",
                            event_request.event_id()
                        ),
                    )
                })?;
            validate_source_semantic_request(&event, event_request)?;
            events.push(event);
        }
        let references = events.iter().collect::<Vec<_>>();
        let mut hydrated = PinnedSourceBackedGeneration::hydrate_source_complete_events(
            self.index,
            self.data_root,
            &references,
        )
        .map_err(daemon_source_hydration_failure)?;
        let records = request
            .events()
            .iter()
            .map(|event| {
                let text = hydrated
                    .remove(&event.event_id().as_uuid())
                    .filter(|text| !text.is_empty())
                    .ok_or_else(|| {
                        source_hydration_failure(
                            HydrationFailureKind::MissingRecord,
                            format!(
                                "daemon source hydration omitted semantic session event {}",
                                event.event_id()
                            ),
                        )
                    })?;
                Ok(HydratedProviderRecord {
                    event_id: event.event_id(),
                    provider_bytes: text.into_bytes(),
                })
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let result = BatchHydrationResult::new(records).map_err(|error| {
            source_hydration_failure(
                HydrationFailureKind::InvalidLocator,
                format!("construct daemon semantic batch hydration result: {error}"),
            )
        })?;
        result.validate_for_request(request)?;
        Ok(result)
    }
}

fn daemon_source_hydration_failure(error: anyhow::Error) -> HydrationFailure {
    PinnedSourceBackedGeneration::source_hydration_failure(&error).unwrap_or_else(|| {
        source_hydration_failure(
            HydrationFailureKind::TemporarilyUnavailable,
            format!(
                "request generation-bound daemon source hydration for semantic projection: {error:#}"
            ),
        )
    })
}

fn annotate_source_backed_semantic_progress(
    job: &mut Value,
    outcome: &SourceBackedSemanticOutcome,
) {
    job["source_records_scanned"] = json!(outcome.records_scanned);
    job["source_records_embedded"] = json!(outcome.records_embedded);
    job["source_records_reused"] = json!(outcome.records_reused);
    job["source_records_filtered"] = json!(outcome.records_filtered);
    job["source_invalidated_chunks"] = json!(outcome.invalidated_chunks);
    job["source_deleted_chunks"] = json!(outcome.deleted_chunks);
    job["source_generation_ready"] = json!(outcome.ready);
    job["source_work_remaining"] = json!(outcome.work_remaining);
}

struct RuntimeSourceSemanticEmbedder<'a> {
    runtime: &'a SharedSemanticRuntime,
    cache_dir: &'a Path,
    deadline: Option<Instant>,
    indexed_chunks: usize,
}

impl SourceBackedSemanticEmbedder for RuntimeSourceSemanticEmbedder<'_> {
    fn embed_chunks(&mut self, chunks: &[SemanticChunkDocument]) -> Result<Vec<Vec<f32>>> {
        let texts = chunks
            .iter()
            .map(|chunk| chunk.text.clone())
            .collect::<Vec<_>>();
        let (embeddings, _) = self
            .runtime
            .embed_documents(self.cache_dir, texts, self.deadline)?;
        self.indexed_chunks = self.indexed_chunks.saturating_add(embeddings.len());
        Ok(embeddings)
    }
}

trait SourceSemanticSessionReader {
    fn events_for_semantic_session(
        &self,
        anchor: &EventRecord,
    ) -> std::result::Result<Vec<EventRecord>, HydrationFailure>;
}

impl SourceSemanticSessionReader for VerifiedIndex {
    fn events_for_semantic_session(
        &self,
        anchor: &EventRecord,
    ) -> std::result::Result<Vec<EventRecord>, HydrationFailure> {
        self.events_for_session(anchor.session_id.as_uuid())
            .map_err(|error| {
                source_hydration_failure(
                    HydrationFailureKind::TemporarilyUnavailable,
                    format!(
                        "read source-backed session {} for semantic lite turn: {error}",
                        anchor.session_id
                    ),
                )
            })
    }
}

struct ProviderSourceSemanticResolver<'a, Index, Sources> {
    index: &'a Index,
    sources: Sources,
}

impl<Index, Sources> SourceBackedSemanticResolver
    for ProviderSourceSemanticResolver<'_, Index, Sources>
where
    Index: SourceSemanticSessionReader,
    Sources: ContentSourceResolver,
{
    fn resolve_document(
        &mut self,
        event: &EventRecord,
        request: &EventHydrationRequest,
    ) -> std::result::Result<SemanticEventDocument, HydrationFailure> {
        validate_source_semantic_request(event, request)?;
        let user_text = hydrate_source_semantic_text(&self.sources, event, request)?;
        let mut sections = vec![format!("user:\n{}", user_text.trim())];
        let mut occurred_at_ms = event.occurred_at_unix_ms.unwrap_or_default();
        if !semantic_hydrated_source_is_control(&sections[0]) {
            if let Some((assistant_text, assistant_at_ms)) = self.paired_assistant(event)? {
                if !assistant_text.trim().is_empty() {
                    sections.push(format!("assistant:\n{}", assistant_text.trim()));
                }
                occurred_at_ms = occurred_at_ms.max(assistant_at_ms);
            }
        }
        let text = sections
            .join("\n\n")
            .chars()
            .take(SEMANTIC_SOURCE_MAX_CHARS)
            .collect::<String>();
        Ok(SemanticEventDocument {
            event_id: event.event_id.as_uuid(),
            history_record_id: None,
            session_id: Some(event.session_id.as_uuid()),
            seq: event.event_sequence,
            occurred_at_ms,
            anchor_occurred_at_ms: event.occurred_at_unix_ms.unwrap_or_default(),
            event_type: parse_source_event_type(&event.event_type)?,
            role: event
                .role
                .as_deref()
                .map(parse_source_event_role)
                .transpose()?,
            rank_bucket: "lite_turn".to_owned(),
            provider: Some(parse_source_provider(&event.provider)?),
            source_format: Some(event.source_format.clone()),
            agent_type: Some(parse_source_agent_type(&event.agent_type)?),
            session_is_primary: Some(event.is_primary),
            cwd: event.cwd.clone(),
            raw_source_path: event.source_path.clone(),
            record_title: None,
            record_kind: Some(event.event_type.clone()),
            record_workspace: event.workspace.clone(),
            text,
        })
    }
}

impl<Index, Sources> ProviderSourceSemanticResolver<'_, Index, Sources>
where
    Index: SourceSemanticSessionReader,
    Sources: ContentSourceResolver,
{
    fn paired_assistant(
        &self,
        anchor: &EventRecord,
    ) -> std::result::Result<Option<(String, i64)>, HydrationFailure> {
        let events = self.index.events_for_semantic_session(anchor)?;
        let anchor_index = events
            .iter()
            .position(|event| event.event_id == anchor.event_id)
            .ok_or_else(|| {
                source_hydration_failure(
                    HydrationFailureKind::MissingRecord,
                    format!(
                        "semantic anchor {} is absent from its session",
                        anchor.event_id
                    ),
                )
            })?;
        let assistant = events[anchor_index.saturating_add(1)..]
            .iter()
            .take_while(|event| {
                !(event.event_type == EventType::Message.as_str()
                    && event.role.as_deref() == Some(EventRole::User.as_str()))
            })
            .filter(|event| {
                event.event_type == EventType::Message.as_str()
                    && event.role.as_deref() == Some(EventRole::Assistant.as_str())
            })
            .last();
        let Some(assistant) = assistant else {
            return Ok(None);
        };
        let request = EventHydrationRequest::new(assistant.event_id, assistant.locator.clone())
            .map_err(|error| {
                source_hydration_failure(
                    HydrationFailureKind::InvalidLocator,
                    format!(
                        "validate paired assistant locator {}: {error}",
                        assistant.event_id
                    ),
                )
            })?;
        Ok(Some((
            hydrate_source_semantic_text(&self.sources, assistant, &request)?,
            assistant.occurred_at_unix_ms.unwrap_or_default(),
        )))
    }
}

fn validate_source_semantic_request(
    event: &EventRecord,
    request: &EventHydrationRequest,
) -> std::result::Result<(), HydrationFailure> {
    let source = request.locator().source();
    if request.event_id() != event.event_id
        || request.locator() != &event.locator
        || source.provider() != event.provider.as_str()
        || source.source_format() != event.source_format.as_str()
    {
        return Err(source_hydration_failure(
            HydrationFailureKind::InvalidLocator,
            format!(
                "mismatched source-backed semantic identity or locator for {}",
                event.event_id
            ),
        ));
    }
    request.locator().validate_contract().map_err(|error| {
        source_hydration_failure(
            HydrationFailureKind::InvalidLocator,
            format!(
                "invalid typed source-backed semantic locator for {}: {error}",
                event.event_id
            ),
        )
    })
}

fn hydrate_source_semantic_text(
    sources: &impl ContentSourceResolver,
    event: &EventRecord,
    request: &EventHydrationRequest,
) -> std::result::Result<String, HydrationFailure> {
    validate_source_semantic_request(event, request)?;
    let hydrated = sources.hydrate_event(request)?;
    if hydrated.event_id != request.event_id() {
        return Err(source_hydration_failure(
            HydrationFailureKind::InvalidLocator,
            format!(
                "provider resolver returned mismatched identity {} for {}",
                hydrated.event_id,
                request.event_id()
            ),
        ));
    }
    let text = String::from_utf8(hydrated.provider_bytes).map_err(|error| {
        source_hydration_failure(
            HydrationFailureKind::UnsupportedParserRevision,
            format!(
                "provider resolver returned non-UTF-8 source content for {}: {}",
                request.event_id(),
                error.utf8_error()
            ),
        )
    })?;
    if text.trim().is_empty() {
        return Err(source_hydration_failure(
            HydrationFailureKind::MissingRecord,
            format!(
                "provider resolver returned no source content for {}",
                request.event_id()
            ),
        ));
    }
    Ok(text)
}

fn source_hydration_failure(
    kind: HydrationFailureKind,
    detail: impl Into<String>,
) -> HydrationFailure {
    HydrationFailure {
        kind,
        detail: detail.into(),
    }
}

fn parse_source_event_type(value: &str) -> std::result::Result<EventType, HydrationFailure> {
    value.parse().map_err(|error| {
        source_hydration_failure(
            HydrationFailureKind::InvalidLocator,
            format!("invalid source-backed event type {value:?}: {error}"),
        )
    })
}

fn parse_source_event_role(value: &str) -> std::result::Result<EventRole, HydrationFailure> {
    value.parse().map_err(|error| {
        source_hydration_failure(
            HydrationFailureKind::InvalidLocator,
            format!("invalid source-backed event role {value:?}: {error}"),
        )
    })
}

fn parse_source_provider(value: &str) -> std::result::Result<CaptureProvider, HydrationFailure> {
    value.parse().map_err(|error| {
        source_hydration_failure(
            HydrationFailureKind::InvalidLocator,
            format!("invalid source-backed provider {value:?}: {error}"),
        )
    })
}

fn parse_source_agent_type(value: &str) -> std::result::Result<AgentType, HydrationFailure> {
    value.parse().map_err(|error| {
        source_hydration_failure(
            HydrationFailureKind::InvalidLocator,
            format!("invalid source-backed agent type {value:?}: {error}"),
        )
    })
}

#[cfg(test)]
pub(super) fn reconcile_committed_semantic_work_with_state(
    data_root: &Path,
    store: &Store,
    sweep: &mut SemanticReconciliationSweepState,
) -> Result<SemanticReconciliationOutcome> {
    let vector_path = source_backed_semantic_vector_path(data_root);
    if !vector_path.exists() {
        sweep.target_version = Some(store.canonical_semantic_projection_version()?);
        sweep.committed_store_complete = true;
        sweep.pruning_complete = true;
        return Ok(SemanticReconciliationOutcome::default());
    }
    let mut vector_store = SemanticVectorStore::open(&vector_path)?;
    prepare_semantic_reconciliation_version(&mut vector_store, store, sweep)?;

    let prune = if sweep.pruning_complete {
        super::vector_store::SemanticPruneOutcome {
            scan_complete: true,
            ..super::vector_store::SemanticPruneOutcome::default()
        }
    } else {
        let prune = vector_store.prune_ineligible_events(store)?;
        if prune.scan_complete {
            sweep.pruning_complete = true;
        }
        prune
    };

    let mut outcome = SemanticReconciliationOutcome {
        pruned_events_scanned: prune.scanned_events,
        deleted_chunks: prune.deleted_chunks,
        queued_stale_events: prune.queued_stale_events,
        ..SemanticReconciliationOutcome::default()
    };
    if sweep.committed_store_complete {
        compare_and_ack_semantic_reconciliation_version(
            &mut vector_store,
            store,
            sweep,
            &mut outcome,
        )?;
        outcome.work_remaining = !sweep.committed_store_complete || !sweep.pruning_complete;
        return Ok(outcome);
    }

    let before = vector_store.committed_store_reconciliation_cursor()?;
    let docs = store
        .recent_event_embedding_documents(before, SEMANTIC_DIRTY_QUEUE_RECENT_LIMIT)?
        .into_iter()
        .map(|document| semantic_event_document_from_store_projection!(document))
        .collect::<Vec<_>>();
    if docs.is_empty() {
        vector_store.set_committed_store_reconciliation_cursor(None)?;
        sweep.committed_store_complete = true;
        compare_and_ack_semantic_reconciliation_version(
            &mut vector_store,
            store,
            sweep,
            &mut outcome,
        )?;
        outcome.work_remaining = !sweep.committed_store_complete || !sweep.pruning_complete;
        return Ok(outcome);
    }
    outcome.committed_documents_scanned = docs.len();
    let next_cursor = if docs.len() == SEMANTIC_DIRTY_QUEUE_RECENT_LIMIT {
        docs.iter()
            .map(|doc| (doc.anchor_occurred_at_ms, doc.seq))
            .min()
    } else {
        None
    };
    let event_ids = docs.iter().map(|doc| doc.event_id).collect::<Vec<_>>();
    let existing_hashes = vector_store.existing_hashes_for_event_ids(&event_ids)?;
    let missing_or_changed = docs
        .into_iter()
        .filter(|doc| {
            let source_text = semantic_source_text(&doc.text);
            let current_hash = semantic_document_hash(doc, &source_text);
            existing_hashes
                .get(&doc.event_id)
                .is_none_or(|stored_hash| stored_hash != &current_hash)
        })
        .collect::<Vec<_>>();
    outcome.committed_documents_queued =
        vector_store.enqueue_dirty_documents(&missing_or_changed, "committed_store_reconcile")?;
    vector_store.set_committed_store_reconciliation_cursor(next_cursor)?;
    if next_cursor.is_none() {
        sweep.committed_store_complete = true;
    }
    compare_and_ack_semantic_reconciliation_version(&mut vector_store, store, sweep, &mut outcome)?;
    outcome.work_remaining = !sweep.committed_store_complete || !sweep.pruning_complete;
    Ok(outcome)
}

#[cfg(test)]
fn prepare_semantic_reconciliation_version(
    vector_store: &mut SemanticVectorStore,
    store: &Store,
    sweep: &mut SemanticReconciliationSweepState,
) -> Result<()> {
    let current_version = store.canonical_semantic_projection_version()?;
    let acknowledged_version = vector_store.reconciled_store_version()?;
    let durable_target_version = vector_store.reconciliation_target_store_version()?;

    match sweep.target_version {
        Some(target_version) if current_version.store_identity != target_version.store_identity => {
            rearm_semantic_reconciliation(vector_store, sweep, current_version)
        }
        Some(target_version)
            if sweep.committed_store_complete
                && sweep.pruning_complete
                && current_version != target_version =>
        {
            rearm_semantic_reconciliation(vector_store, sweep, current_version)
        }
        // Finish an active sweep even when its epoch advances. Completion
        // compare-and-ack will start a successor sweep at the newest epoch.
        // Restarting here would starve large histories under steady ingestion.
        Some(_) => Ok(()),
        None if acknowledged_version == Some(current_version) => {
            sweep.target_version = Some(current_version);
            sweep.committed_store_complete = true;
            sweep.pruning_complete = true;
            Ok(())
        }
        None if durable_target_version
            .is_some_and(|target| target.store_identity == current_version.store_identity) =>
        {
            sweep.target_version = durable_target_version;
            Ok(())
        }
        None => rearm_semantic_reconciliation(vector_store, sweep, current_version),
    }
}

#[cfg(test)]
fn rearm_semantic_reconciliation(
    vector_store: &mut SemanticVectorStore,
    sweep: &mut SemanticReconciliationSweepState,
    target_version: CanonicalSemanticProjectionVersion,
) -> Result<()> {
    vector_store.begin_reconciliation_version(target_version)?;
    sweep.target_version = Some(target_version);
    sweep.committed_store_complete = false;
    sweep.pruning_complete = false;
    Ok(())
}

#[cfg(test)]
fn compare_and_ack_semantic_reconciliation_version(
    vector_store: &mut SemanticVectorStore,
    store: &Store,
    sweep: &mut SemanticReconciliationSweepState,
    outcome: &mut SemanticReconciliationOutcome,
) -> Result<()> {
    if !sweep.committed_store_complete || !sweep.pruning_complete {
        return Ok(());
    }
    let target_version = sweep
        .target_version
        .ok_or_else(|| anyhow!("semantic reconciliation completed without a Store version"))?;
    let completion_version = store.canonical_semantic_projection_version()?;
    if completion_version == target_version {
        vector_store.acknowledge_reconciliation_version(target_version)?;
        let post_ack_version = store.canonical_semantic_projection_version()?;
        if post_ack_version != target_version {
            rearm_semantic_reconciliation(vector_store, sweep, post_ack_version)?;
            outcome.work_remaining = true;
        }
        return Ok(());
    }

    rearm_semantic_reconciliation(vector_store, sweep, completion_version)?;
    outcome.work_remaining = true;
    Ok(())
}

#[cfg(test)]
pub(super) fn daemon_semantic_requested_seconds(args: &DaemonRunArgs) -> u64 {
    semantic_worker_seconds_budget(&SemanticWorkerArgs {
        max_chunks: args.max_chunks,
        max_seconds: args.max_seconds,
    })
}

#[cfg(test)]
pub(super) fn semantic_daemon_model_load_needed(
    report: &SemanticWorkerReport,
    runtime_loaded: bool,
) -> bool {
    report.searchable_items > 0 && !runtime_loaded
}

#[cfg(test)]
pub(super) fn daemon_semantic_worker_seconds_budget(
    args: &DaemonRunArgs,
    deadline: Option<Instant>,
) -> u64 {
    let requested = daemon_semantic_requested_seconds(args);
    let Some(remaining) = daemon_deadline_remaining(deadline) else {
        return if deadline.is_none() { requested } else { 0 };
    };
    let remaining_secs = remaining
        .as_secs()
        .saturating_sub(DAEMON_SEMANTIC_RESERVE_GRACE_SECS);
    requested.min(remaining_secs)
}

pub(super) fn daemon_semantic_deadline_skipped_job(data_root: &Path) -> Value {
    let _ = data_root;
    daemon_semantic_job_json(
        "skipped",
        Some("daemon_deadline"),
        utc_now().timestamp_millis(),
        None,
        None,
    )
}

pub(super) fn daemon_semantic_skipped_job(
    data_root: &Path,
    semantic_enabled: bool,
    reason: &str,
) -> Value {
    let _ = data_root;
    daemon_semantic_job_json(
        if semantic_enabled {
            "skipped"
        } else {
            "disabled"
        },
        Some(if semantic_enabled {
            reason
        } else {
            "semantic_disabled"
        }),
        utc_now().timestamp_millis(),
        None,
        None,
    )
}

pub(super) fn daemon_semantic_retry_backoff_job(
    data_root: &Path,
    backoff: &DaemonRetryBackoff,
) -> Value {
    let mut job = daemon_semantic_skipped_job(data_root, true, "retry_backoff");
    job["retryable"] = Value::Bool(true);
    job["retry_after_ms"] = json!(backoff.retry_after_ms().unwrap_or(0));
    job["consecutive_failures"] = json!(backoff.consecutive_failures);
    job["retry_not_before_at_ms"] = json!(backoff.retry_not_before_at_ms);
    job
}

pub(super) fn daemon_semantic_failed_job(data_root: &Path, error: anyhow::Error) -> Value {
    let _ = data_root;
    let failure_class = classify_semantic_failure(&error);
    annotate_semantic_failure(
        daemon_semantic_job_json(
            "failed",
            None,
            utc_now().timestamp_millis(),
            None,
            Some(format!("{error:#}")),
        ),
        failure_class,
    )
}

pub(super) fn daemon_semantic_job_json(
    status: &str,
    reason: Option<&str>,
    last_run_at_ms: i64,
    indexed_chunks: Option<usize>,
    last_error: Option<String>,
) -> Value {
    compact_json(json!({
        "schema_version": 1,
        "status": status,
        "model_key": semantic_model_key(),
        "reason": reason,
        "last_run_at_ms": last_run_at_ms,
        "last_error": last_error,
        "indexed_chunks": indexed_chunks,
    }))
}

pub(super) fn daemon_semantic_model_load_deferred_job(
    last_run_at_ms: i64,
    deferred: &SemanticModelLoadDeferred,
) -> Value {
    let mut value = daemon_semantic_job_json(
        "skipped",
        Some("memory_pressure"),
        last_run_at_ms,
        None,
        None,
    );
    value["failure_class"] = Value::String("resource_pressure".to_owned());
    value["retryable"] = Value::Bool(true);
    value["available_memory_bytes"] = json!(deferred.available_memory_bytes);
    value["required_available_memory_bytes"] = json!(deferred.required_available_memory_bytes);
    compact_json(value)
}

pub(super) fn daemon_semantic_resource_deferred_job(
    last_run_at_ms: i64,
    deferred: SemanticResourceDeferred,
) -> Value {
    let mut value = daemon_semantic_job_json(
        "resource_deferred",
        Some(deferred.reason.as_str()),
        last_run_at_ms,
        None,
        None,
    );
    value["failure_class"] = Value::String("resource_pressure".to_owned());
    value["retryable"] = Value::Bool(true);
    value["resource_deferral"] = deferred.to_json();
    compact_json(value)
}

#[cfg(test)]
pub(super) fn write_daemon_lifecycle_status(
    data_root: &Path,
    args: &DaemonRunArgs,
    status: &str,
    started_at_ms: i64,
    finished_at_ms: Option<i64>,
    last_error: Option<String>,
) -> Result<()> {
    write_daemon_lifecycle_status_observed(
        data_root,
        args,
        status,
        started_at_ms,
        finished_at_ms,
        last_error,
        None,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn write_daemon_lifecycle_status_with_runtime(
    data_root: &Path,
    args: &DaemonRunArgs,
    status: &str,
    started_at_ms: i64,
    finished_at_ms: Option<i64>,
    last_error: Option<String>,
    semantic_runtime_active: bool,
    config_reload: &Value,
) -> Result<()> {
    write_daemon_lifecycle_status_observed(
        data_root,
        args,
        status,
        started_at_ms,
        finished_at_ms,
        last_error,
        Some(semantic_runtime_active),
        Some(config_reload),
    )
}

#[allow(clippy::too_many_arguments)]
fn write_daemon_lifecycle_status_observed(
    data_root: &Path,
    args: &DaemonRunArgs,
    status: &str,
    started_at_ms: i64,
    finished_at_ms: Option<i64>,
    last_error: Option<String>,
    semantic_runtime_active: Option<bool>,
    config_reload: Option<&Value>,
) -> Result<()> {
    write_daemon_status(
        data_root,
        &compact_json(json!({
            "schema_version": 1,
            "status": status,
            "pid": process::id(),
            "started_at_ms": started_at_ms,
            "heartbeat_at_ms": utc_now().timestamp_millis(),
            "finished_at_ms": finished_at_ms,
            "start_mode": daemon_run_start_mode(args).as_str(),
            "trigger_command": args.trigger_command.map(DaemonTriggerCommandArg::as_str),
            "last_error": last_error,
            "semantic_runtime_active": semantic_runtime_active,
            "config_reload": config_reload,
        })),
    )
}

pub(super) fn semantic_worker_report_for_daemon(data_root: &Path) -> SemanticWorkerReport {
    let source_vector_path = source_backed_semantic_vector_path(data_root);
    let report = (|| -> Result<SemanticWorkerReport> {
        let source_generation = pin_published_generation(data_root)?;
        let (searchable_items, searchable_items_known, embedded_items, embedded_chunks, ready) =
            if let Some(source_generation) = source_generation {
                let semantic_documents = source_generation.semantic_eligible_event_count()?;
                let searchable_items = usize::try_from(semantic_documents).unwrap_or(usize::MAX);
                let vector_store = SemanticVectorStore::open_read_only(&source_vector_path)?;
                let stats = vector_store
                    .as_ref()
                    .map(SemanticVectorStore::cached_or_exact_stats)
                    .transpose()?
                    .unwrap_or_default();
                let ready = vector_store
                    .as_ref()
                    .map(|store| {
                        store.source_backed_generation_ready_exact(
                            source_generation.generation_id(),
                            semantic_documents,
                        )
                    })
                    .transpose()?
                    .unwrap_or(false);
                (
                    searchable_items,
                    true,
                    stats.embedded_items,
                    stats.embedded_chunks,
                    ready,
                )
            } else {
                (0, false, 0, 0, false)
            };
        Ok(SemanticWorkerReport {
            status: if ready {
                "ready".to_owned()
            } else if searchable_items_known {
                "pending".to_owned()
            } else {
                "waiting_for_source_generation".to_owned()
            },
            running: false,
            pid: None,
            started_at_ms: None,
            heartbeat_at_ms: None,
            finished_at_ms: None,
            indexed_chunks: None,
            model_init_ms: None,
            last_error: None,
            searchable_items,
            searchable_items_known,
            embedded_items,
            embedded_chunks,
            dirty_items: 0,
            queued_items_estimate: searchable_items.saturating_sub(embedded_items),
            model_cache_available: semantic_model_cache_available(&semantic_worker_cache_dir(
                data_root,
            )),
            model_acquisition: semantic_model_acquisition_status_json(&semantic_worker_cache_dir(
                data_root,
            )),
            embed_policy: Some(semantic_embed_policy_status_json()),
            embedding_runtime: None,
            failure_class: None,
            resource_deferral: None,
            vector_path: source_vector_path.clone(),
            lock_path: semantic_worker_lock_path(data_root),
            status_path: semantic_worker_status_path(data_root),
        })
    })();
    report.unwrap_or_else(|error| {
        let mut report = SemanticWorkerReport::unavailable(data_root, format!("{error:#}"));
        report.vector_path = source_vector_path;
        report
    })
}

#[cfg(test)]
pub(super) fn write_semantic_worker_failure_status(
    data_root: &Path,
    message: String,
) -> Result<()> {
    let now = utc_now().timestamp_millis();
    write_semantic_worker_status(
        data_root,
        &json!({
            "schema_version": 1,
            "status": "failed",
            "model_key": semantic_model_key(),
            "pid": process::id(),
            "heartbeat_at_ms": now,
            "finished_at_ms": now,
            "last_error": message,
            "model_acquisition": semantic_model_acquisition_status_json(
                &semantic_worker_cache_dir(data_root),
            ),
            "embed_policy": semantic_embed_policy_status_json(),
        }),
    )
}

pub(super) fn write_semantic_model_acquisition_status(
    data_root: &Path,
    status: &str,
    message: Option<String>,
    failure_class: Option<SemanticFailureClass>,
) -> Result<()> {
    let now = utc_now().timestamp_millis();
    write_semantic_worker_status(
        data_root,
        &json!({
            "schema_version": 1,
            "status": status,
            "model_key": semantic_model_key(),
            "pid": process::id(),
            "heartbeat_at_ms": now,
            "finished_at_ms": matches!(
                status,
                "model_acquisition_failed" | "model_integrity_failed"
            )
            .then_some(now),
            "last_error": message,
            "failure_class": failure_class.map(SemanticFailureClass::as_str),
            "retryable": failure_class.map(|class| matches!(
                class,
                SemanticFailureClass::Retryable | SemanticFailureClass::ResourcePressure
            )),
            "model_acquisition": semantic_model_acquisition_status_json(
                &semantic_worker_cache_dir(data_root),
            ),
            "embed_policy": semantic_embed_policy_status_json(),
        }),
    )
}

pub(super) fn write_semantic_model_load_status(
    data_root: &Path,
    status: &str,
    message: Option<String>,
    failure_class: Option<SemanticFailureClass>,
) -> Result<()> {
    let now = utc_now().timestamp_millis();
    write_semantic_worker_status(
        data_root,
        &json!({
            "schema_version": 1,
            "status": status,
            "model_key": semantic_model_key(),
            "pid": process::id(),
            "heartbeat_at_ms": now,
            "finished_at_ms": (status == "model_load_failed").then_some(now),
            "last_error": message,
            "failure_class": failure_class.map(SemanticFailureClass::as_str),
            "retryable": failure_class.map(|class| matches!(
                class,
                SemanticFailureClass::Retryable | SemanticFailureClass::ResourcePressure
            )),
            "model_acquisition": semantic_model_acquisition_status_json(
                &semantic_worker_cache_dir(data_root),
            ),
            "embed_policy": semantic_embed_policy_status_json(),
        }),
    )
}

pub(super) fn write_semantic_model_load_deferred_status(
    data_root: &Path,
    deferred: &SemanticModelLoadDeferred,
) -> Result<()> {
    let now = utc_now().timestamp_millis();
    write_semantic_worker_status(
        data_root,
        &compact_json(json!({
            "schema_version": 1,
            "status": "model_load_deferred",
            "model_key": semantic_model_key(),
            "pid": process::id(),
            "heartbeat_at_ms": now,
            "finished_at_ms": now,
            "last_error": null,
            "failure_class": "resource_pressure",
            "retryable": true,
            "available_memory_bytes": deferred.available_memory_bytes,
            "required_available_memory_bytes": deferred.required_available_memory_bytes,
            "model_acquisition": semantic_model_acquisition_status_json(
                &semantic_worker_cache_dir(data_root),
            ),
            "embed_policy": semantic_embed_policy_status_json(),
        })),
    )
}

pub(super) fn write_semantic_resource_deferred_status(
    data_root: &Path,
    deferred: SemanticResourceDeferred,
) -> Result<()> {
    let now = utc_now().timestamp_millis();
    write_semantic_worker_status(
        data_root,
        &compact_json(json!({
            "schema_version": 1,
            "status": "resource_deferred",
            "model_key": semantic_model_key(),
            "pid": process::id(),
            "heartbeat_at_ms": now,
            "finished_at_ms": now,
            "last_error": null,
            "failure_class": "resource_pressure",
            "retryable": true,
            "resource_deferral": deferred.to_json(),
            "model_acquisition": semantic_model_acquisition_status_json(
                &semantic_worker_cache_dir(data_root),
            ),
            "embed_policy": semantic_embed_policy_status_json(),
        })),
    )
}

pub(super) fn write_semantic_model_loaded_status(
    data_root: &Path,
    embedding_runtime: Option<Value>,
    embed_policy: Value,
) -> Result<()> {
    let now = utc_now().timestamp_millis();
    write_semantic_worker_status(
        data_root,
        &json!({
            "schema_version": 1,
            "status": "model_loaded",
            "model_key": semantic_model_key(),
            "pid": process::id(),
            "heartbeat_at_ms": now,
            "finished_at_ms": now,
            "model_acquisition": semantic_model_acquisition_status_json(
                &semantic_worker_cache_dir(data_root),
            ),
            "embedding_runtime": embedding_runtime,
            "embed_policy": embed_policy,
        }),
    )
}

#[cfg(test)]
pub(super) fn run_semantic_worker_inner_with_runtime(
    args: SemanticWorkerArgs,
    data_root: &Path,
    runtime: &SharedSemanticRuntime,
) -> Result<()> {
    let Some(_lock) = SemanticWorkerLock::acquire(data_root)? else {
        return Ok(());
    };

    let db_path = database_path(data_root.to_path_buf());
    if !db_path.exists() {
        return Err(anyhow!(
            "ctx index does not exist yet; run `ctx import --all` or `ctx setup` first"
        ));
    }
    let cache_dir = semantic_worker_cache_dir(data_root);
    if !runtime.is_loaded() && !semantic_model_cache_available(&cache_dir) {
        return Err(anyhow!(
            "semantic model is not available in the local cache; background indexing will not initialize or download {SEMANTIC_MODEL_ID}"
        ));
    }
    let store = Store::open(&db_path).context("open ctx store for semantic worker")?;
    refresh_semantic_document_count_cache(&store)?;
    let vector_path = source_backed_semantic_vector_path(data_root);
    let mut vector_store = SemanticVectorStore::open(&vector_path)?;
    let prune_outcome = vector_store.prune_ineligible_events(&store)?;
    let started_at_ms = utc_now().timestamp_millis();
    let initial_stats = vector_store
        .cached_stats()?
        .unwrap_or_else(SemanticSidecarStats::default);
    let initial_dirty_items = vector_store.dirty_event_count()?;
    let searchable_items = store.event_embedding_document_count_cached_or_exact()?;
    let initial_queued_items_estimate = searchable_items
        .saturating_sub(initial_stats.embedded_items)
        .max(initial_dirty_items);
    let was_ready_before_worker =
        semantic_worker_status_was_ready_for_stats(data_root, initial_stats);
    let continue_past_indexed_pages = !was_ready_before_worker
        || initial_queued_items_estimate > SEMANTIC_DIRTY_QUEUE_RECENT_LIMIT;
    let starting_embed_policy = runtime.policy_status_json()?;
    let starting_embedding_runtime = runtime.runtime_status_json()?;
    write_semantic_worker_status(
        data_root,
        &json!({
            "schema_version": 1,
            "status": "running",
            "model_key": semantic_model_key(),
            "pid": process::id(),
            "started_at_ms": started_at_ms,
            "heartbeat_at_ms": started_at_ms,
            "indexed_chunks": 0,
            "pruned_chunks": prune_outcome.deleted_chunks,
            "stale_events_queued": prune_outcome.queued_stale_events,
            "searchable_items": searchable_items,
            "embedded_items": initial_stats.embedded_items,
            "embedded_chunks": initial_stats.embedded_chunks,
            "dirty_items": initial_dirty_items,
            "embed_policy": starting_embed_policy,
            "embedding_runtime": starting_embedding_runtime,
            "last_error": null,
        }),
    )?;
    let max_chunks = semantic_worker_chunk_budget(&args);
    let max_seconds = semantic_worker_seconds_budget(&args);
    let started = Instant::now();
    let deadline = started + StdDuration::from_secs(max_seconds);
    let mut model_init_ms = None;
    let indexed_chunks = if Instant::now() >= deadline {
        0
    } else {
        backfill_semantic_embeddings(
            &store,
            &mut vector_store,
            runtime,
            &mut model_init_ms,
            &cache_dir,
            max_chunks,
            continue_past_indexed_pages,
            Some(deadline),
        )?
    };
    let elapsed = started.elapsed();
    let finished_embed_policy = runtime.policy_status_json()?;
    let finished_embedding_runtime = runtime.runtime_status_json()?;
    let elapsed_ms = elapsed.as_millis() as u64;
    let final_stats = vector_store
        .cached_stats()?
        .unwrap_or_else(SemanticSidecarStats::default);
    let final_dirty_items = vector_store.dirty_event_count()?;
    refresh_semantic_document_count_cache(&store)?;
    let searchable_items = store.event_embedding_document_count_cached_or_exact()?;
    let status = if searchable_items > 0
        && final_stats.embedded_items >= searchable_items
        && final_dirty_items == 0
    {
        vector_store.set_backfill_cursor(None)?;
        "ready"
    } else if elapsed >= StdDuration::from_secs(max_seconds) {
        "budget_exhausted"
    } else {
        "completed"
    };
    let finished_at_ms = utc_now().timestamp_millis();
    write_semantic_worker_status(
        data_root,
        &json!({
            "schema_version": 1,
            "status": status,
            "model_key": semantic_model_key(),
            "pid": process::id(),
            "started_at_ms": started_at_ms,
            "heartbeat_at_ms": finished_at_ms,
            "finished_at_ms": finished_at_ms,
            "indexed_chunks": indexed_chunks,
            "pruned_chunks": prune_outcome.deleted_chunks,
            "stale_events_queued": prune_outcome.queued_stale_events,
            "elapsed_ms": elapsed_ms,
            "model_init_ms": model_init_ms,
            "searchable_items": searchable_items,
            "embedded_items": final_stats.embedded_items,
            "embedded_chunks": final_stats.embedded_chunks,
            "dirty_items": final_dirty_items,
            "embed_policy": finished_embed_policy,
            "embedding_runtime": finished_embedding_runtime,
            "last_error": null,
        }),
    )?;
    drop(_lock);
    Ok(())
}

#[cfg(test)]
pub(super) fn semantic_worker_chunk_budget(args: &SemanticWorkerArgs) -> usize {
    args.max_chunks
        .or_else(|| env_usize("CTX_SEMANTIC_WORKER_MAX_CHUNKS"))
        .map(|value| value.min(SEMANTIC_WORKER_BATCH_MAX))
        .unwrap_or(SEMANTIC_WORKER_BATCH_DEFAULT)
}

#[cfg(test)]
pub(super) fn semantic_worker_seconds_budget(args: &SemanticWorkerArgs) -> u64 {
    args.max_seconds
        .or_else(|| {
            env::var("CTX_SEMANTIC_WORKER_MAX_SECONDS")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|value| *value > 0)
        })
        .map(|value| value.min(SEMANTIC_WORKER_MAX_SECONDS_CAP))
        .unwrap_or(SEMANTIC_WORKER_MAX_SECONDS_DEFAULT)
}

#[cfg(test)]
pub(super) fn semantic_worker_status_was_ready_for_stats(
    data_root: &Path,
    stats: SemanticSidecarStats,
) -> bool {
    let Some(value) = read_semantic_worker_status(data_root) else {
        return false;
    };
    if !semantic_status_file_model_matches(Some(&value)) {
        return false;
    }
    let status_ready = json_string(&value, "status").is_some_and(|status| status == "ready");
    let dirty_items = json_usize(&value, "dirty_items").unwrap_or(usize::MAX);
    let embedded_items = json_usize(&value, "embedded_items").unwrap_or(0);
    let searchable_items = json_usize(&value, "searchable_items").unwrap_or(usize::MAX);
    status_ready
        && dirty_items == 0
        && embedded_items == stats.embedded_items
        && embedded_items >= searchable_items
}

#[cfg(test)]
pub(super) fn queue_recent_semantic_work(
    data_root: &Path,
    store: &Store,
    reason: &str,
) -> Result<usize> {
    let vector_path = source_backed_semantic_vector_path(data_root);
    if !vector_path.exists()
        && !semantic_model_cache_available(&semantic_worker_cache_dir(data_root))
    {
        return Ok(0);
    }
    let docs = store
        .recent_event_embedding_documents(None, SEMANTIC_DIRTY_QUEUE_RECENT_LIMIT)?
        .into_iter()
        .map(|document| semantic_event_document_from_store_projection!(document))
        .collect::<Vec<_>>();
    if docs.is_empty() {
        return Ok(0);
    }
    let mut vector_store = SemanticVectorStore::open(&vector_path)?;
    let existing_hashes = vector_store
        .existing_hashes_for_event_ids(&docs.iter().map(|doc| doc.event_id).collect::<Vec<_>>())?;
    let docs = docs
        .into_iter()
        .filter(|doc| {
            let source_text = semantic_source_text(&doc.text);
            let hash = semantic_document_hash(doc, &source_text);
            existing_hashes
                .get(&doc.event_id)
                .map(|existing| existing != &hash)
                .unwrap_or(true)
        })
        .collect::<Vec<_>>();
    vector_store.enqueue_dirty_documents(&docs, reason)
}

#[cfg(test)]
#[path = "daemon_worker_tests.rs"]
mod source_semantic_tests;
