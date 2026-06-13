#![allow(clippy::await_holding_lock)]
#![cfg(target_os = "macos")]

use super::*;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

struct EnvGuard {
    key: &'static str,
    prev: Option<String>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let prev = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, prev }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(prev) = self.prev.take() {
            std::env::set_var(self.key, prev);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

fn env_var_test_lock() -> &'static tokio::sync::Mutex<()> {
    crate::test_support::sandbox_cli_env_test_lock()
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn idle_runtime_reclaim_stops_machine_even_with_running_ctx_harness_container() {
    use std::os::unix::fs::PermissionsExt;

    let _serial = env_var_test_lock().lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let manager = HarnessRuntimeManager::new(temp.path().to_path_buf());
    let machine_name = sandbox_machine_name(temp.path());
    let log_path = temp.path().join("sandbox-cli-invocations.log");
    let sandbox_cli_path = temp.path().join("sandbox-cli.sh");
    std::fs::write(
        &sandbox_cli_path,
        format!(
            "#!/bin/sh\nLOG=\"{log}\"\nprintf '%s\\n' \"$*\" >> \"$LOG\"\nif [ \"$1\" = \"ps\" ]; then\n  printf 'ctx-harness-123\\n'\n  exit 0\nfi\nif [ \"$1\" = \"machine\" ] && [ \"$2\" = \"inspect\" ]; then\n  printf '[]\\n'\n  exit 0\nfi\nif [ \"$1\" = \"machine\" ] && [ \"$2\" = \"stop\" ]; then\n  exit 0\nfi\necho \"unexpected sandbox CLI invocation: $*\" >&2\nexit 1\n",
            log = log_path.display(),
        ),
    )
    .expect("write sandbox CLI shim");
    std::fs::set_permissions(&sandbox_cli_path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod sandbox CLI shim");
    let _guard = EnvGuard::set(
        "CTX_HARNESS_SANDBOX_CLI_PATH",
        &sandbox_cli_path.to_string_lossy(),
    );
    let _available = EnvGuard::set("CTX_TEST_SANDBOX_CLI_AVAILABLE", "1");
    manager.set_last_activity_for_test(Instant::now() - Duration::from_secs(600));
    let settings = ContainerExecutionSettings {
        machine: ctx_settings_model::ContainerMachineSettings {
            idle_shutdown_seconds: 60,
            ..ctx_settings_model::ContainerMachineSettings::default()
        },
        runtime: ctx_settings_model::ContainerRuntimeKind::NativeContainer,
        ..ContainerExecutionSettings::default()
    };
    let stores = StoreManager::open(temp.path()).await.expect("open stores");
    let running_sessions = Arc::new(Mutex::new(HashSet::new()));
    let terminals = TerminalManager::default();
    let snapshot = SystemSnapshot {
        cpu_pct: 0.0,
        memory_total_bytes: 32 * 1024 * 1024 * 1024,
        memory_used_bytes: 8 * 1024 * 1024 * 1024,
        swap_total_bytes: 4 * 1024 * 1024 * 1024,
        swap_used_bytes: 0,
    };

    let stopped = manager
        .maybe_reclaim_sandbox_machine(
            &settings,
            &snapshot,
            None,
            &stores,
            &running_sessions,
            &terminals,
        )
        .await
        .unwrap_or_else(|err| {
            let log = std::fs::read_to_string(&log_path).unwrap_or_default();
            panic!(
                "idle reclaim should not be blocked by parked workspace containers: {err:#}\ninvocation log:\n{log}"
            );
        });

    let log = std::fs::read_to_string(&log_path).expect("read invocation log");
    assert!(stopped);
    assert!(log.contains(&format!("machine stop {machine_name}")));
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn active_prewarm_artifact_activity_suppresses_reclaim() {
    use std::os::unix::fs::PermissionsExt;

    let _serial = env_var_test_lock().lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let manager = HarnessRuntimeManager::new(temp.path().to_path_buf());
    let machine_name = sandbox_machine_name(temp.path());
    let log_path = temp.path().join("sandbox-cli-invocations.log");
    let sandbox_cli_path = temp.path().join("sandbox-cli.sh");
    std::fs::write(
        &sandbox_cli_path,
        format!(
            "#!/bin/sh\nLOG=\"{log}\"\nprintf '%s\\n' \"$*\" >> \"$LOG\"\nif [ \"$1\" = \"ps\" ]; then\n  exit 0\nfi\nif [ \"$1\" = \"machine\" ] && [ \"$2\" = \"inspect\" ]; then\n  printf '[]\\n'\n  exit 0\nfi\nif [ \"$1\" = \"machine\" ] && [ \"$2\" = \"stop\" ]; then\n  exit 0\nfi\necho \"unexpected sandbox CLI invocation: $*\" >&2\nexit 1\n",
            log = log_path.display(),
        ),
    )
    .expect("write sandbox CLI shim");
    std::fs::set_permissions(&sandbox_cli_path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod sandbox CLI shim");
    let _guard = EnvGuard::set(
        "CTX_HARNESS_SANDBOX_CLI_PATH",
        &sandbox_cli_path.to_string_lossy(),
    );
    let _available = EnvGuard::set("CTX_TEST_SANDBOX_CLI_AVAILABLE", "1");
    manager.set_last_activity_for_test(Instant::now() - Duration::from_secs(600));
    let settings = ContainerExecutionSettings {
        machine: ctx_settings_model::ContainerMachineSettings {
            idle_shutdown_seconds: 60,
            ..ctx_settings_model::ContainerMachineSettings::default()
        },
        runtime: ctx_settings_model::ContainerRuntimeKind::NativeContainer,
        ..ContainerExecutionSettings::default()
    };
    let stores = StoreManager::open(temp.path()).await.expect("open stores");
    let running_sessions = Arc::new(Mutex::new(HashSet::new()));
    let terminals = TerminalManager::default();
    let snapshot = SystemSnapshot {
        cpu_pct: 0.0,
        memory_total_bytes: 32 * 1024 * 1024 * 1024,
        memory_used_bytes: 8 * 1024 * 1024 * 1024,
        swap_total_bytes: 4 * 1024 * 1024 * 1024,
        swap_used_bytes: 0,
    };
    let _prewarm_activity = manager.begin_prewarm_artifact_activity();

    let stopped = manager
        .maybe_reclaim_sandbox_machine(
            &settings,
            &snapshot,
            None,
            &stores,
            &running_sessions,
            &terminals,
        )
        .await
        .expect("active prewarm should suppress reclaim");

    let log = std::fs::read_to_string(&log_path).unwrap_or_default();
    assert!(!stopped);
    assert!(!log.contains(&format!("machine stop {machine_name}")));
}
