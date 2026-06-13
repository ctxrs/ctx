use ctx_observability::ops_events::OpsEvent;
use serde_json::{json, Value};

use crate::daemon::scheduler::host::TurnEventLoopHost;

use super::super::TurnEventLoop;

pub(super) fn emit_failed_turn_ops_event(
    ctx: &TurnEventLoop,
    host: &TurnEventLoopHost,
    error_message: String,
    details: Option<Value>,
    kind: Option<Value>,
) {
    let mut fail_event = OpsEvent::new("error", "provider_run_failed");
    fail_event.session_id = Some(ctx.session_id.0.to_string());
    fail_event.worktree_id = Some(ctx.worktree_id.0.to_string());
    fail_event.run_id = Some(ctx.run_id.0.to_string());
    fail_event.turn_id = Some(ctx.turn_id.0.to_string());
    fail_event.provider_id = Some(ctx.provider_id.clone());
    fail_event.cwd = Some(ctx.workdir_str.clone());
    fail_event.worktree_root = Some(ctx.workdir_str.clone());
    fail_event.meta = Some(json!({
        "model_id": ctx.model_id.clone(),
        "execution_environment": ctx.execution_environment_label.clone(),
        "session_root_kind": ctx.session_root_kind.clone(),
        "error": error_message,
        "details": details,
        "kind": kind,
    }));
    host.emit_ops_event(fail_event);
}
