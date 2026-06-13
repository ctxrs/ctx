use super::*;

use anyhow::{Context, Result};
use chrono::Utc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use ctx_core::ids::TurnId;
use ctx_core::models::{
    ExecutionEnvironment, SessionEventType, SessionTurn, SessionTurnStatus, VcsKind,
};

const CLOSE_COMPLETION_TIMEOUT: Duration = Duration::from_secs(30);

#[tokio::test]
async fn evicted_workspace_clone_remains_usable_until_last_handle_drops() -> Result<()> {
    let _serial = crate::manager::close_lifecycle_test_lock()
        .clone()
        .lock_owned()
        .await;
    let temp = tempfile::tempdir()?;
    let manager = StoreManager::open_with_config(
        temp.path(),
        StoreManagerConfig {
            max_cached_workspaces: 1,
            ..StoreManagerConfig::default()
        },
    )
    .await?;
    let workspace_a = manager
        .global()
        .create_workspace(
            "a".to_string(),
            temp.path().join("a").to_string_lossy().to_string(),
            VcsKind::Git,
        )
        .await?;
    let workspace_b = manager
        .global()
        .create_workspace(
            "b".to_string(),
            temp.path().join("b").to_string_lossy().to_string(),
            VcsKind::Git,
        )
        .await?;

    let store_a = manager.workspace(workspace_a.id).await?;
    let worktree_a = store_a
        .create_worktree(
            workspace_a.id,
            temp.path().join("a").to_string_lossy().to_string(),
            "base".to_string(),
            None,
        )
        .await?;
    let task_a = store_a
        .create_task(workspace_a.id, "task".to_string(), None)
        .await?;
    let session_a = store_a
        .create_session(
            task_a.id,
            workspace_a.id,
            worktree_a.id,
            ExecutionEnvironment::Host,
            "fake".to_string(),
            "fake".to_string(),
            "implementer".to_string(),
            None,
            None,
            None,
        )
        .await?;
    store_a
        .set_task_primary_session(task_a.id, session_a.id, worktree_a.id)
        .await?;
    let turn_id = TurnId::new();
    let now = Utc::now();
    store_a
        .insert_session_turn(SessionTurn {
            turn_id,
            session_id: session_a.id,
            run_id: None,
            user_message_id: None,
            status: SessionTurnStatus::Running,
            start_seq: Some(1),
            end_seq: None,
            started_at: now,
            updated_at: now,
            assistant_partial: None,
            thought_partial: None,
            metrics_json: None,
            failure: None,
            tool_total: 0,
            tool_pending: 0,
            tool_running: 0,
            tool_completed: 0,
            tool_failed: 0,
        })
        .await?;
    let _ = store_a
        .append_session_event(
            session_a.id,
            None,
            Some(turn_id),
            SessionEventType::Notice,
            serde_json::json!({"msg":"before evict"}),
        )
        .await?;
    let _ = manager.workspace(workspace_b.id).await?;
    let evicted = manager
        .evict_workspaces_to_cap(&HashSet::from([workspace_b.id]))
        .await;
    assert_eq!(evicted, 1);
    assert_eq!(manager.stats().await.workspace_store_count, 1);

    let reopened_before_drop = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        manager.workspace_access(workspace_a.id),
    )
    .await
    .context("reopen should reuse the draining store, not deadlock")??;
    assert_eq!(
        reopened_before_drop.kind,
        WorkspaceStoreAccessKind::Reactivated
    );
    assert!(reopened_before_drop
        .store
        .get_task(task_a.id)
        .await?
        .is_some());
    drop(reopened_before_drop);

    let task = store_a
        .create_task(workspace_a.id, "still-live".to_string(), None)
        .await?;
    assert_eq!(task.workspace_id, workspace_a.id);

    drop(store_a);

    assert!(matches!(
        manager.workspace_access(workspace_a.id).await?.kind,
        WorkspaceStoreAccessKind::Cached
    ));
    Ok(())
}

#[tokio::test]
async fn deleted_workspace_is_not_rehydrated_from_pending_close_store() -> Result<()> {
    let _serial = crate::manager::close_lifecycle_test_lock()
        .clone()
        .lock_owned()
        .await;
    let temp = tempfile::tempdir()?;
    let manager = StoreManager::open_with_config(
        temp.path(),
        StoreManagerConfig {
            max_cached_workspaces: 1,
            ..StoreManagerConfig::default()
        },
    )
    .await?;
    let workspace = manager
        .global()
        .create_workspace(
            "a".to_string(),
            temp.path().join("a").to_string_lossy().to_string(),
            VcsKind::Git,
        )
        .await?;
    let store = manager.workspace(workspace.id).await?;

    manager.evict_workspace(workspace.id).await;
    manager.global().delete_workspace(workspace.id).await?;

    let manager_for_reopen = manager.clone();
    let reopen =
        tokio::spawn(async move { manager_for_reopen.workspace_access(workspace.id).await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(
        !reopen.is_finished(),
        "reopen should wait for the draining store to finish closing"
    );

    drop(store);

    let err = match reopen.await.context("reopen task should join")? {
        Ok(_) => panic!("deleted workspace should not reopen from a draining store"),
        Err(err) => err.to_string(),
    };
    assert!(
        err.contains("not found"),
        "expected missing workspace error after delete, got: {err}"
    );
    Ok(())
}

#[tokio::test]
async fn delete_barrier_blocks_cached_workspace_access() -> Result<()> {
    let _serial = crate::manager::close_lifecycle_test_lock()
        .clone()
        .lock_owned()
        .await;
    let temp = tempfile::tempdir()?;
    let manager = StoreManager::open_with_config(
        temp.path(),
        StoreManagerConfig {
            max_cached_workspaces: 1,
            ..StoreManagerConfig::default()
        },
    )
    .await?;
    let workspace = manager
        .global()
        .create_workspace(
            "a".to_string(),
            temp.path().join("a").to_string_lossy().to_string(),
            VcsKind::Git,
        )
        .await?;
    let store = manager.workspace(workspace.id).await?;

    manager.begin_workspace_delete(workspace.id).await;
    let err = match manager.workspace_access(workspace.id).await {
        Ok(_) => panic!("delete barrier should block cached workspace access"),
        Err(err) => err.to_string(),
    };
    assert!(
        err.contains("not found"),
        "delete barrier should block cached workspace access, got: {err}"
    );

    manager.finish_workspace_delete(workspace.id).await;
    drop(store);
    Ok(())
}

#[tokio::test]
async fn evict_workspace_and_wait_closed_blocks_until_last_handle_drops() -> Result<()> {
    let _serial = crate::manager::close_lifecycle_test_lock()
        .clone()
        .lock_owned()
        .await;
    let temp = tempfile::tempdir()?;
    let manager = StoreManager::open_with_config(
        temp.path(),
        StoreManagerConfig {
            max_cached_workspaces: 1,
            ..StoreManagerConfig::default()
        },
    )
    .await?;
    let workspace = manager
        .global()
        .create_workspace(
            "wait-close".to_string(),
            temp.path().join("wait-close").to_string_lossy().to_string(),
            VcsKind::Git,
        )
        .await?;
    let store = manager.workspace(workspace.id).await?;

    let manager_for_wait = manager.clone();
    let wait = tokio::spawn(async move {
        manager_for_wait
            .evict_workspace_and_wait_closed(workspace.id)
            .await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !wait.is_finished(),
        "eviction should wait while a leased workspace store is still in use"
    );

    drop(store);

    tokio::time::timeout(CLOSE_COMPLETION_TIMEOUT, wait)
        .await
        .context("eviction should finish once the last store handle is dropped")?
        .context("eviction task should join")?;
    assert_eq!(manager.stats().await.workspace_store_count, 0);
    Ok(())
}

#[tokio::test]
async fn delete_barrier_blocks_pending_close_reactivation() -> Result<()> {
    let _serial = crate::manager::close_lifecycle_test_lock()
        .clone()
        .lock_owned()
        .await;
    let temp = tempfile::tempdir()?;
    let manager = StoreManager::open_with_config(
        temp.path(),
        StoreManagerConfig {
            max_cached_workspaces: 1,
            ..StoreManagerConfig::default()
        },
    )
    .await?;
    let workspace = manager
        .global()
        .create_workspace(
            "a".to_string(),
            temp.path().join("a").to_string_lossy().to_string(),
            VcsKind::Git,
        )
        .await?;
    let store = manager.workspace(workspace.id).await?;

    manager.evict_workspace(workspace.id).await;
    manager.begin_workspace_delete(workspace.id).await;

    let err = tokio::time::timeout(
        Duration::from_secs(1),
        manager.workspace_access(workspace.id),
    )
    .await
    .context("delete barrier should reject reactivation promptly")?
    .err()
    .context("delete barrier should reject pending-close reactivation")?
    .to_string();
    assert!(
        err.contains("not found"),
        "delete barrier should block pending-close reactivation, got: {err}"
    );

    manager.finish_workspace_delete(workspace.id).await;
    drop(store);
    tokio::time::timeout(
        CLOSE_COMPLETION_TIMEOUT,
        manager.store_leases.wait_for_workspace_close(workspace.id),
    )
    .await
    .context("draining store should still finish closing after barrier rejection")?;
    Ok(())
}

#[tokio::test]
async fn transient_workspace_access_uses_tracked_delete_lifecycle() -> Result<()> {
    let _serial = crate::manager::close_lifecycle_test_lock()
        .clone()
        .lock_owned()
        .await;
    let temp = tempfile::tempdir()?;
    let manager = StoreManager::open_with_config(
        temp.path(),
        StoreManagerConfig {
            max_cached_workspaces: 1,
            ..StoreManagerConfig::default()
        },
    )
    .await?;
    let workspace = manager
        .global()
        .create_workspace(
            "transient".to_string(),
            temp.path().join("transient").to_string_lossy().to_string(),
            VcsKind::Git,
        )
        .await?;

    let store = manager.workspace_transient(workspace.id).await?;
    assert_eq!(
        manager.stats().await.workspace_store_count,
        0,
        "transient access should not leave a cached workspace entry behind"
    );

    manager.begin_workspace_delete(workspace.id).await;
    let err = manager
        .workspace_access(workspace.id)
        .await
        .err()
        .context("delete barrier should reject tracked access while transient handle is live")?
        .to_string();
    assert!(err.contains("not found"));

    drop(store);
    tokio::time::timeout(
        CLOSE_COMPLETION_TIMEOUT,
        manager.store_leases.wait_for_workspace_close(workspace.id),
    )
    .await
    .context("transient tracked handle should finish closing once dropped")?;
    manager.finish_workspace_delete(workspace.id).await;
    Ok(())
}

#[tokio::test]
async fn concurrent_reactivation_does_not_cold_open_duplicate_store() -> Result<()> {
    let _serial = crate::manager::close_lifecycle_test_lock()
        .clone()
        .lock_owned()
        .await;
    let temp = tempfile::tempdir()?;
    let manager = StoreManager::open_with_config(
        temp.path(),
        StoreManagerConfig {
            max_cached_workspaces: 1,
            ..StoreManagerConfig::default()
        },
    )
    .await?;
    let workspace_a = manager
        .global()
        .create_workspace(
            "a".to_string(),
            temp.path().join("a").to_string_lossy().to_string(),
            VcsKind::Git,
        )
        .await?;
    let workspace_b = manager
        .global()
        .create_workspace(
            "b".to_string(),
            temp.path().join("b").to_string_lossy().to_string(),
            VcsKind::Git,
        )
        .await?;

    let store_a = manager.workspace(workspace_a.id).await?;
    let _ = manager.workspace(workspace_b.id).await?;
    let next_before = manager.next_store_instance_id.load(Ordering::Relaxed);

    let evicted = manager
        .evict_workspaces_to_cap(&HashSet::from([workspace_b.id]))
        .await;
    assert_eq!(evicted, 1);

    let manager_for_first = manager.clone();
    let manager_for_second = manager.clone();
    let first =
        tokio::spawn(async move { manager_for_first.workspace_access(workspace_a.id).await });
    let second =
        tokio::spawn(async move { manager_for_second.workspace_access(workspace_a.id).await });

    let first = tokio::time::timeout(Duration::from_secs(1), first)
        .await
        .context("first reactivation should not deadlock")???;
    let second = tokio::time::timeout(Duration::from_secs(1), second)
        .await
        .context("second reactivation should not deadlock")???;

    assert!(matches!(
        first.kind,
        WorkspaceStoreAccessKind::Reactivated | WorkspaceStoreAccessKind::Cached
    ));
    assert!(matches!(
        second.kind,
        WorkspaceStoreAccessKind::Reactivated | WorkspaceStoreAccessKind::Cached
    ));
    assert_eq!(
        manager.next_store_instance_id.load(Ordering::Relaxed),
        next_before,
        "reactivation should not cold-open a duplicate store instance",
    );

    drop(first);
    drop(second);
    drop(store_a);
    Ok(())
}
