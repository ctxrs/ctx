use super::*;

#[tokio::test]
async fn sweeper_keeps_merge_queue_running_workspaces_resident() {
    let temp = tempdir().unwrap();
    let stores = StoreManager::open(temp.path()).await.unwrap();
    let state = Arc::new(DaemonState::new(
        temp.path().to_path_buf(),
        stores.clone(),
        HashMap::new(),
        "http://localhost".to_string(),
        None,
    ));

    let workspace = state
        .global_store()
        .create_workspace(
            "ws".to_string(),
            temp.path().to_string_lossy().to_string(),
            VcsKind::Git,
        )
        .await
        .unwrap();
    let _ = state.store_for_workspace(workspace.id).await.unwrap();
    assert_eq!(stores.stats().await.workspace_store_count, 1);

    assert!(
        state
            .transport
            .merge_queue
            .begin_workspace_drain(workspace.id)
            .await
    );

    let config = CacheSweepConfig {
        session_ttl: Duration::from_secs(0),
        workspace_ttl: Duration::from_secs(0),
        interval: Duration::from_secs(30),
    };
    let _ = state.sweep_idle_caches(Instant::now(), config).await;
    assert_eq!(stores.stats().await.workspace_store_count, 1);

    let _ = state
        .transport
        .merge_queue
        .finish_workspace_drain(workspace.id)
        .await;

    let _ = state.sweep_idle_caches(Instant::now(), config).await;
    assert_eq!(stores.stats().await.workspace_store_count, 0);
}
