use super::*;

#[test]
fn sandbox_cli_binary_path_uses_env_override() {
    let _serial = env_var_test_lock().blocking_lock();
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_string_lossy().to_string();
    let _guard = EnvGuard::set("CTX_HARNESS_SANDBOX_CLI_PATH", &path);
    let resolved = sandbox_cli_binary_path(Path::new("/tmp")).expect("env override should resolve");
    assert_eq!(resolved, tmp.path());
}

#[test]
fn sandbox_cli_invocation_sets_paths_under_sandbox_root() {
    let _serial = env_var_test_lock().blocking_lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let sandbox_cli_bin = tempfile::NamedTempFile::new().expect("sandbox CLI bin");
    let _guard = EnvGuard::set(
        "CTX_HARNESS_SANDBOX_CLI_PATH",
        &sandbox_cli_bin.path().to_string_lossy(),
    );
    let inv = sandbox_cli_invocation(tmp.path()).expect("sandbox CLI invocation");
    let runtime_dir = inv
        .env
        .get("XDG_RUNTIME_DIR")
        .cloned()
        .expect("runtime dir env");
    let home = inv.env.get("HOME").cloned().expect("home env");
    let tmpdir = inv.env.get("TMPDIR").cloned().expect("tmpdir env");
    assert_eq!(
        PathBuf::from(runtime_dir),
        tmp.path().join("sandbox").join("run")
    );
    assert_eq!(PathBuf::from(home), tmp.path().join("sandbox").join("home"));
    assert_eq!(
        PathBuf::from(tmpdir),
        tmp.path().join("sandbox").join("tmp")
    );
    assert_eq!(
        inv.env.get("CONTAINERD_ADDRESS").map(String::as_str),
        Some("/run/containerd/containerd.sock")
    );
    assert_eq!(
        inv.env.get("CONTAINERD_NAMESPACE").map(String::as_str),
        Some("default")
    );
}

#[test]
fn keep_id_userns_is_only_enabled_on_linux() {
    assert_eq!(should_use_keep_id_userns(), cfg!(target_os = "linux"));
}

#[test]
fn rewrite_daemon_url_for_avf_guest_uses_guest_gateway_host() {
    let rewritten = rewrite_daemon_url_for_avf_guest("http://127.0.0.1:4399");
    assert_eq!(rewritten, "http://192.168.64.1:4399/");
}
