use super::fixtures::*;
use super::*;

#[tokio::test]
async fn seed_shared_sandbox_machine_cache_populates_local_cache_root() {
    let _serial = env_var_test_lock().lock().await;
    let shared = tempfile::tempdir().expect("shared tempdir");
    let data_root = tempfile::tempdir().expect("data root tempdir");
    let _guard = EnvGuard::set(
        "CTX_SANDBOX_MACHINE_CACHE_DIR",
        &shared.path().to_string_lossy(),
    );
    let relpath = PathBuf::from("applehv")
        .join("cache")
        .join("78e5fea350d7.raw.zst");
    let shared_file = shared.path().join(&relpath);
    std::fs::create_dir_all(shared_file.parent().expect("shared cache parent"))
        .expect("create shared cache dir");
    std::fs::write(&shared_file, b"seeded-machine-cache").expect("write shared cache file");

    seed_shared_sandbox_machine_cache(data_root.path(), None)
        .await
        .expect("seed shared cache");

    let local_file = sandbox_machine_cache_root(data_root.path()).join(relpath);
    let local_body = std::fs::read(&local_file).expect("read local cache file");
    assert_eq!(local_body, b"seeded-machine-cache");
}

#[tokio::test]
async fn persist_sandbox_machine_cache_to_shared_does_not_depend_on_local_path() {
    let _serial = env_var_test_lock().lock().await;
    let shared = tempfile::tempdir().expect("shared tempdir");
    let data_root = tempfile::tempdir().expect("data root tempdir");
    let _guard = EnvGuard::set(
        "CTX_SANDBOX_MACHINE_CACHE_DIR",
        &shared.path().to_string_lossy(),
    );
    let relpath = PathBuf::from("applehv")
        .join("cache")
        .join("persisted-machine-cache.raw.zst");
    let local_file = sandbox_machine_cache_root(data_root.path()).join(&relpath);
    std::fs::create_dir_all(local_file.parent().expect("local cache parent"))
        .expect("create local cache dir");
    std::fs::write(&local_file, b"persisted-machine-cache").expect("write local cache file");

    persist_sandbox_machine_cache_to_shared(data_root.path(), None)
        .await
        .expect("persist shared cache");

    std::fs::remove_file(&local_file).expect("remove local cache file");
    let shared_file = shared.path().join(relpath);
    let shared_body = std::fs::read(&shared_file).expect("read shared cache file");
    assert_eq!(shared_body, b"persisted-machine-cache");
}

#[tokio::test]
async fn ensure_sandbox_machine_download_skips_when_machine_lock_is_busy() {
    use std::os::unix::fs::PermissionsExt;

    let _serial = env_var_test_lock().lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let log_path = temp.path().join("sandbox-cli-invocations.log");
    let sandbox_cli_path = temp.path().join("sandbox-cli.sh");
    std::fs::write(
        &sandbox_cli_path,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"{}\"\nexit 0\n",
            log_path.display()
        ),
    )
    .expect("write sandbox CLI shim");
    std::fs::set_permissions(&sandbox_cli_path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod sandbox CLI shim");
    let _guard = EnvGuard::set(
        "CTX_HARNESS_SANDBOX_CLI_PATH",
        &sandbox_cli_path.to_string_lossy(),
    );
    let _host_memory = EnvGuard::set("CTX_TEST_HOST_MEMORY_MB", "49152");
    let (_machine_cache_guard, machine_cache_server) =
        install_test_managed_machine_cache_source(b"machine-cache".to_vec()).await;

    let manager = HarnessRuntimeManager::new(temp.path().to_path_buf());
    let machine_name = sandbox_machine_name(temp.path());
    let machine_lock = sandbox_machine_singleflight_lock(&machine_name);
    let _machine_guard = machine_lock.lock().await;

    manager
        .ensure_sandbox_machine_download()
        .await
        .expect("prefetch should skip while launch holds the machine lock");

    let log = std::fs::read_to_string(&log_path).unwrap_or_default();
    assert!(log.trim().is_empty());
    machine_cache_server.abort();
}
