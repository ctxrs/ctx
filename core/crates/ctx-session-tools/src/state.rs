use serde_json::Value;

use ctx_core::models::SessionTurnTool;

#[derive(Clone, Debug)]
pub struct TurnToolUpdate {
    pub tool_call_id: String,
    pub order_seq: Option<i64>,
    pub tool_kind: Option<String>,
    pub provider_tool_name: Option<String>,
    pub title: Option<String>,
    pub subtitle: Option<String>,
    pub status: Option<String>,
    pub input_json: Option<Value>,
    pub output_text: Option<String>,
    pub input_truncated: Option<bool>,
    pub input_original_bytes: Option<i64>,
    pub output_truncated: Option<bool>,
    pub output_original_bytes: Option<i64>,
}

pub fn merge_tool_update(
    prev: Option<&SessionTurnTool>,
    update: TurnToolUpdate,
    session_id: ctx_core::ids::SessionId,
    turn_id: ctx_core::ids::TurnId,
    event_seq: i64,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<SessionTurnTool> {
    let created_at = prev.map(|tool| tool.created_at).unwrap_or(now);
    let order_seq = match (prev.map(|tool| tool.order_seq), update.order_seq) {
        (Some(prev_seq), Some(next_seq)) => prev_seq.min(next_seq),
        (Some(prev_seq), None) => prev_seq,
        (None, Some(next_seq)) => next_seq,
        (None, None) => {
            tracing::error!(
                tool_call_id = %update.tool_call_id,
                session_id = %session_id.0,
                turn_id = %turn_id.0,
                "tool update missing canonical order_seq"
            );
            return None;
        }
    };
    let first_event_seq = match prev.and_then(|tool| tool.first_event_seq) {
        Some(prev_seq) => Some(prev_seq.min(event_seq)),
        None => Some(event_seq),
    };
    let tool_kind = update
        .tool_kind
        .or_else(|| prev.and_then(|tool| tool.tool_kind.clone()));
    let provider_tool_name = update
        .provider_tool_name
        .or_else(|| prev.and_then(|tool| tool.provider_tool_name.clone()));
    let title = update
        .title
        .or_else(|| prev.and_then(|tool| tool.title.clone()));
    let subtitle = update
        .subtitle
        .or_else(|| prev.and_then(|tool| tool.subtitle.clone()));
    let status = update
        .status
        .or_else(|| prev.and_then(|tool| tool.status.clone()));
    let input_json = update
        .input_json
        .or_else(|| prev.and_then(|tool| tool.input_json.clone()));
    let input_truncated = update
        .input_truncated
        .or_else(|| prev.and_then(|tool| tool.input_truncated));
    let input_original_bytes = update
        .input_original_bytes
        .or_else(|| prev.and_then(|tool| tool.input_original_bytes));
    let output_text = update
        .output_text
        .or_else(|| prev.and_then(|tool| tool.output_text.clone()));
    let output_truncated = match (
        update.output_truncated,
        prev.and_then(|tool| tool.output_truncated),
    ) {
        (Some(next), Some(prev)) => Some(prev || next),
        (Some(next), None) => Some(next),
        (None, Some(prev)) => Some(prev),
        (None, None) => None,
    };
    let output_original_bytes = match (
        update.output_original_bytes,
        prev.and_then(|tool| tool.output_original_bytes),
    ) {
        (Some(next), Some(prev)) => Some(prev.max(next)),
        (Some(next), None) => Some(next),
        (None, Some(prev)) => Some(prev),
        (None, None) => None,
    };
    Some(SessionTurnTool {
        session_id,
        tool_call_id: update.tool_call_id,
        turn_id,
        tool_kind,
        provider_tool_name,
        title,
        subtitle,
        status,
        input_json,
        output_text,
        order_seq,
        first_event_seq,
        input_truncated,
        input_original_bytes,
        output_truncated,
        output_original_bytes,
        created_at,
        updated_at: now,
    })
}

pub fn tool_count_deltas(
    prev: Option<&SessionTurnTool>,
    next: &SessionTurnTool,
) -> (i64, i64, i64, i64, i64) {
    let prev_bucket = tool_status_bucket(prev.and_then(|tool| tool.status.as_deref()));
    let next_bucket = tool_status_bucket(next.status.as_deref());

    let mut delta_total = 0;
    let mut delta_pending = 0;
    let mut delta_running = 0;
    let mut delta_completed = 0;
    let mut delta_failed = 0;

    if prev.is_none() {
        delta_total += 1;
    }
    if prev_bucket != next_bucket {
        if let Some(bucket) = prev_bucket {
            match bucket {
                "pending" => delta_pending -= 1,
                "in_progress" => delta_running -= 1,
                "completed" => delta_completed -= 1,
                "failed" => delta_failed -= 1,
                _ => {}
            }
        }
        if let Some(bucket) = next_bucket {
            match bucket {
                "pending" => delta_pending += 1,
                "in_progress" => delta_running += 1,
                "completed" => delta_completed += 1,
                "failed" => delta_failed += 1,
                _ => {}
            }
        }
    }

    (
        delta_total,
        delta_pending,
        delta_running,
        delta_completed,
        delta_failed,
    )
}

fn tool_status_bucket(status: Option<&str>) -> Option<&'static str> {
    let normalized = status.unwrap_or("").to_lowercase();
    match normalized.as_str() {
        "pending" | "queued" => Some("pending"),
        "in_progress" | "inprogress" | "running" => Some("in_progress"),
        "completed" | "complete" | "ok" | "succeeded" => Some("completed"),
        "failed" | "error" => Some("failed"),
        "" => Some("pending"),
        _ => Some("pending"),
    }
}
