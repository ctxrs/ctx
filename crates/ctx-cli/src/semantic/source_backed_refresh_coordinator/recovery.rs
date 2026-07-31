use super::*;

const COMMITTED_PROGRESS_PHASE: &str = "committed";

pub(super) fn reconcile_persisted_refresh_job(data_root: &Path) -> Result<Option<Value>> {
    let path = daemon_source_backed_refresh_job_path(data_root);
    let Some(job) = read_daemon_job_status(&path) else {
        return Ok(None);
    };
    if !persisted_job_is_active(&job) {
        return Ok(None);
    }

    let reconciled = match open_published_generation(data_root)? {
        Some(index) => {
            recover_verified_publication(&job, &index)?.unwrap_or_else(|| queued_replay_job(job))
        }
        None => queued_replay_job(job),
    };
    write_daemon_job_status(&path, &reconciled)?;
    Ok(Some(reconciled))
}

pub(super) fn persisted_job_needs_replay(job: &Value) -> bool {
    persisted_job_is_active(job)
}

fn persisted_job_is_active(job: &Value) -> bool {
    job.get("owner").and_then(Value::as_str) == Some("daemon")
        && job.get("kind").and_then(Value::as_str) == Some("source_backed")
        && job
            .get("request_id")
            .and_then(Value::as_str)
            .is_some_and(|request_id| !request_id.is_empty())
        && job
            .get("request_state")
            .and_then(Value::as_str)
            .is_some_and(|state| matches!(state, "queued" | "running"))
}

fn recover_verified_publication(job: &Value, index: &VerifiedIndex) -> Result<Option<Value>> {
    if job.get("request_state").and_then(Value::as_str) != Some("running") {
        return Ok(None);
    }
    let generation_id = index.generation_id();
    let previous_generation = job.get("previous_generation").and_then(Value::as_str);
    let commit_was_observed = job
        .get("progress")
        .and_then(|progress| progress.get("phase"))
        .and_then(Value::as_str)
        == Some(COMMITTED_PROGRESS_PHASE);
    if previous_generation == Some(generation_id) && !commit_was_observed {
        return Ok(None);
    }
    let Some(catalog_value) = job.get("requested_explicit_source_catalog") else {
        return Ok(None);
    };
    let Ok(published_catalog) = ExplicitSourceCatalogAuthority::from_json(catalog_value) else {
        return Ok(None);
    };
    let manifest = index.manifest();
    let current =
        SourceBackedRefreshCurrent::from_sources(&manifest.sources, manifest.removals.len())?;
    let receipt = SourceBackedRefreshReceipt {
        previous_generation: previous_generation.map(str::to_owned),
        published_generation: generation_id.to_owned(),
        generation_changed: previous_generation != Some(generation_id),
        published_explicit_source_catalog: published_catalog.clone(),
        current,
    };

    let mut recovered = job.clone();
    recovered["status"] = json!("completed");
    recovered["request_state"] = json!("published");
    recovered["published_generation"] = json!(generation_id);
    recovered["published_explicit_source_catalog"] = published_catalog.to_json();
    recovered["generation_changed"] = json!(receipt.generation_changed);
    recovered["receipt"] = receipt.to_json();
    recovered["source_count"] = json!(current.source_count);
    recovered["certified_source_count"] = json!(current.source_count);
    recovered["certified_source_bytes"] = json!(current.certified_source_bytes);
    let total_sources = recovered
        .get("progress")
        .and_then(|progress| progress.get("total_sources"))
        .and_then(Value::as_u64)
        .unwrap_or(current.source_count as u64);
    recovered["progress"] = compact_json(json!({
        "phase": "published",
        "completed_sources": total_sources,
        "total_sources": total_sources,
        "current_source": Value::Null,
    }));
    clear_retry_failure_fields(&mut recovered);
    Ok(Some(compact_json(recovered)))
}

fn queued_replay_job(mut job: Value) -> Value {
    job["status"] = json!("running");
    job["request_state"] = json!("queued");
    let total_sources = job
        .get("progress")
        .and_then(|progress| progress.get("total_sources"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    job["progress"] = compact_json(json!({
        "phase": "queued",
        "completed_sources": 0,
        "total_sources": total_sources,
        "current_source": Value::Null,
    }));
    clear_retry_failure_fields(&mut job);
    compact_json(job)
}

fn clear_retry_failure_fields(job: &mut Value) {
    let Some(object) = job.as_object_mut() else {
        return;
    };
    for field in [
        "reason",
        "error_code",
        "last_error",
        "retryable",
        "consecutive_failures",
        "retry_after_ms",
        "retry_not_before_at_ms",
    ] {
        object.remove(field);
    }
}
