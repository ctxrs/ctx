use super::*;

#[tokio::test]
async fn workspace_activation_only_schedules_the_opened_workspace() {
    let (data_dir, state) = setup_state().await;
    let workspace_a = create_workspace(&state, &data_dir, "a").await;
    let workspace_b = create_workspace(&state, &data_dir, "b").await;

    let _ = state.core.stores.workspace(workspace_a.id).await.unwrap();
    let _ = state.core.stores.workspace(workspace_b.id).await.unwrap();
    state.core.stores.evict_workspace(workspace_a.id).await;
    state.core.stores.evict_workspace(workspace_b.id).await;
    assert_eq!(state.core.stores.stats().await.workspace_store_count, 0);

    spawn_merge_queue_runner(state.clone());
    activate_workspace_merge_queue(&state, workspace_a.id).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let stats = state.core.stores.stats().await;
    assert_eq!(stats.workspace_store_count, 1);
    assert!(state.core.stores.workspace(workspace_a.id).await.is_ok());
    assert_eq!(state.core.stores.stats().await.workspace_store_count, 1);
}
