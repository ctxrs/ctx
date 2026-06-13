use super::super::*;

#[tokio::test]
async fn prune_archived_session_data_does_not_cache_historical_workspace_stores() {
    let temp = tempdir().unwrap();
    let stores = StoreManager::open(temp.path()).await.unwrap();

    for idx in 0..4 {
        stores
            .global()
            .create_workspace(
                format!("ws-{idx}"),
                temp.path()
                    .join(format!("ws-{idx}"))
                    .to_string_lossy()
                    .to_string(),
                VcsKind::Git,
            )
            .await
            .unwrap();
    }

    prune_archived_session_data_for_all_workspaces(&stores, 30)
        .await
        .unwrap();

    let stats = stores.stats().await;
    assert_eq!(stats.workspace_store_count, 0);
}

#[tokio::test]
async fn prune_archived_session_data_keeps_cached_workspace_store_usable() {
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
            temp.path().join("ws").to_string_lossy().to_string(),
            VcsKind::Git,
        )
        .await
        .unwrap();
    let cached_store = state.store_for_workspace(workspace.id).await.unwrap();
    cached_store
        .create_task(workspace.id, "before-prune".to_string(), None)
        .await
        .unwrap();

    prune_archived_session_data_for_all_workspaces(&stores, 30)
        .await
        .unwrap();

    let reopened = state.store_for_workspace(workspace.id).await.unwrap();
    reopened
        .create_task(workspace.id, "after-prune".to_string(), None)
        .await
        .expect("cached workspace store should remain usable after prune");
}
