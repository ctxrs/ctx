//! Machine-readable operation progress.
use super::*;
use serde_json::json;

pub(super) fn progress_json(
    operation: &'static str,
    line: &ProgressLine,
    elapsed: StdDuration,
) -> String {
    let (completed_bytes, total_bytes) = progress_line_bytes(line);
    let mut value = json!({
        "type": "ctx_progress",
        "operation": operation,
        "phase": line.phase,
        "message": line.message,
        "completed_bytes": completed_bytes,
        "total_bytes": total_bytes,
        "percent": progress_line_percent(line),
        "elapsed_seconds": elapsed.as_secs_f64(),
        // Compatibility: this documented legacy field remains byte-rate based.
        // Source-backed consumers use estimated_remaining_millis below for the
        // explicit whole-run time until the refreshed generation is usable.
        "eta_seconds": progress_line_eta_seconds(line, elapsed),
        "completed_files": line.completed_files,
        "total_files": line.total_files,
        "imported_events": line.imported_events,
        "done": line.done,
    });
    if let Some(snapshot) = line.refresh.as_ref() {
        let progress = snapshot.progress();
        value["completed_sources"] = json!(progress.completed_sources);
        value["total_sources"] = json!(progress.total_sources);
        value["total_sources_known"] = json!(snapshot.total_sources_known());
        value["source_completed_records"] = json!(progress.completed_records);
        value["source_completed_bytes"] = json!(progress.completed_bytes);
        value["agent_histories"] = json!(progress.agent_histories);
        value["processed_sessions"] = json!(progress.processed_sessions);
        value["processed_messages"] = json!(progress.processed_messages);
        value["processed_tool_calls"] = json!(progress.processed_tool_calls);
        value["processed_bytes"] = json!(progress.processed_bytes);
        value["whole_run_stage"] = json!(progress.whole_run_stage.as_str());
        value["estimated_remaining_millis"] = json!(progress.estimated_remaining_millis);
        value["refresh_elapsed_millis"] = json!(progress.elapsed_millis);
        value["current_source"] = json!(progress
            .current_source
            .as_deref()
            .map(|source| bounded_progress_text(source, MAX_PROGRESS_SOURCE_BYTES)));
        value["current_source_progress"] = progress
            .current_source_progress
            .as_ref()
            .map(crate::ui::RefreshCurrentSourceProgress::to_json)
            .unwrap_or(serde_json::Value::Null);
        snapshot.append_json_fields(&mut value);
    }
    if let Some(callout) = line.callout.as_ref() {
        value["callout"] = callout.clone();
    }
    if let Some(indexing) = &line.indexing {
        value["completed_sources"] = json!(indexing.completed_sources);
        value["total_sources"] = json!(indexing.total_sources);
        value["applied_changes"] = json!(indexing.applied_changes);
    }
    value.to_string()
}
