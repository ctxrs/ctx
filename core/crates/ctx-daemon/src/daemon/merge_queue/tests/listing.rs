use super::*;

#[tokio::test]
async fn list_queued_entries_and_entry_lookup_are_workspace_scoped() {
    let (_data_dir, state) = setup_state().await;
    let workspace_a = state
        .global_store()
        .create_workspace("a".to_string(), "/tmp/a".to_string(), VcsKind::Git)
        .await
        .unwrap();
    let workspace_b = state
        .global_store()
        .create_workspace("b".to_string(), "/tmp/b".to_string(), VcsKind::Git)
        .await
        .unwrap();
    let store_a = state.store_for_workspace(workspace_a.id).await.unwrap();
    let store_b = state.store_for_workspace(workspace_b.id).await.unwrap();

    let base_time = Utc::now();
    let queued_a = MergeQueueEntry {
        id: MergeQueueEntryId::new(),
        workspace_id: workspace_a.id,
        worktree_id: None,
        session_id: None,
        target_branch: "main".to_string(),
        message: Some("queued-a".to_string()),
        patch_source: MergeQueuePatchSource::Generated,
        base_commit_sha: Some("base-a".to_string()),
        head_commit_sha: Some("head-a".to_string()),
        patch_path: "/tmp/queued-a.patch".to_string(),
        patch_size: 10,
        status: MergeQueueEntryStatus::Queued,
        result_commit_sha: None,
        error_message: None,
        created_at: base_time,
        updated_at: base_time,
    };
    let passed_b = MergeQueueEntry {
        id: MergeQueueEntryId::new(),
        workspace_id: workspace_b.id,
        worktree_id: None,
        session_id: None,
        target_branch: "main".to_string(),
        message: Some("passed-b".to_string()),
        patch_source: MergeQueuePatchSource::Generated,
        base_commit_sha: Some("base-b".to_string()),
        head_commit_sha: Some("head-b".to_string()),
        patch_path: "/tmp/passed-b.patch".to_string(),
        patch_size: 11,
        status: MergeQueueEntryStatus::Passed,
        result_commit_sha: Some("result-b".to_string()),
        error_message: None,
        created_at: base_time + TimeDelta::milliseconds(10),
        updated_at: base_time + TimeDelta::milliseconds(10),
    };
    let queued_b = MergeQueueEntry {
        id: MergeQueueEntryId::new(),
        workspace_id: workspace_b.id,
        worktree_id: None,
        session_id: None,
        target_branch: "main".to_string(),
        message: Some("queued-b".to_string()),
        patch_source: MergeQueuePatchSource::Generated,
        base_commit_sha: Some("base-c".to_string()),
        head_commit_sha: Some("head-c".to_string()),
        patch_path: "/tmp/queued-b.patch".to_string(),
        patch_size: 12,
        status: MergeQueueEntryStatus::Queued,
        result_commit_sha: None,
        error_message: None,
        created_at: base_time + TimeDelta::milliseconds(20),
        updated_at: base_time + TimeDelta::milliseconds(20),
    };

    store_a.create_merge_queue_entry(&queued_a).await.unwrap();
    store_b.create_merge_queue_entry(&passed_b).await.unwrap();
    store_b.create_merge_queue_entry(&queued_b).await.unwrap();

    let looked_up = get_workspace_merge_queue_entry(state.as_ref(), workspace_b.id, queued_b.id)
        .await
        .unwrap();
    assert_eq!(looked_up.id.0, queued_b.id.0);
    assert_eq!(looked_up.workspace_id.0, workspace_b.id.0);

    let queued_a_entries = list_queued_entries_for_workspace(state.as_ref(), workspace_a.id)
        .await
        .unwrap();
    assert_eq!(queued_a_entries.len(), 1);
    assert_eq!(queued_a_entries[0].id.0, queued_a.id.0);

    let queued_b_entries = list_queued_entries_for_workspace(state.as_ref(), workspace_b.id)
        .await
        .unwrap();
    assert_eq!(queued_b_entries.len(), 1);
    assert_eq!(queued_b_entries[0].id.0, queued_b.id.0);
}
