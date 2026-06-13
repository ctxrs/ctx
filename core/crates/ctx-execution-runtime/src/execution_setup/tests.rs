use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;

use super::*;
use crate::{
    ContainerExecutionSettings, ContainerRuntimeKind, ExecutionHarness, ExecutionMode,
    ExecutionSettings, HarnessSetupObserver, NoopRuntimeEventSink, NoopRuntimeMetricsSink,
    RuntimeActivityScope, SharedExecutionHarness,
};

struct EnvVarGuard {
    key: &'static str,
    prev: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let prev = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, prev }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(prev) = self.prev.take() {
            std::env::set_var(self.key, prev);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

#[derive(Default)]
struct NoopWarmupOperations;

#[async_trait]
impl SharedWarmupOperations for NoopWarmupOperations {
    async fn warm_runtime(
        &self,
        _settings: ExecutionSettings,
        _observer: Arc<dyn HarnessSetupObserver>,
    ) -> Result<()> {
        Ok(())
    }

    async fn warm_runtime_launch_ready(
        &self,
        _settings: ExecutionSettings,
        _observer: Arc<dyn HarnessSetupObserver>,
    ) -> Result<()> {
        Ok(())
    }

    async fn warm_builder(&self, _observer: Arc<dyn HarnessSetupObserver>) -> Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct NoopHarness;

#[async_trait]
impl ExecutionHarness for NoopHarness {
    fn begin_runtime_operation(&self) -> RuntimeActivityScope {
        RuntimeActivityScope::noop()
    }

    fn begin_prewarm_artifact_activity(&self) -> RuntimeActivityScope {
        RuntimeActivityScope::noop()
    }

    async fn workspace_container_exists(
        &self,
        _workspace_id: ctx_core::ids::WorkspaceId,
    ) -> Result<bool> {
        Ok(false)
    }

    async fn ensure_workspace_container_with_observer(
        &self,
        _workspace: &ctx_core::models::Workspace,
        _settings: &ExecutionSettings,
        _daemon_url: &str,
        _observer: Option<&dyn HarnessSetupObserver>,
    ) -> Result<()> {
        Ok(())
    }

    async fn ensure_container_machine_ready(
        &self,
        _settings: &ContainerExecutionSettings,
        _observer: Option<&dyn HarnessSetupObserver>,
    ) -> Result<()> {
        Ok(())
    }

    async fn ensure_workspace_container_after_runtime_ready_with_observer(
        &self,
        _workspace: &ctx_core::models::Workspace,
        _settings: &ExecutionSettings,
        _daemon_url: &str,
        _observer: Option<&dyn HarnessSetupObserver>,
    ) -> Result<()> {
        Ok(())
    }

    async fn ensure_workspace_container_after_machine_ready_with_observer(
        &self,
        _workspace: &ctx_core::models::Workspace,
        _settings: &ExecutionSettings,
        _daemon_url: &str,
        _observer: Option<&dyn HarnessSetupObserver>,
    ) -> Result<()> {
        Ok(())
    }

    async fn configured_startup_target(&self) -> Result<String> {
        Ok(ctx_harness_runtime::runtime_prewarm_target(
            &sandbox_execution_settings().container,
        ))
    }
}

fn sandbox_execution_settings() -> ExecutionSettings {
    let mut settings = ExecutionSettings {
        mode: ExecutionMode::Sandbox,
        ..ExecutionSettings::default()
    };
    settings.container.runtime = ContainerRuntimeKind::NativeContainer;
    settings
}

fn write_ready_runtime_sandbox_cli_shim(dir: &Path) -> PathBuf {
    let path = dir.join(if cfg!(windows) {
        "sandbox-cli-ready-runtime-test.cmd"
    } else {
        "sandbox-cli-ready-runtime-test.sh"
    });
    let script = if cfg!(windows) {
        "@echo off\r\nif \"%1\"==\"info\" (\r\n  echo {}\r\n  exit /b 0\r\n)\r\nif \"%1\"==\"image\" if \"%2\"==\"inspect\" exit /b 0\r\n>&2 echo unexpected sandbox CLI invocation: %*\r\nexit /b 1\r\n"
    } else {
        "#!/bin/sh\nif [ \"$1\" = \"info\" ]; then\n  printf '{}\\n'\n  exit 0\nfi\nif [ \"$1\" = \"image\" ] && [ \"$2\" = \"inspect\" ]; then\n  exit 0\nfi\necho \"unexpected sandbox CLI invocation: $*\" >&2\nexit 1\n"
    };
    std::fs::write(&path, script).expect("write ready runtime sandbox CLI shim");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod ready runtime sandbox CLI shim");
    }
    path
}

fn test_coordinator(data_root: PathBuf) -> Arc<ExecutionSetupCoordinator> {
    Arc::new(ExecutionSetupCoordinator::new_with_operations(
        data_root,
        Arc::new(NoopHarness) as SharedExecutionHarness,
        Arc::new(NoopRuntimeEventSink),
        Arc::new(NoopRuntimeMetricsSink),
        Arc::new(NoopWarmupOperations),
    ))
}

async fn wait_for_startup_prewarm_attempt(
    coordinator: &Arc<ExecutionSetupCoordinator>,
    timeout: Duration,
) -> StartupPrewarmSnapshot {
    tokio::time::timeout(timeout, async {
        loop {
            let latest = coordinator.startup_status().await;
            if latest.last_attempt_at.is_some() {
                break latest;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("timed out waiting for startup prewarm attempt")
}

#[cfg(unix)]
#[tokio::test]
async fn spawned_startup_prewarm_respects_sandbox_cli_env_test_lock() {
    let data_dir = tempfile::tempdir().expect("tempdir");
    let serial = sandbox_cli_env_test_lock().lock().await;
    let sandbox_cli_path = write_ready_runtime_sandbox_cli_shim(data_dir.path());
    let _sandbox_cli = EnvVarGuard::set("CTX_TEST_SANDBOX_CLI_AVAILABLE", "1");
    let _sandbox_cli_path = EnvVarGuard::set(
        "CTX_HARNESS_SANDBOX_CLI_PATH",
        &sandbox_cli_path.to_string_lossy(),
    );
    let coordinator = test_coordinator(data_dir.path().to_path_buf());

    coordinator.spawn_startup_prewarm(sandbox_execution_settings());
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;

    let startup = coordinator.startup_status().await;
    assert!(
        startup.last_attempt_at.is_none(),
        "startup prewarm should not begin while the sandbox CLI env test lock is held: {startup:?}"
    );

    drop(serial);

    let terminal = wait_for_startup_prewarm_attempt(&coordinator, Duration::from_secs(30)).await;
    assert!(
        terminal.last_attempt_at.is_some(),
        "startup prewarm should begin once the env lock is released: {terminal:?}"
    );
}
