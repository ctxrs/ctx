use super::*;
use chrono::Utc;
use ctx_core::ids::{ArtifactId, MergeQueueEntryId, MessageId};
use ctx_core::models::VcsKind;
use ctx_core::models::{
    Artifact, MergeQueueEntry, MergeQueueEntryStatus, MergeQueuePatchSource, Message,
    MessageDelivery, MessageRole, SubagentInvocation,
};
use std::fs;

#[tokio::test]
async fn evict_idle_workspaces_skips_active() {
    let temp = tempfile::tempdir().unwrap();
    let manager = StoreManager::open(temp.path()).await.unwrap();
    let workspace = manager
        .global()
        .create_workspace(
            "ws".to_string(),
            temp.path().to_string_lossy().to_string(),
            VcsKind::Git,
        )
        .await
        .unwrap();
    let _store = manager.workspace(workspace.id).await.unwrap();

    let mut active = HashSet::new();
    active.insert(workspace.id);
    let evicted = manager
        .evict_idle_workspaces(Duration::from_secs(0), &active)
        .await;
    assert_eq!(evicted, 0);

    let evicted = manager
        .evict_idle_workspaces(Duration::from_secs(0), &HashSet::new())
        .await;
    assert_eq!(evicted, 1);
    assert_eq!(manager.stats().await.workspace_store_count, 0);

    let reopened = manager.workspace(workspace.id).await.unwrap();
    assert!(reopened
        .list_workspaces()
        .await
        .unwrap()
        .iter()
        .any(|candidate| candidate.id == workspace.id));
}

#[tokio::test]
async fn startup_uses_split_layout_without_legacy_root_migration() {
    let temp = tempfile::tempdir().unwrap();
    let manager = StoreManager::open(temp.path()).await.unwrap();
    let split_global = temp.path().join("db").join("db.sqlite");
    assert!(split_global.exists(), "expected split-layout global db");
    drop(manager);

    let legacy_root_db = temp.path().join("db.sqlite");
    fs::write(&legacy_root_db, b"legacy-data").unwrap();
    assert!(legacy_root_db.exists());

    let _manager = StoreManager::open(temp.path()).await.unwrap();
    assert!(
        legacy_root_db.exists(),
        "legacy root db must be ignored, not renamed"
    );
    let legacy_bytes = fs::read(&legacy_root_db).unwrap();
    assert_eq!(legacy_bytes, b"legacy-data");
}

#[tokio::test]
async fn reopening_workspace_store_preserves_workspace_owned_records() {
    let temp = tempfile::tempdir().unwrap();
    let manager = StoreManager::open(temp.path()).await.unwrap();
    let workspace = manager
        .global()
        .create_workspace(
            "ws".to_string(),
            temp.path().to_string_lossy().to_string(),
            VcsKind::Git,
        )
        .await
        .unwrap();
    let store = manager.workspace(workspace.id).await.unwrap();
    let worktree = store
        .create_worktree(
            workspace.id,
            temp.path().to_string_lossy().to_string(),
            "base".to_string(),
            None,
        )
        .await
        .unwrap();
    let task = store
        .create_task(workspace.id, "task".to_string(), None)
        .await
        .unwrap();
    let session = store
        .create_session(
            task.id,
            workspace.id,
            worktree.id,
            ctx_core::models::ExecutionEnvironment::Host,
            "fake".to_string(),
            "fake-model".to_string(),
            "assistant".to_string(),
            None,
            None,
            None,
        )
        .await
        .unwrap();

    let artifact = Artifact {
        id: ArtifactId::new(),
        session_id: session.id,
        task_id: task.id,
        workspace_id: workspace.id,
        worktree_id: worktree.id,
        name: Some("artifact.txt".to_string()),
        absolute_path: temp
            .path()
            .join("artifact.txt")
            .to_string_lossy()
            .to_string(),
        mime_type: "text/plain".to_string(),
        bytes: 4,
        missing: None,
        created_at: Utc::now(),
    };
    std::fs::write(&artifact.absolute_path, "test").unwrap();
    store
        .replace_session_artifacts(session.id, std::slice::from_ref(&artifact))
        .await
        .unwrap();

    let message = store
        .insert_message(Message {
            id: MessageId::new(),
            session_id: session.id,
            task_id: task.id,
            run_id: None,
            turn_id: None,
            turn_sequence: None,
            order_seq: None,
            role: MessageRole::User,
            content: "queued".to_string(),
            attachments: Vec::new(),
            delivery: MessageDelivery::Queued,
            delivered_at: None,
            created_at: Utc::now(),
        })
        .await
        .unwrap();

    let invocation = store
        .upsert_subagent_invocation(SubagentInvocation {
            id: "subagent-test".to_string(),
            tool_call_id: "tool-call".to_string(),
            parent_session_id: session.id,
            parent_turn_id: None,
            requested_count: 1,
            request_json: None,
            status: "running".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            children: Vec::new(),
        })
        .await
        .unwrap();

    let entry = MergeQueueEntry {
        id: MergeQueueEntryId::new(),
        workspace_id: workspace.id,
        worktree_id: Some(worktree.id),
        session_id: Some(session.id),
        target_branch: "main".to_string(),
        message: Some("merge me".to_string()),
        patch_source: MergeQueuePatchSource::Generated,
        base_commit_sha: Some("base".to_string()),
        head_commit_sha: Some("head".to_string()),
        patch_path: temp.path().join("patch.diff").to_string_lossy().to_string(),
        patch_size: 12,
        status: MergeQueueEntryStatus::Queued,
        result_commit_sha: None,
        error_message: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    store.create_merge_queue_entry(&entry).await.unwrap();

    manager.evict_workspace(workspace.id).await;
    let reopened_store = manager.workspace(workspace.id).await.unwrap();

    assert!(reopened_store
        .get_artifact(artifact.id)
        .await
        .unwrap()
        .is_some());

    assert!(reopened_store
        .get_message(message.id)
        .await
        .unwrap()
        .is_some());

    assert!(reopened_store
        .get_subagent_invocation(&invocation.id)
        .await
        .unwrap()
        .is_some());

    assert!(reopened_store
        .get_merge_queue_entry(entry.id)
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn startup_does_not_bootstrap_workspace_dbs() {
    let temp = tempfile::tempdir().unwrap();
    let manager = StoreManager::open(temp.path()).await.unwrap();
    let workspace = manager
        .global()
        .create_workspace(
            "ws".to_string(),
            temp.path().to_string_lossy().to_string(),
            VcsKind::Git,
        )
        .await
        .unwrap();
    let workspace_db_path = temp
        .path()
        .join("db")
        .join("workspaces")
        .join(workspace.id.0.to_string())
        .join("db.sqlite");
    assert!(!workspace_db_path.exists());
    drop(manager);

    let manager = StoreManager::open(temp.path()).await.unwrap();
    assert!(
        !workspace_db_path.exists(),
        "workspace db should not be created until first workspace access"
    );

    let _store = manager.workspace(workspace.id).await.unwrap();
    assert!(workspace_db_path.exists());
}

#[tokio::test]
async fn workspace_open_does_not_import_rows_from_global_db() {
    let temp = tempfile::tempdir().unwrap();
    let manager = StoreManager::open(temp.path()).await.unwrap();
    let workspace = manager
        .global()
        .create_workspace(
            "ws".to_string(),
            temp.path().to_string_lossy().to_string(),
            VcsKind::Git,
        )
        .await
        .unwrap();
    let global_task = manager
        .global()
        .create_task(workspace.id, "legacy-task".to_string(), None)
        .await
        .unwrap();
    assert_eq!(
        manager
            .global()
            .list_tasks(workspace.id)
            .await
            .unwrap()
            .len(),
        1,
        "global db task setup failed"
    );

    let workspace_store = manager.workspace(workspace.id).await.unwrap();
    let workspace_tasks = workspace_store.list_tasks(workspace.id).await.unwrap();
    assert!(
        workspace_tasks.is_empty(),
        "workspace open must not import rows"
    );
    assert!(
        workspace_store
            .get_task(global_task.id)
            .await
            .unwrap()
            .is_none(),
        "global task id must not appear in workspace db"
    );
}

#[tokio::test]
async fn workspace_uncached_does_not_populate_workspace_cache() {
    let temp = tempfile::tempdir().unwrap();
    let manager = StoreManager::open(temp.path()).await.unwrap();
    let workspace = manager
        .global()
        .create_workspace(
            "ws".to_string(),
            temp.path().to_string_lossy().to_string(),
            VcsKind::Git,
        )
        .await
        .unwrap();

    let store = manager.workspace_uncached(workspace.id).await.unwrap();
    assert_eq!(manager.stats().await.workspace_store_count, 0);

    store.close().await;
    assert_eq!(manager.stats().await.workspace_store_count, 0);
}

#[tokio::test]
async fn workspace_access_reports_open_state_and_enforces_cache_cap() {
    let temp = tempfile::tempdir().unwrap();
    let manager = StoreManager::open_with_config(
        temp.path(),
        StoreManagerConfig {
            max_cached_workspaces: 2,
            ..StoreManagerConfig::default()
        },
    )
    .await
    .unwrap();
    let workspace_a = manager
        .global()
        .create_workspace(
            "a".to_string(),
            temp.path().join("a").to_string_lossy().to_string(),
            VcsKind::Git,
        )
        .await
        .unwrap();
    let workspace_b = manager
        .global()
        .create_workspace(
            "b".to_string(),
            temp.path().join("b").to_string_lossy().to_string(),
            VcsKind::Git,
        )
        .await
        .unwrap();
    let workspace_c = manager
        .global()
        .create_workspace(
            "c".to_string(),
            temp.path().join("c").to_string_lossy().to_string(),
            VcsKind::Git,
        )
        .await
        .unwrap();

    assert!(matches!(
        manager.workspace_access(workspace_a.id).await.unwrap().kind,
        WorkspaceStoreAccessKind::ColdOpen
    ));
    assert!(matches!(
        manager.workspace_access(workspace_a.id).await.unwrap().kind,
        WorkspaceStoreAccessKind::Cached
    ));
    assert!(matches!(
        manager.workspace_access(workspace_b.id).await.unwrap().kind,
        WorkspaceStoreAccessKind::ColdOpen
    ));
    assert_eq!(manager.stats().await.workspace_store_count, 2);

    assert!(matches!(
        manager.workspace_access(workspace_c.id).await.unwrap().kind,
        WorkspaceStoreAccessKind::ColdOpen
    ));
    assert_eq!(manager.stats().await.workspace_store_count, 3);

    let evicted = manager
        .evict_workspaces_to_cap(&HashSet::from([workspace_c.id]))
        .await;
    assert_eq!(evicted, 1);
    assert_eq!(manager.stats().await.workspace_store_count, 2);

    assert!(matches!(
        manager.workspace_access(workspace_a.id).await.unwrap().kind,
        WorkspaceStoreAccessKind::ColdOpen
    ));
    assert_eq!(manager.stats().await.workspace_store_count, 3);
}

#[tokio::test]
async fn evict_workspaces_to_cap_preserves_protected_workspaces() {
    let temp = tempfile::tempdir().unwrap();
    let manager = StoreManager::open_with_config(
        temp.path(),
        StoreManagerConfig {
            max_cached_workspaces: 2,
            ..StoreManagerConfig::default()
        },
    )
    .await
    .unwrap();
    let workspace_a = manager
        .global()
        .create_workspace(
            "a".to_string(),
            temp.path().join("a").to_string_lossy().to_string(),
            VcsKind::Git,
        )
        .await
        .unwrap();
    let workspace_b = manager
        .global()
        .create_workspace(
            "b".to_string(),
            temp.path().join("b").to_string_lossy().to_string(),
            VcsKind::Git,
        )
        .await
        .unwrap();
    let workspace_c = manager
        .global()
        .create_workspace(
            "c".to_string(),
            temp.path().join("c").to_string_lossy().to_string(),
            VcsKind::Git,
        )
        .await
        .unwrap();

    let _ = manager.workspace_access(workspace_a.id).await.unwrap();
    let _ = manager.workspace_access(workspace_b.id).await.unwrap();
    let _ = manager.workspace_access(workspace_c.id).await.unwrap();
    assert_eq!(manager.stats().await.workspace_store_count, 3);

    let evicted = manager
        .evict_workspaces_to_cap(&HashSet::from([workspace_a.id]))
        .await;
    assert_eq!(evicted, 1);
    assert_eq!(manager.stats().await.workspace_store_count, 2);
    assert!(matches!(
        manager.workspace_access(workspace_a.id).await.unwrap().kind,
        WorkspaceStoreAccessKind::Cached
    ));
    assert!(matches!(
        manager.workspace_access(workspace_b.id).await.unwrap().kind,
        WorkspaceStoreAccessKind::ColdOpen
    ));
}
