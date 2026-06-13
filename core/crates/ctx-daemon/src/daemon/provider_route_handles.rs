use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use ctx_core::ids::WorkspaceId;
use ctx_observability::ops_events::OpsEvents;
use ctx_provider_install::install_state::{
    InstallId, InstallInfo, InstallProgressEvent, InstallTarget,
};
use ctx_provider_runtime::ProviderRuntime;
use ctx_store::Store;
use tokio::sync::broadcast;

use super::{state::ProtectedWorkspaceStoreLookup, ProviderWorkspaceLaunchRuntime};

#[derive(Clone)]
pub struct ProviderAccountsHandle {
    data_root: PathBuf,
    daemon_url: String,
    providers: Arc<ProviderRuntime>,
}

impl ProviderAccountsHandle {
    pub(in crate::daemon) fn new(
        data_root: PathBuf,
        daemon_url: String,
        providers: Arc<ProviderRuntime>,
    ) -> Self {
        Self {
            data_root,
            daemon_url,
            providers,
        }
    }

    pub(in crate::daemon) fn data_root(&self) -> &Path {
        &self.data_root
    }

    pub(in crate::daemon) fn daemon_url(&self) -> &str {
        &self.daemon_url
    }

    pub(in crate::daemon) fn providers(&self) -> &ProviderRuntime {
        self.providers.as_ref()
    }

    pub(in crate::daemon) fn providers_arc(&self) -> Arc<ProviderRuntime> {
        Arc::clone(&self.providers)
    }
}

#[derive(Clone)]
pub struct ProviderBootstrapHandle {
    data_root: PathBuf,
    workspace_stores: ProtectedWorkspaceStoreLookup,
    providers: Arc<ProviderRuntime>,
    ops_events: OpsEvents,
}

impl ProviderBootstrapHandle {
    pub(in crate::daemon) fn new(
        data_root: PathBuf,
        workspace_stores: ProtectedWorkspaceStoreLookup,
        providers: Arc<ProviderRuntime>,
        ops_events: OpsEvents,
    ) -> Self {
        Self {
            data_root,
            workspace_stores,
            providers,
            ops_events,
        }
    }

    pub(in crate::daemon) fn data_root(&self) -> &Path {
        &self.data_root
    }

    pub(in crate::daemon) fn providers(&self) -> &ProviderRuntime {
        self.providers.as_ref()
    }

    pub(in crate::daemon) fn ops_events(&self) -> &OpsEvents {
        &self.ops_events
    }

    pub(in crate::daemon) fn global_store(&self) -> &Store {
        self.workspace_stores.global_store()
    }

    pub(in crate::daemon) async fn store_for_workspace(
        &self,
        workspace_id: WorkspaceId,
    ) -> anyhow::Result<Store> {
        self.workspace_stores
            .store_for_workspace(workspace_id)
            .await
    }

    pub(in crate::daemon) async fn install_target_for_workspace(
        &self,
        workspace_id: WorkspaceId,
    ) -> anyhow::Result<InstallTarget> {
        let store = self.store_for_workspace(workspace_id).await?;
        let effective =
            ctx_settings_service::effective_execution_settings(self.global_store(), &store)
                .await
                .with_context(|| {
                    format!(
                        "loading execution settings for workspace {}",
                        workspace_id.0
                    )
                })?;
        Ok(ctx_settings_service::install_target_for_settings(
            &effective,
        ))
    }
}

#[derive(Clone)]
pub struct ProviderOptionsHandle {
    launch: Arc<ProviderWorkspaceLaunchRuntime>,
}

impl ProviderOptionsHandle {
    pub(in crate::daemon) fn new(launch: Arc<ProviderWorkspaceLaunchRuntime>) -> Self {
        Self { launch }
    }

    pub(in crate::daemon) fn launch(&self) -> &ProviderWorkspaceLaunchRuntime {
        self.launch.as_ref()
    }
}

#[derive(Clone)]
pub struct ProviderWorkspaceAuthHandle {
    launch: Arc<ProviderWorkspaceLaunchRuntime>,
}

impl ProviderWorkspaceAuthHandle {
    pub(in crate::daemon) fn new(launch: Arc<ProviderWorkspaceLaunchRuntime>) -> Self {
        Self { launch }
    }

    pub(in crate::daemon) fn launch(&self) -> &ProviderWorkspaceLaunchRuntime {
        self.launch.as_ref()
    }
}

#[derive(Clone)]
pub struct ProviderStatusHandle {
    data_root: PathBuf,
    providers: Arc<ProviderRuntime>,
    ops_events: OpsEvents,
}

impl ProviderStatusHandle {
    pub(in crate::daemon) fn new(
        data_root: PathBuf,
        providers: Arc<ProviderRuntime>,
        ops_events: OpsEvents,
    ) -> Self {
        Self {
            data_root,
            providers,
            ops_events,
        }
    }

    pub(in crate::daemon) fn data_root(&self) -> &Path {
        &self.data_root
    }

    pub(in crate::daemon) fn providers(&self) -> &ProviderRuntime {
        self.providers.as_ref()
    }

    pub(in crate::daemon) fn ops_events(&self) -> &OpsEvents {
        &self.ops_events
    }
}

#[derive(Clone)]
pub struct ProviderAdminHandle {
    data_root: PathBuf,
    providers: Arc<ProviderRuntime>,
    ops_events: OpsEvents,
}

impl ProviderAdminHandle {
    pub(in crate::daemon) fn new(
        data_root: PathBuf,
        providers: Arc<ProviderRuntime>,
        ops_events: OpsEvents,
    ) -> Self {
        Self {
            data_root,
            providers,
            ops_events,
        }
    }

    pub(in crate::daemon) fn data_root(&self) -> &Path {
        &self.data_root
    }

    pub(in crate::daemon) fn providers(&self) -> &ProviderRuntime {
        self.providers.as_ref()
    }

    pub(in crate::daemon) fn ops_events(&self) -> &OpsEvents {
        &self.ops_events
    }
}

#[derive(Clone)]
pub struct ProviderInstallHandle {
    data_root: PathBuf,
    providers: Arc<ProviderRuntime>,
    ops_events: OpsEvents,
}

impl ProviderInstallHandle {
    pub(in crate::daemon) fn new(
        data_root: PathBuf,
        providers: Arc<ProviderRuntime>,
        ops_events: OpsEvents,
    ) -> Self {
        Self {
            data_root,
            providers,
            ops_events,
        }
    }

    pub(in crate::daemon) fn data_root(&self) -> &Path {
        &self.data_root
    }

    pub(in crate::daemon) fn providers(&self) -> &ProviderRuntime {
        self.providers.as_ref()
    }

    pub(in crate::daemon) fn ops_events(&self) -> &OpsEvents {
        &self.ops_events
    }

    pub(in crate::daemon) async fn get_install_polling_info(
        &self,
        install_id: InstallId,
    ) -> Option<InstallInfo> {
        let outcome = self.providers.get_install_polling_info(install_id).await;
        crate::daemon::provider_capability_hosts::emit_provider_install_ops_events(
            &self.ops_events,
            outcome.ops_events,
        );
        outcome.info
    }

    pub(in crate::daemon) async fn cancel_install(
        &self,
        install_id: InstallId,
    ) -> Option<InstallInfo> {
        let outcome = self.providers.cancel_install(install_id).await?;
        crate::daemon::provider_capability_hosts::emit_provider_install_ops_events(
            &self.ops_events,
            outcome.ops_events,
        );
        Some(outcome.info)
    }

    pub(in crate::daemon) async fn list_install_events(
        &self,
        install_id: InstallId,
    ) -> Option<Vec<InstallProgressEvent>> {
        let outcome = self.providers.get_install_events(install_id).await;
        crate::daemon::provider_capability_hosts::emit_provider_install_ops_events(
            &self.ops_events,
            outcome.ops_events,
        );
        outcome.events
    }

    pub(in crate::daemon) async fn install_event_sender(
        &self,
        install_id: InstallId,
    ) -> Option<broadcast::Sender<InstallProgressEvent>> {
        self.providers.get_install_sender(install_id).await
    }
}

#[derive(Clone)]
pub struct ProviderAuthImportHandle {
    data_root: PathBuf,
    providers: Arc<ProviderRuntime>,
}

impl ProviderAuthImportHandle {
    pub(in crate::daemon) fn new(data_root: PathBuf, providers: Arc<ProviderRuntime>) -> Self {
        Self {
            data_root,
            providers,
        }
    }

    pub(in crate::daemon) fn data_root(&self) -> &Path {
        &self.data_root
    }

    pub(in crate::daemon) fn providers(&self) -> &ProviderRuntime {
        self.providers.as_ref()
    }
}

#[derive(Clone)]
pub struct ProviderUsageHandle {
    data_root: PathBuf,
    providers: Arc<ProviderRuntime>,
    shutdown_tx: broadcast::Sender<()>,
}

impl ProviderUsageHandle {
    pub(in crate::daemon) fn new(
        data_root: PathBuf,
        providers: Arc<ProviderRuntime>,
        shutdown_tx: broadcast::Sender<()>,
    ) -> Self {
        Self {
            data_root,
            providers,
            shutdown_tx,
        }
    }

    pub(in crate::daemon) fn data_root(&self) -> &Path {
        &self.data_root
    }

    pub(in crate::daemon) fn providers(&self) -> &ProviderRuntime {
        self.providers.as_ref()
    }

    pub(in crate::daemon) fn shutdown_tx(&self) -> &broadcast::Sender<()> {
        &self.shutdown_tx
    }
}

#[derive(Clone)]
pub struct ProviderHarnessConfigHandle {
    data_root: PathBuf,
    providers: Arc<ProviderRuntime>,
}

impl ProviderHarnessConfigHandle {
    pub(in crate::daemon) fn new(data_root: PathBuf, providers: Arc<ProviderRuntime>) -> Self {
        Self {
            data_root,
            providers,
        }
    }

    pub(in crate::daemon) fn data_root(&self) -> &Path {
        &self.data_root
    }

    pub(in crate::daemon) fn providers(&self) -> &ProviderRuntime {
        self.providers.as_ref()
    }
}
