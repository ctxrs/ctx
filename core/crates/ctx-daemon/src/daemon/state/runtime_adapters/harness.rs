use std::sync::Arc;

use async_trait::async_trait;
use ctx_core::ids::WorkspaceId;
use ctx_core::models::Workspace;
use ctx_execution_runtime::{
    ContainerExecutionSettings, ExecutionHarness, ExecutionSettings, HarnessSetupObserver,
    RuntimeActivityScope,
};
use ctx_store::Store;
use ctx_workspace_runtime::HarnessRuntimeManager;

#[derive(Clone)]
pub struct CtxExecutionHarness {
    inner: Arc<HarnessRuntimeManager>,
}

impl CtxExecutionHarness {
    pub fn new(inner: Arc<HarnessRuntimeManager>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl ExecutionHarness for CtxExecutionHarness {
    fn begin_runtime_operation(&self) -> RuntimeActivityScope {
        self.inner.begin_runtime_operation_scope()
    }

    fn begin_prewarm_artifact_activity(&self) -> RuntimeActivityScope {
        self.inner.begin_prewarm_artifact_activity_scope()
    }

    async fn workspace_container_exists(&self, workspace_id: WorkspaceId) -> anyhow::Result<bool> {
        self.inner.workspace_container_exists(workspace_id).await
    }

    async fn ensure_workspace_container_with_observer(
        &self,
        workspace: &Workspace,
        settings: &ExecutionSettings,
        daemon_url: &str,
        observer: Option<&dyn HarnessSetupObserver>,
    ) -> anyhow::Result<()> {
        self.inner
            .ensure_workspace_container_with_observer(workspace, settings, daemon_url, observer)
            .await
    }

    async fn ensure_container_machine_ready(
        &self,
        settings: &ContainerExecutionSettings,
        observer: Option<&dyn HarnessSetupObserver>,
    ) -> anyhow::Result<()> {
        self.inner
            .ensure_container_machine_ready(settings, observer)
            .await
    }

    async fn ensure_workspace_container_after_runtime_ready_with_observer(
        &self,
        workspace: &Workspace,
        settings: &ExecutionSettings,
        daemon_url: &str,
        observer: Option<&dyn HarnessSetupObserver>,
    ) -> anyhow::Result<()> {
        self.inner
            .ensure_workspace_container_after_runtime_ready_with_observer(
                workspace, settings, daemon_url, observer,
            )
            .await
    }

    async fn ensure_workspace_container_after_machine_ready_with_observer(
        &self,
        workspace: &Workspace,
        settings: &ExecutionSettings,
        daemon_url: &str,
        observer: Option<&dyn HarnessSetupObserver>,
    ) -> anyhow::Result<()> {
        self.inner
            .ensure_workspace_container_after_machine_ready_with_observer(
                workspace, settings, daemon_url, observer,
            )
            .await
    }

    async fn configured_startup_target(&self) -> anyhow::Result<String> {
        let db_path = self.inner.data_root().join("db").join("db.sqlite");
        let store = Store::open_sqlite(&db_path, Some(1)).await?;
        let settings = ctx_settings_service::load_settings(&store).await?;
        store.close().await;
        Ok(ctx_harness_runtime::runtime_prewarm_target(
            &settings.execution.unwrap_or_default().container,
        ))
    }
}
