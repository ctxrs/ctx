use super::*;
use crate::daemon::DaemonState;

pub(in crate::daemon::scheduler::runtime::event_loop::tests) struct LoopFixture {
    pub(in crate::daemon::scheduler::runtime::event_loop::tests) state: Arc<DaemonState>,
    pub(in crate::daemon::scheduler::runtime::event_loop::tests) store: ctx_store::Store,
    pub(in crate::daemon::scheduler::runtime::event_loop::tests) workspace_id:
        ctx_core::ids::WorkspaceId,
    pub(in crate::daemon::scheduler::runtime::event_loop::tests) worktree_id:
        ctx_core::ids::WorktreeId,
    pub(in crate::daemon::scheduler::runtime::event_loop::tests) task_id: ctx_core::ids::TaskId,
    pub(in crate::daemon::scheduler::runtime::event_loop::tests) session_id:
        ctx_core::ids::SessionId,
    pub(in crate::daemon::scheduler::runtime::event_loop::tests) turn_id: TurnId,
    pub(in crate::daemon::scheduler::runtime::event_loop::tests) run_id: RunId,
    pub(in crate::daemon::scheduler::runtime::event_loop::tests) message_id: MessageId,
    pub(in crate::daemon::scheduler::runtime::event_loop::tests) workspace_root: std::path::PathBuf,
}

pub(in crate::daemon::scheduler::runtime::event_loop::tests) async fn build_loop_fixture(
    data_dir: &Path,
    provider_id: &str,
    model_id: &str,
) -> LoopFixture {
    let stores = StoreManager::open(data_dir).await.expect("open stores");
    let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
    providers.insert("fake".into(), Arc::new(FakeProviderAdapter::new()));
    let state = Arc::new(DaemonState::new(
        data_dir.to_path_buf(),
        stores.clone(),
        providers,
        "http://localhost".to_string(),
        None,
    ));

    let workspace_root = data_dir.join("workspace");
    tokio::fs::create_dir_all(&workspace_root)
        .await
        .expect("workspace root");
    let workspace = state
        .global_store()
        .create_workspace(
            "ws".to_string(),
            workspace_root.to_string_lossy().to_string(),
            VcsKind::Git,
        )
        .await
        .expect("workspace");
    let store = state
        .store_for_workspace(workspace.id)
        .await
        .expect("workspace store");
    let worktree = store
        .create_worktree(
            workspace.id,
            workspace_root.to_string_lossy().to_string(),
            "deadbeef".to_string(),
            None,
        )
        .await
        .expect("worktree");
    let task = store
        .create_task(workspace.id, "task".to_string(), None)
        .await
        .expect("task");
    let session = store
        .create_session(
            task.id,
            workspace.id,
            worktree.id,
            ExecutionEnvironment::Host,
            provider_id.to_string(),
            model_id.to_string(),
            "implementer".to_string(),
            None,
            None,
            None,
        )
        .await
        .expect("session");
    store
        .set_task_primary_session(task.id, session.id, worktree.id)
        .await
        .expect("primary session");
    state
        .global_store()
        .upsert_workspace_session_index(session.id, workspace.id)
        .await
        .expect("workspace session index");
    state.sessions.remember_session_meta(&session).await;

    let run_id = RunId::new();
    let turn_id = TurnId::new();
    let now = chrono::Utc::now();
    store
        .insert_session_turn(SessionTurn {
            turn_id,
            session_id: session.id,
            run_id: Some(run_id),
            user_message_id: None,
            status: SessionTurnStatus::Starting,
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
        .expect("insert turn");

    let seeded = store
        .get_session_head_snapshot(session.id, 60, false)
        .await
        .expect("load baseline head")
        .expect("baseline head");
    state
        .workspaces
        .workspace_active_snapshot
        .update_session_head(seeded)
        .await;
    let active_task_summary = store
        .get_workspace_active_task_summary(task.id)
        .await
        .expect("load active task summary")
        .expect("active task summary exists");
    state
        .workspaces
        .workspace_active_snapshot
        .publish_active_task_upsert(workspace.id, active_task_summary)
        .await;

    LoopFixture {
        state,
        store,
        workspace_id: workspace.id,
        worktree_id: worktree.id,
        task_id: task.id,
        session_id: session.id,
        turn_id,
        run_id,
        message_id: MessageId::new(),
        workspace_root,
    }
}
