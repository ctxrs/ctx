use std::path::PathBuf;
use std::sync::Arc;

use ctx_observability::ops_events::OpsEvents;
use ctx_provider_runtime::ProviderRuntime;
use ctx_workspace_runtime::HarnessRuntimeManager;
use tokio::sync::broadcast;

use crate::daemon::plugins::PluginInventoryRuntime;
use crate::daemon::state::ProtectedWorkspaceStoreLookup;
use crate::daemon::{
    ProviderAccountsHandle, ProviderAdminHandle, ProviderAuthImportHandle, ProviderBootstrapHandle,
    ProviderHarnessConfigHandle, ProviderInstallHandle, ProviderOptionsHandle,
    ProviderStatusHandle, ProviderUsageHandle, ProviderWorkspaceAuthHandle,
    ProviderWorkspaceLaunchRuntime,
};

#[derive(Clone)]
pub(super) struct ProviderRouteDeps {
    data_root: PathBuf,
    daemon_url: String,
    workspace_stores: ProtectedWorkspaceStoreLookup,
    providers: Arc<ProviderRuntime>,
    plugins: Arc<PluginInventoryRuntime>,
    ops_events: OpsEvents,
    shutdown_tx: broadcast::Sender<()>,
    workspace_launch_runtime: Arc<ProviderWorkspaceLaunchRuntime>,
}

pub(super) struct ProviderRouteDepsParts {
    pub(super) data_root: PathBuf,
    pub(super) daemon_url: String,
    pub(super) auth_token: Option<String>,
    pub(super) workspace_stores: ProtectedWorkspaceStoreLookup,
    pub(super) providers: Arc<ProviderRuntime>,
    pub(super) plugins: Arc<PluginInventoryRuntime>,
    pub(super) ops_events: OpsEvents,
    pub(super) shutdown_tx: broadcast::Sender<()>,
    pub(super) harness: Arc<HarnessRuntimeManager>,
}

impl ProviderRouteDeps {
    pub(super) fn new(parts: ProviderRouteDepsParts) -> Self {
        let workspace_launch_runtime = Arc::new(ProviderWorkspaceLaunchRuntime::new(
            parts.data_root.clone(),
            parts.daemon_url.clone(),
            parts.auth_token,
            parts.workspace_stores.clone(),
            Arc::clone(&parts.providers),
            Arc::clone(&parts.plugins),
            parts.ops_events.clone(),
            parts.harness,
        ));
        Self {
            data_root: parts.data_root,
            daemon_url: parts.daemon_url,
            workspace_stores: parts.workspace_stores,
            providers: parts.providers,
            plugins: parts.plugins,
            ops_events: parts.ops_events,
            shutdown_tx: parts.shutdown_tx,
            workspace_launch_runtime,
        }
    }

    pub(super) fn provider_accounts(&self) -> ProviderAccountsHandle {
        ProviderAccountsHandle::new(
            self.data_root.clone(),
            self.daemon_url.clone(),
            Arc::clone(&self.providers),
        )
    }

    pub(super) fn provider_bootstrap(&self) -> ProviderBootstrapHandle {
        ProviderBootstrapHandle::new(
            self.data_root.clone(),
            self.workspace_stores.clone(),
            Arc::clone(&self.providers),
            Arc::clone(&self.plugins),
            self.ops_events.clone(),
        )
    }

    pub(super) fn provider_workspace_launch_runtime(&self) -> Arc<ProviderWorkspaceLaunchRuntime> {
        Arc::clone(&self.workspace_launch_runtime)
    }

    pub(super) fn provider_options(&self) -> ProviderOptionsHandle {
        ProviderOptionsHandle::new(self.provider_workspace_launch_runtime())
    }

    pub(super) fn provider_workspace_auth(&self) -> ProviderWorkspaceAuthHandle {
        ProviderWorkspaceAuthHandle::new(self.provider_workspace_launch_runtime())
    }

    pub(super) fn provider_status(&self) -> ProviderStatusHandle {
        ProviderStatusHandle::new(
            self.data_root.clone(),
            Arc::clone(&self.providers),
            Arc::clone(&self.plugins),
            self.ops_events.clone(),
        )
    }

    pub(super) fn provider_admin(&self) -> ProviderAdminHandle {
        ProviderAdminHandle::new(
            self.data_root.clone(),
            Arc::clone(&self.providers),
            Arc::clone(&self.plugins),
            self.ops_events.clone(),
        )
    }

    pub(super) fn provider_install(&self) -> ProviderInstallHandle {
        ProviderInstallHandle::new(
            self.data_root.clone(),
            Arc::clone(&self.providers),
            self.ops_events.clone(),
        )
    }

    pub(super) fn provider_auth_import(&self) -> ProviderAuthImportHandle {
        ProviderAuthImportHandle::new(self.data_root.clone(), Arc::clone(&self.providers))
    }

    pub(super) fn provider_usage(&self) -> ProviderUsageHandle {
        ProviderUsageHandle::new(
            self.data_root.clone(),
            Arc::clone(&self.providers),
            self.shutdown_tx.clone(),
        )
    }

    pub(super) fn provider_harness_config(&self) -> ProviderHarnessConfigHandle {
        ProviderHarnessConfigHandle::new(self.data_root.clone(), Arc::clone(&self.providers))
    }
}
