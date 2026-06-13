use ctx_core::ids::SessionId;
use ctx_core::models::{SessionTurn, SessionTurnStatus};
pub(super) use ctx_subagent_service::agent_health;
pub(in crate::daemon::sessions::subagents) use ctx_subagent_service::is_active_turn_status;
pub(in crate::daemon::sessions::subagents::details) use ctx_subagent_service::{
    agent_active_state, agent_terminal_result_status,
};

use super::super::super::{internal_api_error, ApiResult};

fn is_terminal_turn_status(status: &SessionTurnStatus) -> bool {
    matches!(
        status,
        SessionTurnStatus::Completed | SessionTurnStatus::Interrupted | SessionTurnStatus::Failed
    )
}

pub(super) async fn latest_terminal_turn_for_session(
    store: &ctx_store::Store,
    session_id: SessionId,
    latest_turn: Option<&SessionTurn>,
) -> ApiResult<Option<SessionTurn>> {
    if latest_turn
        .as_ref()
        .is_some_and(|turn| is_terminal_turn_status(&turn.status))
    {
        return Ok(latest_turn.cloned());
    }

    let mut before_seq = latest_turn.as_ref().and_then(|turn| turn.start_seq);
    loop {
        let page = store
            .list_session_turns_page_by_seq(session_id, before_seq, Some(50))
            .await
            .map_err(internal_api_error)?;
        if page.is_empty() {
            return Ok(None);
        }
        if let Some(turn) = page
            .iter()
            .rev()
            .find(|turn| is_terminal_turn_status(&turn.status))
            .cloned()
        {
            return Ok(Some(turn));
        }
        before_seq = page.first().and_then(|turn| turn.start_seq);
        if before_seq.is_none() {
            return Ok(None);
        }
    }
}
