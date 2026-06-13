use super::*;

pub(super) async fn create_session_with_turn_status(
    state: &Arc<DaemonState>,
    root: &Path,
    environment: ExecutionEnvironment,
    status: SessionTurnStatus,
) -> (WorkspaceId, ctx_core::ids::SessionId) {
    let workspace = state
        .global_store()
        .create_workspace(
            format!("ws-{}", uuid::Uuid::new_v4()),
            root.join(format!("ws-{}", uuid::Uuid::new_v4()))
                .to_string_lossy()
                .to_string(),
            VcsKind::Git,
        )
        .await
        .unwrap();
    let store = state.store_for_workspace(workspace.id).await.unwrap();
    let worktree = store
        .create_worktree(
            workspace.id,
            root.join(format!("worktree-{}", uuid::Uuid::new_v4()))
                .to_string_lossy()
                .to_string(),
            "deadbeef".to_string(),
            None,
        )
        .await
        .unwrap();
    let task = store
        .create_task(workspace.id, "task".to_string(), None)
        .await
        .unwrap();
    let session = store
        .create_session(
            task.id,
            workspace.id,
            worktree.id,
            environment,
            "fake".to_string(),
            "model".to_string(),
            "implementer".to_string(),
            None,
            None,
            None,
        )
        .await
        .unwrap();
    state
        .global_store()
        .upsert_workspace_session_index(session.id, workspace.id)
        .await
        .unwrap();
    let now = Utc::now();
    store
        .insert_session_turn(SessionTurn {
            turn_id: TurnId::new(),
            session_id: session.id,
            run_id: Some(RunId::new()),
            user_message_id: None,
            status,
            start_seq: Some(1),
            end_seq: None,
            started_at: now,
            updated_at: now,
            assistant_partial: None,
            thought_partial: None,
            metrics_json: None,
            failure: None,
            tool_total: 0,
            tool_pending: 0,
            tool_running: 0,
            tool_completed: 0,
            tool_failed: 0,
        })
        .await
        .unwrap();
    (workspace.id, session.id)
}
