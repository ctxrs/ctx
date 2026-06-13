use ctx_core::models::{SessionEvent, SessionEventType};
use ctx_session_tools::{sanitize_normalized_tool_event_payload, NormalizedToolEvent};
use serde_json::Value;

use crate::daemon::scheduler::host::TurnEventLoopHost;

use self::ops::emit_tool_call_ops;
use super::super::tool_runtime::{self, maybe_spool_tool_output};
use super::state::EventLoopRuntimeState;
use super::TurnEventLoop;

mod ops;
mod persistence;

pub(super) async fn prepare_tool_event_payload(
    ctx: &TurnEventLoop,
    host: &TurnEventLoopHost,
    event_type: &SessionEventType,
    tool_event: &NormalizedToolEvent,
) -> Value {
    if matches!(event_type, SessionEventType::ToolCall) {
        emit_tool_call_ops(ctx, host, tool_event);
    }

    let output_artifact = if matches!(event_type, SessionEventType::ToolResult) {
        maybe_spool_tool_output(
            host.tool_output_spool_enabled(),
            host.tool_output_spool_dir(),
            &ctx.store,
            tool_event,
            tool_runtime::ToolOutputArtifactScope {
                session_id: ctx.session_id,
                task_id: ctx.task_id,
                workspace_id: ctx.workspace_id,
                worktree_id: ctx.worktree_id,
                turn_id: ctx.turn_id,
            },
        )
        .await
    } else {
        None
    };

    sanitize_normalized_tool_event_payload(tool_event, output_artifact.as_ref())
}

pub(super) async fn handle_persisted_tool_event(
    ctx: &TurnEventLoop,
    runtime: &mut EventLoopRuntimeState,
    event: &SessionEvent,
    tool_event: &NormalizedToolEvent,
) {
    persistence::handle_persisted_tool_event(ctx, runtime, event, tool_event).await;
}
