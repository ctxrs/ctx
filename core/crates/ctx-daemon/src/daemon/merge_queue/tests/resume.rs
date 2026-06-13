use super::*;

#[tokio::test]
async fn enabled_workspace_queued_rows_resume_only_after_open() {
    let (data_dir, state) = setup_state().await;
    let workspace = create_workspace(&state, &data_dir, "enabled").await;
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
    let entry = queued_entry(workspace.id, "enabled-queued");
    store.create_merge_queue_entry(&entry).await.unwrap();
    drop(store);
    state.core.stores.evict_workspace(workspace.id).await;

    spawn_merge_queue_runner(state.clone());
    tokio::time::sleep(Duration::from_millis(100)).await;

    let cold_store = state.core.stores.workspace(workspace.id).await.unwrap();
    let queued = cold_store
        .get_merge_queue_entry(entry.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(queued.status, MergeQueueEntryStatus::Queued);
    drop(cold_store);
    state.core.stores.evict_workspace(workspace.id).await;
    assert_eq!(state.core.stores.stats().await.workspace_store_count, 0);

    activate_workspace_merge_queue(&state, workspace.id).await;
    let resumed = wait_for_entry_status(
        &state,
        workspace.id,
        entry.id,
        |status| status != MergeQueueEntryStatus::Queued,
        Duration::from_secs(2),
    )
    .await;
    assert_ne!(resumed.status, MergeQueueEntryStatus::Queued);
}

#[tokio::test]
async fn enabled_workspace_queued_rows_resume_when_reopened_from_draining_store() {
    let (data_dir, state) = setup_state().await;
    let workspace = create_workspace(&state, &data_dir, "draining").await;
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
    let entry = queued_entry(workspace.id, "draining-queued");
    store.create_merge_queue_entry(&entry).await.unwrap();

    spawn_merge_queue_runner(state.clone());
    state.core.stores.evict_workspace(workspace.id).await;
    assert_eq!(state.core.stores.stats().await.workspace_store_count, 0);

    let reopened = state.store_for_workspace(workspace.id).await.unwrap();
    activate_workspace_merge_queue(&state, workspace.id).await;
    let resumed = wait_for_entry_status(
        &state,
        workspace.id,
        entry.id,
        |status| status != MergeQueueEntryStatus::Queued,
        Duration::from_secs(2),
    )
    .await;
    assert_ne!(resumed.status, MergeQueueEntryStatus::Queued);

    drop(reopened);
    drop(store);
}

#[tokio::test]
async fn enabling_queue_reschedules_existing_queued_workspace() {
    let (data_dir, state) = setup_state().await;
    let workspace = create_workspace(&state, &data_dir, "reenable").await;
    let store = state.core.stores.workspace(workspace.id).await.unwrap();
    let entry = queued_entry(workspace.id, "reenable-queued");
    store.create_merge_queue_entry(&entry).await.unwrap();

    spawn_merge_queue_runner(state.clone());

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
    assert!(
        schedule_workspace_if_enabled_and_queued(&state, workspace.id)
            .await
            .unwrap()
    );

    let resumed = wait_for_entry_status(
        &state,
        workspace.id,
        entry.id,
        |status| status != MergeQueueEntryStatus::Queued,
        Duration::from_secs(2),
    )
    .await;
    assert_ne!(resumed.status, MergeQueueEntryStatus::Queued);
}

#[tokio::test]
async fn pending_wakeup_restarts_disabled_drain_after_reenable() {
    let (data_dir, state) = setup_state().await;
    let workspace = create_workspace(&state, &data_dir, "reenable-race").await;
    let store = state.core.stores.workspace(workspace.id).await.unwrap();
    let entry = queued_entry(workspace.id, "reenable-race-entry");
    store.create_merge_queue_entry(&entry).await.unwrap();
    spawn_merge_queue_runner(state.clone());

    assert!(
        begin_workspace_drain(state.as_ref(), workspace.id).await,
        "test should start with a simulated disabled drain already running"
    );

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

    schedule_workspace_drain(&state, workspace.id).await;
    assert!(
        state.transport.merge_queue.is_pending(workspace.id).await,
        "wakeups that land during an in-flight drain should be preserved"
    );

    if finish_workspace_drain(state.as_ref(), workspace.id).await {
        state.transport.merge_queue.schedule(workspace.id);
    }

    let resumed = wait_for_entry_status(
        &state,
        workspace.id,
        entry.id,
        |status| status != MergeQueueEntryStatus::Queued,
        Duration::from_secs(2),
    )
    .await;
    assert_ne!(resumed.status, MergeQueueEntryStatus::Queued);
}
