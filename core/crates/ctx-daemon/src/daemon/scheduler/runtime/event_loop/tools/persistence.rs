use ctx_core::models::{SessionEvent, SessionEventType};
use ctx_session_tools::order_seq::read_order_seq;
use ctx_session_tools::{
    build_turn_tool_update, merge_tool_update, tool_count_deltas, NormalizedToolEvent,
};
use ctx_store::store::SessionTurnToolCountDeltas;

use super::super::state::EventLoopRuntimeState;
use super::super::TurnEventLoop;

pub(super) async fn handle_persisted_tool_event(
    ctx: &TurnEventLoop,
    runtime: &mut EventLoopRuntimeState,
    event: &SessionEvent,
    tool_event: &NormalizedToolEvent,
) {
    let order_seq = read_order_seq(&event.payload_json);
    let Some(update) = build_turn_tool_update(tool_event, order_seq) else {
        return;
    };

    let prev = if matches!(&event.event_type, SessionEventType::ToolCallUpdate) {
        runtime.tool_cache.get(&update.tool_call_id).cloned()
    } else if let Some(cached) = runtime.tool_cache.get(&update.tool_call_id).cloned() {
        Some(cached)
    } else {
        ctx.store
            .get_session_turn_tool(ctx.session_id, &update.tool_call_id)
            .await
            .ok()
            .flatten()
    };

    let Some(merged) = merge_tool_update(
        prev.as_ref(),
        update,
        ctx.session_id,
        ctx.turn_id,
        event.seq,
        event.created_at,
    ) else {
        return;
    };

    if matches!(&event.event_type, SessionEventType::ToolCallUpdate) {
        runtime
            .tool_cache
            .insert(merged.tool_call_id.clone(), merged);
        return;
    }

    let (delta_total, delta_pending, delta_running, delta_completed, delta_failed) =
        tool_count_deltas(prev.as_ref(), &merged);
    let _ = ctx.store.upsert_session_turn_tool(merged.clone()).await;
    if delta_total != 0
        || delta_pending != 0
        || delta_running != 0
        || delta_completed != 0
        || delta_failed != 0
    {
        let _ = ctx
            .store
            .update_session_turn_tool_counts(
                ctx.session_id,
                ctx.turn_id,
                SessionTurnToolCountDeltas {
                    total: delta_total,
                    pending: delta_pending,
                    running: delta_running,
                    completed: delta_completed,
                    failed: delta_failed,
                },
                event.created_at,
            )
            .await;
    }
    runtime
        .tool_cache
        .insert(merged.tool_call_id.clone(), merged);
}
