use ctx_observability::ops_events::OpsEvent;
use ctx_session_tools::{build_tool_ops_meta_from_normalized, NormalizedToolEvent};
use serde_json::{json, Value};

use crate::daemon::scheduler::host::TurnEventLoopHost;

use super::super::super::tool_runtime::cwd_outside_worktree;
use super::super::TurnEventLoop;

pub(super) fn emit_tool_call_ops(
    ctx: &TurnEventLoop,
    host: &TurnEventLoopHost,
    tool_event: &NormalizedToolEvent,
) {
    let tool_meta = build_tool_ops_meta_from_normalized(tool_event);
    let mut meta = serde_json::Map::new();
    if let Some(tool_call_id) = tool_meta.tool_call_id.clone() {
        meta.insert("tool_call_id".to_string(), json!(tool_call_id));
    }
    if let Some(title) = tool_meta.title.clone() {
        meta.insert("title".to_string(), json!(title));
    }
    if let Some(status) = tool_meta.status.clone() {
        meta.insert("status".to_string(), json!(status));
    }
    if let Some(input_preview) = tool_meta.input_preview.clone() {
        meta.insert("input".to_string(), input_preview);
    }

    let mut event = OpsEvent::new("info", "tool_exec");
    event.session_id = Some(ctx.session_id.0.to_string());
    event.worktree_id = Some(ctx.worktree_id.0.to_string());
    event.run_id = Some(ctx.run_id.0.to_string());
    event.turn_id = Some(ctx.turn_id.0.to_string());
    event.provider_id = Some(ctx.provider_id.clone());
    event.tool_kind = tool_meta.tool_kind.clone();
    event.cwd = tool_meta.cwd.clone();
    event.worktree_root = Some(ctx.workdir_str.clone());
    event.meta = if meta.is_empty() {
        None
    } else {
        Some(Value::Object(meta))
    };
    host.emit_ops_event(event);

    if let Some(cwd) = tool_meta.cwd.as_deref() {
        if cwd_outside_worktree(cwd, &ctx.workdir_root, ctx.workdir_canonical.as_ref()) {
            let mut warn_event = OpsEvent::new("warn", "tool_exec_anomaly");
            warn_event.session_id = Some(ctx.session_id.0.to_string());
            warn_event.worktree_id = Some(ctx.worktree_id.0.to_string());
            warn_event.run_id = Some(ctx.run_id.0.to_string());
            warn_event.turn_id = Some(ctx.turn_id.0.to_string());
            warn_event.provider_id = Some(ctx.provider_id.clone());
            warn_event.tool_kind = tool_meta.tool_kind.clone();
            warn_event.cwd = Some(cwd.to_string());
            warn_event.worktree_root = Some(ctx.workdir_str.clone());
            warn_event.meta = Some(json!({
                "reason": "cwd_outside_worktree",
                "tool_call_id": tool_meta.tool_call_id,
            }));
            host.emit_ops_event(warn_event);
        }
    }
}
