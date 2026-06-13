impl WorkspaceStoreLeaseRegistry {
    pub(super) fn new() -> Result<Self> {
        Ok(Self {
            state: StdMutex::new(WorkspaceStoreLeaseState::default()),
            close_executor: StoreCloseExecutor::new()?,
        })
    }

    pub(super) fn acquire(
        self: &Arc<Self>,
        workspace_id: WorkspaceId,
        instance_id: u64,
    ) -> Arc<dyn crate::store::StoreLeaseGuard> {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        let entry = state
            .entries
            .entry(instance_id)
            .or_insert_with(|| WorkspaceStoreLeaseEntry {
                workspace_id,
                in_use: 0,
                pending_close: None,
            });
        entry.workspace_id = workspace_id;
        entry.in_use += 1;
        Arc::new(WorkspaceStoreLease {
            instance_id,
            registry: Arc::clone(self),
        })
    }

    pub(super) async fn wait_for_workspace_close(&self, workspace_id: WorkspaceId) {
        loop {
            let maybe_signal = {
                let state = match self.state.lock() {
                    Ok(state) => state,
                    Err(poisoned) => poisoned.into_inner(),
                };
                state.closing_workspaces.get(&workspace_id).cloned()
            };
            match maybe_signal {
                Some(signal) => {
                    signal.wait().await;
                }
                None => return,
            }
        }
    }

    pub(super) fn is_workspace_closing(&self, workspace_id: WorkspaceId) -> bool {
        let state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        state.closing_workspaces.contains_key(&workspace_id)
    }

    pub(super) fn has_pending_close_store(&self, workspace_id: WorkspaceId) -> bool {
        let state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        state
            .entries
            .values()
            .any(|entry| entry.workspace_id == workspace_id && entry.pending_close.is_some())
    }

    pub(super) fn reactivate_pending_close_store(
        &self,
        workspace_id: WorkspaceId,
    ) -> Option<ReactivatedWorkspaceStore> {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        let instance_id = state.entries.iter().find_map(|(instance_id, entry)| {
            (entry.workspace_id == workspace_id && entry.pending_close.is_some())
                .then_some(*instance_id)
        })?;
        let store = state.entries.get_mut(&instance_id)?.pending_close.take()?;
        let notify = state.closing_workspaces.get(&workspace_id)?.clone();
        Some(ReactivatedWorkspaceStore {
            store,
            instance_id,
            notify,
        })
    }

    pub(super) fn publish_reactivated_store(
        &self,
        workspace_id: WorkspaceId,
        notify: &Arc<WorkspaceCloseSignal>,
    ) {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        clear_closing_marker(&mut state, workspace_id, notify);
    }

    pub(super) fn restore_pending_close_store(
        &self,
        workspace_id: WorkspaceId,
        instance_id: u64,
        store: Store,
    ) {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        let entry = state
            .entries
            .entry(instance_id)
            .or_insert_with(|| WorkspaceStoreLeaseEntry {
                workspace_id,
                in_use: 0,
                pending_close: None,
            });
        entry.workspace_id = workspace_id;
        entry.pending_close = Some(store);
        let _ = closing_notify(&mut state, workspace_id);
    }

    pub(super) fn queue_close(
        &self,
        workspace_id: WorkspaceId,
        instance_id: u64,
        store: Store,
    ) -> Option<PendingWorkspaceStoreClose> {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        let close_now = state
            .entries
            .get(&instance_id)
            .map(|entry| entry.in_use == 0)
            .unwrap_or(true);
        if close_now {
            state.entries.remove(&instance_id);
            Some(PendingWorkspaceStoreClose {
                workspace_id,
                store,
                notify: closing_notify(&mut state, workspace_id),
            })
        } else {
            let entry =
                state
                    .entries
                    .entry(instance_id)
                    .or_insert_with(|| WorkspaceStoreLeaseEntry {
                        workspace_id,
                        in_use: 0,
                        pending_close: None,
                    });
            entry.workspace_id = workspace_id;
            entry.pending_close = Some(store);
            let _ = closing_notify(&mut state, workspace_id);
            None
        }
    }

    fn release(&self, instance_id: u64) -> Option<PendingWorkspaceStoreClose> {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        let (should_remove, workspace_id, pending_close) = {
            let entry = state.entries.get_mut(&instance_id)?;
            entry.in_use = entry.in_use.saturating_sub(1);
            if entry.in_use == 0 {
                (true, entry.workspace_id, entry.pending_close.take())
            } else {
                (false, entry.workspace_id, None)
            }
        };
        if should_remove {
            state.entries.remove(&instance_id);
            pending_close.map(|store| PendingWorkspaceStoreClose {
                workspace_id,
                store,
                notify: closing_notify(&mut state, workspace_id),
            })
        } else {
            None
        }
    }

    pub(super) fn finish_close(
        &self,
        workspace_id: WorkspaceId,
        notify: &Arc<WorkspaceCloseSignal>,
    ) {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        clear_closing_marker(&mut state, workspace_id, notify);
    }

    pub(super) fn start_close(self: &Arc<Self>, close: PendingWorkspaceStoreClose) {
        spawn_store_close(
            close.store,
            Arc::clone(self),
            close.workspace_id,
            close.notify,
        );
    }
}
