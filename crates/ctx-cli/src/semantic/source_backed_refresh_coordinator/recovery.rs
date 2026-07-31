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

pub(super) fn persisted_verified_publication(
    data_root: &Path,
) -> Result<Option<SourceBackedRefreshReceipt>> {
    let Some(job) = read_daemon_job_status(&daemon_source_backed_refresh_job_path(data_root))
    else {
        return Ok(None);
    };
    let Some(index) = open_published_generation(data_root)? else {
        return Ok(None);
    };
    if let Some(receipt) = job
        .get("receipt")
        .map(|value| verified_receipt(value, &index))
        .transpose()?
        .flatten()
        .filter(|receipt| primary_receipt_matches_job(&job, receipt))
    {
        return Ok(Some(receipt));
    }
    job.get("retained_publication")
        .map(|value| verified_receipt(value, &index))
        .transpose()
        .map(Option::flatten)
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

fn verified_receipt(
    value: &Value,
    index: &VerifiedIndex,
) -> Result<Option<SourceBackedRefreshReceipt>> {
    let Some(receipt) = value.as_object() else {
        return Ok(None);
    };
    let Some(published_generation) = receipt
        .get("published_generation")
        .and_then(Value::as_str)
        .filter(|generation| !generation.is_empty())
    else {
        return Ok(None);
    };
    if published_generation != index.generation_id() {
        return Ok(None);
    }
    let previous_generation = match receipt.get("previous_generation") {
        None | Some(Value::Null) => None,
        Some(Value::String(generation)) if !generation.is_empty() => Some(generation.clone()),
        _ => return Ok(None),
    };
    let Some(generation_changed) = receipt.get("generation_changed").and_then(Value::as_bool)
    else {
        return Ok(None);
    };
    if generation_changed != (previous_generation.as_deref() != Some(published_generation)) {
        return Ok(None);
    }
    let Some(published_explicit_source_catalog) = receipt
        .get("published_explicit_source_catalog")
        .and_then(|value| ExplicitSourceCatalogAuthority::from_json(value).ok())
    else {
        return Ok(None);
    };
    let Some(current) = receipt.get("current").and_then(verified_current_from_json) else {
        return Ok(None);
    };
    let manifest = index.manifest();
    if current
        != SourceBackedRefreshCurrent::from_sources(&manifest.sources, manifest.removals.len())?
    {
        return Ok(None);
    }
    Ok(Some(SourceBackedRefreshReceipt {
        previous_generation,
        published_generation: published_generation.to_owned(),
        generation_changed,
        published_explicit_source_catalog,
        current,
    }))
}

fn primary_receipt_matches_job(job: &Value, receipt: &SourceBackedRefreshReceipt) -> bool {
    let requested_catalog = job
        .get("requested_explicit_source_catalog")
        .and_then(|value| ExplicitSourceCatalogAuthority::from_json(value).ok());
    let published_catalog = job
        .get("published_explicit_source_catalog")
        .and_then(|value| ExplicitSourceCatalogAuthority::from_json(value).ok());
    job.get("request_state").and_then(Value::as_str) == Some("published")
        && job.get("previous_generation") == receipt.to_json().get("previous_generation")
        && job.get("published_generation").and_then(Value::as_str)
            == Some(receipt.published_generation.as_str())
        && job.get("generation_changed").and_then(Value::as_bool)
            == Some(receipt.generation_changed)
        && requested_catalog.as_ref() == Some(&receipt.published_explicit_source_catalog)
        && published_catalog.as_ref() == Some(&receipt.published_explicit_source_catalog)
}

fn verified_current_from_json(value: &Value) -> Option<SourceBackedRefreshCurrent> {
    let current = value.as_object()?;
    Some(SourceBackedRefreshCurrent {
        source_count: json_usize(current, "current_source_count")?,
        indexed_documents: current.get("current_indexed_documents")?.as_u64()?,
        complete_records: current.get("current_complete_records")?.as_u64()?,
        retained_records: current.get("current_retained_records")?.as_u64()?,
        rejected_records: current.get("current_rejected_records")?.as_u64()?,
        ignored_records: current.get("current_ignored_records")?.as_u64()?,
        certified_source_bytes: current.get("current_certified_source_bytes")?.as_u64()?,
        sources_with_rejections: json_usize(current, "current_sources_with_rejections")?,
        removed_source_count: json_usize(current, "removed_source_count")?,
    })
}

fn json_usize(object: &serde_json::Map<String, Value>, field: &str) -> Option<usize> {
    object
        .get(field)?
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
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
    if let Some(object) = recovered.as_object_mut() {
        object.remove("retained_publication");
    }
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
