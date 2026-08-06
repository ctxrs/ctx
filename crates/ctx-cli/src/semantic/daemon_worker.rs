use std::{path::Path, process, time::Instant};

use anyhow::{anyhow, Result};
use ctx_history_core::{utc_now, AgentType, CaptureProvider, EventRole, EventType};
use ctx_history_index::{CoreEventPageBudget, CoreEventRecord, VerifiedIndex};
use ctx_semantic_index::{
    semantic_core_content_is_control, source_backed_semantic_vector_path, SemanticBatchEmbedder,
    SemanticChunkDocument, SemanticDocumentBuilder, SemanticEventDocument, SemanticVectorStore,
    SourceBackedGenerationPin, SourceBackedSemanticOutcome,
};
use ctx_semantic_model::{
    semantic_model_acquisition_integrity_error, semantic_model_key, ArtifactFetchRequest,
    ArtifactFetcher, SemanticDaemonCpuFallbackRequired, SemanticDaemonModelAcquisition,
    SemanticModelConfig, SemanticModelLoadDeferred, SharedSemanticRuntime,
};
use serde_json::{json, Value};

use crate::{DaemonRunArgs, DaemonTriggerCommandArg};

use super::{
    daemon::DaemonRuntime,
    daemon_retry::{annotate_semantic_failure, classify_semantic_failure, DaemonRetryBackoff},
    daemon_scheduler::{daemon_deadline_has_min_budget, daemon_run_start_mode},
    model_config::semantic_model_config,
    paths_status::write_daemon_status,
    resource_policy::{
        semantic_background_resource_deferred, semantic_resource_deferral_releases_runtime,
        SemanticBackgroundOperation, SemanticResourceDeferred,
    },
    runtime_limits::{
        DAEMON_MIN_REMAINING_FOR_JOB_SECS, DAEMON_SEMANTIC_RESERVE_GRACE_SECS,
        SEMANTIC_MODEL_INIT_MIN_REMAINING_SECS,
    },
    source_backed_refresh_coordinator::{pin_published_generation, PinnedSourceBackedGeneration},
};

#[cfg(test)]
use super::daemon::daemon_test_job;

use crate::output::compact_json;

const MAX_LITE_TURN_PAIRING_PAGE_RECORDS: usize = 64;
const LITE_TURN_PAIRING_BUDGET: CoreEventPageBudget =
    CoreEventPageBudget::new(64 * 1024 * 1024, 16 * 1024 * 1024);

struct CliDaemonArtifactFetcher;

impl ArtifactFetcher for CliDaemonArtifactFetcher {
    fn fetch_to_writer(
        &self,
        request: ArtifactFetchRequest<'_>,
        mut writer: &mut dyn std::io::Write,
    ) -> Result<u64> {
        crate::net::get_to_writer_limited(
            request.endpoint(),
            request.max_bytes(),
            request.timeout(),
            &mut writer,
        )
    }
}

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
        if semantic_resource_deferral_releases_runtime(deferred.reason()) {
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
    let source_pending = matches!(
        vector_store.source_backed_generation_pin_exact(
            source_generation.generation_id(),
            source_eligible_events,
        )?,
        SourceBackedGenerationPin::NotReady
    );
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
    let model_config = semantic_model_config(data_root);
    if source_model_load_needed {
        match run_daemon_semantic_model_startup_with(
            last_run_at_ms,
            || {
                runtime
                    .semantic_runtime
                    .acquire_for_daemon(&model_config, &CliDaemonArtifactFetcher)
            },
            |fallback| {
                runtime
                    .semantic_runtime
                    .acquire_cpu_fallback_for_daemon(&model_config, fallback)
            },
            |acquisition| {
                runtime
                    .semantic_runtime
                    .ensure_loaded_after_daemon_acquisition(&model_config, acquisition)?;
                Ok(())
            },
        )? {
            DaemonSemanticModelStartup::Loaded => {}
            DaemonSemanticModelStartup::Finished(job) => return Ok(job),
        }
    }
    let (outcome, indexed_chunks) = reconcile_source_backed_semantic_page(
        data_root,
        source_generation,
        &mut vector_store,
        &runtime.semantic_runtime,
        &model_config,
        deadline,
    )?;
    let (status, reason, last_error) = if outcome.ready() {
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
    _data_root: &Path,
    generation: PinnedSourceBackedGeneration,
    vector_store: &mut SemanticVectorStore,
    runtime: &SharedSemanticRuntime,
    model_config: &SemanticModelConfig,
    deadline: Option<Instant>,
) -> Result<(SourceBackedSemanticOutcome, usize)> {
    let index = generation.into_index();
    let mut builder = CoreSemanticDocumentBuilder::new(&index);
    let mut embedder = RuntimeSourceSemanticEmbedder {
        runtime,
        model_config,
        deadline,
        indexed_chunks: 0,
    };
    let outcome =
        vector_store.reconcile_source_backed_index(&index, &mut builder, &mut embedder)?;
    Ok((outcome, embedder.indexed_chunks))
}

struct CoreSemanticDocumentBuilder<'a> {
    index: &'a VerifiedIndex,
    pairing_page_records: usize,
    pairing_budget: CoreEventPageBudget,
}

impl SemanticDocumentBuilder for CoreSemanticDocumentBuilder<'_> {
    fn build_document(
        &mut self,
        record: &CoreEventRecord,
    ) -> Result<Option<SemanticEventDocument>> {
        let user_text = record.core_record.content.meaningful_text();
        if user_text.trim().is_empty() {
            return Ok(None);
        }
        let mut sections = vec![format!("user:\n{}", user_text.trim())];
        let mut occurred_at_ms = record.occurred_at_unix_ms.unwrap_or_default();
        if !semantic_core_content_is_control(&sections[0]) {
            if let Some((assistant_text, assistant_at_ms)) = self.paired_assistant(record)? {
                sections.push(format!("assistant:\n{}", assistant_text.trim()));
                occurred_at_ms = occurred_at_ms.max(assistant_at_ms);
            }
        }
        Ok(Some(SemanticEventDocument::new(
            record.event_id.as_uuid(),
            Some(record.session_id.as_uuid()),
            record.event_sequence,
            occurred_at_ms,
            parse_core_event_type(&record.event_type)?,
            record
                .role
                .as_deref()
                .map(parse_core_event_role)
                .transpose()?,
            "lite_turn".to_owned(),
            Some(parse_core_provider(&record.provider)?),
            Some(record.source_format.clone()),
            Some(parse_core_agent_type(&record.agent_type)?),
            Some(record.is_primary),
            record.cwd.clone(),
            None,
            Some(record.event_type.clone()),
            record.workspace.clone(),
            sections.join("\n\n"),
        )))
    }
}

impl CoreSemanticDocumentBuilder<'_> {
    fn new(index: &VerifiedIndex) -> CoreSemanticDocumentBuilder<'_> {
        CoreSemanticDocumentBuilder {
            index,
            pairing_page_records: MAX_LITE_TURN_PAIRING_PAGE_RECORDS,
            pairing_budget: LITE_TURN_PAIRING_BUDGET,
        }
    }

    fn paired_assistant(&self, anchor: &CoreEventRecord) -> Result<Option<(String, i64)>> {
        Ok(self.index.semantic_lite_turn_assistant(
            anchor,
            self.pairing_page_records,
            self.pairing_budget,
        )?)
    }
}

fn annotate_source_backed_semantic_progress(
    job: &mut Value,
    outcome: &SourceBackedSemanticOutcome,
) {
    job["source_records_decoded"] = json!(outcome.records_decoded());
    job["source_records_embedded"] = json!(outcome.records_embedded());
    job["source_records_reused"] = json!(outcome.records_reused());
    job["source_records_filtered"] = json!(outcome.records_filtered());
    job["source_invalidated_chunks"] = json!(outcome.invalidated_chunks());
    job["source_deleted_chunks"] = json!(outcome.deleted_chunks());
    job["source_generation_ready"] = json!(outcome.ready());
    job["source_work_remaining"] = json!(outcome.work_remaining());
}

struct RuntimeSourceSemanticEmbedder<'a> {
    runtime: &'a SharedSemanticRuntime,
    model_config: &'a SemanticModelConfig,
    deadline: Option<Instant>,
    indexed_chunks: usize,
}

impl SemanticBatchEmbedder for RuntimeSourceSemanticEmbedder<'_> {
    fn embed_chunks(&mut self, chunks: &[SemanticChunkDocument]) -> Result<Vec<Vec<f32>>> {
        let texts = chunks
            .iter()
            .map(|chunk| chunk.text().to_owned())
            .collect::<Vec<_>>();
        let (embeddings, _) =
            self.runtime
                .embed_documents(self.model_config, texts, self.deadline)?;
        self.indexed_chunks = self.indexed_chunks.saturating_add(embeddings.len());
        Ok(embeddings)
    }
}

fn parse_core_event_type(value: &str) -> Result<EventType> {
    value
        .parse()
        .map_err(|error| anyhow!("invalid Core event type {value:?}: {error}"))
}

fn parse_core_event_role(value: &str) -> Result<EventRole> {
    value
        .parse()
        .map_err(|error| anyhow!("invalid Core event role {value:?}: {error}"))
}

fn parse_core_provider(value: &str) -> Result<CaptureProvider> {
    value
        .parse()
        .map_err(|error| anyhow!("invalid Core provider {value:?}: {error}"))
}

fn parse_core_agent_type(value: &str) -> Result<AgentType> {
    value
        .parse()
        .map_err(|error| anyhow!("invalid Core agent type {value:?}: {error}"))
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
    value["available_memory_bytes"] = json!(deferred.available_memory_bytes());
    value["required_available_memory_bytes"] = json!(deferred.required_available_memory_bytes());
    compact_json(value)
}

pub(super) fn daemon_semantic_resource_deferred_job(
    last_run_at_ms: i64,
    deferred: SemanticResourceDeferred,
) -> Value {
    let mut value = daemon_semantic_job_json(
        "resource_deferred",
        Some(deferred.reason().as_str()),
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
