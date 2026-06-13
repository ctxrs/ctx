use std::time::Duration;

use ctx_core::ids::SessionId;
use ctx_core::models::SessionTurn;

use super::super::{api_error, internal_api_error, ApiResult, SubagentErrorKind};
use super::refs::{decode_agent_ref, encode_agent_ref, encode_run_ref};
use crate::daemon::sessions::subagents::AgentSummary;

pub(in crate::daemon::sessions::subagents) use self::turns::is_active_turn_status;
pub(super) use self::turns::{agent_active_state, agent_terminal_result_status};
use self::turns::{agent_health, latest_terminal_turn_for_session};

mod turns;

pub(in crate::daemon) async fn resolve_child_agent_session(
    store: &ctx_store::Store,
    parent: &ctx_core::models::Session,
    raw_agent_id: &str,
) -> ApiResult<ctx_core::models::Session> {
    let agent_id = decode_agent_ref(raw_agent_id)
        .map_err(|error| api_error(SubagentErrorKind::BadRequest, error))?;
    let child = store
        .get_active_subagent_session(parent.id, agent_id)
        .await
        .map_err(internal_api_error)?
        .ok_or_else(|| api_error(SubagentErrorKind::NotFound, "agent not found"))?;
    Ok(child)
}

pub(in crate::daemon) async fn build_agent_summary(
    store: &ctx_store::Store,
    session_id: SessionId,
    session_title: &str,
    inactivity_timeout: Duration,
) -> ApiResult<(AgentSummary, Option<SessionTurn>)> {
    let latest_turn = store
        .get_latest_turn_for_session(session_id)
        .await
        .map_err(internal_api_error)?;
    let latest_terminal_turn =
        latest_terminal_turn_for_session(store, session_id, latest_turn.as_ref()).await?;
    let active_turn = match store.get_running_turn_for_session(session_id).await {
        Ok(Some(turn)) => Some(turn),
        Ok(None) => latest_turn
            .clone()
            .filter(|turn| is_active_turn_status(&turn.status)),
        Err(error) => return Err(internal_api_error(error)),
    };
    let task_label = {
        let trimmed = session_title.trim();
        if trimmed.is_empty() {
            format!("agent-{}", session_id.0)
        } else {
            trimmed.to_string()
        }
    };
    let summary = AgentSummary {
        agent_id: encode_agent_ref(session_id),
        task_label,
        state: active_turn
            .as_ref()
            .map(|turn| agent_active_state(turn.status.clone()).to_string())
            .unwrap_or_else(|| "waiting_input".to_string()),
        health: agent_health(active_turn.as_ref(), inactivity_timeout).to_string(),
        current_run_id: active_turn
            .as_ref()
            .and_then(|turn| turn.run_id.map(encode_run_ref)),
        latest_result_status: latest_terminal_turn
            .as_ref()
            .and_then(|turn| agent_terminal_result_status(turn.status.clone()))
            .map(str::to_string),
        last_progress_at: active_turn
            .as_ref()
            .or(latest_turn.as_ref())
            .map(|turn| turn.updated_at.to_rfc3339()),
        last_event_seq: store
            .get_session_last_event_seq(session_id)
            .await
            .map_err(internal_api_error)?,
    };
    Ok((summary, latest_terminal_turn))
}
