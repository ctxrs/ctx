use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use ctx_avf_linux_runtime::SubstrateLifecycleRecord;
use ctx_core::ids::WorkspaceId;
use ctx_core::models::Workspace;

pub mod execution_setup;
pub mod route_contract;

pub use ctx_harness_setup::{
    HarnessSetupDownloadStatus, HarnessSetupLogLevel, HarnessSetupObserver,
    HarnessSetupObserver as SetupObserver, HarnessSetupPhase, HarnessSetupProgressUpdate,
};
pub use ctx_sandbox_container_runtime::{
    bundled_default_container_image_tar, command_output_message,
};
pub use ctx_sandbox_contract::{
    ContainerExecutionSettings, ContainerMountMode, ContainerNetworkMode, ContainerRuntimeKind,
    ExecutionMode, ExecutionSettings,
};
pub use execution_setup::{
    ExecutionLaunchLogLine, ExecutionLaunchPhaseStatus, ExecutionLaunchSnapshot,
    ExecutionLaunchState, ExecutionLaunchStreamEvent, ExecutionSetupCoordinator,
    ExecutionSetupJobKind, RuntimePrewarmScope, SharedWarmupOperations, StartupPrewarmSnapshot,
    StartupPrewarmState,
};

pub trait RuntimeEventSink: Send + Sync {
    fn emit_event(&self, level: &'static str, name: &'static str, meta: Option<Value>);

    fn emit_substrate_lifecycle(
        &self,
        record: &SubstrateLifecycleRecord,
        source: &'static str,
        workspace_id: Option<WorkspaceId>,
    );
}

pub trait RuntimeMetricsSink: Send + Sync {
    fn record_phase_duration(
        &self,
        phase: ctx_harness_setup::HarnessSetupPhase,
        elapsed_ms: u64,
        result: &'static str,
    );

    fn record_launch_duration(&self, elapsed_ms: u64, result: &'static str);
}

#[derive(Default)]
pub struct NoopRuntimeEventSink;

impl RuntimeEventSink for NoopRuntimeEventSink {
    fn emit_event(&self, _level: &'static str, _name: &'static str, _meta: Option<Value>) {}

    fn emit_substrate_lifecycle(
        &self,
        _record: &SubstrateLifecycleRecord,
        _source: &'static str,
        _workspace_id: Option<WorkspaceId>,
    ) {
    }
}

#[derive(Default)]
pub struct NoopRuntimeMetricsSink;

impl RuntimeMetricsSink for NoopRuntimeMetricsSink {
    fn record_phase_duration(
        &self,
        _phase: ctx_harness_setup::HarnessSetupPhase,
        _elapsed_ms: u64,
        _result: &'static str,
    ) {
    }

    fn record_launch_duration(&self, _elapsed_ms: u64, _result: &'static str) {}
}

trait RuntimeActivityRelease: Send {
    fn release(self: Box<Self>);
}

impl<F> RuntimeActivityRelease for F
where
    F: FnOnce() + Send + 'static,
{
    fn release(self: Box<Self>) {
        (*self)();
    }
}

pub struct RuntimeActivityScope(Option<Box<dyn RuntimeActivityRelease>>);

impl RuntimeActivityScope {
    pub fn noop() -> Self {
        Self(None)
    }

    pub fn new<F>(release: F) -> Self
    where
        F: FnOnce() + Send + 'static,
    {
        Self(Some(Box::new(release)))
    }
}

impl Drop for RuntimeActivityScope {
    fn drop(&mut self) {
        if let Some(release) = self.0.take() {
            release.release();
        }
    }
}

#[async_trait]
pub trait ExecutionHarness: Send + Sync {
    fn begin_runtime_operation(&self) -> RuntimeActivityScope;

    fn begin_prewarm_artifact_activity(&self) -> RuntimeActivityScope;

    async fn workspace_container_exists(&self, workspace_id: WorkspaceId) -> anyhow::Result<bool>;

    async fn ensure_workspace_container_with_observer(
        &self,
        workspace: &Workspace,
        settings: &ExecutionSettings,
        daemon_url: &str,
        observer: Option<&dyn HarnessSetupObserver>,
    ) -> anyhow::Result<()>;

    async fn ensure_container_machine_ready(
        &self,
        settings: &ContainerExecutionSettings,
        observer: Option<&dyn HarnessSetupObserver>,
    ) -> anyhow::Result<()>;

    async fn ensure_workspace_container_after_runtime_ready_with_observer(
        &self,
        workspace: &Workspace,
        settings: &ExecutionSettings,
        daemon_url: &str,
        observer: Option<&dyn HarnessSetupObserver>,
    ) -> anyhow::Result<()>;

    async fn ensure_workspace_container_after_machine_ready_with_observer(
        &self,
        workspace: &Workspace,
        settings: &ExecutionSettings,
        daemon_url: &str,
        observer: Option<&dyn HarnessSetupObserver>,
    ) -> anyhow::Result<()>;

    async fn configured_startup_target(&self) -> anyhow::Result<String>;
}

pub type SharedExecutionHarness = Arc<dyn ExecutionHarness>;
