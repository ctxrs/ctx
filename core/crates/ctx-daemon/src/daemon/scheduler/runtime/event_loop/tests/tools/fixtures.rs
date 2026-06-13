use super::*;
use crate::daemon::DaemonState;

mod runner;

pub(super) enum HeadCacheSeed {
    Active,
    Compact,
}

pub(super) struct ToolEventLoopFixture {
    pub(super) state: Arc<DaemonState>,
    pub(super) store: ctx_store::Store,
    pub(super) session_id: ctx_core::ids::SessionId,
    pub(super) turn_id: TurnId,
    task_id: ctx_core::ids::TaskId,
    workspace_id: ctx_core::ids::WorkspaceId,
    worktree_id: ctx_core::ids::WorktreeId,
    run_id: RunId,
    message_id: MessageId,
    workspace_root: std::path::PathBuf,
    _data_dir: tempfile::TempDir,
}

impl ToolEventLoopFixture {
    pub(super) async fn new(spool_enabled: bool, head_cache_seed: HeadCacheSeed) -> Self {
        let data_dir = tempdir().expect("temp dir");
        let stores = StoreManager::open(data_dir.path())
            .await
            .expect("open stores");
        let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
        providers.insert("fake".into(), Arc::new(FakeProviderAdapter::new()));
        let mut app_state = DaemonState::new(
            data_dir.path().to_path_buf(),
            stores.clone(),
            providers,
            "http://localhost".to_string(),
            None,
        );
        app_state
            .session_scheduler_worker_host
            .configure_tool_output_spool_for_test(
                spool_enabled,
                app_state.core.tool_output_spool_dir.clone(),
            );
        if spool_enabled {
            std::fs::create_dir_all(&app_state.core.tool_output_spool_dir)
                .expect("tool output spool dir");
        }
        let state = Arc::new(app_state);

        let workspace_root = data_dir.path().join("workspace");
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
                "fake".to_string(),
                "model".to_string(),
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
                status: SessionTurnStatus::Running,
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
        match head_cache_seed {
            HeadCacheSeed::Active => {
                state
                    .workspaces
                    .workspace_active_snapshot
                    .update_session_head(seeded)
                    .await;
            }
            HeadCacheSeed::Compact => {
                state
                    .workspaces
                    .workspace_active_snapshot
                    .update_compact_session_head(seeded)
                    .await;
            }
        }

        Self {
            state,
            store,
            session_id: session.id,
            turn_id,
            task_id: task.id,
            workspace_id: workspace.id,
            worktree_id: worktree.id,
            run_id,
            message_id: MessageId::new(),
            workspace_root,
            _data_dir: data_dir,
        }
    }
}
