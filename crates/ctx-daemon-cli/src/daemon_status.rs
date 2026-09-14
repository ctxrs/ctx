use serde_json::Value;

mod render;

pub(super) use render::{
    render_daemon_disable_receipt, render_daemon_enable_receipt,
    render_daemon_prepare_uninstall_receipt, render_daemon_status_human, DaemonStatusView,
};

fn job_status(job: Option<&Value>) -> &str {
    job.and_then(|job| job.get("status"))
        .and_then(Value::as_str)
        .unwrap_or("unknown")
}

fn job_error(job: Option<&Value>) -> Option<&str> {
    job.and_then(|job| job.get("last_error"))
        .and_then(Value::as_str)
        .filter(|error| !error.is_empty())
}

pub(super) fn daemon_jobs_failure_message(
    core_refresh: Option<&Value>,
    semantic: Option<&Value>,
) -> Option<String> {
    if let Some(error) = job_error(core_refresh) {
        return Some(format!("Core refresh failed: {error}"));
    }
    if job_status(core_refresh) == "failed" {
        return Some("Core refresh failed".to_owned());
    }
    if let Some(error) = job_error(semantic) {
        return Some(format!("semantic indexing failed: {error}"));
    }
    if job_status(semantic) == "failed" {
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
        jobs.and_then(|jobs| jobs.get("core_refresh")),
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
