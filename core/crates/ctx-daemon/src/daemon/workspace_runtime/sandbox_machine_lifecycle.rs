use super::*;
use ctx_harness_setup::{HarnessSetupLogLevel, HarnessSetupObserver, HarnessSetupPhase};
use ctx_settings_model::ContainerMountMode;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::collections::HashSet;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::sync::Arc;
use std::time::Duration;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use tokio::sync::Mutex;

mod disk_state;
mod ext;
mod inspection;
mod machine_ops;
mod materialization;
mod reclaim;

const SANDBOX_OP_TIMEOUT: Duration = Duration::from_secs(60);

pub use ext::SandboxMachineLifecycleExt;

#[async_trait::async_trait]
impl SandboxMachineLifecycleExt for HarnessRuntimeManager {
    async fn ensure_sandbox_machine_download(&self) -> Result<()> {
        machine_ops::ensure_sandbox_machine_download(self).await
    }

    async fn inspect_sandbox_machine_memory_mb(&self, machine_name: &str) -> Result<Option<u32>> {
        inspection::inspect_sandbox_machine_memory_mb(self.data_root(), machine_name).await
    }

    async fn inspect_sandbox_machine_state(&self, machine_name: &str) -> Result<Option<String>> {
        inspection::inspect_sandbox_machine_state(self.data_root(), machine_name).await
    }

    async fn init_sandbox_machine_locked(
        &self,
        machine_name: &str,
        desired_memory_mb: u32,
        observer: Option<&dyn HarnessSetupObserver>,
    ) -> Result<()> {
        machine_ops::init_sandbox_machine_locked(self, machine_name, desired_memory_mb, observer)
            .await
    }

    async fn stop_sandbox_machine_locked(
        &self,
        machine_name: &str,
        observer: Option<&dyn HarnessSetupObserver>,
    ) -> Result<bool> {
        machine_ops::stop_sandbox_machine_locked(self, machine_name, observer).await
    }

    async fn remove_sandbox_machine_locked(
        &self,
        machine_name: &str,
        observer: Option<&dyn HarnessSetupObserver>,
    ) -> Result<()> {
        machine_ops::remove_sandbox_machine_locked(self, machine_name, observer).await
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    async fn ensure_sandbox_machine_materialized(
        &self,
        settings: &ContainerExecutionSettings,
        observer: Option<&dyn HarnessSetupObserver>,
    ) -> Result<()> {
        materialization::ensure_sandbox_machine_materialized(self, settings, observer).await
    }

    #[allow(dead_code)]
    async fn reconcile_running_sandbox_machine_memory(
        &self,
        settings: &ContainerExecutionSettings,
        observer: Option<&dyn HarnessSetupObserver>,
    ) -> Result<()> {
        materialization::reconcile_running_sandbox_machine_memory(self, settings, observer).await
    }

    async fn should_defer_disk_isolated_machine_reconfiguration(
        &self,
        settings: &ContainerExecutionSettings,
        machine_name: &str,
        observer: Option<&dyn HarnessSetupObserver>,
    ) -> Result<bool> {
        disk_state::should_defer_disk_isolated_machine_reconfiguration(
            self,
            settings,
            machine_name,
            observer,
        )
        .await
    }

    async fn has_running_workspace_containers_for_stopped_machine_reconfiguration(
        &self,
        observer: Option<&dyn HarnessSetupObserver>,
    ) -> Result<bool> {
        disk_state::has_running_workspace_containers_for_stopped_machine_reconfiguration(
            self, observer,
        )
        .await
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    async fn maybe_reclaim_sandbox_machine(
        &self,
        settings: &ContainerExecutionSettings,
        system: &SystemSnapshot,
        observer: Option<&dyn HarnessSetupObserver>,
        stores: &StoreManager,
        running_sessions: &Arc<Mutex<HashSet<SessionId>>>,
        terminals: &TerminalManager,
    ) -> Result<bool> {
        reclaim::maybe_reclaim_sandbox_machine(
            self,
            settings,
            system,
            observer,
            stores,
            running_sessions,
            terminals,
        )
        .await
    }
}
