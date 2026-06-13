use super::*;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

#[cfg(unix)]
struct EnvVarGuard {
    key: &'static str,
    prev: Option<String>,
}

#[cfg(unix)]
impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let prev = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, prev }
    }
}

#[cfg(unix)]
impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(prev) = self.prev.take() {
            std::env::set_var(self.key, prev);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

#[cfg(unix)]
fn env_var_test_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn sample_container_settings() -> ContainerExecutionSettings {
    ContainerExecutionSettings::default()
}

fn sample_cached_container() -> WorkspaceContainer {
    WorkspaceContainer {
        name: "ctx-harness-test".to_string(),
        mount_mode: ContainerMountMode::DiskIsolated,
        network_mode: ContainerNetworkMode::LlmOnly,
        allowlist: Vec::new(),
        external_mounts: HashSet::from(["/tmp/hooks".to_string()]),
        egress_guard: true,
    }
}

#[test]
fn cached_container_action_reuses_when_mounts_and_network_match() {
    let cached = sample_cached_container();
    let settings = sample_container_settings();
    let action = cached_container_action(&cached, &settings, &cached.external_mounts);
    assert_eq!(action, CachedContainerAction::Reuse);
}

#[test]
fn cached_container_action_recreates_when_mount_mode_changes() {
    let cached = sample_cached_container();
    let mut settings = sample_container_settings();
    settings.mount_mode = ContainerMountMode::Legacy;
    let action = cached_container_action(&cached, &settings, &cached.external_mounts);
    assert_eq!(action, CachedContainerAction::Recreate);
}

#[test]
fn cached_container_action_reconfigures_when_allowlist_changes() {
    let cached = sample_cached_container();
    let mut settings = sample_container_settings();
    settings.allowlist = vec!["example.com".to_string()];
    let action = cached_container_action(&cached, &settings, &cached.external_mounts);
    assert_eq!(action, CachedContainerAction::Reconfigure);
}

#[test]
fn bundle_dir_mount_policy_matches_platform_expectations() {
    if cfg!(target_os = "linux") {
        assert!(container::should_mount_bundle_dir_in_container(Path::new(
            "/Applications/ctx.app/Contents/Resources/bundles"
        )));
        assert!(!container::should_mount_bundle_dir_in_container(Path::new(
            "/tmp/.mount_ctx.ApaHoLEe/usr/lib/ctx/bundles"
        )));
        assert!(!container::should_mount_bundle_dir_in_container(Path::new(
            "/var/tmp/.mount_ctx.ApaHoLEe/usr/lib/ctx/bundles"
        )));
        return;
    }
    if cfg!(target_os = "macos") || cfg!(target_os = "windows") {
        assert!(!container::should_mount_bundle_dir_in_container(Path::new(
            "/Applications/ctx.app/Contents/Resources/bundles"
        )));
        let home_var = if cfg!(target_os = "windows") {
            "USERPROFILE"
        } else {
            "HOME"
        };
        if let Some(home) = std::env::var_os(home_var).map(PathBuf::from) {
            assert!(!container::should_mount_bundle_dir_in_container(
                &home.join("ctx-bundles")
            ));
        }
    }
}

#[test]
fn shared_vm_container_launch_networking_uses_default_bridge() {
    let settings = ContainerExecutionSettings {
        runtime: ContainerRuntimeKind::SharedVmContainer,
        ..ContainerExecutionSettings::default()
    };
    let networking = sandbox_container_launch_networking(&settings);
    assert_eq!(networking.network, None);
    assert_eq!(networking.add_host, "host.containers.internal:host-gateway");
}

#[cfg(unix)]
#[tokio::test]
async fn running_workspace_container_names_filters_ctx_workspace_containers() {
    let _serial = env_var_test_lock().lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let cli_path = temp.path().join("sandbox-cli.sh");
    let log_path = temp.path().join("sandbox-cli.log");
    std::fs::write(
            &cli_path,
            format!(
                "#!/bin/sh\nLOG=\"{log}\"\nprintf '%s\\n' \"$*\" >> \"$LOG\"\nif [ \"$1\" = \"container\" ] && [ \"$2\" = \"ls\" ] && [ \"$3\" = \"--format\" ] && [ \"$4\" = \"{{{{.Names}}}}\" ]; then\n  printf 'ctx-harness-one\\nctx-harness-two\\npostgres\\n\\n'\n  exit 0\nfi\necho \"unexpected invocation: $*\" >&2\nexit 1\n",
                log = log_path.display(),
            ),
        )
        .expect("write sandbox cli shim");
    std::fs::set_permissions(&cli_path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod sandbox cli shim");
    let _guard = EnvVarGuard::set(
        ctx_sandbox_container_runtime::CTX_HARNESS_SANDBOX_CLI_PATH_ENV,
        &cli_path.to_string_lossy(),
    );

    let names =
        list_running_workspace_container_names(temp.path(), &SandboxCommandMode::NativeContainer)
            .await
            .expect("list running workspace containers");
    assert_eq!(names, vec!["ctx-harness-one", "ctx-harness-two"]);
    let log = std::fs::read_to_string(&log_path).expect("read sandbox cli log");
    assert!(
        log.contains("container ls --format {{.Names}}"),
        "expected running-container probe in log:\n{log}"
    );
}
