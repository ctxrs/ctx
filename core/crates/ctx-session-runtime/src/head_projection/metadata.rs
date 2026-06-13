use ctx_core::models::{Session, SessionMetadata};

pub fn session_metadata_from_session(session: &Session) -> SessionMetadata {
    SessionMetadata {
        id: session.id,
        task_id: session.task_id,
        workspace_id: session.workspace_id,
        worktree_id: session.worktree_id,
        execution_environment: session.execution_environment,
        parent_session_id: session.parent_session_id,
        relationship: session.relationship.clone(),
        provider_id: session.provider_id.clone(),
        model_id: session.model_id.clone(),
        reasoning_effort: session.reasoning_effort.clone(),
        title: session.title.clone(),
        agent_role: session.agent_role.clone(),
        status: session.status.clone(),
        provider_session_ref: session.provider_session_ref.clone(),
        created_at: session.created_at,
        updated_at: session.updated_at,
    }
}
