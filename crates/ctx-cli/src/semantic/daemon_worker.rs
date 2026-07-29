use std::{path::Path, process, time::Instant};

use anyhow::Result;
#[cfg(test)]
use ctx_history_capture::SourceBackedResolverRegistry;
use ctx_history_core::{
    utc_now, AgentType, BatchHydrationRequest, BatchHydrationResult, CaptureProvider,
    ContentSourceResolver, EventHydrationRequest, EventRole, EventType, HydratedProviderRecord,
    HydrationFailure, HydrationFailureKind,
};
use ctx_history_index::{EventRecord, VerifiedIndex};
use serde_json::{json, Value};

use crate::{DaemonRunArgs, DaemonTriggerCommandArg};

use super::{
    daemon::DaemonRuntime,
    daemon_retry::{
        annotate_semantic_failure, classify_semantic_failure, DaemonRetryBackoff,
    },
    daemon_scheduler::{daemon_deadline_has_min_budget, daemon_run_start_mode},
    health_search::{semantic_model_acquisition_integrity_error, semantic_worker_cache_dir},
    model_contract::{semantic_model_key, SemanticModelLoadDeferred},
    model_runtime::{
        SemanticDaemonCpuFallbackRequired, SemanticDaemonModelAcquisition, SharedSemanticRuntime,
    },
    paths_status::write_daemon_status,
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
use super::daemon::daemon_test_job;

use crate::output::compact_json;

#[derive(Debug)]
pub(super) enum DaemonSemanticModelStartup {
    Loaded,
    Finished(Value),
}

fn daemon_semantic_model_acquisition_error(
    last_run_at_ms: i64,
    error: anyhow::Error,
) -> DaemonSemanticModelStartup {
    if let Some(deferred) = error.downcast_ref::<SemanticModelLoadDeferred>() {
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
    last_run_at_ms: i64,
    acquire: Acquire,
    acquire_cpu_fallback: AcquireCpuFallback,
    mut load: Load,
) -> Result<DaemonSemanticModelStartup>
where
    Acquire: FnOnce() -> Result<SemanticDaemonModelAcquisition>,
    AcquireCpuFallback: FnOnce(&'static str) -> Result<SemanticDaemonModelAcquisition>,
    Load: FnMut(SemanticDaemonModelAcquisition) -> Result<()>,
{
    let mut acquisition = match acquire() {
        Ok(acquisition) => acquisition,
        Err(error) => {
            return Ok(daemon_semantic_model_acquisition_error(
                last_run_at_ms,
                error,
            ))
        }
    };
    let mut acquire_cpu_fallback = Some(acquire_cpu_fallback);

    loop {
        match load(acquisition) {
            Ok(()) => return Ok(DaemonSemanticModelStartup::Loaded),
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
                acquisition = match acquire_cpu_fallback(reason) {
                    Ok(acquisition) => acquisition,
                    Err(error) => {
                        return Ok(daemon_semantic_model_acquisition_error(
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
                return Ok(DaemonSemanticModelStartup::Finished(
                    daemon_semantic_model_load_deferred_job(last_run_at_ms, deferred),
                ));
            }
            Err(error) => {
                let message = format!("{error:#}");
                let failure_class = classify_semantic_failure(&error);
                let failure_code = "model_load_failed";
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
                Ok(())
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

#[cfg(test)]
#[path = "daemon_worker_tests.rs"]
mod source_semantic_tests;
