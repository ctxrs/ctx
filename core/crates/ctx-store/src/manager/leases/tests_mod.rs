mod tests {
    use super::*;

    use std::time::Duration;

    const CLOSE_COMPLETION_TIMEOUT: Duration = Duration::from_secs(30);
    const CLOSE_PENDING_PROBE_TIMEOUT: Duration = Duration::from_millis(20);

    async fn open_test_store(temp: &tempfile::TempDir, name: &str) -> Store {
        let path = temp.path().join(name);
        Store::open_sqlite(&path, Some(1)).await.unwrap()
    }

    #[tokio::test]
    async fn immediate_close_registers_workspace_as_closing() {
        let _serial = crate::manager::close_lifecycle_test_lock()
            .clone()
            .lock_owned()
            .await;
        let temp = tempfile::tempdir().unwrap();
        let registry = Arc::new(WorkspaceStoreLeaseRegistry::new().unwrap());
        let workspace_id = WorkspaceId::new();
        let store = open_test_store(&temp, "immediate-close.sqlite").await;

        let close = registry
            .queue_close(workspace_id, 1, store)
            .expect("close without leases should start immediately");

        let blocked = tokio::time::timeout(
            CLOSE_PENDING_PROBE_TIMEOUT,
            registry.wait_for_workspace_close(workspace_id),
        )
        .await;
        assert!(
            blocked.is_err(),
            "waiters must observe an in-progress immediate close"
        );

        registry.start_close(close);

        tokio::time::timeout(
            CLOSE_COMPLETION_TIMEOUT,
            registry.wait_for_workspace_close(workspace_id),
        )
        .await
        .expect("waiter should be released after close completes");
    }

    #[tokio::test]
    async fn waiters_do_not_miss_close_notifications() {
        let _serial = crate::manager::close_lifecycle_test_lock()
            .clone()
            .lock_owned()
            .await;
        let temp = tempfile::tempdir().unwrap();

        for idx in 0..64 {
            let registry = Arc::new(WorkspaceStoreLeaseRegistry::new().unwrap());
            let workspace_id = WorkspaceId::new();
            let store = open_test_store(&temp, &format!("close-race-{idx}.sqlite")).await;
            let close = registry
                .queue_close(workspace_id, idx + 1, store)
                .expect("close without leases should start immediately");
            let signal = {
                let state = match registry.state.lock() {
                    Ok(state) => state,
                    Err(poisoned) => poisoned.into_inner(),
                };
                state
                    .closing_workspaces
                    .get(&workspace_id)
                    .cloned()
                    .expect("queue_close should register a closing signal")
            };
            let waiter = signal.wait();
            tokio::pin!(waiter);

            let blocked = tokio::time::timeout(CLOSE_PENDING_PROBE_TIMEOUT, &mut waiter).await;
            assert!(
                blocked.is_err(),
                "waiter should remain pending until close completion"
            );

            close.store.close().await;
            registry.finish_close(workspace_id, &close.notify);

            let waiter_result = tokio::time::timeout(CLOSE_COMPLETION_TIMEOUT, waiter).await;
            assert!(
                waiter_result.is_ok(),
                "waiter should not miss the close notification (idx={idx}, closing={}, pending_close={})",
                registry.is_workspace_closing(workspace_id),
                registry.has_pending_close_store(workspace_id),
            );
        }
    }

    #[tokio::test]
    async fn pending_close_marks_workspace_as_closing_before_last_lease_drops() {
        let _serial = crate::manager::close_lifecycle_test_lock()
            .clone()
            .lock_owned()
            .await;
        let temp = tempfile::tempdir().unwrap();
        let registry = Arc::new(WorkspaceStoreLeaseRegistry::new().unwrap());
        let workspace_id = WorkspaceId::new();
        let lease = registry.acquire(workspace_id, 7);
        let store = open_test_store(&temp, "pending-close.sqlite").await;

        assert!(
            registry.queue_close(workspace_id, 7, store).is_none(),
            "active lease should defer the close"
        );

        let blocked = tokio::time::timeout(
            CLOSE_PENDING_PROBE_TIMEOUT,
            registry.wait_for_workspace_close(workspace_id),
        )
        .await;
        assert!(
            blocked.is_err(),
            "reopens must wait even while the last lease is still draining"
        );

        drop(lease);

        tokio::time::timeout(
            CLOSE_COMPLETION_TIMEOUT,
            registry.wait_for_workspace_close(workspace_id),
        )
        .await
        .expect("waiter should be released once deferred close completes");
    }

    #[tokio::test]
    async fn pending_close_store_can_be_reacquired_without_deadlock() {
        let _serial = crate::manager::close_lifecycle_test_lock()
            .clone()
            .lock_owned()
            .await;
        let temp = tempfile::tempdir().unwrap();
        let registry = Arc::new(WorkspaceStoreLeaseRegistry::new().unwrap());
        let workspace_id = WorkspaceId::new();
        let lease = registry.acquire(workspace_id, 9);
        let store = open_test_store(&temp, "pending-reacquire.sqlite").await;

        assert!(
            registry.queue_close(workspace_id, 9, store).is_none(),
            "active lease should defer the close"
        );

        let reopened = registry
            .reactivate_pending_close_store(workspace_id)
            .expect("deferred close should reactivate the draining store");
        assert!(
            registry.is_workspace_closing(workspace_id),
            "reactivation should keep the closing marker until publication"
        );
        registry.publish_reactivated_store(workspace_id, &reopened.notify);
        drop(reopened.store);
        drop(lease);

        assert!(
            !registry.is_workspace_closing(workspace_id),
            "publishing the reactivated store should cancel the deferred close"
        );
    }

    #[test]
    fn close_without_runtime_completes_inline() {
        let _serial = crate::manager::close_lifecycle_test_lock().blocking_lock();
        let temp = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let store = runtime.block_on(open_test_store(&temp, "inline-close.sqlite"));
        drop(runtime);

        let registry = Arc::new(WorkspaceStoreLeaseRegistry::new().unwrap());
        let workspace_id = WorkspaceId::new();
        let close = registry
            .queue_close(workspace_id, 11, store)
            .expect("close without leases should start immediately");

        spawn_store_close(
            close.store,
            Arc::clone(&registry),
            close.workspace_id,
            Arc::clone(&close.notify),
        );

        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            tokio::time::timeout(
                CLOSE_COMPLETION_TIMEOUT,
                registry.wait_for_workspace_close(workspace_id),
            )
            .await
            .expect("inline close should finish without a background runtime");
        });

        assert!(
            !registry.is_workspace_closing(workspace_id),
            "inline close should clear the closing marker once shutdown completes"
        );
    }

    #[test]
    fn close_without_runtime_executor_path_clears_closing_marker() {
        let _serial = crate::manager::close_lifecycle_test_lock().blocking_lock();
        let temp = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let store = runtime.block_on(open_test_store(&temp, "inline-close-executor.sqlite"));
        drop(runtime);

        let registry = Arc::new(WorkspaceStoreLeaseRegistry::new().unwrap());
        let workspace_id = WorkspaceId::new();
        let close = registry
            .queue_close(workspace_id, 12, store)
            .expect("close without leases should start immediately");

        spawn_store_close(
            close.store,
            Arc::clone(&registry),
            close.workspace_id,
            Arc::clone(&close.notify),
        );

        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            tokio::time::timeout(
                CLOSE_COMPLETION_TIMEOUT,
                registry.wait_for_workspace_close(workspace_id),
            )
            .await
            .expect("executor-backed close should still release the closing marker");
        });

        assert!(
            !registry.is_workspace_closing(workspace_id),
            "executor-backed close should clear the closing marker after dropping the final store handle"
        );
    }
}
