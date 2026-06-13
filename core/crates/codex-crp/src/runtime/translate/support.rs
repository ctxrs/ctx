use serde_json::{json, Value};
use tracing::warn;

use crate::protocol::{CrpChannel, CrpEvent, CrpTurnError, CrpTurnStatus};

use super::super::io::{dispatch_event, CrpEventRouter};

pub(super) fn merge_error_details(
    codex_error_info: Option<Value>,
    additional_details: Option<String>,
) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(value) = codex_error_info {
        parts.push(value.to_string());
    }
    if let Some(details) = additional_details {
        let trimmed = details.trim();
        if !trimmed.is_empty() {
            parts.push(trimmed.to_string());
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

pub(super) fn patch_input_preview(changes: &[crate::app_server::FileUpdateChange]) -> Value {
    let mut paths: Vec<String> = changes.iter().map(|change| change.path.clone()).collect();
    paths.sort();
    let mut added = 0usize;
    let mut removed = 0usize;
    for change in changes {
        for line in change.diff.lines() {
            if line.starts_with("+++") || line.starts_with("---") {
                continue;
            }
            if line.starts_with('+') {
                added += 1;
            } else if line.starts_with('-') {
                removed += 1;
            }
        }
    }
    json!({
        "paths": paths,
        "diff_stats": {
            "added": added,
            "removed": removed,
            "files": changes.len(),
        }
    })
}

pub(in crate::runtime) fn canonical_context_window_from_thread_usage(
    token_usage: &crate::app_server::ThreadTokenUsage,
) -> Option<Value> {
    let context_window_tokens = token_usage.model_context_window?;
    if context_window_tokens == 0 {
        return None;
    }
    let total_tokens = token_usage.last.total_tokens;
    let input_tokens = token_usage.last.input_tokens;
    let output_tokens = token_usage.last.output_tokens;
    let reasoning_output_tokens = token_usage.last.reasoning_output_tokens;
    let remaining_tokens_estimate = context_window_tokens.saturating_sub(total_tokens);
    let remaining_fraction = remaining_tokens_estimate as f64 / context_window_tokens as f64;
    Some(json!({
        "context_tokens_estimate": total_tokens,
        "context_window_tokens": context_window_tokens,
        "remaining_tokens_estimate": remaining_tokens_estimate,
        "remaining_fraction": remaining_fraction,
        "total_input_tokens": input_tokens,
        "total_output_tokens": output_tokens.saturating_add(reasoning_output_tokens),
    }))
}

pub(in crate::runtime) fn emit_unsupported_server_request_notice(
    router: &CrpEventRouter,
    session_id: &str,
    turn_id: Option<String>,
    code: &str,
    method: &str,
) {
    dispatch_event(
        router,
        CrpChannel::Control,
        CrpEvent::SessionNotice {
            session_id: session_id.to_string(),
            turn_id,
            code: code.to_string(),
            severity: Some("warning".to_string()),
            message: Some(format!(
                "app-server request `{method}` is not supported by codex-crp"
            )),
            details: Some(json!({ "request_method": method })),
            transient: Some(false),
        },
    );
}

pub(in crate::runtime) fn emit_turn_request_error(
    router: &CrpEventRouter,
    session_id: &str,
    turn_id: Option<String>,
    kind: &str,
    message: String,
) {
    let Some(turn_id) = turn_id else {
        warn!(%kind, %message, "request failed without turn_id; unable to emit turn.completed");
        return;
    };
    dispatch_event(
        router,
        CrpChannel::Control,
        CrpEvent::TurnCompleted {
            session_id: session_id.to_string(),
            turn_id,
            status: CrpTurnStatus::Error,
            context_window: None,
            error: Some(CrpTurnError {
                message,
                kind: Some(kind.to_string()),
                details: None,
            }),
        },
    );
}
