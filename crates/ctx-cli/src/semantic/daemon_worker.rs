use std::{
    env,
    path::Path,
    process,
    time::{Duration as StdDuration, Instant},
};

use anyhow::{anyhow, Context, Result};
use ctx_history_core::{database_path, utc_now};
use ctx_history_store::Store;
use serde_json::{json, Value};

use crate::{store_util::open_existing_store_read_only, DaemonRunArgs, DaemonTriggerCommandArg};

use super::{
    daemon::DaemonRuntime,
    daemon_retry::DaemonRetryBackoff,
    daemon_scheduler::{
        daemon_deadline_has_min_budget, daemon_deadline_remaining, daemon_run_start_mode,
        refresh_semantic_document_count_cache, semantic_report_should_queue_recent_work,
    },
    health_search::{
        env_usize, json_string, json_usize, semantic_embed_policy_status_json,
        semantic_model_acquisition_integrity_error, semantic_model_acquisition_status_json,
        semantic_model_cache_available, semantic_worker_cache_dir,
    },
    indexing::{backfill_semantic_embeddings, semantic_document_hash, semantic_source_text},
    model_contract::{semantic_model_key, SemanticModelLoadDeferred, SEMANTIC_MODEL_ID},
    model_runtime::SharedSemanticRuntime,
    paths_status::{
        read_semantic_worker_status, semantic_status_file_model_matches, semantic_vector_path,
        semantic_worker_report, semantic_worker_report_best_effort, semantic_worker_report_cached,
        write_daemon_status, write_semantic_worker_status, SemanticWorkerLock,
    },
    reports::SemanticWorkerReport,
    runtime_limits::{
        DAEMON_MIN_REMAINING_FOR_JOB_SECS, DAEMON_SEMANTIC_RESERVE_GRACE_SECS,
        SEMANTIC_DIRTY_QUEUE_RECENT_LIMIT, SEMANTIC_MODEL_INIT_MIN_REMAINING_SECS,
        SEMANTIC_WORKER_BATCH_DEFAULT, SEMANTIC_WORKER_BATCH_MAX, SEMANTIC_WORKER_MAX_SECONDS_CAP,
        SEMANTIC_WORKER_MAX_SECONDS_DEFAULT,
    },
    vector_store::{SemanticSidecarStats, SemanticVectorStore},
};

#[cfg(all(test, ctx_sqlite_vec))]
use super::daemon::daemon_test_job;

use crate::output::compact_json;

#[derive(Debug, Clone)]
pub(super) struct SemanticWorkerArgs {
    pub(super) max_chunks: Option<usize>,
    pub(super) max_seconds: Option<u64>,
}

pub(super) fn run_daemon_semantic_job(
    args: &DaemonRunArgs,
    data_root: &Path,
    runtime: &mut DaemonRuntime,
    deadline: Option<Instant>,
    semantic_enabled: bool,
) -> Result<Value> {
    let last_run_at_ms = utc_now().timestamp_millis();
    if !semantic_enabled {
        let report = semantic_worker_report_best_effort(data_root);
        return Ok(daemon_semantic_job_json(
            "disabled",
            Some("semantic_disabled"),
            last_run_at_ms,
            &report,
            None,
            None,
        ));
    }

    #[cfg(all(test, ctx_sqlite_vec))]
    if let Some(value) = daemon_test_job("semantic_index") {
        return Ok(value);
    }

    let db_path = database_path(data_root.to_path_buf());
    if !db_path.exists() {
        let report = semantic_worker_report_best_effort(data_root);
        return Ok(daemon_semantic_job_json(
            "skipped",
            Some("store_missing"),
            last_run_at_ms,
            &report,
            None,
            None,
        ));
    }

    let store = Store::open(&db_path).context("open ctx store for daemon semantic job")?;
    refresh_semantic_document_count_cache(&store)?;
    reconcile_committed_semantic_work(data_root, &store)?;
    let mut before = semantic_worker_report(data_root, Some(&store))?;
    if semantic_report_should_queue_recent_work(&before)
        && queue_recent_semantic_work(data_root, &store, "daemon_recent")? > 0
    {
        before = semantic_worker_report(data_root, Some(&store))?;
    }
    if before.searchable_items == 0 {
        return Ok(daemon_semantic_job_json(
            "empty",
            Some("no_searchable_items"),
            last_run_at_ms,
            &before,
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
            &before,
            None,
            None,
        ));
    }
    if semantic_daemon_model_load_needed(&before, runtime.semantic_runtime.is_loaded()) {
        let cache_dir = semantic_worker_cache_dir(data_root);
        let _ = write_semantic_model_acquisition_status(data_root, "acquiring_model", None);
        match runtime
            .semantic_runtime
            .ensure_loaded_for_daemon(&cache_dir)
        {
            Ok(_) => {
                let embedding_runtime = runtime.semantic_runtime.runtime_status_json()?;
                let embed_policy = runtime.semantic_runtime.policy_status_json()?;
                let _ = write_semantic_model_acquired_status(
                    data_root,
                    embedding_runtime,
                    embed_policy,
                );
                before = semantic_worker_report(data_root, Some(&store))?;
            }
            Err(error) if error.downcast_ref::<SemanticModelLoadDeferred>().is_some() => {
                let deferred = error
                    .downcast_ref::<SemanticModelLoadDeferred>()
                    .expect("matched semantic model load deferral");
                let _ = write_semantic_model_load_deferred_status(data_root, deferred);
                let report = semantic_worker_report(data_root, Some(&store))?;
                return Ok(daemon_semantic_model_load_deferred_job(
                    last_run_at_ms,
                    &report,
                    deferred,
                ));
            }
            Err(error) => {
                let message = format!("{error:#}");
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
                );
                return Ok(daemon_semantic_job_json(
                    "skipped",
                    Some(failure_code),
                    last_run_at_ms,
                    &before,
                    None,
                    Some(message),
                ));
            }
        }
    }
    if before.queued_items_estimate == 0 {
        return Ok(daemon_semantic_job_json(
            "ready",
            None,
            last_run_at_ms,
            &before,
            None,
            None,
        ));
    }
    drop(store);

    let worker_max_seconds = daemon_semantic_worker_seconds_budget(args, deadline);
    if worker_max_seconds == 0 {
        let report = semantic_worker_report_for_daemon(data_root);
        return Ok(daemon_semantic_job_json(
            "skipped",
            Some("daemon_deadline"),
            last_run_at_ms,
            &report,
            None,
            None,
        ));
    }
    let worker_args = SemanticWorkerArgs {
        max_chunks: args.max_chunks,
        max_seconds: Some(worker_max_seconds),
    };
    let worker_result = run_semantic_worker_inner_with_runtime(
        worker_args,
        data_root,
        None,
        &runtime.semantic_runtime,
    );
    if let Err(error) = worker_result {
        if let Some(deferred) = error.downcast_ref::<SemanticModelLoadDeferred>() {
            let _ = write_semantic_model_load_deferred_status(data_root, deferred);
            let report = semantic_worker_report_for_daemon(data_root);
            return Ok(daemon_semantic_model_load_deferred_job(
                last_run_at_ms,
                &report,
                deferred,
            ));
        }
        let message = format!("{error:#}");
        let _ = write_semantic_worker_failure_status(data_root, message.clone());
        let report = semantic_worker_report_for_daemon(data_root);
        return Ok(daemon_semantic_job_json(
            "failed",
            None,
            last_run_at_ms,
            &report,
            None,
            Some(message),
        ));
    }
    let report = semantic_worker_report_for_daemon(data_root);
    let indexed_chunks_now = report
        .embedded_chunks
        .saturating_sub(before.embedded_chunks);
    let indexed_chunks = (indexed_chunks_now > 0).then_some(indexed_chunks_now);
    let status = if report.running {
        "running"
    } else if report.queued_items_estimate == 0 {
        "ready"
    } else if indexed_chunks_now > 0 {
        "budget_exhausted"
    } else {
        report.status.as_str()
    };
    Ok(daemon_semantic_job_json(
        status,
        None,
        last_run_at_ms,
        &report,
        indexed_chunks,
        None,
    ))
}

pub(super) fn reconcile_committed_semantic_work(data_root: &Path, store: &Store) -> Result<()> {
    let vector_path = semantic_vector_path(data_root);
    if !vector_path.exists() {
        return Ok(());
    }
    let mut vector_store = SemanticVectorStore::open(&vector_path)?;
    vector_store.prune_ineligible_events(store)?;
    let before = vector_store.committed_store_reconciliation_cursor()?;
    let docs = store.recent_event_embedding_documents(before, SEMANTIC_DIRTY_QUEUE_RECENT_LIMIT)?;
    if docs.is_empty() {
        vector_store.set_committed_store_reconciliation_cursor(None)?;
        return Ok(());
    }
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
    vector_store.enqueue_dirty_documents(&missing_or_changed, "committed_store_reconcile")?;
    vector_store.set_committed_store_reconciliation_cursor(next_cursor)?;
    Ok(())
}

pub(super) fn daemon_semantic_requested_seconds(args: &DaemonRunArgs) -> u64 {
    semantic_worker_seconds_budget(&SemanticWorkerArgs {
        max_chunks: args.max_chunks,
        max_seconds: args.max_seconds,
    })
}

pub(super) fn semantic_daemon_model_load_needed(
    report: &SemanticWorkerReport,
    runtime_loaded: bool,
) -> bool {
    report.searchable_items > 0 && !runtime_loaded
}

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
    let report = semantic_worker_report_for_daemon(data_root);
    daemon_semantic_job_json(
        "skipped",
        Some("daemon_deadline"),
        utc_now().timestamp_millis(),
        &report,
        None,
        None,
    )
}

pub(super) fn daemon_semantic_skipped_job(
    data_root: &Path,
    semantic_enabled: bool,
    reason: &str,
) -> Value {
    let report = semantic_worker_report_for_daemon(data_root);
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
        &report,
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

pub(super) fn daemon_semantic_failed_job(data_root: &Path, message: String) -> Value {
    let report = semantic_worker_report_for_daemon(data_root);
    daemon_semantic_job_json(
        "failed",
        None,
        utc_now().timestamp_millis(),
        &report,
        None,
        Some(message),
    )
}

pub(super) fn daemon_semantic_job_json(
    status: &str,
    reason: Option<&str>,
    last_run_at_ms: i64,
    report: &SemanticWorkerReport,
    indexed_chunks: Option<usize>,
    last_error: Option<String>,
) -> Value {
    compact_json(json!({
        "schema_version": 1,
        "status": status,
        "model_key": semantic_model_key(),
        "enabled": true,
        "reason": reason,
        "last_run_at_ms": last_run_at_ms,
        "last_error": last_error,
        "indexed_chunks": indexed_chunks,
        "model_cache_available": report.model_cache_available,
        "model_acquisition": report.model_acquisition.clone(),
        "embed_policy": report.embed_policy.clone(),
        "embedding_runtime": report.embedding_runtime.clone(),
        "worker_status": report.status,
        "coverage": {
            "searchable_items": report.searchable_items,
            "completed_items": report.embedded_items,
            "embedded_items": report.embedded_items,
            "embedded_chunks": report.embedded_chunks,
            "dirty_items": report.dirty_items,
            "queued_items_estimate": report.queued_items_estimate,
        },
    }))
}

pub(super) fn daemon_semantic_model_load_deferred_job(
    last_run_at_ms: i64,
    report: &SemanticWorkerReport,
    deferred: &SemanticModelLoadDeferred,
) -> Value {
    let mut value = daemon_semantic_job_json(
        "skipped",
        Some("memory_pressure"),
        last_run_at_ms,
        report,
        None,
        None,
    );
    value["retryable"] = Value::Bool(true);
    value["available_memory_bytes"] = json!(deferred.available_memory_bytes);
    value["required_available_memory_bytes"] = json!(deferred.required_available_memory_bytes);
    compact_json(value)
}

pub(super) fn write_daemon_lifecycle_status(
    data_root: &Path,
    args: &DaemonRunArgs,
    status: &str,
    started_at_ms: i64,
    finished_at_ms: Option<i64>,
    last_error: Option<String>,
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
        })),
    )
}

pub(super) fn semantic_worker_report_for_daemon(data_root: &Path) -> SemanticWorkerReport {
    let db_path = database_path(data_root.to_path_buf());
    if db_path.exists() {
        match open_existing_store_read_only(&db_path, "ctx daemon status") {
            Ok(store) => {
                return semantic_worker_report_cached(data_root, Some(&store)).unwrap_or_else(
                    |error| SemanticWorkerReport::unavailable(data_root, format!("{error:#}")),
                );
            }
            Err(error) => {
                return SemanticWorkerReport::unavailable(data_root, format!("{error:#}"));
            }
        }
    }
    semantic_worker_report_best_effort(data_root)
}

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

pub(super) fn write_semantic_model_acquired_status(
    data_root: &Path,
    embedding_runtime: Option<Value>,
    embed_policy: Value,
) -> Result<()> {
    let now = utc_now().timestamp_millis();
    write_semantic_worker_status(
        data_root,
        &json!({
            "schema_version": 1,
            "status": "model_acquired",
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

pub(super) fn run_semantic_worker_inner_with_runtime(
    args: SemanticWorkerArgs,
    data_root: &Path,
    query_hint: Option<String>,
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
    let vector_path = semantic_vector_path(data_root);
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
            query_hint.as_deref(),
            max_chunks,
            true,
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

pub(super) fn semantic_worker_chunk_budget(args: &SemanticWorkerArgs) -> usize {
    args.max_chunks
        .or_else(|| env_usize("CTX_SEMANTIC_WORKER_MAX_CHUNKS"))
        .map(|value| value.min(SEMANTIC_WORKER_BATCH_MAX))
        .unwrap_or(SEMANTIC_WORKER_BATCH_DEFAULT)
}

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

pub(super) fn queue_recent_semantic_work(
    data_root: &Path,
    store: &Store,
    reason: &str,
) -> Result<usize> {
    let vector_path = semantic_vector_path(data_root);
    if !vector_path.exists()
        && !semantic_model_cache_available(&semantic_worker_cache_dir(data_root))
    {
        return Ok(0);
    }
    let docs = store.recent_event_embedding_documents(None, SEMANTIC_DIRTY_QUEUE_RECENT_LIMIT)?;
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
