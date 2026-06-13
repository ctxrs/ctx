use super::*;
use ctx_sandbox_container_runtime::CTX_HARNESS_SANDBOX_CLI_PATH_ENV;

#[cfg(unix)]
#[tokio::test]
async fn shared_vm_container_launch_omits_slirp_network_flag() {
    let _serial = env_var_test_lock().lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let manager = runtime_manager(&temp).await;
    let helper_path = write_avf_linux_lifecycle_helper(temp.path());
    let sandbox_cli_path = write_ready_runtime_sandbox_cli_shim(temp.path());
    let _helper_guard = EnvGuard::set(AVF_LINUX_HELPER_PATH_ENV, &helper_path.to_string_lossy());
    let _sandbox_cli_available = EnvGuard::set("CTX_TEST_SANDBOX_CLI_AVAILABLE", "1");
    let _sandbox_cli_path = EnvGuard::set(
        CTX_HARNESS_SANDBOX_CLI_PATH_ENV,
        &sandbox_cli_path.to_string_lossy(),
    );
    let (_runtime_guard, servers) = install_test_managed_avf_linux_runtime_source().await;
    let image_bytes = b"ctx-harness-image".to_vec();
    let (image_url, image_server) =
        spawn_static_http_server_with_suffix(image_bytes.clone(), "ctx-harness.tar").await;
    let _image_guard = bundled_assets::override_managed_ctx_harness_image_source_for_test(
        bundled_assets::ManagedArtifactSource {
            uri: image_url,
            sha256: hex::encode(Sha256::digest(&image_bytes)),
        },
    );
    let workspace = sample_workspace(&temp);
    let worktree = sample_worktree(&temp, workspace.id);
    let settings = ExecutionSettings {
        mode: ExecutionMode::Sandbox,
        container: ContainerExecutionSettings {
            runtime: ctx_settings_model::ContainerRuntimeKind::SharedVmContainer,
            mount_mode: ContainerMountMode::DiskIsolated,
            network_mode: ContainerNetworkMode::All,
            ..ContainerExecutionSettings::default()
        },
    };

    manager
        .prepare(&workspace, &worktree, &settings, "http://192.168.64.1:4399")
        .await
        .expect("AVF prepare should use the shared-VM container runtime");

    let log_path = temp.path().join(format!(
        "managed/vms/avf-linux/{}/{}/shared/sandbox-cli-invocations.log",
        std::env::consts::OS,
        std::env::consts::ARCH
    ));
    let log = std::fs::read_to_string(&log_path).expect("read AVF sandbox CLI invocation log");
    let run_line = log
        .lines()
        .find(|line| line.contains("run") && line.contains("ctx-harness-"))
        .expect("shared VM sandbox CLI run invocation");
    assert!(
        !run_line.contains("slirp4netns:allow_host_loopback=true"),
        "shared VM run should not use stale slirp networking: {run_line}"
    );
    assert!(
        run_line.contains("--add-host host.containers.internal:host-gateway"),
        "shared VM run should keep host gateway mapping: {run_line}"
    );
    assert!(
        run_line.contains("--hostname ws-container"),
        "shared VM run should use the human-readable workspace hostname: {run_line}"
    );
    for (guest_src, dst_suffix) in [
        (
            format!(
                "/mnt/ctx-host/containers/workspaces/{}/data",
                workspace.id.0
            ),
            format!("/containers/workspaces/{}/data", workspace.id.0),
        ),
        (
            "/mnt/ctx-host/providers/agent-servers".to_string(),
            "/providers/agent-servers".to_string(),
        ),
        (
            "/mnt/ctx-host/runtimes".to_string(),
            "/runtimes".to_string(),
        ),
        (
            "/mnt/ctx-host/vcs-hooks".to_string(),
            "/vcs-hooks".to_string(),
        ),
    ] {
        assert!(
            run_line.contains(&format!("src={guest_src}")),
            "shared VM run should use guest-visible mount source {guest_src}: {run_line}"
        );
        assert!(
            run_line.contains(&format!("dst={}{}", temp.path().display(), dst_suffix)),
            "shared VM run should preserve destination path {}{}: {run_line}",
            temp.path().display(),
            dst_suffix
        );
        assert!(
            !run_line.contains(&format!("src={}{}", temp.path().display(), dst_suffix)),
            "shared VM run should not leak host data-root source {}{}: {run_line}",
            temp.path().display(),
            dst_suffix
        );
    }
    assert!(
        log.lines().any(|line| {
            line.contains("exec --user 0")
                && line.contains("CTX_CONTAINER_TERMINAL_USER=ctx-user")
                && line.contains("ctx-harness-")
        }),
        "shared VM prepare should synchronize the terminal identity inside the container: {log}"
    );

    for server in servers {
        server.abort();
    }
    image_server.abort();
}
