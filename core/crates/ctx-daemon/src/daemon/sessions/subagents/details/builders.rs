use std::time::Duration;

use super::super::context::{
    context_window_for_run_in_store, worktree_path_for_child_in_store,
    LegacyContextWindowRejectCounter,
};
use super::super::{
    internal_api_error, wait_for_run_assistant_message_in_store, AgentDetail, AgentResult,
    AgentSummary, ApiResult, SpawnedChild,
};
use super::refs::{encode_agent_ref, encode_run_ref};
use super::summary::{agent_terminal_result_status, build_agent_summary};

pub(in crate::daemon) async fn build_agent_detail_for_mcp_read(
    store: &ctx_store::Store,
    parent: &ctx_core::models::Session,
    session: &ctx_core::models::Session,
    inactivity_timeout: Duration,
    emit_legacy_context_window_key_reject: &LegacyContextWindowRejectCounter,
) -> ApiResult<AgentDetail> {
    let (summary, latest_turn) =
        build_agent_summary(store, session.id, &session.title, inactivity_timeout).await?;
    let latest_result = if let Some(turn) = latest_turn.as_ref() {
        if let Some(status) = agent_terminal_result_status(turn.status.clone()) {
            let content = if let Some(run_id) = turn.run_id {
                if status == "completed" {
                    wait_for_run_assistant_message_in_store(store, session.id, run_id)
                        .await
                        .map_err(internal_api_error)?
                } else {
                    store
                        .get_last_assistant_message_for_run(session.id, run_id)
                        .await
                        .map_err(internal_api_error)?
                        .map(|message| message.content)
                }
            } else {
                None
            };
            Some(AgentResult {
                run_id: turn.run_id.map(encode_run_ref),
                status: status.to_string(),
                content,
                context_window: if let Some(run_id) = turn.run_id {
                    context_window_for_run_in_store(
                        store,
                        session.id,
                        run_id,
                        emit_legacy_context_window_key_reject,
                    )
                    .await
                } else {
                    None
                },
            })
        } else {
            None
        }
    } else {
        None
    };

    Ok(AgentDetail {
        agent: summary,
        latest_result,
        worktree_path: worktree_path_for_child_in_store(store, parent.worktree_id, session.id)
            .await,
    })
}

pub(in crate::daemon::sessions::subagents) fn build_spawned_agent_detail(
    spawned: &SpawnedChild,
) -> AgentDetail {
    let child = &spawned.child;
    AgentDetail {
        agent: AgentSummary {
            agent_id: encode_agent_ref(child.child_session_id),
            task_label: child
                .label
                .clone()
                .unwrap_or_else(|| format!("Subagent {}", child.position + 1)),
            state: "running".to_string(),
            health: "healthy".to_string(),
            current_run_id: child.run_id.map(encode_run_ref),
            latest_result_status: None,
            last_progress_at: Some(child.updated_at.to_rfc3339()),
            last_event_seq: spawned.last_event_seq,
        },
        latest_result: None,
        worktree_path: spawned.worktree_path.clone(),
    }
}

pub(in crate::daemon) async fn collect_wait_targets(
    store: &ctx_store::Store,
    parent: &ctx_core::models::Session,
    agent_ids: &[String],
) -> ApiResult<Vec<ctx_core::models::Session>> {
    let mut agents = Vec::with_capacity(agent_ids.len());
    for agent_id in agent_ids {
        agents.push(super::summary::resolve_child_agent_session(store, parent, agent_id).await?);
    }
    Ok(agents)
}
