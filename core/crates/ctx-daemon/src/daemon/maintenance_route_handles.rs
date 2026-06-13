use std::path::{Path, PathBuf};
use std::sync::Arc;

use ctx_store::{Store, StoreManager};
use ctx_update_service::UpdateDrainCoordinator;

use super::DaemonShutdownHost;

#[derive(Clone)]
pub struct UpdateActivityHandle {
    global_store: Store,
    stores: StoreManager,
    update_drain: Arc<UpdateDrainCoordinator>,
    data_root: PathBuf,
}

impl UpdateActivityHandle {
    pub(in crate::daemon) fn new(
        global_store: Store,
        stores: StoreManager,
        update_drain: Arc<UpdateDrainCoordinator>,
        data_root: PathBuf,
    ) -> Self {
        Self {
            global_store,
            stores,
            update_drain,
            data_root,
        }
    }

    pub(in crate::daemon) fn global_store(&self) -> &Store {
        &self.global_store
    }

    pub(in crate::daemon) fn stores(&self) -> &StoreManager {
        &self.stores
    }

    pub(in crate::daemon) fn update_drain(&self) -> &UpdateDrainCoordinator {
        self.update_drain.as_ref()
    }

    pub(in crate::daemon) fn data_root(&self) -> &Path {
        &self.data_root
    }
}

#[derive(Clone)]
pub struct UpdateDrainHandle {
    global_store: Store,
    stores: StoreManager,
    update_drain: Arc<UpdateDrainCoordinator>,
}

impl UpdateDrainHandle {
    pub(in crate::daemon) fn new(
        global_store: Store,
        stores: StoreManager,
        update_drain: Arc<UpdateDrainCoordinator>,
    ) -> Self {
        Self {
            global_store,
            stores,
            update_drain,
        }
    }

    pub(in crate::daemon) fn global_store(&self) -> &Store {
        &self.global_store
    }

    pub(in crate::daemon) fn stores(&self) -> &StoreManager {
        &self.stores
    }

    pub(in crate::daemon) fn update_drain(&self) -> Arc<UpdateDrainCoordinator> {
        Arc::clone(&self.update_drain)
    }
}

#[derive(Clone)]
pub struct DaemonShutdownHandle {
    local_shutdown_token: Option<String>,
    shutdown_host: DaemonShutdownHost,
}

impl DaemonShutdownHandle {
    pub(in crate::daemon) fn new(
        local_shutdown_token: Option<String>,
        shutdown_host: DaemonShutdownHost,
    ) -> Self {
        Self {
            local_shutdown_token,
            shutdown_host,
        }
    }

    pub(in crate::daemon) fn local_shutdown_token_authorized(
        &self,
        supplied: Option<&str>,
    ) -> bool {
        let Some(expected) = self.local_shutdown_token.as_deref() else {
            return false;
        };
        supplied.is_some_and(|value| value == expected)
    }

    pub(in crate::daemon) async fn request_shutdown(
        &self,
        reason: String,
    ) -> Result<
        crate::daemon::DaemonTurnActivitySummary,
        crate::daemon::maintenance::DaemonShutdownError,
    > {
        crate::daemon::maintenance::request_daemon_shutdown(&self.shutdown_host, reason).await
    }
}
