use super::*;

#[async_trait::async_trait]
pub trait SandboxMachineLifecycleExt {
    async fn ensure_sandbox_machine_download(&self) -> Result<()>;
    async fn inspect_sandbox_machine_memory_mb(&self, machine_name: &str) -> Result<Option<u32>>;
    async fn inspect_sandbox_machine_state(&self, machine_name: &str) -> Result<Option<String>>;
    async fn init_sandbox_machine_locked(
        &self,
        machine_name: &str,
        desired_memory_mb: u32,
        observer: Option<&dyn HarnessSetupObserver>,
    ) -> Result<()>;
    async fn stop_sandbox_machine_locked(
        &self,
        machine_name: &str,
        observer: Option<&dyn HarnessSetupObserver>,
    ) -> Result<bool>;
    async fn remove_sandbox_machine_locked(
        &self,
        machine_name: &str,
        observer: Option<&dyn HarnessSetupObserver>,
    ) -> Result<()>;
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    async fn ensure_sandbox_machine_materialized(
        &self,
        settings: &ContainerExecutionSettings,
        observer: Option<&dyn HarnessSetupObserver>,
    ) -> Result<()>;
    #[allow(dead_code)]
    async fn reconcile_running_sandbox_machine_memory(
        &self,
        settings: &ContainerExecutionSettings,
        observer: Option<&dyn HarnessSetupObserver>,
    ) -> Result<()>;
    async fn should_defer_disk_isolated_machine_reconfiguration(
        &self,
        settings: &ContainerExecutionSettings,
        machine_name: &str,
        observer: Option<&dyn HarnessSetupObserver>,
    ) -> Result<bool>;
    async fn has_running_workspace_containers_for_stopped_machine_reconfiguration(
        &self,
        observer: Option<&dyn HarnessSetupObserver>,
    ) -> Result<bool>;
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    async fn maybe_reclaim_sandbox_machine(
        &self,
        settings: &ContainerExecutionSettings,
        system: &SystemSnapshot,
        observer: Option<&dyn HarnessSetupObserver>,
        stores: &StoreManager,
        running_sessions: &Arc<Mutex<HashSet<SessionId>>>,
        terminals: &TerminalManager,
    ) -> Result<bool>;
}
