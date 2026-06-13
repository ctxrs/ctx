use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result};
use ctx_core::ids::WorkspaceId;
use ctx_linux_sandbox_runtime::preferred_native_sandbox_cli_path;
use tokio::process::Command;

use crate::{
    SandboxCommandMode, CTX_HARNESS_SANDBOX_CLI_PATH_ENV, SANDBOX_INFO_TIMEOUT, SANDBOX_OP_TIMEOUT,
};

pub const SHARED_VM_SANDBOX_CLI_GUEST_BIN: &str = "/usr/local/bin/nerdctl";
const SHARED_VM_GUEST_ROOT_HOME: &str = "/ctx/home/root";
const SHARED_VM_GUEST_ROOT_XDG_CONFIG_ROOT: &str = "/ctx/cache/xdg/config";
const SHARED_VM_GUEST_ROOT_XDG_DATA_ROOT: &str = "/ctx/cache/xdg/data";
const SHARED_VM_GUEST_ROOT_XDG_CACHE_ROOT: &str = "/ctx/cache/xdg/cache";
const SHARED_VM_GUEST_ROOT_XDG_RUNTIME_ROOT: &str = "/ctx/tmp/xdg-runtime-root";
const SHARED_VM_GUEST_TMP_ROOT: &str = "/ctx/tmp";

fn find_binary_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

fn explicit_sandbox_cli_binary_path() -> Option<PathBuf> {
    let raw = std::env::var(CTX_HARNESS_SANDBOX_CLI_PATH_ENV).ok()?;
    let path = PathBuf::from(raw.trim());
    if path.exists() {
        Some(path)
    } else {
        None
    }
}

fn sandbox_cli_available(_data_root: &Path) -> bool {
    if explicit_sandbox_cli_binary_path().is_some() {
        return true;
    }
    if cfg!(test) {
        if let Ok(value) = std::env::var("CTX_TEST_SANDBOX_CLI_AVAILABLE") {
            let value = value.trim().to_ascii_lowercase();
            return matches!(value.as_str(), "1" | "true" | "yes" | "y");
        }
    }
    preferred_native_sandbox_cli_path().is_some() || find_binary_in_path("nerdctl").is_some()
}

pub fn native_container_runtime_available(data_root: &Path) -> bool {
    sandbox_cli_available(data_root)
}

pub fn sandbox_cli_binary_path(_data_root: &Path) -> Option<PathBuf> {
    if let Some(path) = explicit_sandbox_cli_binary_path() {
        return Some(path);
    }
    if let Some(path) = preferred_native_sandbox_cli_path() {
        return Some(path);
    }
    find_binary_in_path("nerdctl")
}

#[derive(Debug, Clone)]
pub struct SandboxCliInvocation {
    pub bin: PathBuf,
    pub env: HashMap<String, String>,
}

pub fn sandbox_cli_invocation(data_root: &Path) -> Result<SandboxCliInvocation> {
    let bin = sandbox_cli_binary_path(data_root)
        .ok_or_else(|| anyhow::anyhow!("sandbox container CLI unavailable"))?;
    let env = sandbox_cli_env_for_mode(data_root, &SandboxCommandMode::NativeContainer)?;
    Ok(SandboxCliInvocation { bin, env })
}

fn native_sandbox_cli_env_for_data_root(data_root: &Path) -> Result<HashMap<String, String>> {
    let xdg_root = data_root.join("sandbox").join("xdg");
    let xdg_config = xdg_root.join("config");
    let xdg_data = xdg_root.join("data");
    let xdg_run = data_root.join("sandbox").join("run");
    let sandbox_home = data_root.join("sandbox").join("home");
    let sandbox_tmp_root = data_root.join("sandbox").join("tmp");
    std::fs::create_dir_all(&xdg_config)
        .with_context(|| format!("create dir {}", xdg_config.display()))?;
    std::fs::create_dir_all(&xdg_data)
        .with_context(|| format!("create dir {}", xdg_data.display()))?;
    std::fs::create_dir_all(&xdg_run)
        .with_context(|| format!("create dir {}", xdg_run.display()))?;
    std::fs::create_dir_all(&sandbox_home)
        .with_context(|| format!("create dir {}", sandbox_home.display()))?;
    std::fs::create_dir_all(&sandbox_tmp_root)
        .with_context(|| format!("create dir {}", sandbox_tmp_root.display()))?;
    let _ = std::fs::set_permissions(&xdg_run, std::fs::Permissions::from_mode(0o700));
    let _ = std::fs::set_permissions(&sandbox_home, std::fs::Permissions::from_mode(0o700));

    let mut env = HashMap::new();
    env.insert(
        "XDG_CONFIG_HOME".to_string(),
        xdg_config.to_string_lossy().to_string(),
    );
    env.insert(
        "XDG_DATA_HOME".to_string(),
        xdg_data.to_string_lossy().to_string(),
    );
    env.insert(
        "XDG_CACHE_HOME".to_string(),
        xdg_root.join("cache").to_string_lossy().to_string(),
    );
    env.insert(
        "XDG_RUNTIME_DIR".to_string(),
        xdg_run.to_string_lossy().to_string(),
    );
    env.insert(
        "HOME".to_string(),
        sandbox_home.to_string_lossy().to_string(),
    );
    env.insert(
        "CONTAINERD_ADDRESS".to_string(),
        "/run/containerd/containerd.sock".to_string(),
    );
    env.insert("CONTAINERD_NAMESPACE".to_string(), "default".to_string());
    let tmp = sandbox_tmp_root.to_string_lossy().to_string();
    env.insert("TMPDIR".to_string(), tmp.clone());
    env.insert("TMP".to_string(), tmp.clone());
    env.insert("TEMP".to_string(), tmp);
    Ok(env)
}

fn shared_vm_guest_sandbox_cli_env() -> HashMap<String, String> {
    let mut env = HashMap::new();
    env.insert(
        "XDG_CONFIG_HOME".to_string(),
        SHARED_VM_GUEST_ROOT_XDG_CONFIG_ROOT.to_string(),
    );
    env.insert(
        "XDG_DATA_HOME".to_string(),
        SHARED_VM_GUEST_ROOT_XDG_DATA_ROOT.to_string(),
    );
    env.insert(
        "XDG_CACHE_HOME".to_string(),
        SHARED_VM_GUEST_ROOT_XDG_CACHE_ROOT.to_string(),
    );
    env.insert(
        "XDG_RUNTIME_DIR".to_string(),
        SHARED_VM_GUEST_ROOT_XDG_RUNTIME_ROOT.to_string(),
    );
    env.insert("HOME".to_string(), SHARED_VM_GUEST_ROOT_HOME.to_string());
    env.insert(
        "CONTAINERD_ADDRESS".to_string(),
        "/run/containerd/containerd.sock".to_string(),
    );
    env.insert("CONTAINERD_NAMESPACE".to_string(), "default".to_string());
    env.insert("TMPDIR".to_string(), SHARED_VM_GUEST_TMP_ROOT.to_string());
    env.insert("TMP".to_string(), SHARED_VM_GUEST_TMP_ROOT.to_string());
    env.insert("TEMP".to_string(), SHARED_VM_GUEST_TMP_ROOT.to_string());
    env
}

pub fn sandbox_cli_env_for_data_root(data_root: &Path) -> Result<HashMap<String, String>> {
    native_sandbox_cli_env_for_data_root(data_root)
}

pub fn sandbox_cli_env_for_mode(
    data_root: &Path,
    mode: &SandboxCommandMode,
) -> Result<HashMap<String, String>> {
    match mode {
        SandboxCommandMode::NativeContainer => native_sandbox_cli_env_for_data_root(data_root),
        SandboxCommandMode::SharedVm { .. } => Ok(shared_vm_guest_sandbox_cli_env()),
    }
}

pub fn sandbox_container_command(data_root: &Path, mode: &SandboxCommandMode) -> Result<Command> {
    if let Some(bin) = explicit_sandbox_cli_binary_path() {
        let env = sandbox_cli_env_for_mode(data_root, mode)?;
        let mut cmd = Command::new(bin);
        for (key, value) in env {
            cmd.env(key, value);
        }
        return Ok(cmd);
    }

    match mode {
        SandboxCommandMode::NativeContainer => {
            let inv = sandbox_cli_invocation(data_root)?;
            let mut cmd = Command::new(inv.bin);
            for (key, value) in inv.env {
                cmd.env(key, value);
            }
            Ok(cmd)
        }
        SandboxCommandMode::SharedVm { helper_path } => {
            let env = sandbox_cli_env_for_mode(data_root, mode)?;
            let mut cmd = Command::new(helper_path);
            cmd.arg("shared-vm-exec")
                .arg("--data-root")
                .arg(data_root)
                .arg("--cwd")
                .arg("/")
                .arg("--command")
                .arg(SHARED_VM_SANDBOX_CLI_GUEST_BIN)
                .arg("--user")
                .arg("root");
            let mut env_pairs = env.into_iter().collect::<Vec<_>>();
            env_pairs.sort_by(|(left, _), (right, _)| left.cmp(right));
            for (key, value) in env_pairs {
                cmd.arg("--env").arg(format!("{key}={value}"));
            }
            cmd.arg("--");
            Ok(cmd)
        }
    }
}

pub async fn command_output_with_timeout(
    mut cmd: Command,
    timeout: Duration,
) -> Result<std::process::Output> {
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.kill_on_drop(true);
    let child = cmd.spawn().context("spawning command")?;
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(res) => Ok(res?),
        Err(_) => anyhow::bail!("command timed out after {}s", timeout.as_secs()),
    }
}

pub fn command_output_message(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    format!("{stderr}\n{stdout}").trim().to_string()
}

pub async fn sandbox_engine_ready(data_root: &Path, mode: &SandboxCommandMode) -> Result<bool> {
    let mut cmd = sandbox_container_command(data_root, mode)?;
    cmd.arg("info");
    match command_output_with_timeout(cmd, SANDBOX_INFO_TIMEOUT).await {
        Ok(out) => Ok(out.status.success()),
        Err(_) => Ok(false),
    }
}

pub async fn container_exists(
    data_root: &Path,
    mode: &SandboxCommandMode,
    name: &str,
) -> Result<bool> {
    let mut cmd = sandbox_container_command(data_root, mode)?;
    cmd.arg("container").arg("inspect").arg(name);
    let output = command_output_with_timeout(cmd, SANDBOX_OP_TIMEOUT).await?;
    Ok(output.status.success())
}

pub async fn container_running(
    data_root: &Path,
    mode: &SandboxCommandMode,
    name: &str,
) -> Result<Option<bool>> {
    let mut cmd = sandbox_container_command(data_root, mode)?;
    cmd.arg("container")
        .arg("inspect")
        .arg("--format")
        .arg("{{.State.Running}}")
        .arg(name);
    let output = command_output_with_timeout(cmd, SANDBOX_OP_TIMEOUT).await?;
    if !output.status.success() {
        return Ok(None);
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(Some(stdout.trim() == "true"))
}

pub async fn ensure_workspace_volume(
    data_root: &Path,
    mode: &SandboxCommandMode,
    workspace_id: WorkspaceId,
) -> Result<String> {
    let name = format!("ctx-ws-{}", workspace_id.0);
    let mut inspect = sandbox_container_command(data_root, mode)?;
    inspect.arg("volume").arg("inspect").arg(&name);
    let out = command_output_with_timeout(inspect, SANDBOX_OP_TIMEOUT).await?;
    if out.status.success() {
        return Ok(name);
    }
    let mut create = sandbox_container_command(data_root, mode)?;
    create.arg("volume").arg("create").arg(&name);
    let out = command_output_with_timeout(create, SANDBOX_OP_TIMEOUT).await?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let combined = format!("{stderr}\n{stdout}").trim().to_string();
        if combined.is_empty() {
            anyhow::bail!(
                "container volume create failed for {name} (status: {})",
                out.status
            );
        }
        anyhow::bail!("container volume create failed for {name}: {combined}");
    }
    Ok(name)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use tempfile::tempdir;

    use super::*;

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

    fn env_var_test_lock() -> &'static tokio::sync::Mutex<()> {
        crate::sandbox_cli_env_test_lock()
    }

    #[tokio::test]
    async fn sandbox_cli_available_uses_test_override() {
        let _serial = env_var_test_lock().lock().await;
        let _guard = EnvVarGuard::set("CTX_TEST_SANDBOX_CLI_AVAILABLE", "true");
        assert!(sandbox_cli_available(tempdir().unwrap().path()));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn explicit_cli_override_beats_negative_test_override() {
        let _serial = env_var_test_lock().lock().await;
        let temp = tempdir().expect("tempdir");
        let cli_path = temp.path().join("sandbox-cli.sh");
        std::fs::write(
            &cli_path,
            "#!/bin/sh\nif [ \"$1\" = \"info\" ]; then\n  printf '{}\\n'\n  exit 0\nfi\necho \"unexpected invocation: $*\" >&2\nexit 1\n",
        )
        .expect("write sandbox cli shim");
        std::fs::set_permissions(&cli_path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod sandbox cli shim");
        let _override = EnvVarGuard::set("CTX_TEST_SANDBOX_CLI_AVAILABLE", "0");
        let _guard = EnvVarGuard::set(
            CTX_HARNESS_SANDBOX_CLI_PATH_ENV,
            &cli_path.to_string_lossy(),
        );

        assert!(sandbox_cli_available(temp.path()));
        assert!(
            sandbox_engine_ready(temp.path(), &SandboxCommandMode::NativeContainer)
                .await
                .expect("sandbox engine ready check"),
            "sandbox engine should honor the explicit CLI override even when the negative test override is set",
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn sandbox_engine_ready_uses_explicit_cli_override() {
        let _serial = env_var_test_lock().lock().await;
        let temp = tempdir().expect("tempdir");
        let cli_path = temp.path().join("sandbox-cli.sh");
        std::fs::write(
            &cli_path,
            "#!/bin/sh\nif [ \"$1\" = \"info\" ]; then\n  printf '{}\\n'\n  exit 0\nfi\necho \"unexpected invocation: $*\" >&2\nexit 1\n",
        )
        .expect("write sandbox cli shim");
        std::fs::set_permissions(&cli_path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod sandbox cli shim");
        let _guard = EnvVarGuard::set(
            CTX_HARNESS_SANDBOX_CLI_PATH_ENV,
            &cli_path.to_string_lossy(),
        );

        assert!(
            sandbox_engine_ready(temp.path(), &SandboxCommandMode::NativeContainer)
                .await
                .expect("sandbox engine ready check"),
            "sandbox engine should honor the explicit CLI override",
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn container_exists_uses_runtime_neutral_inspect_probe() {
        let _serial = env_var_test_lock().lock().await;
        let temp = tempdir().expect("tempdir");
        let cli_path = temp.path().join("sandbox-cli.sh");
        let log_path = temp.path().join("sandbox-cli.log");
        std::fs::write(
            &cli_path,
            format!(
                "#!/bin/sh\nLOG=\"{log}\"\nprintf '%s\\n' \"$*\" >> \"$LOG\"\nif [ \"$1\" = \"container\" ] && [ \"$2\" = \"inspect\" ] && [ \"$3\" = \"ctx-harness-test\" ]; then\n  printf '[{{}}]\\n'\n  exit 0\nfi\necho \"unexpected invocation: $*\" >&2\nexit 1\n",
                log = log_path.display(),
            ),
        )
        .expect("write sandbox cli shim");
        std::fs::set_permissions(&cli_path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod sandbox cli shim");
        let _guard = EnvVarGuard::set(
            CTX_HARNESS_SANDBOX_CLI_PATH_ENV,
            &cli_path.to_string_lossy(),
        );

        assert!(
            container_exists(
                temp.path(),
                &SandboxCommandMode::NativeContainer,
                "ctx-harness-test",
            )
            .await
            .expect("container exists probe"),
            "inspect-based probe should treat the container as existing",
        );

        let log = std::fs::read_to_string(&log_path).expect("read sandbox cli log");
        assert!(
            log.contains("container inspect ctx-harness-test"),
            "expected inspect probe in log:\n{log}"
        );
        assert!(
            !log.contains("container exists"),
            "inspect-based probe should not call unsupported container exists:\n{log}"
        );
    }

    #[test]
    fn sandbox_cli_env_for_shared_vm_uses_guest_paths() {
        let env = sandbox_cli_env_for_mode(
            Path::new("/unused-host-root"),
            &SandboxCommandMode::SharedVm {
                helper_path: PathBuf::from("/tmp/helper"),
            },
        )
        .expect("shared vm env");

        assert_eq!(
            env.get("XDG_RUNTIME_DIR").map(String::as_str),
            Some(SHARED_VM_GUEST_ROOT_XDG_RUNTIME_ROOT)
        );
        assert_eq!(
            env.get("HOME").map(String::as_str),
            Some(SHARED_VM_GUEST_ROOT_HOME)
        );
        assert_eq!(
            env.get("TMPDIR").map(String::as_str),
            Some(SHARED_VM_GUEST_TMP_ROOT)
        );
        assert_eq!(
            env.get("XDG_CONFIG_HOME").map(String::as_str),
            Some(SHARED_VM_GUEST_ROOT_XDG_CONFIG_ROOT)
        );
        assert_eq!(
            env.get("XDG_DATA_HOME").map(String::as_str),
            Some(SHARED_VM_GUEST_ROOT_XDG_DATA_ROOT)
        );
        assert_eq!(
            env.get("XDG_CACHE_HOME").map(String::as_str),
            Some(SHARED_VM_GUEST_ROOT_XDG_CACHE_ROOT)
        );
    }

    #[tokio::test]
    async fn sandbox_container_command_shared_vm_does_not_leak_host_runtime_paths() {
        let _serial = env_var_test_lock().lock().await;
        let data_root = Path::new("/home/fixture/.ctx");
        let helper_path = PathBuf::from("/tmp/ctx-avf-linux-helper");
        let cmd = sandbox_container_command(
            data_root,
            &SandboxCommandMode::SharedVm {
                helper_path: helper_path.clone(),
            },
        )
        .expect("shared vm command");
        let std_cmd = cmd.as_std();

        assert_eq!(std_cmd.get_program(), helper_path.as_os_str());

        let args = std_cmd
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect::<Vec<_>>();
        let rendered = args.join("\n");

        assert!(
            rendered.contains("--env\nXDG_RUNTIME_DIR=/ctx/tmp/xdg-runtime-root"),
            "shared VM command must inject guest runtime dir env:\n{rendered}"
        );
        assert!(
            rendered.contains("--env\nHOME=/ctx/home/root"),
            "shared VM command must inject guest home env:\n{rendered}"
        );
        assert!(
            rendered.contains("--env\nTMPDIR=/ctx/tmp"),
            "shared VM command must inject guest tmp env:\n{rendered}"
        );
        assert!(
            !rendered.contains("/home/fixture/.ctx/sandbox/run"),
            "shared VM command must not leak host sandbox runtime paths:\n{rendered}"
        );
        assert!(
            !rendered.contains("/home/fixture/.ctx/sandbox/home"),
            "shared VM command must not leak host sandbox home paths:\n{rendered}"
        );
    }
}
