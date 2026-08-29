use std::path::Path;

use ctx_daemon_runtime::read_daemon_job_status;
use ctx_daemon_service::daemon_core_refresh_job_path;
use serde_json::{json, Value};

use super::{compact_json, json_i64, json_string};

pub(super) fn daemon_core_refresh_job_report(
    data_root: &Path,
    disabled_overrides_lifecycle: bool,
    daemon_enabled: bool,
) -> Value {
    let status_value = read_daemon_job_status(&daemon_core_refresh_job_path(data_root));
    let job = status_value.as_ref();
    let disabled = !daemon_enabled && disabled_overrides_lifecycle;
    compact_json(json!({
        "status": if disabled {
            "disabled"
        } else {
            job.and_then(|value| value.get("status"))
                .and_then(Value::as_str)
                .unwrap_or("unknown")
        },
        "enabled": daemon_enabled,
        "reason": if disabled {
            Some("daemon_disabled".to_owned())
        } else {
            job.and_then(|value| json_string(value, "reason"))
        },
        "error_code": job.and_then(|value| json_string(value, "error_code")),
        "mode": job.and_then(|value| json_string(value, "mode")),
        "owner": job.and_then(|value| json_string(value, "owner")),
        "kind": job.and_then(|value| json_string(value, "kind")),
        "request_id": job.and_then(|value| json_string(value, "request_id")),
        "request_state": job.and_then(|value| json_string(value, "request_state")),
        "last_run_at_ms": job.and_then(|value| json_i64(value, "last_run_at_ms")),
        "source_count": job.and_then(|value| value.get("source_count").cloned()),
        "previous_generation": job.and_then(|value| json_string(value, "previous_generation")),
        "published_generation": job.and_then(|value| json_string(value, "published_generation")),
        "generation_changed": job.and_then(|value| value.get("generation_changed").cloned()),
        "receipt": job.and_then(|value| value.get("receipt").cloned()),
        "coalesced_requests": job.and_then(|value| value.get("coalesced_requests").cloned()),
        "progress": job.and_then(|value| value.get("progress").cloned()),
        "daemon_mode": job.and_then(|value| json_string(value, "daemon_mode")),
        "trigger": job.and_then(|value| json_string(value, "trigger")),
        "trigger_provenance": job.and_then(|value| json_string(value, "trigger_provenance")),
        "scanned_routes": job.and_then(|value| value.get("scanned_routes").cloned()),
        "unsupported_routes": job.and_then(|value| value.get("unsupported_routes").cloned()),
        "certified_source_count": job.and_then(|value| value.get("certified_source_count").cloned()),
        "certified_source_bytes": job.and_then(|value| value.get("certified_source_bytes").cloned()),
        "timings_us": job.and_then(|value| value.get("timings_us").cloned()),
        "structured_outcome": job.and_then(|value| value.get("structured_outcome").cloned()),
        "automatic_retry": job.and_then(|value| value.get("automatic_retry").cloned()),
        "retryable": job.and_then(|value| value.get("retryable").cloned()),
        "retry_after_ms": job.and_then(|value| value.get("retry_after_ms").cloned()),
        "consecutive_failures": job.and_then(|value| value.get("consecutive_failures").cloned()),
        "retry_not_before_at_ms": job.and_then(|value| value.get("retry_not_before_at_ms").cloned()),
        "last_error": job.and_then(|value| json_string(value, "last_error")),
    }))
}
