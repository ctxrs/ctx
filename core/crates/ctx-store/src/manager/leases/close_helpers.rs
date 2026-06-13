fn closing_notify(
    state: &mut WorkspaceStoreLeaseState,
    workspace_id: WorkspaceId,
) -> Arc<WorkspaceCloseSignal> {
    state
        .closing_workspaces
        .entry(workspace_id)
        .or_insert_with(|| Arc::new(WorkspaceCloseSignal::new()))
        .clone()
}

fn clear_closing_marker(
    state: &mut WorkspaceStoreLeaseState,
    workspace_id: WorkspaceId,
    notify: &Arc<WorkspaceCloseSignal>,
) {
    if state
        .closing_workspaces
        .get(&workspace_id)
        .is_some_and(|current| Arc::ptr_eq(current, notify))
    {
        state.closing_workspaces.remove(&workspace_id);
    }
    notify.finish();
}

impl Drop for WorkspaceStoreLease {
    fn drop(&mut self) {
        if let Some(close) = self.registry.release(self.instance_id) {
            spawn_store_close(
                close.store,
                Arc::clone(&self.registry),
                close.workspace_id,
                close.notify,
            );
        }
    }
}

fn spawn_store_close(
    store: Store,
    registry: Arc<WorkspaceStoreLeaseRegistry>,
    workspace_id: WorkspaceId,
    notify: Arc<WorkspaceCloseSignal>,
) {
    if let Err(err) = registry.close_executor.submit(StoreCloseJob {
        store: store.clone(),
        registry: Arc::clone(&registry),
        workspace_id,
        notify: Arc::clone(&notify),
    }) {
        tracing::warn!(
            workspace_id = %workspace_id.0,
            "failed to submit workspace close to executor: {err:#}"
        );
        store.close_blocking();
        registry.finish_close(workspace_id, &notify);
    }
}

async fn close_store_and_finish(
    store: Store,
    registry: Arc<WorkspaceStoreLeaseRegistry>,
    workspace_id: WorkspaceId,
    notify: Arc<WorkspaceCloseSignal>,
) {
    store.close().await;
    registry.finish_close(workspace_id, &notify);
}
