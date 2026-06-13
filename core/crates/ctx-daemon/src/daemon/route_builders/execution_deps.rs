use std::path::PathBuf;
use std::sync::Arc;

use ctx_execution_runtime::ExecutionSetupCoordinator;
use ctx_store::{Store, StoreManager};
use ctx_transport_runtime::terminals::TerminalManager;
use ctx_update_service::UpdateDrainCoordinator;
use ctx_workspace_runtime::HarnessRuntimeManager;

pub(super) struct ExecutionRouteDepsParts {
    pub(super) data_root: PathBuf,
    pub(super) daemon_url: String,
    pub(super) global_store: Store,
    pub(super) stores: StoreManager,
    pub(super) update_drain: Arc<UpdateDrainCoordinator>,
    pub(super) execution_setup: Arc<ExecutionSetupCoordinator>,
    pub(super) harness: Arc<HarnessRuntimeManager>,
    pub(super) terminals: Arc<TerminalManager>,
}

#[derive(Clone)]
pub(super) struct ExecutionRouteDeps {
    pub(super) data_root: PathBuf,
    pub(super) daemon_url: String,
    pub(super) global_store: Store,
    pub(super) stores: StoreManager,
    pub(super) update_drain: Arc<UpdateDrainCoordinator>,
    pub(super) execution_setup: Arc<ExecutionSetupCoordinator>,
    pub(super) harness: Arc<HarnessRuntimeManager>,
    pub(super) terminals: Arc<TerminalManager>,
}

impl ExecutionRouteDeps {
    pub(super) fn new(parts: ExecutionRouteDepsParts) -> Self {
        Self {
            data_root: parts.data_root,
            daemon_url: parts.daemon_url,
            global_store: parts.global_store,
            stores: parts.stores,
            update_drain: parts.update_drain,
            execution_setup: parts.execution_setup,
            harness: parts.harness,
            terminals: parts.terminals,
        }
    }
}
