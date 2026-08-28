use std::{path::Path, process, time::Instant};

use anyhow::Result;
use ctx_history_core::utc_now;
use ctx_semantic_index::{
    source_backed_semantic_vector_path, SemanticBatchEmbedder, SemanticChunkDocument,
    SemanticVectorStore, SourceBackedGenerationPin, SourceBackedSemanticDocumentBuilder,
    SourceBackedSemanticOutcome,
};
use ctx_semantic_model::{
    semantic_model_acquisition_integrity_error, semantic_model_key, ArtifactFetcher,
    BuiltinSemanticEmbeddingExecutor, SemanticDaemonCpuFallbackRequired,
    SemanticDaemonModelAcquisition, SemanticEmbeddingExecutor, SemanticModelLoadDeferred,
};
use serde_json::{json, Value};

use crate::{DaemonConfigPort, DaemonRunArgs, DaemonTriggerCommandArg};

use super::{
    daemon::DaemonRuntime,
    daemon_retry::{annotate_semantic_failure, classify_semantic_failure, DaemonRetryBackoff},
    daemon_scheduler::{daemon_deadline_has_min_budget, daemon_run_start_mode},
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

use crate::compact_json;

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
    artifact_fetcher: &dyn ArtifactFetcher,
    config: &dyn DaemonConfigPort,
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

    let executor = BuiltinSemanticEmbeddingExecutor::new(
        runtime.semantic_runtime.clone(),
        config.semantic_model_config(data_root),
    );
    // Bazel may materialize the model crate separately across this dependency
    // boundary, so bridge compatibility by fingerprint before opening the
    // index with its own contract type.
    let index_contract = ctx_semantic_index::semantic_model_contract();
    if executor.contract().fingerprint() != index_contract.fingerprint() {
        return Err(anyhow::anyhow!(
            "semantic executor model contract does not match the semantic index contract"
        ));
    }
    let admission_operation = if executor.shared_runtime().is_loaded() {
        SemanticBackgroundOperation::IndexBatch
    } else {
        SemanticBackgroundOperation::ModelLoad
    };
    if let Some(deferred) = semantic_background_resource_deferred(data_root, admission_operation) {
        if semantic_resource_deferral_releases_runtime(deferred.reason()) {
            let _ = executor.shared_runtime().release_if_idle();
        }
        return Ok(daemon_semantic_resource_deferred_job(
            last_run_at_ms,
            deferred,
        ));
    }

    let vector_path = source_backed_semantic_vector_path(data_root);
    let mut vector_store = SemanticVectorStore::open(&vector_path, index_contract)?;
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
    let min_remaining_secs = if executor.shared_runtime().is_loaded() {
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
        source_eligible_events > 0 && !executor.shared_runtime().is_loaded();
    if source_model_load_needed {
        match run_daemon_semantic_model_startup_with(
            last_run_at_ms,
            || {
                executor
                    .shared_runtime()
                    .acquire_for_daemon(executor.config(), artifact_fetcher)
            },
            |fallback| {
                executor
                    .shared_runtime()
                    .acquire_cpu_fallback_for_daemon(executor.config(), fallback)
            },
            |acquisition| {
                executor
                    .shared_runtime()
                    .ensure_loaded_after_daemon_acquisition(executor.config(), acquisition)?;
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
        &executor,
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
    executor: &dyn SemanticEmbeddingExecutor,
    deadline: Option<Instant>,
) -> Result<(SourceBackedSemanticOutcome, usize)> {
    let index = generation.into_index();
    let mut builder = SourceBackedSemanticDocumentBuilder::new(&index);
    let mut embedder = RuntimeSourceSemanticEmbedder {
        executor,
        deadline,
        indexed_chunks: 0,
    };
    let outcome =
        vector_store.reconcile_source_backed_index(&index, &mut builder, &mut embedder)?;
    Ok((outcome, embedder.indexed_chunks))
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
    executor: &'a dyn SemanticEmbeddingExecutor,
    deadline: Option<Instant>,
    indexed_chunks: usize,
}

impl SemanticBatchEmbedder for RuntimeSourceSemanticEmbedder<'_> {
    fn embed_chunks(&mut self, chunks: &[SemanticChunkDocument]) -> Result<Vec<Vec<f32>>> {
        let texts = chunks
            .iter()
            .map(|chunk| chunk.text().to_owned())
            .collect::<Vec<_>>();
        let embeddings = execute_document_embeddings(self.executor, texts, self.deadline)?;
        self.indexed_chunks = self.indexed_chunks.saturating_add(embeddings.len());
        Ok(embeddings)
    }
}

fn execute_document_embeddings(
    executor: &dyn SemanticEmbeddingExecutor,
    texts: Vec<String>,
    deadline: Option<Instant>,
) -> Result<Vec<Vec<f32>>> {
    executor.embed_documents(executor.contract().prepare_documents(texts), deadline)
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
