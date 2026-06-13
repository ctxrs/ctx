use std::collections::HashMap;
use std::sync::Arc;

use ctx_core::ids::TurnId;
use ctx_core::models::{
    ExecutionEnvironment, Session, SubagentInvocationChild, VcsKind, Workspace,
};
use ctx_session_tools::model_resolution::ModelCatalog;
use ctx_subagent_service::SubagentWorktreeSelection;
use tokio::sync::Mutex;

use ctx_settings_model::ExecutionSettings;

use super::super::errors::{api_error, internal_api_error, ApiResult, SubagentErrorKind};
use super::super::{AgentInitItem, SpawnedChild, SubagentSpawnHost};

mod model;
mod session_index;
mod worktree;

use model::resolve_child_model;
use session_index::index_child_session;
use worktree::resolve_child_worktree;

#[derive(Clone)]
pub(super) struct SubagentChildInit {
    pub(super) host: Arc<SubagentSpawnHost>,
    pub(super) parent: Session,
    pub(super) workspace: Workspace,
    pub(super) model_catalogs: HashMap<String, Option<ModelCatalog>>,
    pub(super) invocation_id: String,
    pub(super) tool_call_id: String,
    pub(super) child_ids: Arc<Mutex<Vec<String>>>,
    pub(super) parent_turn_id: Option<TurnId>,
    pub(super) worktree_selection: SubagentWorktreeSelection,
    pub(super) worktree_plan: Option<(VcsKind, String)>,
    pub(super) parent_effective: ExecutionSettings,
    pub(super) execution_environment: ExecutionEnvironment,
}

pub(super) struct SubagentChildInitItem {
    pub(super) idx: usize,
    pub(super) agent: AgentInitItem,
    pub(super) label: String,
}

pub(super) async fn create_subagent_child(
    init: SubagentChildInit,
    item: SubagentChildInitItem,
) -> ApiResult<SpawnedChild> {
    let store = init.host.store_for_session(init.parent.id).await?;
    let prompt = item.agent.prompt.trim().to_string();
    if prompt.is_empty() {
        return Err(api_error(
            SubagentErrorKind::BadRequest,
            format!("agent {} prompt is required", item.idx + 1),
        ));
    }

    let resolved = resolve_child_model(&init, &item).await?;

    let prompt_length = prompt.chars().count() as i64;
    let provider_id = resolved.provider_id.clone();
    let reasoning_effort = resolved.reasoning_effort.clone();
    let (worktree_id, worktree_path) = resolve_child_worktree(&init, &store).await?;

    let session = store
        .create_session_with_reasoning_effort(
            init.parent.task_id,
            init.parent.workspace_id,
            worktree_id,
            init.execution_environment,
            provider_id,
            resolved.model_id.clone(),
            reasoning_effort.clone(),
            "subagent".into(),
            Some(init.parent.id),
            Some("sub_agent".to_string()),
            None,
        )
        .await
        .map_err(internal_api_error)?;
    index_child_session(&init, &store, &session, &item.label).await;

    let child_created_at = chrono::Utc::now();
    let persisted = init.host.persist_subagent_prompt(&session, prompt).await?;
    let child_session_id = session.id;
    let child = SubagentInvocationChild {
        invocation_id: init.invocation_id.clone(),
        child_session_id,
        run_id: Some(persisted.run_id),
        position: item.idx as i64,
        status: "running".to_string(),
        label: Some(item.label),
        harness: Some(resolved.provider_id),
        model: Some(resolved.full_model_id),
        reasoning_effort,
        prompt_length,
        created_at: child_created_at,
        updated_at: child_created_at,
    };
    store
        .upsert_subagent_invocation_child(child.clone())
        .await
        .map_err(internal_api_error)?;

    let child_ids_snapshot = {
        let mut ids = init.child_ids.lock().await;
        ids.push(child_session_id.0.to_string());
        ids.clone()
    };
    init.host
        .emit_subagent_invocation_notice(
            init.parent.id,
            init.parent_turn_id,
            serde_json::json!({
            "kind": "subagent_invocation_updated",
            "invocation_id": init.invocation_id.clone(),
            "tool_call_id": init.tool_call_id.clone(),
            "status": "running",
            "child_session_ids": child_ids_snapshot,
            }),
        )
        .await?;
    init.host
        .dispatch_subagent_prompt(&session, &persisted.saved_message)
        .await;

    Ok(SpawnedChild {
        child,
        worktree_path,
        last_event_seq: persisted.last_event_seq,
    })
}
