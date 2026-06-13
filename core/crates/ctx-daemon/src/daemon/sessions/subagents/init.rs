use std::sync::Arc;

use ctx_core::ids::SessionId;

use super::*;

mod children;
mod invocation;
mod parent;
mod request;
mod spawning;

use children::{create_subagent_child, SubagentChildInit, SubagentChildInitItem};
use invocation::StartedSubagentInvocation;
use parent::validate_parent_spawn_capacity;
use request::{
    build_subagent_request_agents, prepare_subagent_init_request, PreparedSubagentInitRequest,
};
use spawning::spawn_subagent_completion_tasks;

pub async fn init_subagents(
    host: Arc<SubagentSpawnHost>,
    parent_id: SessionId,
    req: AgentInitReq,
) -> ApiResult<Vec<SpawnedChild>> {
    let PreparedSubagentInitRequest {
        agents,
        labels,
        request_json,
        tool_call_id,
        worktree_selection,
    } = prepare_subagent_init_request(host.as_ref(), &req).await?;

    let (store, parent) = host.load_parent_session(parent_id).await?;
    let creation_lock = host.task_session_creation_lock(parent.task_id).await;
    let _creation_guard = creation_lock.lock().await;
    validate_parent_spawn_capacity(&store, &parent, agents.len()).await?;

    ensure_requested_labels_available(&store, parent.task_id, &labels).await?;
    let request_agents = build_subagent_request_agents(&agents);
    let provider_ids = collect_provider_ids(&request_agents, &parent.provider_id)
        .map_err(|error| api_error(SubagentErrorKind::BadRequest, error))?;

    let parent_context = host.load_parent_worktree_context(&store, &parent).await?;

    let model_catalogs = host
        .load_requested_model_catalogs(
            &parent_context.workspace,
            &provider_ids,
            parent_context.execution_environment,
        )
        .await?;
    let worktree_plan = host
        .plan_subagent_worktree_creation(&parent_context.worktree, worktree_selection)
        .await?;

    let StartedSubagentInvocation {
        invocation_id,
        tool_call_id,
        parent_turn_id,
    } = host
        .start_subagent_invocation(
            &store,
            &parent,
            agents.len(),
            Some(request_json),
            tool_call_id.as_deref(),
        )
        .await?;

    let child_ids = Arc::new(tokio::sync::Mutex::new(Vec::<String>::new()));
    let child_init = SubagentChildInit {
        host: Arc::clone(&host),
        parent: parent.clone(),
        workspace: parent_context.workspace.clone(),
        model_catalogs,
        invocation_id: invocation_id.clone(),
        tool_call_id: tool_call_id.clone(),
        child_ids: child_ids.clone(),
        parent_turn_id,
        worktree_selection,
        worktree_plan,
        parent_effective: parent_context.effective.clone(),
        execution_environment: parent_context.execution_environment,
    };

    let mut futures = Vec::with_capacity(agents.len());
    for (idx, agent) in agents.into_iter().enumerate() {
        let label = labels
            .get(idx)
            .cloned()
            .unwrap_or_else(|| format!("Subagent {}", idx + 1));
        futures.push(create_subagent_child(
            child_init.clone(),
            SubagentChildInitItem { idx, agent, label },
        ));
    }

    let spawned_children = match futures::future::try_join_all(futures).await {
        Ok(children) => children,
        Err(error) => {
            let child_session_ids = {
                let ids = child_ids.lock().await;
                ids.clone()
            };
            host.mark_subagent_invocation_failed(
                &parent,
                &invocation_id,
                &tool_call_id,
                parent_turn_id,
                &child_session_ids,
            )
            .await;
            return Err(error);
        }
    };

    spawn_subagent_completion_tasks(
        &host,
        &spawned_children,
        invocation_id,
        tool_call_id,
        parent.id,
        parent_turn_id,
        parent.worktree_id,
    );

    Ok(spawned_children)
}
