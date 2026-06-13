use ctx_core::ids::TurnId;
use ctx_core::models::{Session, SessionTurnStatus};

pub(super) async fn resolve_parent_turn_id(
    store: &ctx_store::Store,
    parent: &Session,
    tool_call_id: &str,
) -> Option<TurnId> {
    if !tool_call_id.trim().is_empty() {
        if let Ok(Some(tool)) = store.get_session_turn_tool(parent.id, tool_call_id).await {
            return Some(tool.turn_id);
        }
    }
    let Ok(turns) = store
        .list_session_turns_page_by_seq(parent.id, None, Some(5))
        .await
    else {
        return None;
    };
    turns.iter().rev().find_map(|turn| {
        matches!(
            turn.status,
            SessionTurnStatus::Starting | SessionTurnStatus::Running | SessionTurnStatus::Queued
        )
        .then_some(turn.turn_id)
    })
}
