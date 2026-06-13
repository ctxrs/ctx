use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(test)]
use std::future::Future;
#[cfg(test)]
use std::pin::Pin;

use anyhow::Context;
use ctx_core::ids::{WorkspaceId, WorktreeId};
use ctx_core::models::{ExecutionEnvironment, TerminalSession};
use ctx_execution_runtime::ExecutionSetupCoordinator;
use ctx_observability::ops_events::OpsEvents;
use ctx_provider_install::install_state::InstallTarget;
use ctx_provider_runtime::ProviderRuntime;
use ctx_store::{Store, StoreManager};
use ctx_transport_runtime::terminal_launch::TerminalLaunchError;
use ctx_transport_runtime::terminals::TerminalManager;
use ctx_transport_runtime::web_sessions::{WebSessionInfo, WebSessionManager};
use ctx_update_service::UpdateDrainCoordinator;
use ctx_workspace_runtime::HarnessRuntimeManager;

use super::{
    state::ProtectedWorkspaceStoreLookup,
    terminals::{CreateTerminalLaunchRequest, TerminalLaunchHost},
    web_sessions::{WebSessionLaunchError, WebSessionLaunchHost, WebSessionLaunchRequest},
};

#[derive(Clone)]
pub(in crate::daemon) struct ProviderWorkspaceLaunchRuntime {
    data_root: PathBuf,
    daemon_url: String,
    auth_token: Option<String>,
    workspace_stores: ProtectedWorkspaceStoreLookup,
    providers: Arc<ProviderRuntime>,
    ops_events: OpsEvents,
    harness: Arc<HarnessRuntimeManager>,
}

impl ProviderWorkspaceLaunchRuntime {
    pub(in crate::daemon) fn new(
        data_root: PathBuf,
        daemon_url: String,
        auth_token: Option<String>,
        workspace_stores: ProtectedWorkspaceStoreLookup,
        providers: Arc<ProviderRuntime>,
        ops_events: OpsEvents,
        harness: Arc<HarnessRuntimeManager>,
    ) -> Self {
        Self {
            data_root,
            daemon_url,
            auth_token,
            workspace_stores,
            providers,
            ops_events,
            harness,
        }
    }

    pub(in crate::daemon) fn data_root(&self) -> &Path {
        &self.data_root
    }

    pub(in crate::daemon) fn daemon_url(&self) -> &str {
        &self.daemon_url
    }

    pub(in crate::daemon) fn auth_token(&self) -> Option<&String> {
        self.auth_token.as_ref()
    }

    pub(in crate::daemon) fn providers(&self) -> &ProviderRuntime {
        self.providers.as_ref()
    }

    pub(in crate::daemon) fn ops_events(&self) -> &OpsEvents {
        &self.ops_events
    }

    pub(in crate::daemon) fn harness(&self) -> &HarnessRuntimeManager {
        self.harness.as_ref()
    }

    pub(in crate::daemon) fn global_store(&self) -> &Store {
        self.workspace_stores.global_store()
    }

    pub(in crate::daemon) async fn load_workspace(
        &self,
        workspace_id: WorkspaceId,
    ) -> anyhow::Result<Option<ctx_core::models::Workspace>> {
        self.global_store().get_workspace(workspace_id).await
    }

    pub(in crate::daemon) async fn store_for_workspace(
        &self,
        workspace_id: WorkspaceId,
    ) -> anyhow::Result<Store> {
        self.workspace_stores
            .store_for_workspace(workspace_id)
            .await
    }

    pub(in crate::daemon) async fn resolve_existing_worktree_execution(
        &self,
        store: &Store,
        worktree_id: WorktreeId,
    ) -> anyhow::Result<crate::daemon::workspaces::ResolvedExistingWorktreeExecution> {
        let worktree = store
            .get_worktree(worktree_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("worktree not found"))?;
        let base_effective =
            ctx_settings_service::effective_execution_settings(self.global_store(), store)
                .await
                .context("loading workspace execution settings")?;
        let data_plane =
            ctx_worktree_data_plane::resolve_worktree_data_plane_with_host(self, &worktree)
                .await
                .context("resolving worktree data plane")?;
        let effective = ctx_worktree_data_plane::apply_data_plane_to_execution_settings(
            &base_effective,
            &data_plane,
        )
        .context("applying worktree data plane to execution settings")?;
        Ok(
            crate::daemon::workspaces::ResolvedExistingWorktreeExecution {
                worktree,
                effective,
            },
        )
    }

    pub(in crate::daemon) async fn effective_install_target_for_environment(
        &self,
        workspace_id: WorkspaceId,
        execution_environment: ExecutionEnvironment,
    ) -> anyhow::Result<InstallTarget> {
        let store = self.store_for_workspace(workspace_id).await?;
        ctx_settings_service::effective_install_target_for_environment(
            self.global_store(),
            &store,
            execution_environment,
        )
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
pub struct ExecutionLaunchHandle {
    global_store: Store,
    stores: StoreManager,
    update_drain: Arc<UpdateDrainCoordinator>,
    execution_setup: Arc<ExecutionSetupCoordinator>,
    daemon_url: String,
}

impl ExecutionLaunchHandle {
    pub(in crate::daemon) fn new(
        global_store: Store,
        stores: StoreManager,
        update_drain: Arc<UpdateDrainCoordinator>,
        execution_setup: Arc<ExecutionSetupCoordinator>,
        daemon_url: String,
    ) -> Self {
        Self {
            global_store,
            stores,
            update_drain,
            execution_setup,
            daemon_url,
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

    pub(in crate::daemon) fn execution_setup(&self) -> &Arc<ExecutionSetupCoordinator> {
        &self.execution_setup
    }

    pub(in crate::daemon) fn daemon_url(&self) -> &str {
        &self.daemon_url
    }
}

#[derive(Clone)]
pub struct LinuxSandboxRuntimeHandle {
    data_root: PathBuf,
    global_store: Store,
    stores: StoreManager,
    update_drain: Arc<UpdateDrainCoordinator>,
    terminals: Arc<TerminalManager>,
    harness: Arc<HarnessRuntimeManager>,
}

impl LinuxSandboxRuntimeHandle {
    pub(in crate::daemon) fn new(
        data_root: PathBuf,
        global_store: Store,
        stores: StoreManager,
        update_drain: Arc<UpdateDrainCoordinator>,
        terminals: Arc<TerminalManager>,
        harness: Arc<HarnessRuntimeManager>,
    ) -> Self {
        Self {
            data_root,
            global_store,
            stores,
            update_drain,
            terminals,
            harness,
        }
    }

    pub(in crate::daemon) fn data_root(&self) -> &Path {
        &self.data_root
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

    pub(in crate::daemon) fn terminals(&self) -> &TerminalManager {
        self.terminals.as_ref()
    }

    pub(in crate::daemon) fn harness(&self) -> &HarnessRuntimeManager {
        self.harness.as_ref()
    }
}

#[cfg(test)]
pub(in crate::daemon) type CreateTerminalFuture =
    Pin<Box<dyn Future<Output = Result<TerminalSession, TerminalLaunchError>> + Send + 'static>>;
#[cfg(test)]
pub(in crate::daemon) type CreateTerminalEffect =
    Arc<dyn Fn(CreateTerminalLaunchRequest) -> CreateTerminalFuture + Send + Sync>;

#[derive(Clone)]
enum TerminalRouteLaunch {
    Host(TerminalLaunchHost),
    #[cfg(test)]
    Override(CreateTerminalEffect),
}

#[derive(Clone)]
pub struct TerminalRouteHandle {
    terminals: Arc<TerminalManager>,
    launch: TerminalRouteLaunch,
}

impl TerminalRouteHandle {
    pub(in crate::daemon) fn new(
        terminals: Arc<TerminalManager>,
        launch: TerminalLaunchHost,
    ) -> Self {
        Self {
            terminals,
            launch: TerminalRouteLaunch::Host(launch),
        }
    }

    #[cfg(test)]
    pub(in crate::daemon) fn new_for_test(
        terminals: Arc<TerminalManager>,
        create_terminal: CreateTerminalEffect,
    ) -> Self {
        Self {
            terminals,
            launch: TerminalRouteLaunch::Override(create_terminal),
        }
    }

    pub(in crate::daemon) fn terminals(&self) -> &TerminalManager {
        self.terminals.as_ref()
    }

    pub(in crate::daemon) async fn create_terminal(
        &self,
        req: CreateTerminalLaunchRequest,
    ) -> Result<TerminalSession, TerminalLaunchError> {
        match &self.launch {
            TerminalRouteLaunch::Host(host) => {
                crate::daemon::terminals::create_workspace_terminal(host, req).await
            }
            #[cfg(test)]
            TerminalRouteLaunch::Override(create_terminal) => create_terminal(req).await,
        }
    }
}

#[cfg(test)]
pub(in crate::daemon) type CreateWebSessionFuture =
    Pin<Box<dyn Future<Output = Result<WebSessionInfo, WebSessionLaunchError>> + Send + 'static>>;
#[cfg(test)]
pub(in crate::daemon) type CreateWebSessionEffect =
    Arc<dyn Fn(WebSessionLaunchRequest) -> CreateWebSessionFuture + Send + Sync>;

#[derive(Clone)]
enum WebSessionRouteLaunch {
    Host(WebSessionLaunchHost),
    #[cfg(test)]
    Override(CreateWebSessionEffect),
}

#[derive(Clone)]
pub struct WebSessionRouteHandle {
    web_sessions: Arc<WebSessionManager>,
    launch: WebSessionRouteLaunch,
}

impl WebSessionRouteHandle {
    pub(in crate::daemon) fn new(
        web_sessions: Arc<WebSessionManager>,
        launch: WebSessionLaunchHost,
    ) -> Self {
        Self {
            web_sessions,
            launch: WebSessionRouteLaunch::Host(launch),
        }
    }

    #[cfg(test)]
    pub(in crate::daemon) fn new_for_test(
        web_sessions: Arc<WebSessionManager>,
        create_web_session: CreateWebSessionEffect,
    ) -> Self {
        Self {
            web_sessions,
            launch: WebSessionRouteLaunch::Override(create_web_session),
        }
    }

    pub(in crate::daemon) fn web_sessions(&self) -> &WebSessionManager {
        self.web_sessions.as_ref()
    }

    pub(in crate::daemon) fn web_sessions_arc(&self) -> Arc<WebSessionManager> {
        Arc::clone(&self.web_sessions)
    }

    pub(in crate::daemon) async fn create_web_session(
        &self,
        req: WebSessionLaunchRequest,
    ) -> Result<WebSessionInfo, WebSessionLaunchError> {
        match &self.launch {
            WebSessionRouteLaunch::Host(host) => {
                crate::daemon::web_sessions::create_web_session(host, req).await
            }
            #[cfg(test)]
            WebSessionRouteLaunch::Override(create_web_session) => create_web_session(req).await,
        }
    }
}
