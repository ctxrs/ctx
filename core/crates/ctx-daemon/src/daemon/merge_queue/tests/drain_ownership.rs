use super::*;

#[tokio::test]
async fn workspace_drain_ownership_allows_only_one_runner() {
    let (_data_dir, state) = setup_state().await;
    let workspace_id = WorkspaceId::new();

    assert!(begin_workspace_drain(state.as_ref(), workspace_id).await);
    assert!(!begin_workspace_drain(state.as_ref(), workspace_id).await);
    finish_workspace_drain(state.as_ref(), workspace_id).await;
    assert!(begin_workspace_drain(state.as_ref(), workspace_id).await);
}
