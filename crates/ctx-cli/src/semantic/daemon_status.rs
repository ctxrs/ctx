use serde_json::Value;

mod render;

#[allow(unused_imports)]
pub(super) use render::{
    render_daemon_disable_receipt, render_daemon_enable_receipt,
    render_daemon_prepare_uninstall_receipt, render_daemon_status_human, DaemonStatusView,
};

pub(super) fn print_daemon_status_human(daemon: &Value) {
    println!(
        "daemon_enabled: {}",
        daemon
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true)
    );
    println!(
        "daemon_status: {}",
        daemon
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    );
    println!(
        "daemon_mode: {}",
        daemon.get("mode").and_then(Value::as_str).unwrap_or("full")
    );
    println!(
        "daemon_running: {}",
        daemon
            .get("running")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    );
    if let Some(pid) = daemon.get("live_pid").and_then(Value::as_u64) {
        println!("daemon_pid: {pid}");
    }
    if let Some(provenance) = daemon.get("trigger_provenance").and_then(Value::as_str) {
        println!("daemon_trigger_provenance: {provenance}");
    }
    if let Some(path) = daemon
        .get("source_refresh_endpoint")
        .and_then(|endpoint| endpoint.get("identity_path"))
        .and_then(Value::as_str)
    {
        println!("source_refresh_endpoint_identity: {path}");
    }
    if let Some(owner) = daemon
        .get("lock_identity")
        .and_then(|lock| lock.get("owner_id"))
        .and_then(Value::as_str)
    {
        println!("daemon_lock_identity: {owner}");
    }
    println!(
        "daemon_config_reload_status: {}",
        daemon
            .get("config_reload")
            .and_then(|reload| reload.get("status"))
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    );
    println!(
        "semantic_runtime_active: {}",
        daemon
            .get("semantic_runtime_active")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    );
    if let Some(reason) = daemon.get("reason").and_then(Value::as_str) {
        println!("daemon_reason: {reason}");
    }
    if daemon
        .get("recoverable")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        println!("daemon_recoverable: true");
    }
    println!(
        "history_refresh_status: {}",
        daemon
            .get("jobs")
            .and_then(|jobs| jobs.get("history_refresh"))
            .and_then(|job| job.get("status"))
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    );
    let history_refresh = daemon
        .get("jobs")
        .and_then(|jobs| jobs.get("history_refresh"));
    let source_backed_refresh = daemon
        .get("jobs")
        .and_then(|jobs| jobs.get("source_backed_refresh"));
    println!(
        "source_backed_refresh_status: {}",
        source_backed_refresh
            .and_then(|job| job.get("status"))
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    );
    if let Some(generation) = source_backed_refresh
        .and_then(|job| job.get("published_generation"))
        .and_then(Value::as_str)
    {
        println!("source_backed_refresh_published_generation: {generation}");
    }
    if let Some(phase) = source_backed_refresh
        .and_then(|job| job.get("progress"))
        .and_then(|progress| progress.get("phase"))
        .and_then(Value::as_str)
    {
        println!("source_backed_refresh_progress: {phase}");
    }
    if let Some(count) = source_backed_refresh
        .and_then(|job| job.get("certified_source_count"))
        .and_then(Value::as_u64)
    {
        println!("source_backed_refresh_certified_sources: {count}");
    }
    if let Some(bytes) = source_backed_refresh
        .and_then(|job| job.get("certified_source_bytes"))
        .and_then(Value::as_u64)
    {
        println!("source_backed_refresh_certified_bytes: {bytes}");
    }
    if let Some(error) = source_backed_refresh
        .and_then(|job| job.get("last_error"))
        .and_then(Value::as_str)
    {
        println!("source_backed_refresh_error: {error}");
    }
    if let Some(rejected_records) = history_refresh
        .and_then(|job| {
            job.get("rejection_diagnostics")
                .and_then(|diagnostics| diagnostics.get("rejected_records"))
                .or_else(|| {
                    job.get("totals")
                        .and_then(|totals| totals.get("rejected_records"))
                })
        })
        .and_then(Value::as_u64)
        .filter(|count| *count > 0)
    {
        println!("history_refresh_rejected_records: {rejected_records}");
    }
    if let Some(error) = history_refresh
        .and_then(|job| job.get("last_error"))
        .and_then(Value::as_str)
    {
        println!("history_refresh_error: {error}");
    }
    println!(
        "semantic_index_status: {}",
        daemon
            .get("jobs")
            .and_then(|jobs| jobs.get("semantic_index"))
            .and_then(|job| job.get("status"))
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    );
    let embedding_runtime = daemon
        .get("jobs")
        .and_then(|jobs| jobs.get("semantic_index"))
        .and_then(|job| job.get("embedding_runtime"));
    if let Some(backend) = embedding_runtime
        .and_then(|runtime| runtime.get("backend"))
        .and_then(Value::as_str)
    {
        println!("semantic_embedding_backend: {backend}");
    }
    if let Some(compute_mode) = embedding_runtime
        .and_then(|runtime| runtime.get("compute_mode"))
        .and_then(Value::as_str)
    {
        println!("semantic_embedding_compute_mode: {compute_mode}");
    }
    if let Some(fallback) = embedding_runtime
        .and_then(|runtime| runtime.get("acquisition_fallback"))
        .and_then(Value::as_str)
    {
        println!("semantic_embedding_fallback: {fallback}");
    }
    if let Some(error) = daemon.get("last_error").and_then(Value::as_str) {
        println!("daemon_last_error: {error}");
    }
}

pub(super) fn daemon_jobs_failure_message(
    history: Option<&Value>,
    semantic: Option<&Value>,
) -> Option<String> {
    if let Some(error) = history
        .and_then(|job| job.get("last_error"))
        .and_then(Value::as_str)
        .filter(|error| !error.is_empty())
    {
        return Some(format!("history refresh failed: {error}"));
    }
    if let Some(failed_sources) = history
        .and_then(|job| job.get("totals"))
        .and_then(|totals| totals.get("failed_sources"))
        .and_then(Value::as_u64)
        .filter(|count| *count > 0)
    {
        let source = if failed_sources == 1 {
            "source"
        } else {
            "sources"
        };
        return Some(format!(
            "history refresh failed for {failed_sources} {source}; run `ctx import --all --no-daemon` for source-level details"
        ));
    }
    if history
        .and_then(|job| job.get("status"))
        .and_then(Value::as_str)
        == Some("failed")
    {
        return Some("history refresh failed".to_owned());
    }
    if let Some(error) = semantic
        .and_then(|job| job.get("last_error"))
        .and_then(Value::as_str)
        .filter(|error| !error.is_empty())
    {
        return Some(format!("semantic indexing failed: {error}"));
    }
    if semantic
        .and_then(|job| job.get("status"))
        .and_then(Value::as_str)
        == Some("failed")
    {
        return Some("semantic indexing failed".to_owned());
    }
    None
}

pub(super) fn daemon_report_failure_message(report: &Value) -> Option<String> {
    if report.get("status").and_then(Value::as_str) != Some("failed") {
        return None;
    }
    let jobs = report.get("jobs");
    daemon_jobs_failure_message(
        jobs.and_then(|jobs| jobs.get("history_refresh")),
        jobs.and_then(|jobs| jobs.get("semantic_index")),
    )
    .or_else(|| {
        report
            .get("last_error")
            .and_then(Value::as_str)
            .map(str::to_owned)
    })
    .or_else(|| Some("one or more daemon jobs failed".to_owned()))
}

#[cfg(test)]
#[path = "daemon_status/tests.rs"]
mod tests;
