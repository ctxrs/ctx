use std::{path::Path, process, sync::Arc, time::Instant};

use anyhow::Result;
use ctx_history_core::utc_now;
use ctx_semantic_index::{
    source_backed_semantic_vector_path, SemanticBatchEmbedder, SemanticChunkDocument,
    SemanticNotReady, SemanticQueryPin, SemanticVectorStore, SourceBackedSemanticDocumentBuilder,
    SourceBackedSemanticOutcome,
};
use ctx_semantic_model::{
    semantic_model_acquisition_integrity_error, semantic_model_key, ArtifactFetcher,
    SemanticDaemonCpuFallbackRequired, SemanticDaemonModelAcquisition, SemanticEmbeddingExecutor,
    SemanticEmbeddingExecutorConfig, SemanticModelLoadDeferred,
};
use serde_json::{json, Value};

use crate::{DaemonConfigPort, DaemonRunArgs, DaemonTriggerCommandArg};

use super::{
    daemon::DaemonRuntime,
    daemon_retry::{annotate_semantic_failure, classify_semantic_failure, DaemonRetryBackoff},
    daemon_scheduler::{daemon_deadline_has_min_budget, daemon_run_start_mode},
    paths_status::{daemon_semantic_job_path, write_daemon_job_status, write_daemon_status},
    resource_policy::{
        semantic_background_resource_deferred, semantic_external_background_resource_deferred,
        semantic_resource_deferral_releases_runtime, SemanticBackgroundOperation,
        SemanticResourceDeferred,
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

#[cfg(test)]
thread_local! {
    static FORCE_SEMANTIC_INDEX_PUBLICATION_DEFERRAL: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

#[cfg(test)]
struct SemanticIndexPublicationDeferralGuard;

#[cfg(test)]
impl Drop for SemanticIndexPublicationDeferralGuard {
    fn drop(&mut self) {
        FORCE_SEMANTIC_INDEX_PUBLICATION_DEFERRAL.with(|forced| forced.set(false));
    }
}

#[cfg(test)]
fn force_semantic_index_publication_deferral_for_test() -> SemanticIndexPublicationDeferralGuard {
    FORCE_SEMANTIC_INDEX_PUBLICATION_DEFERRAL.with(|forced| {
        assert!(
            !forced.replace(true),
            "semantic publication deferral already forced"
        );
    });
    SemanticIndexPublicationDeferralGuard
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
    DaemonSemanticModelStartup::Finished(daemon_semantic_model_startup_failure(
        last_run_at_ms,
        failure_code,
        message,
        failure_class,
    ))
}

fn daemon_semantic_model_startup_failure(
    last_run_at_ms: i64,
    failure_code: &'static str,
    message: String,
    failure_class: super::daemon_retry::SemanticFailureClass,
) -> Value {
    let status = if failure_class.blocks_until_restart() {
        "failed"
    } else {
        "skipped"
    };
    annotate_semantic_failure(
        daemon_semantic_job_json(
            status,
            Some(failure_code),
            last_run_at_ms,
            None,
            Some(message),
        ),
        failure_class,
    )
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
                return Ok(DaemonSemanticModelStartup::Finished(
                    daemon_semantic_model_startup_failure(
                        last_run_at_ms,
                        "model_load_failed",
                        message,
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
    // Readiness is an exact semantic-index property. Derive the selected V2
    // contract from configuration alone, then use the ordinary WAL-aware
    // preflight before constructing an executor or touching writable state.
    let index_contract = semantic_index_contract(runtime.config.semantic_executor.contract())?;
    let source_eligible_events = source_generation.semantic_eligible_event_count()?;
    match SemanticQueryPin::preflight(
        source_generation.verified_index(),
        data_root,
        &index_contract,
    ) {
        Ok(_) => {
            return Ok(daemon_semantic_job_json(
                "ready",
                None,
                last_run_at_ms,
                None,
                None,
            ));
        }
        Err(error)
            if error
                .downcast_ref::<SemanticNotReady>()
                .is_some_and(SemanticNotReady::retryable) => {}
        Err(error) => return Err(error),
    }
    let vector_path = source_backed_semantic_vector_path(data_root);
    if !daemon_deadline_has_min_budget(deadline, DAEMON_MIN_REMAINING_FOR_JOB_SECS) {
        return Ok(daemon_semantic_job_json(
            "skipped",
            Some("daemon_deadline"),
            last_run_at_ms,
            None,
            None,
        ));
    }
    // An empty Core generation still needs its durable semantic acknowledgement,
    // but has no embedding work. Admit that index publication through the same
    // deadline and resource boundaries as ordinary daemon work, then complete
    // it from the configuration-derived contract before resolving credentials
    // or constructing an executor.
    if source_eligible_events == 0 && !vector_path.exists() {
        if let Some(deferred) = semantic_index_publication_resource_deferred(
            data_root,
            &runtime.config.semantic_executor,
        ) {
            return Ok(daemon_semantic_resource_deferred_job(
                last_run_at_ms,
                deferred,
            ));
        }
        let mut vector_store = SemanticVectorStore::open(&vector_path, &index_contract)?;
        let (outcome, indexed_chunks) =
            reconcile_empty_source_backed_semantic_page(source_generation, &mut vector_store)?;
        return Ok(daemon_semantic_reconciliation_job(
            last_run_at_ms,
            outcome,
            indexed_chunks,
        ));
    }

    let executor = match runtime.semantic_executor.clone() {
        Some(executor) => executor,
        None => {
            let executor = Arc::new(
                ctx_semantic_model::SemanticEmbeddingExecutorHandle::build_with_auth(
                    runtime.config.semantic_executor.clone(),
                    config.semantic_executor_auth()?,
                    runtime.semantic_runtime.clone(),
                    config.semantic_model_config(data_root),
                )?,
            );
            runtime.semantic_executor = Some(Arc::clone(&executor));
            executor
        }
    };
    let semantic_executor = executor.executor();
    let resource_deferred = if let Some(builtin) = executor.builtin_executor() {
        let admission_operation = if builtin.shared_runtime().is_loaded() {
            SemanticBackgroundOperation::IndexBatch
        } else {
            SemanticBackgroundOperation::ModelLoad
        };
        semantic_background_resource_deferred(data_root, admission_operation)
    } else {
        semantic_external_background_resource_deferred(data_root)
    };
    if let Some(deferred) = resource_deferred {
        if semantic_resource_deferral_releases_runtime(deferred.reason()) {
            if let Some(builtin) = executor.builtin_executor() {
                let _ = builtin.shared_runtime().release_if_idle();
            }
        }
        return Ok(daemon_semantic_resource_deferred_job(
            last_run_at_ms,
            deferred,
        ));
    }

    let mut vector_store = open_selected_semantic_vector_store(&vector_path, &executor)?;

    let min_remaining_secs = executor
        .builtin_executor()
        .map(|builtin| {
            if builtin.shared_runtime().is_loaded() {
                DAEMON_MIN_REMAINING_FOR_JOB_SECS
            } else {
                SEMANTIC_MODEL_INIT_MIN_REMAINING_SECS
            }
        })
        .unwrap_or(DAEMON_MIN_REMAINING_FOR_JOB_SECS)
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
    if let Some(builtin) = executor
        .builtin_executor()
        .filter(|builtin| source_eligible_events > 0 && !builtin.shared_runtime().is_loaded())
    {
        match run_daemon_semantic_model_startup_with(
            last_run_at_ms,
            || {
                builtin
                    .shared_runtime()
                    .acquire_for_daemon(builtin.config(), artifact_fetcher)
            },
            |fallback| {
                builtin
                    .shared_runtime()
                    .acquire_cpu_fallback_for_daemon(builtin.config(), fallback)
            },
            |acquisition| {
                builtin
                    .shared_runtime()
                    .ensure_loaded_after_daemon_acquisition(builtin.config(), acquisition)?;
                Ok(())
            },
        )? {
            DaemonSemanticModelStartup::Loaded => {}
            DaemonSemanticModelStartup::Finished(job) => return Ok(job),
        }
    }
    let core_generation_id = source_generation.generation_id().to_owned();
    let source_contract_fingerprint =
        ctx_semantic_index::source_backed_semantic_contract_fingerprint(&index_contract)?;
    let mut publish_progress = |sequence| {
        let mut progress = daemon_semantic_job_json(
            "budget_exhausted",
            None,
            utc_now().timestamp_millis(),
            None,
            None,
        );
        progress["model_key"] = Value::String(index_contract.model_key().to_owned());
        progress["model_contract_fingerprint"] =
            Value::String(index_contract.fingerprint().to_owned());
        progress["source_contract_fingerprint"] =
            Value::String(source_contract_fingerprint.clone());
        progress["core_generation_id"] = Value::String(core_generation_id.clone());
        progress["semantic_progress_sequence"] = json!(sequence);
        progress["source_generation_ready"] = Value::Bool(false);
        progress["source_work_remaining"] = Value::Bool(true);
        write_daemon_job_status(&daemon_semantic_job_path(data_root), &progress)
    };
    let (outcome, indexed_chunks) = reconcile_source_backed_semantic_page(
        data_root,
        source_generation,
        &mut vector_store,
        semantic_executor,
        deadline,
        &mut publish_progress,
    )?;
    Ok(daemon_semantic_reconciliation_job(
        last_run_at_ms,
        outcome,
        indexed_chunks,
    ))
}

fn semantic_index_publication_resource_deferred(
    data_root: &Path,
    executor: &SemanticEmbeddingExecutorConfig,
) -> Option<SemanticResourceDeferred> {
    #[cfg(test)]
    if FORCE_SEMANTIC_INDEX_PUBLICATION_DEFERRAL.with(std::cell::Cell::get) {
        return Some(SemanticResourceDeferred::disk_pressure_for_test());
    }

    if executor.is_builtin() {
        semantic_background_resource_deferred(data_root, SemanticBackgroundOperation::IndexBatch)
    } else {
        semantic_external_background_resource_deferred(data_root)
    }
}

fn open_selected_semantic_vector_store(
    vector_path: &Path,
    executor: &ctx_semantic_model::SemanticEmbeddingExecutorHandle,
) -> Result<SemanticVectorStore> {
    verify_external_semantic_contract_before_store_open(executor)?;
    let contract = semantic_index_contract(executor.executor().contract())?;
    SemanticVectorStore::open(vector_path, &contract)
}

pub(super) fn semantic_index_contract(
    selected: &ctx_semantic_model::SemanticModelContract,
) -> Result<ctx_semantic_index::SemanticModelContract> {
    if let Some(space) = selected.external_space() {
        let endpoint = selected.external_http_endpoint().ok_or_else(|| {
            anyhow::anyhow!("external semantic contract has no endpoint identity")
        })?;
        return ctx_semantic_index::external_http_semantic_model_contract(
            endpoint,
            space.space_id(),
            space.dimensions(),
        );
    }
    if let Some(endpoint) = selected.external_http_endpoint() {
        return ctx_semantic_index::legacy_fixed_http_semantic_model_contract(endpoint);
    }
    let local = ctx_semantic_index::semantic_model_contract();
    if selected.fingerprint() != local.fingerprint() {
        return Err(anyhow::anyhow!(
            "semantic executor model contract does not match the semantic index contract"
        ));
    }
    Ok(local.clone())
}

/// Establishes the endpoint's configured identity before a mismatched writable
/// vector store can perform its existing reset-on-open recovery. V2 verification
/// is a content-free GET; retained fixed-E5 V1 may submit only frozen public
/// canary probes and never user history or query content.
fn verify_external_semantic_contract_before_store_open(
    executor: &ctx_semantic_model::SemanticEmbeddingExecutorHandle,
) -> Result<()> {
    executor.verify_contract()
}

fn reconcile_source_backed_semantic_page(
    _data_root: &Path,
    generation: PinnedSourceBackedGeneration,
    vector_store: &mut SemanticVectorStore,
    executor: &dyn SemanticEmbeddingExecutor,
    deadline: Option<Instant>,
    progress: &mut dyn FnMut(u64) -> Result<()>,
) -> Result<(SourceBackedSemanticOutcome, usize)> {
    let index = generation.into_index();
    let mut builder = SourceBackedSemanticDocumentBuilder::new(&index);
    let mut embedder = RuntimeSourceSemanticEmbedder {
        executor,
        deadline,
        indexed_chunks: 0,
    };
    let outcome = vector_store.reconcile_source_backed_index_with_checkpoint_and_progress(
        &index,
        &mut builder,
        &mut embedder,
        &mut || Ok(()),
        progress,
    )?;
    Ok((outcome, embedder.indexed_chunks))
}

fn reconcile_empty_source_backed_semantic_page(
    generation: PinnedSourceBackedGeneration,
    vector_store: &mut SemanticVectorStore,
) -> Result<(SourceBackedSemanticOutcome, usize)> {
    let index = generation.into_index();
    let mut builder = SourceBackedSemanticDocumentBuilder::new(&index);
    let mut embedder = EmptySourceSemanticEmbedder;
    let outcome =
        vector_store.reconcile_source_backed_index(&index, &mut builder, &mut embedder)?;
    Ok((outcome, 0))
}

fn daemon_semantic_reconciliation_job(
    last_run_at_ms: i64,
    outcome: SourceBackedSemanticOutcome,
    indexed_chunks: usize,
) -> Value {
    let status = if outcome.ready() {
        "ready"
    } else {
        "budget_exhausted"
    };
    let mut job = daemon_semantic_job_json(
        status,
        None,
        last_run_at_ms,
        (indexed_chunks > 0).then_some(indexed_chunks),
        None,
    );
    annotate_source_backed_semantic_progress(&mut job, &outcome);
    if let Some(sequence) = outcome.semantic_progress_sequence() {
        job["semantic_progress_sequence"] = json!(sequence);
    }
    job
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

struct EmptySourceSemanticEmbedder;

impl SemanticBatchEmbedder for EmptySourceSemanticEmbedder {
    fn embed_chunks(&mut self, _chunks: &[SemanticChunkDocument]) -> Result<Vec<Vec<f32>>> {
        anyhow::bail!("zero-eligible semantic reconciliation requested embeddings")
    }
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
