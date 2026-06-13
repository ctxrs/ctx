#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::sync::OnceLock;

use chrono::Utc;
use tempfile::TempDir;
use tokio::sync::Mutex;

use ctx_core::ids::WorkspaceId;
use ctx_core::models::Workspace;
use ctx_settings_model::{
    ContainerExecutionSettings, ContainerMountMode, ContainerNetworkMode, ContainerRuntimeKind,
    ExecutionMode, ExecutionSettings,
};
use ctx_workspace_runtime::HarnessRuntimeManager;

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
        if let Some(value) = self.prev.take() {
            std::env::set_var(self.key, value);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

fn env_var_test_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn sample_workspace(tmp: &TempDir) -> Workspace {
    Workspace {
        id: WorkspaceId::new(),
        name: "ws".to_string(),
        root_path: tmp.path().to_string_lossy().to_string(),
        created_at: Utc::now(),
        vcs_kind: None,
    }
}

async fn runtime_manager(tmp: &TempDir) -> HarnessRuntimeManager {
    HarnessRuntimeManager::new(tmp.path().to_path_buf())
}

#[cfg(unix)]
#[tokio::test]
async fn runtime_ready_container_creation_skips_front_loaded_image_checks() {
    let _serial = env_var_test_lock().lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let log_path = temp.path().join("sandbox-cli-invocations.log");
    let state_path = temp.path().join("container-created");
    let sandbox_cli_path = temp.path().join("sandbox-cli.sh");
    let manager = runtime_manager(&temp).await;
    let workspace = sample_workspace(&temp);
    let container_name = format!("ctx-harness-{}", workspace.id.0);
    let settings = ExecutionSettings {
        mode: ExecutionMode::Sandbox,
        container: ContainerExecutionSettings {
            runtime: ContainerRuntimeKind::NativeContainer,
            mount_mode: ContainerMountMode::DiskIsolated,
            network_mode: ContainerNetworkMode::All,
            allowlist: Vec::new(),
            ..Default::default()
        },
    };

    std::fs::write(
        &sandbox_cli_path,
        format!(
            "#!/bin/sh\nLOG=\"{log}\"\nSTATE=\"{state}\"\nprintf '%s\\n' \"$*\" >> \"$LOG\"\nif [ \"$1\" = \"volume\" ] && [ \"$2\" = \"inspect\" ]; then\n  exit 1\nfi\nif [ \"$1\" = \"volume\" ] && [ \"$2\" = \"create\" ]; then\n  exit 0\nfi\nif [ \"$1\" = \"container\" ] && [ \"$2\" = \"inspect\" ] && [ \"$3\" = \"{container}\" ]; then\n  if [ -f \"$STATE\" ]; then printf '[{{}}]\\n'; exit 0; fi\n  exit 1\nfi\nif [ \"$1\" = \"container\" ] && [ \"$2\" = \"inspect\" ] && [ \"$5\" = \"{container}\" ]; then\n  if [ -f \"$STATE\" ]; then printf 'true\\n'; exit 0; fi\n  exit 1\nfi\nif [ \"$1\" = \"inspect\" ] && [ \"$2\" = \"{container}\" ]; then\n  suffix=${{2#ctx-harness-}}\n  printf '[{{\"Mounts\":[{{\"Type\":\"volume\",\"Name\":\"ctx-ws-%s\",\"Destination\":\"/ctx/ws\"}}]}}]\\n' \"$suffix\"\n  exit 0\nfi\nif [ \"$1\" = \"run\" ]; then\n  : > \"$STATE\"\n  exit 0\nfi\nif [ \"$1\" = \"exec\" ]; then\n  exit 0\nfi\necho \"unexpected sandbox CLI invocation: $*\" >&2\nexit 1\n",
            log = log_path.display(),
            state = state_path.display(),
            container = container_name,
        ),
    )
    .expect("write sandbox CLI shim");
    std::fs::set_permissions(&sandbox_cli_path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod sandbox CLI shim");
    let _guard = EnvGuard::set(
        "CTX_HARNESS_SANDBOX_CLI_PATH",
        &sandbox_cli_path.to_string_lossy(),
    );

    manager
        .ensure_workspace_container_after_runtime_ready_with_observer(
            &workspace,
            &settings,
            "http://127.0.0.1:4399",
            None,
        )
        .await
        .expect("runtime-ready path should create workspace container without image checks");

    let log = std::fs::read_to_string(&log_path).expect("read sandbox CLI invocation log");
    assert!(
        log.contains(&format!("container inspect {container_name}")),
        "expected container existence check in log:\n{log}"
    );
    assert!(
        log.contains(&format!("run -d --name {container_name}")),
        "expected runtime-ready path to create the workspace container:\n{log}"
    );
    assert!(
        !log.contains("image inspect"),
        "runtime-ready path should not front-load image readiness checks:\n{log}"
    );
}
