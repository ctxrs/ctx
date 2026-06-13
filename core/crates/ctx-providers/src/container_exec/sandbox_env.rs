use std::collections::HashMap;
use std::path::PathBuf;

use tokio::process::Command;

const SHARED_VM_SANDBOX_CLI_GUEST_HOME: &str = "/ctx/home/root";
const SHARED_VM_SANDBOX_CLI_GUEST_XDG_CONFIG: &str = "/ctx/cache/xdg/config";
const SHARED_VM_SANDBOX_CLI_GUEST_XDG_DATA: &str = "/ctx/cache/xdg/data";
const SHARED_VM_SANDBOX_CLI_GUEST_XDG_CACHE: &str = "/ctx/cache/xdg/cache";
const SHARED_VM_SANDBOX_CLI_GUEST_XDG_RUNTIME: &str = "/ctx/tmp/xdg-runtime-root";
const SHARED_VM_SANDBOX_CLI_GUEST_TMP: &str = "/ctx/tmp";

pub(super) fn apply_sandbox_cli_env(cmd: &mut Command, data_root: &str) {
    for (key, value) in sandbox_cli_env_for_data_root(data_root) {
        cmd.env(key, value);
    }
}

pub(super) fn sandbox_cli_env_for_data_root(data_root: &str) -> HashMap<String, String> {
    let data_root = data_root.trim();
    if data_root.is_empty() {
        return HashMap::new();
    }
    let root = PathBuf::from(data_root);
    let sandbox_root = root.join("sandbox");
    let xdg_root = sandbox_root.join("xdg");
    let xdg_config = xdg_root.join("config");
    let xdg_data = xdg_root.join("data");
    let xdg_run = sandbox_root.join("run");
    let sandbox_home = sandbox_root.join("home");
    let sandbox_tmp_root = sandbox_root.join("tmp");
    let _ = std::fs::create_dir_all(&xdg_config);
    let _ = std::fs::create_dir_all(&xdg_data);
    let _ = std::fs::create_dir_all(&xdg_run);
    let _ = std::fs::create_dir_all(&sandbox_home);
    let _ = std::fs::create_dir_all(&sandbox_tmp_root);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&xdg_run, std::fs::Permissions::from_mode(0o700));
        let _ = std::fs::set_permissions(&sandbox_home, std::fs::Permissions::from_mode(0o700));
    }

    let tmp = sandbox_tmp_root.to_string_lossy().to_string();
    HashMap::from([
        (
            "XDG_CONFIG_HOME".to_string(),
            xdg_config.to_string_lossy().to_string(),
        ),
        (
            "XDG_DATA_HOME".to_string(),
            xdg_data.to_string_lossy().to_string(),
        ),
        (
            "XDG_CACHE_HOME".to_string(),
            xdg_root.join("cache").to_string_lossy().to_string(),
        ),
        (
            "XDG_RUNTIME_DIR".to_string(),
            xdg_run.to_string_lossy().to_string(),
        ),
        (
            "HOME".to_string(),
            sandbox_home.to_string_lossy().to_string(),
        ),
        (
            "CONTAINERD_ADDRESS".to_string(),
            "/run/containerd/containerd.sock".to_string(),
        ),
        ("CONTAINERD_NAMESPACE".to_string(), "default".to_string()),
        ("TMPDIR".to_string(), tmp.clone()),
        ("TMP".to_string(), tmp.clone()),
        ("TEMP".to_string(), tmp),
    ])
}

pub(super) fn shared_vm_sandbox_cli_guest_env() -> HashMap<String, String> {
    let tmp = SHARED_VM_SANDBOX_CLI_GUEST_TMP.to_string();
    HashMap::from([
        (
            "XDG_CONFIG_HOME".to_string(),
            SHARED_VM_SANDBOX_CLI_GUEST_XDG_CONFIG.to_string(),
        ),
        (
            "XDG_DATA_HOME".to_string(),
            SHARED_VM_SANDBOX_CLI_GUEST_XDG_DATA.to_string(),
        ),
        (
            "XDG_CACHE_HOME".to_string(),
            SHARED_VM_SANDBOX_CLI_GUEST_XDG_CACHE.to_string(),
        ),
        (
            "XDG_RUNTIME_DIR".to_string(),
            SHARED_VM_SANDBOX_CLI_GUEST_XDG_RUNTIME.to_string(),
        ),
        (
            "HOME".to_string(),
            SHARED_VM_SANDBOX_CLI_GUEST_HOME.to_string(),
        ),
        (
            "CONTAINERD_ADDRESS".to_string(),
            "/run/containerd/containerd.sock".to_string(),
        ),
        ("CONTAINERD_NAMESPACE".to_string(), "default".to_string()),
        ("TMPDIR".to_string(), tmp.clone()),
        ("TMP".to_string(), tmp.clone()),
        ("TEMP".to_string(), tmp),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_cli_env_for_data_root_uses_shared_sandbox_paths() {
        let temp = tempfile::tempdir().expect("tempdir");
        let env = sandbox_cli_env_for_data_root(&temp.path().to_string_lossy());
        let sandbox_root = temp.path().join("sandbox");

        let runtime_dir = env.get("XDG_RUNTIME_DIR").cloned().expect("runtime dir");
        let home = env.get("HOME").cloned().expect("home");
        let tmpdir = env.get("TMPDIR").cloned().expect("tmpdir");

        assert_eq!(PathBuf::from(runtime_dir), sandbox_root.join("run"));
        assert_eq!(PathBuf::from(home), sandbox_root.join("home"));
        assert_eq!(PathBuf::from(tmpdir), sandbox_root.join("tmp"));
        assert_eq!(
            env.get("XDG_CONFIG_HOME").map(PathBuf::from),
            Some(sandbox_root.join("xdg").join("config"))
        );
        assert_eq!(
            env.get("XDG_DATA_HOME").map(PathBuf::from),
            Some(sandbox_root.join("xdg").join("data"))
        );
        assert_eq!(
            env.get("XDG_CACHE_HOME").map(PathBuf::from),
            Some(sandbox_root.join("xdg").join("cache"))
        );
        assert_eq!(
            env.get("CONTAINERD_ADDRESS").map(String::as_str),
            Some("/run/containerd/containerd.sock")
        );
        assert_eq!(
            env.get("CONTAINERD_NAMESPACE").map(String::as_str),
            Some("default")
        );
    }

    #[test]
    fn shared_vm_sandbox_cli_guest_env_uses_ctx_paths() {
        let env = shared_vm_sandbox_cli_guest_env();
        assert_eq!(
            env.get("XDG_CONFIG_HOME").map(String::as_str),
            Some("/ctx/cache/xdg/config")
        );
        assert_eq!(
            env.get("XDG_DATA_HOME").map(String::as_str),
            Some("/ctx/cache/xdg/data")
        );
        assert_eq!(
            env.get("XDG_CACHE_HOME").map(String::as_str),
            Some("/ctx/cache/xdg/cache")
        );
        assert_eq!(
            env.get("XDG_RUNTIME_DIR").map(String::as_str),
            Some("/ctx/tmp/xdg-runtime-root")
        );
        assert_eq!(env.get("HOME").map(String::as_str), Some("/ctx/home/root"));
        assert_eq!(env.get("TMPDIR").map(String::as_str), Some("/ctx/tmp"));
        assert_eq!(env.get("TMP").map(String::as_str), Some("/ctx/tmp"));
        assert_eq!(env.get("TEMP").map(String::as_str), Some("/ctx/tmp"));
    }
}
