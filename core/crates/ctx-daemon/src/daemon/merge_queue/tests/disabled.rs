use super::*;
use ctx_merge_queue::WorkspaceDrainStop;

#[tokio::test]
async fn disabled_workspace_with_queued_rows_are_cancelled_after_activation() {
    let (data_dir, state) = setup_state().await;
    let workspace = create_workspace(&state, &data_dir, "disabled").await;
    let store = state.core.stores.workspace(workspace.id).await.unwrap();
    let entry = queued_entry(workspace.id, "disabled-queued");
    store.create_merge_queue_entry(&entry).await.unwrap();
    drop(store);
    state.core.stores.evict_workspace(workspace.id).await;

    spawn_merge_queue_runner(state.clone());
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(state.core.stores.stats().await.workspace_store_count, 0);

    activate_workspace_merge_queue(&state, workspace.id).await;
    let stored = wait_for_entry_status(
        &state,
        workspace.id,
        entry.id,
        |status| status == MergeQueueEntryStatus::Cancelled,
        Duration::from_secs(2),
    )
    .await;
    assert_eq!(stored.status, MergeQueueEntryStatus::Cancelled);
    assert_eq!(
        stored.error_message.as_deref(),
        Some("merge queue disabled while entry was queued")
    );
    assert!(!state
        .transport
        .merge_queue
        .running_workspaces()
        .await
        .contains(&workspace.id));
}

#[tokio::test]
async fn cancel_for_disabled_workspace_noops_if_queue_was_reenabled() {
    let (data_dir, state) = setup_state().await;
    let workspace = create_workspace(&state, &data_dir, "reenabled").await;
    let store = state.core.stores.workspace(workspace.id).await.unwrap();
    let entry = queued_entry(workspace.id, "queued-before-reenable");
    store.create_merge_queue_entry(&entry).await.unwrap();

    update_merge_queue_config(
        &store,
        MergeQueueConfigUpdate {
            enabled: true,
            target_branch: Some("main".to_string()),
            verify_commands: Vec::new(),
            push_on_success: None,
            push_remote: None,
            push_branch: None,
            canonical_sync: None,
        },
    )
    .await
    .unwrap();

    cancel_queued_entries_for_disabled_workspace(&state, &store, workspace.id)
        .await
        .unwrap();

    let stored = store
        .get_merge_queue_entry(entry.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.status, MergeQueueEntryStatus::Queued);
}

#[tokio::test]
async fn disabled_drain_stays_dormant_without_reopening_workspace() {
    let (data_dir, state) = setup_state().await;
    let workspace = create_workspace(&state, &data_dir, "disabled-recheck").await;
    let store = state.core.stores.workspace(workspace.id).await.unwrap();
    update_merge_queue_config(
        &store,
        MergeQueueConfigUpdate {
            enabled: true,
            target_branch: Some("main".to_string()),
            verify_commands: Vec::new(),
            push_on_success: None,
            push_remote: None,
            push_branch: None,
            canonical_sync: None,
        },
    )
    .await
    .unwrap();
    let entry = queued_entry(workspace.id, "disabled-recheck-entry");
    store.create_merge_queue_entry(&entry).await.unwrap();
    drop(store);
    state.core.stores.evict_workspace(workspace.id).await;

    assert!(
        !reschedule_workspace_after_drain(&state, workspace.id, WorkspaceDrainStop::Disabled).await
    );
    assert_eq!(state.core.stores.stats().await.workspace_store_count, 0);
}
