use super::*;

#[tokio::test]
async fn opening_workspace_does_not_evict_active_workspace_store() {
    let temp = tempdir().unwrap();
    let stores = StoreManager::open_with_config(
        temp.path(),
        StoreManagerConfig {
            max_cached_workspaces: 2,
            ..StoreManagerConfig::default()
        },
    )
    .await
    .unwrap();
    let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
    providers.insert("fake".into(), Arc::new(FakeProviderAdapter::new()));
    let state = Arc::new(DaemonState::new(
        temp.path().to_path_buf(),
        stores.clone(),
        providers,
        "http://localhost".to_string(),
        None,
    ));

    let workspace_a = state
        .global_store()
        .create_workspace(
            "ws-a".to_string(),
            temp.path().join("ws-a").to_string_lossy().to_string(),
            VcsKind::Git,
        )
        .await
        .unwrap();
    let workspace_b = state
        .global_store()
        .create_workspace(
            "ws-b".to_string(),
            temp.path().join("ws-b").to_string_lossy().to_string(),
            VcsKind::Git,
        )
        .await
        .unwrap();
    let workspace_c = state
        .global_store()
        .create_workspace(
            "ws-c".to_string(),
            temp.path().join("ws-c").to_string_lossy().to_string(),
            VcsKind::Git,
        )
        .await
        .unwrap();

    let store_a = state.store_for_workspace(workspace_a.id).await.unwrap();
    let worktree_a = store_a
        .create_worktree(
            workspace_a.id,
            temp.path().join("wt-a").to_string_lossy().to_string(),
            "base-a".to_string(),
            None,
        )
        .await
        .unwrap();
    let task_a = store_a
        .create_task(workspace_a.id, "task-a".to_string(), None)
        .await
        .unwrap();
    let session_a = store_a
        .create_session(
            task_a.id,
            workspace_a.id,
            worktree_a.id,
            ctx_core::models::ExecutionEnvironment::Host,
            "fake".to_string(),
            "model".to_string(),
            "implementer".to_string(),
            None,
            None,
            None,
        )
        .await
        .unwrap();
    let _ = state
        .session_scheduler_worker_host
        .ensure_scheduler(&state.sessions, session_a)
        .await;

    let _ = state.store_for_workspace(workspace_b.id).await.unwrap();
    assert_eq!(stores.stats().await.workspace_store_count, 2);

    let _ = state.store_for_workspace(workspace_c.id).await.unwrap();
    assert_eq!(stores.stats().await.workspace_store_count, 2);

    let workspace_a_cached = state
        .core
        .stores
        .workspace_access(workspace_a.id)
        .await
        .unwrap();
    assert!(
        matches!(workspace_a_cached.kind, WorkspaceStoreAccessKind::Cached),
        "active workspace store should stay cached under the cap"
    );
    let workspace_b_reopened = state
        .core
        .stores
        .workspace_access(workspace_b.id)
        .await
        .unwrap();
    assert!(
        matches!(
            workspace_b_reopened.kind,
            WorkspaceStoreAccessKind::ColdOpen | WorkspaceStoreAccessKind::Reactivated
        ),
        "inactive workspace store should be the one evicted under the cap"
    );
}
