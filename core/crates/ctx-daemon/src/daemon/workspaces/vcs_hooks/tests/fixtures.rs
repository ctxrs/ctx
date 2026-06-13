use crate::daemon::DaemonState;
use ctx_settings_model::{ExecutionSettings, Settings};
use ctx_settings_service::save_settings;
use ctx_store::StoreManager;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(super) struct EnvVarGuard {
    key: &'static str,
    prev: Option<String>,
}

impl EnvVarGuard {
    pub(super) fn set(key: &'static str, value: &str) -> Self {
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

pub(super) fn git(args: &[&str], cwd: &Path) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

fn git_output(args: &[&str], cwd: &Path) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("run git");
    assert!(output.status.success(), "git {args:?} failed");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

pub(super) fn init_git_workspace(root: &Path) -> String {
    git(&["init"], root);
    git(&["symbolic-ref", "HEAD", "refs/heads/main"], root);
    git(&["config", "extensions.worktreeConfig", "true"], root);
    git(&["config", "user.email", "ctx@example.com"], root);
    git(&["config", "user.name", "Ctx Test"], root);
    std::fs::write(root.join("README.md"), "hello\n").expect("write readme");
    git(&["add", "README.md"], root);
    git(&["commit", "-m", "initial"], root);
    git_output(&["rev-parse", "HEAD"], root)
}

pub(super) async fn test_state(data_root: &Path) -> Arc<DaemonState> {
    Arc::new(DaemonState::new(
        data_root.to_path_buf(),
        StoreManager::open(data_root).await.expect("open stores"),
        HashMap::new(),
        "http://127.0.0.1:4310".to_string(),
        None,
    ))
}

pub(super) async fn save_test_execution_settings(
    state: &Arc<DaemonState>,
    execution: ExecutionSettings,
) {
    let settings = Settings {
        execution: Some(execution),
        ..Default::default()
    };
    save_settings(state.global_store(), &settings)
        .await
        .expect("save settings");
}

#[cfg(unix)]
pub(super) fn write_sandbox_exec_shim(
    dir: &Path,
    container_name: &str,
    volume_name: &str,
) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let path = dir.join("sandbox-cli-vcs-hooks-test.sh");
    let running_marker = dir.join("workspace-container-running");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\nRUNNING=\"{running_marker}\"\ncmd=\"$1\"\nshift\ncase \"$cmd\" in\n  info)\n    printf '{{}}\\n'\n    exit 0\n    ;;\n  machine)\n    if [ \"$1\" = \"start\" ]; then\n      exit 0\n    fi\n    ;;\n  image)\n    if [ \"$1\" = \"inspect\" ]; then\n      printf '[{{}}]\\n'\n      exit 0\n    fi\n    ;;\n  volume)\n    if [ \"$1\" = \"inspect\" ]; then\n      exit 1\n    fi\n    if [ \"$1\" = \"create\" ]; then\n      printf '%s\\n' \"$2\"\n      exit 0\n    fi\n    ;;\n  container)\n    if [ \"$1\" = \"inspect\" ] && [ \"$2\" = \"--format\" ]; then\n      if [ -f \"$RUNNING\" ]; then\n        printf 'true\\n'\n        exit 0\n      fi\n      exit 1\n    fi\n    if [ \"$1\" = \"inspect\" ]; then\n      if [ -f \"$RUNNING\" ]; then\n        printf '[{{}}]\\n'\n        exit 0\n      fi\n      exit 1\n    fi\n    ;;\n  inspect)\n    printf '[{{\"Mounts\":[{{\"Type\":\"volume\",\"Name\":\"{volume_name}\",\"Destination\":\"{workspace_root}\"}}]}}]\\n'\n    exit 0\n    ;;\n  run)\n    : > \"$RUNNING\"\n    exit 0\n    ;;\n  start)\n    : > \"$RUNNING\"\n    exit 0\n    ;;\n  exec)\n    workdir=\"\"\n    while [ \"$#\" -gt 0 ]; do\n      case \"$1\" in\n        --interactive|--tty)\n          shift\n          ;;\n        --workdir)\n          workdir=\"$2\"\n          shift 2\n          ;;\n        --env)\n          export \"$2\"\n          shift 2\n          ;;\n        --user)\n          shift 2\n          ;;\n        *)\n          break\n          ;;\n      esac\n    done\n    actual_container=\"$1\"\n    shift\n    if [ \"$actual_container\" != \"{container_name}\" ]; then\n      echo \"unexpected container: $actual_container\" >&2\n      exit 1\n    fi\n    if [ \"$1\" = \"git\" ]; then\n      cd \"$workdir\"\n      exec \"$@\"\n    fi\n    exit 0\n    ;;\nesac\necho \"unexpected sandbox cli invocation: $cmd $*\" >&2\nexit 1\n",
            running_marker = running_marker.display(),
            volume_name = volume_name,
            workspace_root = ctx_sandbox_contract::CTX_CONTAINER_WORKSPACE_ROOT,
            container_name = container_name,
        ),
    )
    .expect("write sandbox exec shim");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod sandbox exec shim");
    path
}
