use super::super::*;

#[tokio::test]
async fn merge_queue_startup_runner_does_not_cache_historical_workspace_stores() {
    let temp = tempdir().unwrap();
    let stores = StoreManager::open(temp.path()).await.unwrap();
    let state = Arc::new(DaemonState::new(
        temp.path().to_path_buf(),
        stores.clone(),
        HashMap::new(),
        "http://localhost".to_string(),
        None,
    ));

    for idx in 0..4 {
        state
            .global_store()
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

    crate::daemon::merge_queue::spawn_merge_queue_runner(state);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let stats = stores.stats().await;
    assert_eq!(stats.workspace_store_count, 0);
}
