use super::*;

#[tokio::test]
async fn update_drain_blocks_new_work_until_released() {
    let temp = tempdir().unwrap();
    let stores = StoreManager::open(temp.path()).await.unwrap();
    let state = Arc::new(DaemonState::new(
        temp.path().to_path_buf(),
        stores,
        HashMap::new(),
        "http://localhost".to_string(),
        None,
    ));

    assert!(state
        .core
        .update_drain
        .acquire("test_update", "unit_test")
        .await
        .is_some());
    let err = state
        .core
        .update_drain
        .reject_if_draining()
        .await
        .expect_err("drain should reject new work");
    assert!(err
        .to_string()
        .contains("daemon maintenance is in progress"));
    assert!(state.core.update_drain.release().await);
    state
        .core
        .update_drain
        .reject_if_draining()
        .await
        .expect("released drain should allow work");
}
