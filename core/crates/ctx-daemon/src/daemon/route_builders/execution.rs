use super::*;

impl execution_deps::ExecutionRouteDeps {
    pub fn execution_launch(&self) -> ExecutionLaunchHandle {
        ExecutionLaunchHandle::new(
            self.global_store.clone(),
            self.stores.clone(),
            Arc::clone(&self.update_drain),
            Arc::clone(&self.execution_setup),
            self.daemon_url.clone(),
        )
    }
    pub fn linux_sandbox_runtime(&self) -> LinuxSandboxRuntimeHandle {
        LinuxSandboxRuntimeHandle::new(
            self.data_root.clone(),
            self.global_store.clone(),
            self.stores.clone(),
            Arc::clone(&self.update_drain),
            Arc::clone(&self.terminals),
            Arc::clone(&self.harness),
        )
    }
}
