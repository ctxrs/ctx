use super::*;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::Deserialize;
use serde_yaml::Value;
use std::io::Write;

fn git(args: &[&str], cwd: &Path) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .expect("run git");
    assert!(status.success(), "git {:?} failed", args);
}

fn init_git_repo(root: &Path) {
    git(&["init"], root);
    git(&["symbolic-ref", "HEAD", "refs/heads/main"], root);
}

fn wait_for_child_exit(child: &mut std::process::Child, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if child.try_wait().expect("poll child exit").is_some() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    child.try_wait().expect("poll child exit").is_some()
}

fn gibibytes(value: u64) -> u64 {
    value * 1024 * 1024 * 1024
}

fn write_gzip_file(path: &Path, bytes: &[u8]) {
    let file = File::create(path).expect("create gzip file");
    let mut encoder = GzEncoder::new(file, Compression::default());
    encoder.write_all(bytes).expect("write gzip payload");
    encoder.finish().expect("finish gzip payload");
}

const CONTROLLER_SAFETY_PREFLIGHT_FIXTURE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../../.ctx/exec-plans/apple-silicon-container-platform-benchmark-20260326/controller_safety_trace_fixture.json"
));

#[derive(Debug, Deserialize)]
struct ControllerSafetyFixture {
    gate_pass_requirements: ControllerSafetyGatePassRequirements,
    cases: Vec<ControllerSafetyFixtureCase>,
}

#[derive(Debug, Deserialize)]
struct ControllerSafetyGatePassRequirements {
    all_cases_must_pass: bool,
    exact_expected_output_match: bool,
    canonical_json_repeat_match: bool,
}

#[derive(Debug, Deserialize)]
struct ControllerSafetyFixtureCase {
    case_id: String,
    initial_state: ControllerSafetyInitialState,
    required_invariants: Vec<String>,
    steps: Vec<ControllerSafetyFixtureStep>,
}

#[derive(Debug, Deserialize)]
struct ControllerSafetyInitialState {
    target_bytes: u64,
    floor_bytes: u64,
    ceiling_bytes: u64,
    pressure_state: String,
    cooldown_remaining_ms: u64,
}

#[derive(Debug, Deserialize)]
struct ControllerSafetyFixtureStep {
    step_index: u64,
    time_since_start_ms: u64,
    time_since_last_step_ms: u64,
    phase: String,
    host: ControllerSafetyFixtureHost,
    guest: ControllerSafetyFixtureGuest,
    expected: ControllerSafetyExpectedDecision,
}

#[derive(Debug, Deserialize)]
struct ControllerSafetyFixtureHost {
    available_bytes: u64,
    pressure_state: String,
    compressor_delta_bytes: u64,
    pageout_delta_bytes: u64,
    swap_delta_bytes: u64,
}

#[derive(Debug, Deserialize)]
struct ControllerSafetyFixtureGuest {
    working_set_bytes: u64,
    reclaimable_bytes: u64,
    available_bytes: u64,
    swap_bytes: u64,
    under_pressure: bool,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct ControllerSafetyExpectedDecision {
    step_index: u64,
    action: String,
    target_bytes_before: u64,
    target_bytes_after: u64,
    pressure_state_before: String,
    pressure_state_after: String,
    reason_codes: Vec<String>,
    emergency_path: bool,
    invariants_passed: Vec<String>,
}

fn load_controller_safety_preflight_fixture() -> ControllerSafetyFixture {
    serde_json::from_str(CONTROLLER_SAFETY_PREFLIGHT_FIXTURE)
        .expect("controller-safety preflight fixture should parse")
}

fn controller_safety_replay_state(
    initial_state: &ControllerSafetyInitialState,
) -> SharedVmControllerSafetyReplayState {
    SharedVmControllerSafetyReplayState {
        target_bytes: initial_state.target_bytes,
        floor_bytes: initial_state.floor_bytes,
        ceiling_bytes: initial_state.ceiling_bytes,
        pressure_state: SharedVmControllerSafetyPressureState::try_from(
            initial_state.pressure_state.as_str(),
        )
        .expect("fixture pressure state"),
        cooldown_remaining_ms: initial_state.cooldown_remaining_ms,
    }
}

fn controller_safety_replay_steps(
    steps: &[ControllerSafetyFixtureStep],
) -> Vec<SharedVmControllerSafetyReplayStep> {
    steps
        .iter()
        .map(|step| SharedVmControllerSafetyReplayStep {
            step_index: step.step_index,
            time_since_start_ms: step.time_since_start_ms,
            time_since_last_step_ms: step.time_since_last_step_ms,
            phase: SharedVmControllerSafetyReplayPhase::try_from(step.phase.as_str())
                .expect("fixture replay phase"),
            host_available_bytes: step.host.available_bytes,
            host_pressure_state: SharedVmControllerSafetyHostPressureState::try_from(
                step.host.pressure_state.as_str(),
            )
            .expect("fixture host pressure state"),
            host_compressor_delta_bytes: step.host.compressor_delta_bytes,
            host_pageout_delta_bytes: step.host.pageout_delta_bytes,
            host_swap_delta_bytes: step.host.swap_delta_bytes,
            guest_working_set_bytes: step.guest.working_set_bytes,
            guest_reclaimable_bytes: step.guest.reclaimable_bytes,
            guest_available_bytes: step.guest.available_bytes,
            guest_swap_bytes: step.guest.swap_bytes,
            guest_under_pressure: step.guest.under_pressure,
        })
        .collect()
}

fn normalize_controller_safety_decision(
    decision: &SharedVmControllerSafetyReplayDecision,
) -> ControllerSafetyExpectedDecision {
    ControllerSafetyExpectedDecision {
        step_index: decision.step_index,
        action: decision.action.to_string(),
        target_bytes_before: decision.target_bytes_before,
        target_bytes_after: decision.target_bytes_after,
        pressure_state_before: decision.pressure_state_before.to_string(),
        pressure_state_after: decision.pressure_state_after.to_string(),
        reason_codes: decision
            .reason_codes
            .iter()
            .map(|value| (*value).to_string())
            .collect(),
        emergency_path: decision.emergency_path,
        invariants_passed: decision
            .invariants_passed
            .iter()
            .map(|value| (*value).to_string())
            .collect(),
    }
}

#[test]
fn load_state_defaults_missing_guest_identity_to_supported_shape() {
    let temp = PathBuf::from("/tmp").join(format!(
        "ctxavf-legacy-shared-state-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    fs::create_dir_all(&temp).expect("create tempdir");
    let state_path = temp.join("shared-vm-state.json");
    fs::write(&state_path, br#"{"state":"stopped"}"#).expect("write legacy shared vm state");

    let persisted = load_state(&state_path)
        .expect("load legacy shared vm state")
        .expect("shared vm state present");

    assert_eq!(persisted.guest_identity, supported_guest_identity());
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[test]
fn load_guest_worktree_state_defaults_missing_guest_identity_to_supported_shape() {
    let temp = PathBuf::from("/tmp").join(format!(
        "ctxavf-legacy-worktree-state-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    fs::create_dir_all(&temp).expect("create tempdir");
    let metadata_path = temp.join("worktree.json");
    fs::write(
        &metadata_path,
        format!(
            concat!(
                "{{",
                "\"workspace_id\":\"ws-123\",",
                "\"worktree_id\":\"wt-456\",",
                "\"host_workspace_root\":\"{}\",",
                "\"guest_root\":\"/ctx/ws/worktrees/wt-456\",",
                "\"host_shadow_root\":\"{}\",",
                "\"base_commit_sha\":\"abc123\",",
                "\"branch_name\":\"ctx/test\",",
                "\"updated_at\":\"{}\"",
                "}}"
            ),
            temp.join("repo").display(),
            temp.join("shadow-root").display(),
            now_timestamp_string()
        ),
    )
    .expect("write legacy guest worktree state");

    let persisted = load_guest_worktree_state(&metadata_path)
        .expect("load legacy guest worktree state")
        .expect("guest worktree state present");

    assert_eq!(persisted.guest_identity, supported_guest_identity());
    assert!(persisted.simulated);
    assert!(persisted.notes.is_empty());
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[test]
fn parse_guest_exec_env_rejects_reserved_helper_keys() {
    let err = parse_guest_exec_env(&["CTX_AVF_SECRET=1".to_string()])
        .expect_err("reserved helper keys should be rejected");
    assert!(err.to_string().contains("reserved"));
}

#[cfg(unix)]
#[test]
fn shared_vm_control_socket_root_is_user_scoped() {
    assert_eq!(
        shared_vm_control_socket_root(),
        PathBuf::from("/tmp").join(format!("ctxavf-uid-{}", unsafe { libc::geteuid() }))
    );
}

#[test]
fn shared_vm_saved_state_path_uses_host_private_root_outside_guest_share() {
    let data_root = PathBuf::from("/tmp").join(format!(
        "ctxavf-saved-state-root-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    let host_private_root = shared_vm_host_private_root(&data_root);
    let saved_state_path = shared_vm_saved_state_path(&data_root);

    assert!(
        !saved_state_path.starts_with(&data_root),
        "saved state should live outside the guest-shared data root"
    );
    assert_eq!(
        saved_state_path,
        host_private_root.join(SHARED_VM_SAVED_STATE_FILE)
    );
    assert!(saved_state_path
        .components()
        .any(|component| component.as_os_str() == ".ctx-avf-host-private"));
}

#[test]
fn runtime_without_guest_agent_stays_simulated() {
    let temp = PathBuf::from("/tmp").join(format!(
        "ctxavf-capability-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    fs::create_dir_all(temp.join("helpers")).expect("create helpers dir");
    let (enabled, note) = shared_vm_runtime_supports_real_guest_exec(&temp);
    assert!(!enabled);
    assert!(note.contains("missing"));
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[test]
fn runtime_with_guest_agent_can_enable_real_vm_path() {
    let temp = PathBuf::from("/tmp").join(format!(
        "ctxavf-capability-agent-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    let helper_path = temp.join("helpers").join(AVF_LINUX_GUEST_AGENT_HELPER);
    fs::create_dir_all(helper_path.parent().expect("helper parent")).expect("helpers dir");
    fs::write(&helper_path, b"guest-agent").expect("guest-agent helper");
    let (enabled, note) = shared_vm_runtime_supports_real_guest_exec(&temp);
    assert!(enabled);
    assert!(note.contains("guest-agent payload"));
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[test]
fn build_probe_reports_host_level_save_restore_scope() {
    let probe = build_probe().expect("build probe");
    let expected_scope = if probe.save_restore_supported {
        AvfLinuxSaveRestoreCapabilityScope::HostPrerequisitesOnly
    } else {
        AvfLinuxSaveRestoreCapabilityScope::Unsupported
    };
    assert_eq!(probe.save_restore_capability_scope, expected_scope);
}

#[test]
fn probe_scopes_save_restore_as_host_prerequisites_or_unsupported() {
    let probe = build_probe().expect("build probe");
    if probe.save_restore_supported {
        assert!(matches!(
            probe.save_restore_capability_scope,
            AvfLinuxSaveRestoreCapabilityScope::HostPrerequisitesOnly
        ));
        assert!(probe.notes.iter().any(|note| {
            note.contains("host satisfies AVF save/restore prerequisites")
                || note.contains("host prerequisites only")
        }));
    } else {
        assert!(matches!(
            probe.save_restore_capability_scope,
            AvfLinuxSaveRestoreCapabilityScope::Unsupported
        ));
    }
}

#[test]
fn resolve_avf_vm_sizing_defaults_to_host_logical_cpu_count_and_reserved_memory() {
    let sizing = resolve_avf_vm_sizing(
        1,
        16,
        gibibytes(2),
        gibibytes(64),
        12,
        gibibytes(32),
        None,
        None,
    );

    assert_eq!(sizing.cpu_count, 12);
    assert_eq!(sizing.memory_size_bytes, gibibytes(28));
    assert!(sizing.policy_note.contains("host logical CPU count"));
    assert!(sizing
        .policy_note
        .contains("host RAM minus 4096 MiB reserve"));
}

#[test]
fn resolve_avf_vm_sizing_clamps_defaults_to_avf_limits_and_memory_floor() {
    let sizing = resolve_avf_vm_sizing(
        2,
        8,
        gibibytes(2),
        gibibytes(64),
        32,
        gibibytes(6),
        None,
        None,
    );

    assert_eq!(sizing.cpu_count, 8);
    assert_eq!(sizing.memory_size_bytes, gibibytes(4));
}

#[test]
fn resolve_avf_vm_sizing_applies_debug_overrides() {
    let sizing = resolve_avf_vm_sizing(
        1,
        8,
        gibibytes(2),
        gibibytes(10),
        4,
        gibibytes(32),
        Some(6),
        Some(gibibytes(12) + 1234),
    );

    assert_eq!(sizing.cpu_count, 6);
    assert_eq!(sizing.memory_size_bytes, gibibytes(10));
    assert!(sizing.policy_note.contains(SHARED_VM_CPU_COUNT_ENV));
    assert!(sizing
        .policy_note
        .contains(SHARED_VM_MEMORY_CEILING_BYTES_ENV));
}

#[test]
fn stage_shadow_root_from_host_workspace_copies_standalone_repo() {
    let temp = PathBuf::from("/tmp").join(format!(
        "ctxavf-shadow-copy-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    let repo_root = temp.join("repo");
    fs::create_dir_all(&repo_root).expect("create repo root");
    init_git_repo(&repo_root);
    git(&["config", "user.email", "test@example.com"], &repo_root);
    git(&["config", "user.name", "Test User"], &repo_root);
    fs::write(repo_root.join("README.md"), "hello\n").expect("write readme");
    git(&["add", "README.md"], &repo_root);
    git(&["commit", "-m", "initial"], &repo_root);

    let shadow_root = temp.join("shadow-root");
    stage_shadow_root_from_host_workspace(&repo_root, &shadow_root)
        .expect("stage shadow root from standalone repo");

    assert!(shadow_root.join("README.md").exists());
    assert!(shadow_root.join(".git").is_dir());
    git(&["rev-parse", "--is-inside-work-tree"], &shadow_root);
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[test]
fn stage_shadow_root_from_host_workspace_expands_linked_git_worktree() {
    let temp = PathBuf::from("/tmp").join(format!(
        "ctxavf-shadow-copy-worktree-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    let repo_root = temp.join("repo");
    fs::create_dir_all(&repo_root).expect("create repo root");
    init_git_repo(&repo_root);
    git(&["config", "user.email", "test@example.com"], &repo_root);
    git(&["config", "user.name", "Test User"], &repo_root);
    fs::write(repo_root.join("README.md"), "hello\n").expect("write readme");
    git(&["add", "README.md"], &repo_root);
    git(&["commit", "-m", "initial"], &repo_root);

    let worktree_root = temp.join("linked-worktree");
    let worktree_root_str = worktree_root.to_string_lossy().into_owned();
    git(
        &[
            "worktree",
            "add",
            "-b",
            "ctx/test-shadow-copy",
            worktree_root_str.as_str(),
        ],
        &repo_root,
    );
    assert!(
        !worktree_root.join(".git").is_dir(),
        "source worktree should remain linked"
    );

    let shadow_root = temp.join("shadow-root");
    stage_shadow_root_from_host_workspace(&worktree_root, &shadow_root)
        .expect("stage shadow root from linked worktree");

    assert!(shadow_root.join("README.md").exists());
    assert!(shadow_root.join(".git").is_dir());
    assert!(!shadow_root.join(".git").join("commondir").exists());
    assert!(!shadow_root.join(".git").join("gitdir").exists());
    git(&["rev-parse", "--is-inside-work-tree"], &shadow_root);
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[test]
fn cloud_init_user_data_embeds_guest_agent_and_service() {
    let user_data = render_shared_vm_cloud_init_user_data(
        Path::new("/tmp"),
        b"guest-agent",
        Some(&b"egress-proxy"[..]),
        Path::new("/tmp/runtime/helpers/container-stack.tar.gz"),
        "deadbeef",
    )
    .expect("render cloud-init user-data");
    assert!(user_data.contains("#cloud-config"));
    assert!(user_data.contains("/usr/local/bin/ctx-avf-linux-guest-agent"));
    assert!(user_data.contains("/usr/local/bin/ctx-egress-proxy"));
    assert!(user_data.contains(SHARED_VM_DATA_DISK_INSTALL_PATH));
    assert!(user_data.contains(SHARED_VM_GUEST_POLICY_INSTALL_PATH));
    assert!(user_data.contains("/usr/local/lib/ctx/ctx-avf-install-container-stack.sh"));
    assert!(user_data.contains(SHARED_VM_DATA_DISK_SERVICE_NAME));
    assert!(user_data.contains(SHARED_VM_GUEST_AGENT_SERVICE_NAME));
    assert!(user_data.contains(SHARED_VM_CONTAINERD_SERVICE_NAME));
    assert!(user_data.contains(SHARED_VM_BUILDKIT_SERVICE_NAME));
    assert!(user_data.contains("systemctl enable --now ctx-avf-data-disk.service"));
    assert!(user_data.contains(&format!(
        "systemctl enable --now {service_name}",
        service_name = SHARED_VM_HOST_DATA_SERVICE_NAME
    )));
    assert!(user_data.contains("systemctl enable --now ctx-avf-linux-guest-agent.service"));
    assert!(user_data.contains("systemctl enable --now containerd.service"));
    assert!(user_data.contains("systemctl enable --now buildkit.service"));
    assert!(user_data.contains("mount_root='/ctx'"));
    for (name, path) in [
        ("worktrees_root", SHARED_VM_GUEST_WORKTREES_ROOT),
        ("home_root", SHARED_VM_GUEST_HOME_ROOT),
        ("cache_root", SHARED_VM_GUEST_CACHE_ROOT),
        ("tmp_root", SHARED_VM_GUEST_TMP_ROOT),
        ("log_root", SHARED_VM_GUEST_LOG_ROOT),
        ("containerd_root", SHARED_VM_GUEST_CONTAINERD_ROOT),
        ("buildkit_root", SHARED_VM_GUEST_BUILDKIT_ROOT),
        ("nerdctl_root", SHARED_VM_GUEST_NERDCTL_ROOT),
        ("cni_config_root", SHARED_VM_GUEST_CNI_CONFIG_ROOT),
        ("cni_state_root", SHARED_VM_GUEST_CNI_STATE_ROOT),
        ("root_home", SHARED_VM_GUEST_ROOT_HOME),
        ("root_xdg_config", SHARED_VM_GUEST_ROOT_XDG_CONFIG_ROOT),
        ("root_xdg_data", SHARED_VM_GUEST_ROOT_XDG_DATA_ROOT),
        ("root_xdg_cache", SHARED_VM_GUEST_ROOT_XDG_CACHE_ROOT),
        ("root_xdg_runtime", SHARED_VM_GUEST_ROOT_XDG_RUNTIME_ROOT),
    ] {
        assert!(
            user_data.contains(&format!("{name}='{path}'")),
            "missing writable-surface root {name}={path}"
        );
    }
    assert!(user_data.contains("chmod 1777 \"$tmp_root\""));
    assert!(user_data.contains("current_tmp_source=\"$(findmnt -n -o SOURCE /tmp"));
    for (src, dest) in [
        ("$root_home", "/root"),
        ("$tmp_root", "/tmp"),
        ("$tmp_root", "/var/tmp"),
        ("$log_root", "/var/log"),
        ("$containerd_root", "/var/lib/containerd"),
        ("$buildkit_root", "/var/lib/buildkit"),
        ("$nerdctl_root", "/var/lib/nerdctl"),
        ("$cni_config_root", "/etc/cni/net.d"),
        ("$cni_state_root", "/var/lib/cni"),
    ] {
        assert!(
            user_data.contains(&format!("mount --bind \"{src}\" {dest}")),
            "missing bind mount {src} -> {dest}"
        );
    }
    assert!(user_data.contains("chmod 1777 /tmp"));
    assert!(user_data.contains("chmod 1777 /var/tmp"));
    assert!(user_data.contains("chmod 0700 /root"));
    assert!(user_data.contains(".ctx-avf-data-disk-ready"));
    assert!(user_data.contains("/etc/cni/net.d/10-nerdctl.conflist"));
    assert!(user_data.contains("\"bridge\": \"nerdctl0\""));
    assert!(user_data.contains("guest policy masking"));
    assert!(user_data.contains("guest policy already masked"));
    assert!(user_data.contains("ln -s /dev/null"));
    assert!(user_data.contains("systemctl stop \"$unit\""));
    assert!(user_data.contains("systemctl reset-failed \"$unit\""));
    for unit in SHARED_VM_GUEST_POLICY_MASKED_UNITS {
        assert!(
            user_data.contains(unit),
            "missing masked guest policy unit {unit}"
        );
    }
    assert!(user_data.contains(
        "chmod 0700 \"$root_home\" \"$root_xdg_config\" \"$root_xdg_data\" \"$root_xdg_cache\" \"$root_xdg_runtime\""
    ));
    assert!(user_data.contains("StandardOutput=journal+console"));
    assert!(user_data.contains("starting guest-agent"));
    assert!(user_data.contains("ensuring vsock kernel modules are loaded"));
    assert!(user_data.contains("/usr/sbin/modprobe vsock"));
    assert!(user_data.contains("/usr/sbin/modprobe vmw_vsock_virtio_transport_common"));
    assert!(user_data.contains("/usr/sbin/modprobe vmw_vsock_virtio_transport"));
    assert!(user_data.contains("CTX_AVF_GUEST_CONTROL_READY_MARKER"));
    assert!(user_data.contains(SHARED_VM_GUEST_AGENT_LAUNCHER_PATH));
    assert!(user_data.contains("/mnt/ctx-host/managed/vms/avf-linux"));
    assert!(user_data.contains("guest-control-ready"));
    assert!(user_data.contains("guest-control-failed"));
    assert!(user_data.contains("guest-agent.log"));
    assert!(user_data.contains("guest-agent launcher starting"));
    assert!(user_data.contains("guest-agent started as pid"));
    assert!(user_data.contains("guest-agent did not publish ready marker within"));
    assert!(user_data.contains("guest-agent exited before ready"));
    assert!(user_data.contains(&format!(
        "After={} {}",
        SHARED_VM_DATA_DISK_SERVICE_NAME, SHARED_VM_HOST_DATA_SERVICE_NAME
    )));
    assert!(user_data.contains(&format!(
        "Requires={} {}",
        SHARED_VM_DATA_DISK_SERVICE_NAME, SHARED_VM_HOST_DATA_SERVICE_NAME
    )));
    assert!(user_data.contains("mount -t virtiofs"));
    assert!(user_data.contains("ctx-data-root"));
    assert!(user_data.contains("/mnt/ctx-host"));
    assert!(user_data.contains("preparing ctx-avf-linux-guest-agent.service"));
    assert!(user_data.contains("systemctl status ctx-avf-linux-guest-agent.service --no-pager"));
    assert!(user_data.contains("Restart=no"));
    assert!(user_data.contains("/mnt/ctx-host/runtime/helpers/container-stack.tar.gz"));
    assert!(!user_data.contains("ctx-avf-grow-rootfs.service"));
    let parsed: Value = serde_yaml::from_str(&user_data).expect("cloud-init YAML should parse");
    let bootcmd = parsed["bootcmd"]
        .as_sequence()
        .expect("cloud-init bootcmd should be a sequence");
    assert_eq!(bootcmd.len(), 1);
    let bootcmd_rendered = bootcmd[0]
        .as_str()
        .expect("early bootcmd should be a string");
    assert!(bootcmd_rendered.contains(SHARED_VM_DATA_DISK_INSTALL_PATH));
    assert!(bootcmd_rendered.contains(SHARED_VM_GUEST_POLICY_INSTALL_PATH));
    let runcmd = parsed["runcmd"]
        .as_sequence()
        .expect("cloud-init runcmd should be a sequence");
    assert_eq!(runcmd.len(), 8);
    assert_eq!(
        runcmd[0]
            .as_sequence()
            .expect("first runcmd entry should be daemon reload"),
        &vec![Value::from("systemctl"), Value::from("daemon-reload")]
    );
    let host_data_enable_index = user_data
        .find(&format!(
            "systemctl enable --now {service_name}",
            service_name = SHARED_VM_HOST_DATA_SERVICE_NAME
        ))
        .expect("host-data enable command");
    let data_disk_enable_index = user_data
        .find("systemctl enable --now ctx-avf-data-disk.service")
        .expect("data-disk enable command");
    assert!(runcmd[3]
        .as_str()
        .expect("prepare guest-agent step should be a string")
        .contains("preparing ctx-avf-linux-guest-agent.service"));
    assert!(runcmd[4]
        .as_str()
        .expect("container-stack install step should be a string")
        .contains(SHARED_VM_GUEST_CONTAINER_STACK_INSTALL_PATH));
    assert!(
        host_data_enable_index
            < user_data
                .find("preparing ctx-avf-linux-guest-agent.service")
                .expect("prepare guest-agent command")
    );
    assert!(
        data_disk_enable_index
            < user_data
                .find("preparing ctx-avf-linux-guest-agent.service")
                .expect("prepare guest-agent command")
    );
    let content_lines = user_data
        .lines()
        .skip_while(|line| *line != "    content: |")
        .skip(1)
        .take_while(|line| !line.starts_with("  - path: "))
        .collect::<Vec<_>>();
    assert!(!content_lines.is_empty());
    assert!(content_lines.iter().all(|line| line.starts_with("      ")));
}

#[test]
fn shared_vm_guest_host_share_path_projects_paths_under_stable_guest_mount_root() {
    let data_root = Path::new("/tmp/ctx-data");
    let host_path = data_root
        .join("managed")
        .join("vms")
        .join("avf-linux")
        .join("macos")
        .join("aarch64")
        .join("shared")
        .join("guest-control-ready");
    assert_eq!(
        shared_vm_guest_host_share_path(data_root, &host_path).expect("guest share path"),
        PathBuf::from("/mnt/ctx-host")
            .join("managed")
            .join("vms")
            .join("avf-linux")
            .join("macos")
            .join("aarch64")
            .join("shared")
            .join("guest-control-ready")
    );
}

#[test]
fn materialize_writable_rootfs_image_preserves_small_rootfs_size() {
    let temp = PathBuf::from("/tmp").join(format!(
        "ctxavf-rootfs-grow-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    fs::create_dir_all(&temp).expect("create tempdir");
    let source_rootfs = temp.join("source-rootfs.raw");
    let source_file = File::create(&source_rootfs).expect("create source rootfs");
    source_file
        .set_len(1024 * 1024)
        .expect("seed source rootfs");
    drop(source_file);

    let (staged_rootfs, note) =
        materialize_writable_rootfs_image(&temp, &source_rootfs).expect("materialize rootfs");

    assert_eq!(staged_rootfs, shared_vm_rootfs_path(&temp));
    assert_eq!(
        fs::metadata(&staged_rootfs).expect("rootfs metadata").len(),
        1024 * 1024
    );
    let note = note.expect("copy note");
    assert!(note.contains("copied rootfs image"));
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[test]
fn materialize_bootable_kernel_image_reports_reuse_for_matching_staged_kernel() {
    let temp = PathBuf::from("/tmp").join(format!(
        "ctxavf-kernel-reuse-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    fs::create_dir_all(&temp).expect("create tempdir");
    let kernel_path = temp.join("Image.gz");
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(b"kernel-v1")
        .expect("write gz kernel payload");
    let gz_bytes = encoder.finish().expect("finish gz payload");
    fs::write(&kernel_path, gz_bytes).expect("write gz kernel");

    let (first_path, first_note) =
        materialize_bootable_kernel_image(&temp, &kernel_path).expect("materialize kernel");
    assert_eq!(first_path, shared_vm_boot_kernel_path(&temp));
    assert!(first_note
        .expect("initial materialization note")
        .contains("decompressed gzipped kernel image"));
    let first_modified = fs::metadata(&first_path)
        .expect("first kernel metadata")
        .modified()
        .expect("first kernel modified time");

    std::thread::sleep(Duration::from_millis(20));
    let (second_path, second_note) =
        materialize_bootable_kernel_image(&temp, &kernel_path).expect("reuse kernel");
    assert_eq!(second_path, shared_vm_boot_kernel_path(&temp));
    assert!(second_note
        .expect("reuse note")
        .contains("reused staged decompressed kernel image"));
    let second_modified = fs::metadata(&second_path)
        .expect("second kernel metadata")
        .modified()
        .expect("second kernel modified time");
    assert_eq!(second_modified, first_modified);
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[test]
fn materialize_bootable_kernel_image_rebuilds_when_source_changes() {
    let temp = PathBuf::from("/tmp").join(format!(
        "ctxavf-kernel-refresh-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    fs::create_dir_all(&temp).expect("create tempdir");
    let kernel_path = temp.join("Image.gz");
    write_gzip_file(&kernel_path, b"kernel-v1");

    let (staged_kernel, first_note) =
        materialize_bootable_kernel_image(&temp, &kernel_path).expect("materialize kernel");
    assert!(first_note.is_some());

    write_gzip_file(&kernel_path, b"kernel-version-two");
    let (_, second_note) =
        materialize_bootable_kernel_image(&temp, &kernel_path).expect("refresh kernel");

    assert!(second_note.is_some());
    assert_eq!(
        fs::read(&staged_kernel).expect("read refreshed staged kernel"),
        b"kernel-version-two"
    );
    let metadata_path = shared_vm_boot_kernel_path(&temp).with_extension("metadata.json");
    let metadata = fs::read_to_string(&metadata_path).expect("read staged kernel metadata");
    assert!(metadata.contains("source_gzip_sha256"));
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[test]
fn shared_vm_cloud_init_seed_digest_changes_when_payload_inputs_change() {
    let temp = PathBuf::from("/tmp").join(format!(
        "ctxavf-cloud-init-seed-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    fs::create_dir_all(&temp).expect("create tempdir");
    let guest_agent_bytes = b"guest-agent-v1";
    let meta_v1 = render_shared_vm_cloud_init_meta_data(&temp, guest_agent_bytes, None, "sha-one")
        .expect("render cloud-init meta-data v1");
    let user_v1 = render_shared_vm_cloud_init_user_data(
        &temp,
        guest_agent_bytes,
        None,
        &temp.join("payloads").join("container-stack.tar.gz"),
        "sha-one",
    )
    .expect("render cloud-init user-data v1");
    let network = render_shared_vm_cloud_init_network_config();
    let digest_v1 = shared_vm_cloud_init_seed_digest(&meta_v1, &user_v1, &network);

    let meta_v2 = render_shared_vm_cloud_init_meta_data(&temp, guest_agent_bytes, None, "sha-two")
        .expect("render cloud-init meta-data v2");
    let user_v2 = render_shared_vm_cloud_init_user_data(
        &temp,
        guest_agent_bytes,
        None,
        &temp.join("payloads").join("container-stack.tar.gz"),
        "sha-two",
    )
    .expect("render cloud-init user-data v2");
    let digest_v2 = shared_vm_cloud_init_seed_digest(&meta_v2, &user_v2, &network);

    assert_ne!(digest_v1, digest_v2);
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[test]
fn default_shared_vm_kernel_cmdline_targets_runtime_rootfs_label() {
    let cmdline = default_shared_vm_kernel_cmdline();

    assert!(cmdline.contains("console=hvc0"));
    assert!(cmdline.contains("root=LABEL=cloudimg-rootfs"));
    assert!(cmdline.contains("rootwait"));
    assert!(cmdline.contains("rw"));
    assert!(cmdline.contains("systemd.mask=systemd-networkd-wait-online.service"));
    for unit in SHARED_VM_GUEST_POLICY_MASKED_UNITS {
        assert!(
            cmdline.contains(&format!("systemd.mask={unit}")),
            "missing early-boot mask for {unit}"
        );
    }
}

#[test]
fn writable_surface_contract_digest_includes_early_bootcmd() {
    let temp = PathBuf::from("/tmp").join(format!(
        "ctxavf-writable-surface-digest-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    fs::create_dir_all(&temp).expect("create tempdir");

    let guest_ready_marker_path =
        shared_vm_guest_host_share_path(&temp, &shared_vm_guest_control_ready_path(&temp))
            .expect("guest ready-marker path");
    let guest_failure_marker_path =
        shared_vm_guest_host_share_path(&temp, &shared_vm_guest_control_failed_path(&temp))
            .expect("guest failure-marker path");
    let guest_agent_log_path =
        shared_vm_guest_host_share_path(&temp, &shared_vm_guest_agent_log_path(&temp))
            .expect("guest agent log path");

    let mut expected = Sha256::new();
    for rendered in [
        render_shared_vm_host_data_mount_service(),
        render_shared_vm_data_disk_script(),
        render_shared_vm_guest_policy_script(),
        render_shared_vm_early_bootcmd(),
        render_shared_vm_data_disk_service(),
        render_shared_vm_containerd_service(),
        render_shared_vm_buildkit_service(),
        render_shared_vm_guest_agent_service(
            &guest_ready_marker_path,
            &guest_failure_marker_path,
            &guest_agent_log_path,
        ),
    ] {
        expected.update(rendered.as_bytes());
        expected.update(b"\0");
    }

    assert_eq!(
        shared_vm_writable_surface_contract_digest(&temp).expect("writable-surface contract"),
        hex::encode(expected.finalize())
    );

    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[test]
fn materialize_data_disk_image_initializes_sparse_guest_data_disk() {
    let temp = PathBuf::from("/tmp").join(format!(
        "ctxavf-data-disk-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    fs::create_dir_all(&temp).expect("create tempdir");

    let (data_disk, note) = materialize_data_disk_image(&temp).expect("materialize data disk");

    assert_eq!(data_disk, shared_vm_data_disk_path(&temp));
    assert_eq!(
        fs::metadata(&data_disk).expect("data-disk metadata").len(),
        SHARED_VM_INITIAL_DATA_DISK_BYTES
    );
    let note = note.expect("data-disk note");
    assert!(note.contains("initialized sparse AVF Linux data disk"));
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[test]
fn resolve_shared_vm_data_disk_growth_decision_returns_no_action_when_guest_free_is_healthy() {
    let decision = resolve_shared_vm_data_disk_growth_decision(
        SHARED_VM_INITIAL_DATA_DISK_BYTES,
        SHARED_VM_DATA_DISK_GROWTH_THRESHOLD_BYTES,
        gibibytes(64),
    );

    assert_eq!(decision, SharedVmDataDiskGrowthDecision::NoAction);
}

#[test]
fn resolve_shared_vm_data_disk_growth_decision_grows_when_guest_free_is_low_and_host_has_budget() {
    let decision = resolve_shared_vm_data_disk_growth_decision(
        SHARED_VM_INITIAL_DATA_DISK_BYTES,
        gibibytes(1),
        gibibytes(64),
    );

    assert_eq!(
        decision,
        SharedVmDataDiskGrowthDecision::Grow {
            new_size_bytes: SHARED_VM_INITIAL_DATA_DISK_BYTES
                + SHARED_VM_DATA_DISK_GROWTH_STEP_BYTES,
            additional_bytes: SHARED_VM_DATA_DISK_GROWTH_STEP_BYTES,
        }
    );
}

#[test]
fn resolve_shared_vm_data_disk_growth_decision_blocks_when_host_reserve_would_be_breached() {
    let decision = resolve_shared_vm_data_disk_growth_decision(
        SHARED_VM_INITIAL_DATA_DISK_BYTES,
        gibibytes(1),
        SHARED_VM_HOST_DISK_RESERVE_BYTES,
    );

    assert_eq!(
        decision,
        SharedVmDataDiskGrowthDecision::HostReserveBlocked {
            available_host_bytes: SHARED_VM_HOST_DISK_RESERVE_BYTES,
            reserve_bytes: SHARED_VM_HOST_DISK_RESERVE_BYTES,
            requested_additional_bytes: SHARED_VM_DATA_DISK_GROWTH_STEP_BYTES,
        }
    );
}

#[test]
fn resolve_shared_vm_memory_balloon_action_reclaims_under_host_pressure() {
    let action = resolve_shared_vm_memory_balloon_action(
        gibibytes(16),
        gibibytes(16),
        gibibytes(4),
        true,
        Some(gibibytes(8)),
        gibibytes(3),
    );

    assert_eq!(
        action,
        SharedVmMemoryBalloonAction::Reclaim {
            new_target_bytes: gibibytes(14),
            available_host_bytes: gibibytes(3),
            aggressive: false,
        }
    );
}

#[test]
fn resolve_shared_vm_memory_balloon_action_holds_before_guest_probe_under_host_pressure() {
    let action = resolve_shared_vm_memory_balloon_action(
        gibibytes(16),
        gibibytes(16),
        gibibytes(4),
        false,
        None,
        gibibytes(3),
    );

    assert_eq!(action, SharedVmMemoryBalloonAction::NoAction);
}

#[test]
fn runtime_guest_exec_timeout_exceeds_readiness_timeout() {
    assert!(SHARED_VM_RUNTIME_GUEST_EXEC_IO_TIMEOUT > SHARED_VM_READINESS_GUEST_EXEC_IO_TIMEOUT);
}

#[test]
fn resolve_shared_vm_memory_balloon_action_grows_under_guest_pressure() {
    let action = resolve_shared_vm_memory_balloon_action(
        gibibytes(8),
        gibibytes(16),
        gibibytes(4),
        true,
        Some(gibibytes(1)),
        gibibytes(10),
    );

    assert_eq!(
        action,
        SharedVmMemoryBalloonAction::Grow {
            new_target_bytes: gibibytes(10),
            available_host_bytes: gibibytes(10),
            guest_available_bytes: gibibytes(1),
        }
    );
}

#[test]
fn resolve_shared_vm_memory_balloon_action_requests_emergency_stop_at_floor() {
    let action = resolve_shared_vm_memory_balloon_action(
        gibibytes(4),
        gibibytes(16),
        gibibytes(4),
        true,
        Some(gibibytes(1)),
        gibibytes(0),
    );

    assert_eq!(
        action,
        SharedVmMemoryBalloonAction::EmergencyStop {
            available_host_bytes: gibibytes(0),
            current_target_bytes: gibibytes(4),
            floor_bytes: gibibytes(4),
        }
    );
}

#[test]
fn resolve_shared_vm_memory_watchdog_sample_action_requires_confirmed_emergency_pressure() {
    let first = resolve_shared_vm_memory_watchdog_sample_action(0, gibibytes(0));
    assert_eq!(
        first,
        SharedVmMemoryWatchdogSampleAction::NoAction {
            next_consecutive_emergency_samples: 1,
        }
    );

    let second = resolve_shared_vm_memory_watchdog_sample_action(1, gibibytes(0));
    assert_eq!(
        second,
        SharedVmMemoryWatchdogSampleAction::RequestStop {
            next_consecutive_emergency_samples: 2,
            available_host_bytes: gibibytes(0),
        }
    );
}

#[test]
fn resolve_shared_vm_memory_watchdog_sample_action_resets_after_host_recovers() {
    let action = resolve_shared_vm_memory_watchdog_sample_action(1, gibibytes(2));
    assert_eq!(
        action,
        SharedVmMemoryWatchdogSampleAction::NoAction {
            next_consecutive_emergency_samples: 0,
        }
    );
}

#[test]
fn resolve_shared_vm_memory_watchdog_exit_action_models_sigterm_and_sigkill_escalation() {
    assert_eq!(
        resolve_shared_vm_memory_watchdog_exit_action(true, false),
        SharedVmMemoryWatchdogExitAction::OwnerExitedAfterRequest
    );
    assert_eq!(
        resolve_shared_vm_memory_watchdog_exit_action(false, true),
        SharedVmMemoryWatchdogExitAction::OwnerExitedAfterSigterm
    );
    assert_eq!(
        resolve_shared_vm_memory_watchdog_exit_action(false, false),
        SharedVmMemoryWatchdogExitAction::EscalateToSigkill
    );
}

#[test]
fn controller_safety_preflight_replays_fixture_cases_with_expected_outputs() {
    let fixture = load_controller_safety_preflight_fixture();
    assert!(fixture.gate_pass_requirements.all_cases_must_pass);
    assert!(fixture.gate_pass_requirements.exact_expected_output_match);

    for case in &fixture.cases {
        let decisions = replay_shared_vm_controller_safety_trace(
            &controller_safety_replay_state(&case.initial_state),
            &controller_safety_replay_steps(&case.steps),
        )
        .expect("controller-safety fixture replay");

        assert_eq!(
            decisions.len(),
            case.steps.len(),
            "case {} should emit one decision per replay step",
            case.case_id
        );

        for (decision, step) in decisions.iter().zip(&case.steps) {
            let normalized = normalize_controller_safety_decision(decision);
            assert_eq!(
                normalized, step.expected,
                "case {} step {} normalized output mismatch",
                case.case_id, step.step_index
            );
        }

        let actual_invariants = decisions
            .iter()
            .flat_map(|decision| decision.invariants_passed.iter().copied())
            .collect::<std::collections::BTreeSet<_>>();
        let required_invariants = case
            .required_invariants
            .iter()
            .filter(|value| value.as_str() != "trace_replay_deterministic")
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        assert!(
            required_invariants.is_subset(&actual_invariants),
            "case {} should satisfy required invariants: expected {required_invariants:?}, got {actual_invariants:?}",
            case.case_id
        );
    }
}

#[test]
fn controller_safety_preflight_repeat_replay_is_deterministic() {
    let fixture = load_controller_safety_preflight_fixture();
    assert!(fixture.gate_pass_requirements.canonical_json_repeat_match);

    for case in &fixture.cases {
        let initial_state = controller_safety_replay_state(&case.initial_state);
        let steps = controller_safety_replay_steps(&case.steps);
        let first = replay_shared_vm_controller_safety_trace(&initial_state, &steps)
            .expect("first controller-safety replay");
        let second = replay_shared_vm_controller_safety_trace(&initial_state, &steps)
            .expect("second controller-safety replay");

        assert_eq!(
            shared_vm_controller_safety_trace_canonical_json(&first),
            shared_vm_controller_safety_trace_canonical_json(&second),
            "case {} canonical replay trace should be deterministic",
            case.case_id
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn persist_shared_vm_owner_error_state_marks_vm_error_and_clears_owner_processes() {
    let temp = PathBuf::from("/tmp").join(format!(
        "ctxavf-owner-error-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    fs::create_dir_all(&temp).expect("create tempdir");
    let state_path = temp.join("shared-vm-state.json");
    let mut state = PersistedSharedVmState {
        state: AvfLinuxSharedVmLifecycleState::Running,
        guest_identity: supported_guest_identity(),
        runtime_root: Some(temp.join("runtime")),
        rootfs_image: Some(temp.join("rootfs.raw")),
        kernel_path: Some(temp.join("kernel")),
        initrd_path: Some(temp.join("initrd")),
        runtime_version: Some("test-runtime".to_string()),
        runtime_shape_digest: None,
        writable_surface_contract_digest: None,
        updated_at: None,
        last_started_at: Some("started".to_string()),
        last_saved_at: Some("saved".to_string()),
        last_stopped_at: None,
        transition_status: Some(AvfLinuxSharedVmTransitionStatus::Scaffolded),
        last_start_outcome: Some(AvfLinuxSharedVmStartOutcome::Restored),
        last_stop_outcome: Some(AvfLinuxSharedVmStopOutcome::SavedStateWritten),
        last_restore_error: None,
        last_save_error: None,
        relay_pid: Some(101),
        guest_agent_pid: Some(202),
        simulated: false,
        notes: vec!["running".to_string()],
    };

    persist_shared_vm_owner_error_state(
        &state_path,
        &mut state,
        "disk growth blocked by host reserve".to_string(),
    )
    .expect("persist owner error state");

    assert!(matches!(state.state, AvfLinuxSharedVmLifecycleState::Error));
    assert!(!state.simulated);
    assert!(state.updated_at.is_some());
    assert_eq!(state.last_stopped_at, state.updated_at);
    assert!(state.transition_status.is_none());
    assert!(state.relay_pid.is_none());
    assert!(state.guest_agent_pid.is_none());
    assert_eq!(
        state.notes,
        vec!["disk growth blocked by host reserve".to_string()]
    );

    let persisted = load_state(&state_path)
        .expect("load state")
        .expect("persisted state");
    assert!(matches!(
        persisted.state,
        AvfLinuxSharedVmLifecycleState::Error
    ));
    assert!(persisted.relay_pid.is_none());
    assert!(persisted.guest_agent_pid.is_none());
    assert_eq!(
        persisted.notes,
        vec!["disk growth blocked by host reserve".to_string()]
    );
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[test]
fn shared_vm_state_marks_missing_owner_with_memory_pressure_request_as_error() {
    let temp = PathBuf::from("/tmp").join(format!(
        "ctxavf-memory-request-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    prepare_runtime_layout(&temp).expect("prepare runtime layout");
    let state_path = shared_vm_state_path(&temp);
    persist_state(
        &state_path,
        &PersistedSharedVmState {
            state: AvfLinuxSharedVmLifecycleState::Running,
            guest_identity: supported_guest_identity(),
            runtime_root: None,
            rootfs_image: None,
            kernel_path: None,
            initrd_path: None,
            runtime_version: None,
            runtime_shape_digest: None,
            writable_surface_contract_digest: None,
            updated_at: Some(now_timestamp_string()),
            last_started_at: None,
            last_saved_at: Some(now_timestamp_string()),
            last_stopped_at: None,
            transition_status: Some(AvfLinuxSharedVmTransitionStatus::Ready),
            last_start_outcome: Some(AvfLinuxSharedVmStartOutcome::Restored),
            last_stop_outcome: Some(AvfLinuxSharedVmStopOutcome::SavedStateWritten),
            last_restore_error: None,
            last_save_error: None,
            relay_pid: Some(999_999),
            guest_agent_pid: None,
            simulated: false,
            notes: vec!["running".to_string()],
        },
    )
    .expect("persist running state");
    request_shared_vm_memory_pressure_stop(&temp, "watchdog requested an emergency stop")
        .expect("request emergency stop");

    let response = shared_vm_state(&temp).expect("shared vm state");

    assert!(matches!(
        response.state,
        AvfLinuxSharedVmLifecycleState::Error
    ));
    assert!(response.transition_status.is_none());
    assert!(response
        .notes
        .iter()
        .any(|note| note.contains("watchdog requested an emergency stop")));
    assert!(
        !shared_vm_memory_pressure_request_path(&temp).exists(),
        "request file should be cleared once state is updated"
    );
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[test]
fn shared_vm_state_marks_missing_owner_as_cold_stop_and_clears_stale_save_metadata() {
    let temp = PathBuf::from("/tmp").join(format!(
        "ctxavf-missing-owner-stop-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    prepare_runtime_layout(&temp).expect("prepare runtime layout");
    let state_path = shared_vm_state_path(&temp);
    persist_state(
        &state_path,
        &PersistedSharedVmState {
            state: AvfLinuxSharedVmLifecycleState::Running,
            guest_identity: supported_guest_identity(),
            runtime_root: None,
            rootfs_image: None,
            kernel_path: None,
            initrd_path: None,
            runtime_version: None,
            runtime_shape_digest: None,
            writable_surface_contract_digest: None,
            updated_at: Some(now_timestamp_string()),
            last_started_at: Some("started".to_string()),
            last_saved_at: Some("saved".to_string()),
            last_stopped_at: None,
            transition_status: Some(AvfLinuxSharedVmTransitionStatus::Ready),
            last_start_outcome: Some(AvfLinuxSharedVmStartOutcome::ColdBoot),
            last_stop_outcome: Some(AvfLinuxSharedVmStopOutcome::SavedStateWritten),
            last_restore_error: None,
            last_save_error: Some("old save error".to_string()),
            relay_pid: Some(999_999),
            guest_agent_pid: None,
            simulated: false,
            notes: vec!["running".to_string()],
        },
    )
    .expect("persist running state");

    let response = shared_vm_state(&temp).expect("shared vm state");

    assert!(matches!(
        response.state,
        AvfLinuxSharedVmLifecycleState::Stopped
    ));
    assert_eq!(
        response.last_stop_outcome,
        Some(AvfLinuxSharedVmStopOutcome::ColdStop)
    );
    assert!(response.last_saved_at.is_none());
    assert!(response.last_save_error.is_none());
    assert!(response
        .notes
        .iter()
        .any(|note| note.contains("owner process was not alive")));
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[test]
fn shared_vm_state_surfaces_explicit_start_and_stop_outcomes() {
    let temp = PathBuf::from("/tmp").join(format!(
        "ctxavf-state-outcomes-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    prepare_runtime_layout(&temp).expect("prepare runtime layout");
    persist_state(
        &shared_vm_state_path(&temp),
        &PersistedSharedVmState {
            state: AvfLinuxSharedVmLifecycleState::Stopped,
            guest_identity: supported_guest_identity(),
            runtime_root: Some(temp.join("runtime")),
            rootfs_image: Some(temp.join("rootfs.raw")),
            kernel_path: Some(temp.join("kernel")),
            initrd_path: Some(temp.join("initrd")),
            runtime_version: Some("test-runtime".to_string()),
            runtime_shape_digest: Some("digest".to_string()),
            writable_surface_contract_digest: None,
            updated_at: Some(now_timestamp_string()),
            last_started_at: Some("started".to_string()),
            last_saved_at: None,
            last_stopped_at: Some("stopped".to_string()),
            transition_status: Some(AvfLinuxSharedVmTransitionStatus::Stopped),
            last_start_outcome: Some(AvfLinuxSharedVmStartOutcome::ColdBootAfterRestoreFailure),
            last_stop_outcome: Some(AvfLinuxSharedVmStopOutcome::ColdStopAfterSaveFailure),
            last_restore_error: Some("restore failed".to_string()),
            last_save_error: Some("save failed".to_string()),
            relay_pid: None,
            guest_agent_pid: None,
            simulated: false,
            notes: vec!["stopped".to_string()],
        },
    )
    .expect("persist stopped state");

    let response = shared_vm_state(&temp).expect("shared vm state");
    assert_eq!(
        response.last_start_outcome,
        Some(AvfLinuxSharedVmStartOutcome::ColdBootAfterRestoreFailure)
    );
    assert_eq!(
        response.last_stop_outcome,
        Some(AvfLinuxSharedVmStopOutcome::ColdStopAfterSaveFailure)
    );
    assert_eq!(
        response.last_restore_error.as_deref(),
        Some("restore failed")
    );
    assert_eq!(response.last_save_error.as_deref(), Some("save failed"));
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[test]
fn shared_vm_state_downgrades_real_avf_ready_state_without_guest_probe_marker() {
    let temp = PathBuf::from("/tmp").join(format!(
        "ctxavf-ready-without-probe-marker-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    prepare_runtime_layout(&temp).expect("prepare runtime layout");
    let mut owner = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("spawn owner placeholder");
    persist_state(
        &shared_vm_state_path(&temp),
        &PersistedSharedVmState {
            state: AvfLinuxSharedVmLifecycleState::Running,
            guest_identity: supported_guest_identity(),
            runtime_root: None,
            rootfs_image: None,
            kernel_path: None,
            initrd_path: None,
            runtime_version: None,
            runtime_shape_digest: None,
            writable_surface_contract_digest: None,
            updated_at: Some(now_timestamp_string()),
            last_started_at: Some(now_timestamp_string()),
            last_saved_at: None,
            last_stopped_at: None,
            transition_status: Some(AvfLinuxSharedVmTransitionStatus::Ready),
            last_start_outcome: Some(AvfLinuxSharedVmStartOutcome::ColdBoot),
            last_stop_outcome: None,
            last_restore_error: None,
            last_save_error: None,
            relay_pid: Some(owner.id()),
            guest_agent_pid: None,
            simulated: false,
            notes: vec!["running".to_string()],
        },
    )
    .expect("persist running state");

    let response = shared_vm_state(&temp).expect("shared vm state");

    assert!(matches!(
        response.state,
        AvfLinuxSharedVmLifecycleState::Running
    ));
    assert!(matches!(
        response.transition_status,
        Some(AvfLinuxSharedVmTransitionStatus::Scaffolded)
    ));
    assert!(response
        .notes
        .iter()
        .any(|note| note.contains("waiting for guest-control readiness")));

    owner.kill().expect("stop owner placeholder");
    assert!(wait_for_child_exit(&mut owner, Duration::from_secs(5)));
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[cfg(target_os = "macos")]
#[test]
fn vm_save_restore_timeout_exceeds_guest_exec_connect_timeout() {
    assert!(VM_LIFECYCLE_COMPLETION_TIMEOUT > GUEST_EXEC_CONNECT_TIMEOUT);
    assert!(VM_SAVE_RESTORE_COMPLETION_TIMEOUT > VM_LIFECYCLE_COMPLETION_TIMEOUT);
}

#[test]
fn start_shared_vm_materializes_rootfs_and_data_disk_layout() {
    let temp = PathBuf::from("/tmp").join(format!(
        "ctxavf-start-layout-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    let runtime_root = temp.join("runtime");
    let helpers_root = runtime_root.join("helpers");
    fs::create_dir_all(&helpers_root).expect("create helpers root");
    let source_rootfs = temp.join("source-rootfs.raw");
    fs::write(&source_rootfs, b"rootfs").expect("write rootfs");
    let kernel_path = helpers_root.join("kernel");
    fs::write(&kernel_path, b"kernel").expect("write kernel");
    let initrd_path = helpers_root.join("initrd");
    fs::write(&initrd_path, b"initrd").expect("write initrd");

    let started = start_shared_vm(
        &temp,
        &runtime_root,
        &source_rootfs,
        &kernel_path,
        &initrd_path,
        "test-runtime".to_string(),
    )
    .expect("start shared vm");

    assert!(matches!(
        started.state,
        AvfLinuxSharedVmLifecycleState::Running
    ));
    assert!(started.simulated);
    assert!(shared_vm_rootfs_path(&temp).exists());
    assert!(shared_vm_data_disk_path(&temp).exists());
    assert!(started
        .notes
        .iter()
        .any(|note| note.contains("AVF Linux data disk")));
    assert!(started
        .notes
        .iter()
        .any(|note| note.contains("shared VM start reached launch-ready")));
    assert_eq!(
        started.last_start_outcome,
        Some(AvfLinuxSharedVmStartOutcome::ColdBoot)
    );
    assert!(started.last_restore_error.is_none());
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[test]
fn start_shared_vm_clears_stale_guest_control_ready_marker_before_launch() {
    let temp = PathBuf::from("/tmp").join(format!(
        "ctxavf-start-clears-ready-marker-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    let runtime_root = temp.join("runtime");
    let helpers_root = runtime_root.join("helpers");
    fs::create_dir_all(&helpers_root).expect("create helpers root");
    let source_rootfs = temp.join("source-rootfs.raw");
    fs::write(&source_rootfs, b"rootfs").expect("write rootfs");
    let kernel_path = helpers_root.join("kernel");
    fs::write(&kernel_path, b"kernel").expect("write kernel");
    let initrd_path = helpers_root.join("initrd");
    fs::write(&initrd_path, b"initrd").expect("write initrd");
    let ready_marker = shared_vm_guest_control_ready_path(&temp);
    let failed_marker = shared_vm_guest_control_failed_path(&temp);
    fs::create_dir_all(ready_marker.parent().expect("marker parent"))
        .expect("create marker parent");
    fs::write(&ready_marker, b"ready").expect("seed ready marker");
    fs::write(&failed_marker, b"failed").expect("seed failed marker");

    let started = start_shared_vm(
        &temp,
        &runtime_root,
        &source_rootfs,
        &kernel_path,
        &initrd_path,
        "test-runtime".to_string(),
    )
    .expect("start shared vm");

    assert!(matches!(
        started.state,
        AvfLinuxSharedVmLifecycleState::Running
    ));
    assert!(
        !ready_marker.exists(),
        "stale guest-control ready marker should be removed before launch"
    );
    assert!(
        !failed_marker.exists(),
        "stale guest-control failure marker should be removed before launch"
    );
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[test]
fn start_shared_vm_surfaces_saved_state_downgrade_when_restore_is_unavailable() {
    let temp = PathBuf::from("/tmp").join(format!(
        "ctxavf-start-saved-state-downgrade-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    let runtime_root = temp.join("runtime");
    let helpers_root = runtime_root.join("helpers");
    fs::create_dir_all(&helpers_root).expect("create helpers root");
    let source_rootfs = temp.join("source-rootfs.raw");
    fs::write(&source_rootfs, b"rootfs").expect("write rootfs");
    let kernel_path = helpers_root.join("kernel");
    fs::write(&kernel_path, b"kernel").expect("write kernel");
    let initrd_path = helpers_root.join("initrd");
    fs::write(&initrd_path, b"initrd").expect("write initrd");
    let saved_state_path = shared_vm_saved_state_path(&temp);
    fs::create_dir_all(saved_state_path.parent().expect("saved-state parent"))
        .expect("create saved-state parent");
    fs::write(&saved_state_path, b"saved-state").expect("seed saved state");
    prepare_runtime_layout(&temp).expect("prepare runtime layout");
    persist_state(
        &shared_vm_state_path(&temp),
        &PersistedSharedVmState {
            state: AvfLinuxSharedVmLifecycleState::Stopped,
            guest_identity: supported_guest_identity(),
            runtime_root: Some(runtime_root.clone()),
            rootfs_image: Some(shared_vm_rootfs_path(&temp)),
            kernel_path: Some(shared_vm_boot_kernel_path(&temp)),
            initrd_path: Some(initrd_path.clone()),
            runtime_version: Some("test-runtime".to_string()),
            runtime_shape_digest: Some(shared_vm_runtime_shape_digest(
                &runtime_root,
                &source_rootfs,
                &kernel_path,
                &initrd_path,
                "test-runtime",
            )),
            writable_surface_contract_digest: Some(
                shared_vm_writable_surface_contract_digest(&temp)
                    .expect("render writable-surface contract digest"),
            ),
            updated_at: Some(now_timestamp_string()),
            last_started_at: None,
            last_saved_at: Some("saved".to_string()),
            last_stopped_at: Some("stopped".to_string()),
            transition_status: Some(AvfLinuxSharedVmTransitionStatus::Stopped),
            last_start_outcome: Some(AvfLinuxSharedVmStartOutcome::Restored),
            last_stop_outcome: Some(AvfLinuxSharedVmStopOutcome::SavedStateWritten),
            last_restore_error: None,
            last_save_error: None,
            relay_pid: None,
            guest_agent_pid: None,
            simulated: true,
            notes: vec!["stopped".to_string()],
        },
    )
    .expect("persist prior stopped state");

    let started = start_shared_vm(
        &temp,
        &runtime_root,
        &source_rootfs,
        &kernel_path,
        &initrd_path,
        "test-runtime".to_string(),
    )
    .expect("start shared vm");

    assert_eq!(
        started.last_start_outcome,
        Some(AvfLinuxSharedVmStartOutcome::ColdBoot)
    );
    assert_eq!(
        started.last_restore_error.as_deref(),
        Some(
            "saved workspace VM state was present, but this start could not use it and proceeded with a cold boot"
        )
    );
    assert!(started.notes.iter().any(|note| {
        note.contains("saved workspace VM state was present")
            && note.contains("proceeded with a cold boot")
    }));
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[cfg(unix)]
#[test]
fn start_shared_vm_marks_already_running_path_explicitly() {
    let temp = PathBuf::from("/tmp").join(format!(
        "ctxavf-start-already-running-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    prepare_runtime_layout(&temp).expect("prepare runtime layout");

    let runtime_root = temp.join("runtime");
    let helpers_root = runtime_root.join("helpers");
    fs::create_dir_all(&helpers_root).expect("create helpers root");
    let source_rootfs = temp.join("source-rootfs.raw");
    fs::write(&source_rootfs, b"rootfs").expect("write rootfs");
    let kernel_path = helpers_root.join("kernel");
    fs::write(&kernel_path, b"kernel").expect("write kernel");
    let initrd_path = helpers_root.join("initrd");
    fs::write(&initrd_path, b"initrd").expect("write initrd");

    let mut relay = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("spawn relay placeholder");
    let mut guest = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("spawn guest placeholder");

    persist_state(
        &shared_vm_state_path(&temp),
        &PersistedSharedVmState {
            state: AvfLinuxSharedVmLifecycleState::Running,
            guest_identity: supported_guest_identity(),
            runtime_root: Some(runtime_root.clone()),
            rootfs_image: Some(shared_vm_rootfs_path(&temp)),
            kernel_path: Some(shared_vm_boot_kernel_path(&temp)),
            initrd_path: Some(initrd_path.clone()),
            runtime_version: Some("runtime-current".to_string()),
            runtime_shape_digest: Some(shared_vm_runtime_shape_digest(
                &runtime_root,
                &source_rootfs,
                &kernel_path,
                &initrd_path,
                "runtime-current",
            )),
            writable_surface_contract_digest: Some(
                shared_vm_writable_surface_contract_digest(&temp)
                    .expect("render writable-surface contract digest"),
            ),
            updated_at: Some(now_timestamp_string()),
            last_started_at: Some(now_timestamp_string()),
            last_saved_at: Some("older-save".to_string()),
            last_stopped_at: None,
            transition_status: Some(AvfLinuxSharedVmTransitionStatus::Ready),
            last_start_outcome: Some(AvfLinuxSharedVmStartOutcome::ColdBoot),
            last_stop_outcome: Some(AvfLinuxSharedVmStopOutcome::SavedStateWritten),
            last_restore_error: Some("old restore error".to_string()),
            last_save_error: None,
            relay_pid: Some(relay.id()),
            guest_agent_pid: Some(guest.id()),
            simulated: true,
            notes: vec!["simulated running state".to_string()],
        },
    )
    .expect("persist running state");

    let started = start_shared_vm(
        &temp,
        &runtime_root,
        &source_rootfs,
        &kernel_path,
        &initrd_path,
        "runtime-current".to_string(),
    )
    .expect("reuse already-running shared vm");

    assert_eq!(
        started.last_start_outcome,
        Some(AvfLinuxSharedVmStartOutcome::AlreadyRunning)
    );
    assert!(started.last_restore_error.is_none());
    assert_eq!(
        started.last_stop_outcome,
        Some(AvfLinuxSharedVmStopOutcome::SavedStateWritten)
    );
    assert!(relay.try_wait().expect("poll relay").is_none());
    assert!(guest.try_wait().expect("poll guest").is_none());

    let _ = relay.kill();
    let _ = guest.kill();
    let _ = relay.wait();
    let _ = guest.wait();
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[cfg(unix)]
#[test]
fn start_shared_vm_does_not_reuse_real_avf_without_guest_probe_marker() {
    let temp = PathBuf::from("/tmp").join(format!(
        "ctxavf-start-real-without-probe-marker-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    prepare_runtime_layout(&temp).expect("prepare runtime layout");

    let runtime_root = temp.join("runtime");
    let helpers_root = runtime_root.join("helpers");
    fs::create_dir_all(&helpers_root).expect("create helpers root");
    let source_rootfs = temp.join("source-rootfs.raw");
    fs::write(&source_rootfs, b"rootfs").expect("write rootfs");
    let kernel_path = helpers_root.join("kernel");
    fs::write(&kernel_path, b"kernel").expect("write kernel");
    let initrd_path = helpers_root.join("initrd");
    fs::write(&initrd_path, b"initrd").expect("write initrd");

    let mut relay = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("spawn relay placeholder");

    persist_state(
        &shared_vm_state_path(&temp),
        &PersistedSharedVmState {
            state: AvfLinuxSharedVmLifecycleState::Running,
            guest_identity: supported_guest_identity(),
            runtime_root: Some(runtime_root.clone()),
            rootfs_image: Some(shared_vm_rootfs_path(&temp)),
            kernel_path: Some(shared_vm_boot_kernel_path(&temp)),
            initrd_path: Some(initrd_path.clone()),
            runtime_version: Some("runtime-current".to_string()),
            runtime_shape_digest: Some(shared_vm_runtime_shape_digest(
                &runtime_root,
                &source_rootfs,
                &kernel_path,
                &initrd_path,
                "runtime-current",
            )),
            writable_surface_contract_digest: Some(
                shared_vm_writable_surface_contract_digest(&temp)
                    .expect("render writable-surface contract digest"),
            ),
            updated_at: Some(now_timestamp_string()),
            last_started_at: Some(now_timestamp_string()),
            last_saved_at: None,
            last_stopped_at: None,
            transition_status: Some(AvfLinuxSharedVmTransitionStatus::Ready),
            last_start_outcome: Some(AvfLinuxSharedVmStartOutcome::AlreadyRunning),
            last_stop_outcome: None,
            last_restore_error: None,
            last_save_error: None,
            relay_pid: Some(relay.id()),
            guest_agent_pid: None,
            simulated: false,
            notes: vec!["real avf owner state without guest probe marker".to_string()],
        },
    )
    .expect("persist running real avf state");

    let started = start_shared_vm(
        &temp,
        &runtime_root,
        &source_rootfs,
        &kernel_path,
        &initrd_path,
        "runtime-current".to_string(),
    )
    .expect("restart shared vm when guest probe marker is missing");

    assert_eq!(
        started.last_start_outcome,
        Some(AvfLinuxSharedVmStartOutcome::ColdBoot)
    );
    assert!(!started
        .notes
        .iter()
        .any(|note| note.contains("shared VM start reused an already-running")));
    assert!(
        wait_for_child_exit(&mut relay, Duration::from_secs(2)),
        "missing guest probe marker should force the stale real AVF owner to stop"
    );

    let _ = relay.kill();
    let _ = relay.wait();
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[test]
fn stop_shared_vm_discards_stale_saved_state_and_reports_cold_stop() {
    let temp = PathBuf::from("/tmp").join(format!(
        "ctxavf-stop-stale-save-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    prepare_runtime_layout(&temp).expect("prepare runtime layout");
    let saved_state_path = shared_vm_saved_state_path(&temp);
    fs::create_dir_all(saved_state_path.parent().expect("saved-state parent"))
        .expect("create saved-state parent");
    fs::write(&saved_state_path, b"saved-state").expect("seed saved state");
    persist_state(
        &shared_vm_state_path(&temp),
        &PersistedSharedVmState {
            state: AvfLinuxSharedVmLifecycleState::Running,
            guest_identity: supported_guest_identity(),
            runtime_root: None,
            rootfs_image: None,
            kernel_path: None,
            initrd_path: None,
            runtime_version: None,
            runtime_shape_digest: None,
            writable_surface_contract_digest: None,
            updated_at: Some(now_timestamp_string()),
            last_started_at: Some(now_timestamp_string()),
            last_saved_at: Some("old-save".to_string()),
            last_stopped_at: None,
            transition_status: Some(AvfLinuxSharedVmTransitionStatus::Ready),
            last_start_outcome: Some(AvfLinuxSharedVmStartOutcome::ColdBoot),
            last_stop_outcome: None,
            last_restore_error: None,
            last_save_error: None,
            relay_pid: None,
            guest_agent_pid: None,
            simulated: true,
            notes: vec!["running".to_string()],
        },
    )
    .expect("persist running state");

    let stopped = stop_shared_vm(&temp).expect("stop shared vm");

    assert!(matches!(
        stopped.state,
        AvfLinuxSharedVmLifecycleState::Stopped
    ));
    assert_eq!(
        stopped.last_stop_outcome,
        Some(AvfLinuxSharedVmStopOutcome::ColdStop)
    );
    assert!(stopped.last_save_error.is_none());
    assert!(
        !saved_state_path.exists(),
        "stale saved state should be removed on cold stop"
    );
    assert!(stopped
        .notes
        .iter()
        .any(|note| note.contains("discarded stale workspace VM saved state")));
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[test]
fn stop_shared_vm_clears_guest_control_ready_marker() {
    let temp = PathBuf::from("/tmp").join(format!(
        "ctxavf-stop-clears-ready-marker-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    prepare_runtime_layout(&temp).expect("prepare runtime layout");
    let ready_marker = shared_vm_guest_control_ready_path(&temp);
    let failed_marker = shared_vm_guest_control_failed_path(&temp);
    fs::create_dir_all(ready_marker.parent().expect("marker parent"))
        .expect("create marker parent");
    fs::write(&ready_marker, b"ready").expect("seed ready marker");
    fs::write(&failed_marker, b"failed").expect("seed failure marker");
    persist_state(
        &shared_vm_state_path(&temp),
        &PersistedSharedVmState {
            state: AvfLinuxSharedVmLifecycleState::Running,
            guest_identity: supported_guest_identity(),
            runtime_root: None,
            rootfs_image: None,
            kernel_path: None,
            initrd_path: None,
            runtime_version: None,
            runtime_shape_digest: None,
            writable_surface_contract_digest: None,
            updated_at: Some(now_timestamp_string()),
            last_started_at: Some(now_timestamp_string()),
            last_saved_at: None,
            last_stopped_at: None,
            transition_status: Some(AvfLinuxSharedVmTransitionStatus::Ready),
            last_start_outcome: Some(AvfLinuxSharedVmStartOutcome::ColdBoot),
            last_stop_outcome: None,
            last_restore_error: None,
            last_save_error: None,
            relay_pid: None,
            guest_agent_pid: None,
            simulated: true,
            notes: vec!["running".to_string()],
        },
    )
    .expect("persist running state");

    let stopped = stop_shared_vm(&temp).expect("stop shared vm");

    assert!(matches!(
        stopped.state,
        AvfLinuxSharedVmLifecycleState::Stopped
    ));
    assert!(
        !ready_marker.exists(),
        "guest-control ready marker should be removed on stop"
    );
    assert!(
        !failed_marker.exists(),
        "guest-control failure marker should be removed on stop"
    );
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[cfg(unix)]
#[test]
fn stop_shared_vm_discards_stale_saved_state_on_cold_stop() {
    let temp = PathBuf::from("/tmp").join(format!(
        "ctxavf-stop-cold-discard-save-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    prepare_runtime_layout(&temp).expect("prepare runtime layout");

    let mut relay = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("spawn relay placeholder");
    let mut guest = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("spawn guest placeholder");
    let saved_state_path = shared_vm_saved_state_path(&temp);
    fs::create_dir_all(saved_state_path.parent().expect("saved-state parent"))
        .expect("create saved-state parent");
    fs::write(&saved_state_path, b"stale-save").expect("seed stale saved state");

    persist_state(
        &shared_vm_state_path(&temp),
        &PersistedSharedVmState {
            state: AvfLinuxSharedVmLifecycleState::Running,
            guest_identity: supported_guest_identity(),
            runtime_root: Some(temp.join("runtime")),
            rootfs_image: Some(shared_vm_rootfs_path(&temp)),
            kernel_path: Some(temp.join("kernel")),
            initrd_path: Some(temp.join("initrd")),
            runtime_version: Some("runtime-current".to_string()),
            runtime_shape_digest: Some("digest".to_string()),
            writable_surface_contract_digest: None,
            updated_at: Some(now_timestamp_string()),
            last_started_at: Some(now_timestamp_string()),
            last_saved_at: Some("older-save".to_string()),
            last_stopped_at: None,
            transition_status: Some(AvfLinuxSharedVmTransitionStatus::Ready),
            last_start_outcome: Some(AvfLinuxSharedVmStartOutcome::Restored),
            last_stop_outcome: Some(AvfLinuxSharedVmStopOutcome::SavedStateWritten),
            last_restore_error: None,
            last_save_error: None,
            relay_pid: Some(relay.id()),
            guest_agent_pid: Some(guest.id()),
            simulated: true,
            notes: vec!["simulated running state".to_string()],
        },
    )
    .expect("persist running state");

    let stopped = stop_shared_vm(&temp).expect("stop shared vm");
    assert!(matches!(
        stopped.state,
        AvfLinuxSharedVmLifecycleState::Stopped
    ));
    assert_eq!(
        stopped.last_stop_outcome,
        Some(AvfLinuxSharedVmStopOutcome::ColdStop)
    );
    assert!(stopped.last_save_error.is_none());
    assert!(
        !saved_state_path.exists(),
        "cold stop should discard stale saved state"
    );
    assert!(stopped
        .notes
        .iter()
        .any(|note| note.contains("discarded stale workspace VM saved state")));

    assert!(wait_for_child_exit(&mut relay, Duration::from_secs(2)));
    assert!(wait_for_child_exit(&mut guest, Duration::from_secs(2)));
    let _ = relay.kill();
    let _ = guest.kill();
    let _ = relay.wait();
    let _ = guest.wait();
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[cfg(unix)]
#[test]
fn start_shared_vm_forces_restart_when_runtime_changes_while_vm_is_live() {
    let temp = PathBuf::from("/tmp").join(format!(
        "ctxavf-start-runtime-restart-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    prepare_runtime_layout(&temp).expect("prepare runtime layout");

    let runtime_root = temp.join("runtime-next");
    let helpers_root = runtime_root.join("helpers");
    fs::create_dir_all(&helpers_root).expect("create helpers root");
    let source_rootfs = temp.join("source-rootfs.raw");
    fs::write(&source_rootfs, b"rootfs").expect("write rootfs");
    let kernel_path = helpers_root.join("kernel");
    fs::write(&kernel_path, b"kernel").expect("write kernel");
    let initrd_path = helpers_root.join("initrd");
    fs::write(&initrd_path, b"initrd-next").expect("write initrd");

    let mut relay = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("spawn relay placeholder");
    let mut guest = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("spawn guest placeholder");

    persist_state(
        &shared_vm_state_path(&temp),
        &PersistedSharedVmState {
            state: AvfLinuxSharedVmLifecycleState::Running,
            guest_identity: supported_guest_identity(),
            runtime_root: Some(temp.join("runtime-prev")),
            rootfs_image: Some(shared_vm_rootfs_path(&temp)),
            kernel_path: Some(temp.join("kernel-prev")),
            initrd_path: Some(temp.join("initrd-prev")),
            runtime_version: Some("runtime-prev".to_string()),
            runtime_shape_digest: None,
            writable_surface_contract_digest: Some(
                shared_vm_writable_surface_contract_digest(&temp)
                    .expect("render writable-surface contract digest"),
            ),
            updated_at: Some(now_timestamp_string()),
            last_started_at: Some(now_timestamp_string()),
            last_saved_at: None,
            last_stopped_at: None,
            transition_status: Some(AvfLinuxSharedVmTransitionStatus::Ready),
            last_start_outcome: Some(AvfLinuxSharedVmStartOutcome::AlreadyRunning),
            last_stop_outcome: None,
            last_restore_error: None,
            last_save_error: None,
            relay_pid: Some(relay.id()),
            guest_agent_pid: Some(guest.id()),
            simulated: true,
            notes: vec!["simulated running state".to_string()],
        },
    )
    .expect("persist running state");
    for path in [
        shared_vm_control_socket_path(&temp),
        shared_vm_guest_agent_socket_path(&temp),
        shared_vm_guest_control_ready_path(&temp),
        shared_vm_saved_state_path(&temp),
    ] {
        fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
        fs::write(&path, b"x").expect("seed derived state");
    }

    let started = start_shared_vm(
        &temp,
        &runtime_root,
        &source_rootfs,
        &kernel_path,
        &initrd_path,
        "runtime-next".to_string(),
    )
    .expect("restart shared vm after runtime change");

    assert!(matches!(
        started.state,
        AvfLinuxSharedVmLifecycleState::Running
    ));
    assert!(started
        .notes
        .iter()
        .any(|note| { note.contains("forcing a stop before restart") }));
    assert!(!started
        .notes
        .iter()
        .any(|note| { note.contains("shared VM start reused an already-running") }));
    assert_eq!(started.runtime_version.as_deref(), Some("runtime-next"));
    assert_eq!(
        started.runtime_root.as_deref(),
        Some(runtime_root.as_path())
    );
    let persisted = load_state(&shared_vm_state_path(&temp))
        .expect("load persisted state")
        .expect("persisted shared vm state");
    assert!(persisted.last_saved_at.is_none());
    for path in [
        shared_vm_control_socket_path(&temp),
        shared_vm_guest_agent_socket_path(&temp),
        shared_vm_guest_control_ready_path(&temp),
        shared_vm_saved_state_path(&temp),
    ] {
        assert!(
            !path.exists(),
            "{} should be cleared before restart",
            path.display()
        );
    }
    assert!(wait_for_child_exit(&mut relay, Duration::from_secs(2)));
    assert!(wait_for_child_exit(&mut guest, Duration::from_secs(2)));

    let _ = relay.kill();
    let _ = guest.kill();
    let _ = relay.wait();
    let _ = guest.wait();
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[cfg(unix)]
#[test]
fn start_shared_vm_forces_restart_when_runtime_digest_changes_with_same_version() {
    let temp = PathBuf::from("/tmp").join(format!(
        "ctxavf-start-runtime-digest-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    prepare_runtime_layout(&temp).expect("prepare runtime layout");

    let runtime_root = temp.join("runtime-stable");
    let helpers_root = runtime_root.join("helpers");
    fs::create_dir_all(&helpers_root).expect("create helpers root");
    let source_rootfs_v1 = temp.join("source-rootfs-v1.raw");
    fs::write(&source_rootfs_v1, b"rootfs-v1").expect("write source rootfs v1");
    let source_rootfs_v2 = temp.join("source-rootfs-v2.raw");
    fs::write(&source_rootfs_v2, b"rootfs-v2").expect("write source rootfs v2");
    let kernel_path = helpers_root.join("kernel");
    fs::write(&kernel_path, b"kernel").expect("write kernel");
    let initrd_path = helpers_root.join("initrd");
    fs::write(&initrd_path, b"initrd").expect("write initrd");
    fs::create_dir_all(
        shared_vm_rootfs_path(&temp)
            .parent()
            .expect("rootfs parent"),
    )
    .expect("create rootfs parent");
    fs::write(shared_vm_rootfs_path(&temp), b"stale-rootfs").expect("seed staged rootfs");

    let mut relay = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("spawn relay placeholder");
    let mut guest = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("spawn guest placeholder");

    persist_state(
        &shared_vm_state_path(&temp),
        &PersistedSharedVmState {
            state: AvfLinuxSharedVmLifecycleState::Running,
            guest_identity: supported_guest_identity(),
            runtime_root: Some(runtime_root.clone()),
            rootfs_image: Some(shared_vm_rootfs_path(&temp)),
            kernel_path: Some(shared_vm_boot_kernel_path(&temp)),
            initrd_path: Some(initrd_path.clone()),
            runtime_version: Some("runtime-stable".to_string()),
            runtime_shape_digest: Some(shared_vm_runtime_shape_digest(
                &runtime_root,
                &source_rootfs_v1,
                &kernel_path,
                &initrd_path,
                "runtime-stable",
            )),
            writable_surface_contract_digest: Some(
                shared_vm_writable_surface_contract_digest(&temp)
                    .expect("render writable-surface contract digest"),
            ),
            updated_at: Some(now_timestamp_string()),
            last_started_at: Some(now_timestamp_string()),
            last_saved_at: None,
            last_stopped_at: None,
            transition_status: Some(AvfLinuxSharedVmTransitionStatus::Ready),
            last_start_outcome: Some(AvfLinuxSharedVmStartOutcome::AlreadyRunning),
            last_stop_outcome: None,
            last_restore_error: None,
            last_save_error: None,
            relay_pid: Some(relay.id()),
            guest_agent_pid: Some(guest.id()),
            simulated: true,
            notes: vec!["simulated running state".to_string()],
        },
    )
    .expect("persist running state");

    let started = start_shared_vm(
        &temp,
        &runtime_root,
        &source_rootfs_v2,
        &kernel_path,
        &initrd_path,
        "runtime-stable".to_string(),
    )
    .expect("restart shared vm after runtime digest change");

    assert!(matches!(
        started.state,
        AvfLinuxSharedVmLifecycleState::Running
    ));
    assert!(started
        .notes
        .iter()
        .any(|note| note.contains("forcing a stop before restart")));
    assert!(!started
        .notes
        .iter()
        .any(|note| { note.contains("shared VM start reused an already-running") }));
    assert_eq!(started.runtime_version.as_deref(), Some("runtime-stable"));
    assert_eq!(
        started.runtime_root.as_deref(),
        Some(runtime_root.as_path())
    );
    assert_eq!(
        fs::read(shared_vm_rootfs_path(&temp)).expect("read staged rootfs"),
        b"rootfs-v2"
    );
    assert!(wait_for_child_exit(&mut relay, Duration::from_secs(2)));
    assert!(wait_for_child_exit(&mut guest, Duration::from_secs(2)));

    let _ = relay.kill();
    let _ = guest.kill();
    let _ = relay.wait();
    let _ = guest.wait();
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[cfg(unix)]
#[test]
fn start_shared_vm_forces_restart_when_writable_surface_contract_changes_while_vm_is_live() {
    let temp = PathBuf::from("/tmp").join(format!(
        "ctxavf-start-writable-surface-restart-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    prepare_runtime_layout(&temp).expect("prepare runtime layout");

    let runtime_root = temp.join("runtime");
    let helpers_root = runtime_root.join("helpers");
    fs::create_dir_all(&helpers_root).expect("create helpers root");
    let source_rootfs = temp.join("source-rootfs.raw");
    fs::write(&source_rootfs, b"rootfs").expect("write rootfs");
    let kernel_path = helpers_root.join("kernel");
    fs::write(&kernel_path, b"kernel").expect("write kernel");
    let initrd_path = helpers_root.join("initrd");
    fs::write(&initrd_path, b"initrd").expect("write initrd");
    let expected_writable_surface_digest = shared_vm_writable_surface_contract_digest(&temp)
        .expect("render writable-surface contract digest");

    let mut relay = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("spawn relay placeholder");
    let mut guest = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("spawn guest placeholder");

    persist_state(
        &shared_vm_state_path(&temp),
        &PersistedSharedVmState {
            state: AvfLinuxSharedVmLifecycleState::Running,
            guest_identity: supported_guest_identity(),
            runtime_root: Some(runtime_root.clone()),
            rootfs_image: Some(shared_vm_rootfs_path(&temp)),
            kernel_path: Some(shared_vm_boot_kernel_path(&temp)),
            initrd_path: Some(initrd_path.clone()),
            runtime_version: Some("runtime-current".to_string()),
            runtime_shape_digest: Some(shared_vm_runtime_shape_digest(
                &runtime_root,
                &source_rootfs,
                &kernel_path,
                &initrd_path,
                "runtime-current",
            )),
            writable_surface_contract_digest: Some("stale-writable-surface".to_string()),
            updated_at: Some(now_timestamp_string()),
            last_started_at: Some(now_timestamp_string()),
            last_saved_at: None,
            last_stopped_at: None,
            transition_status: Some(AvfLinuxSharedVmTransitionStatus::Ready),
            last_start_outcome: Some(AvfLinuxSharedVmStartOutcome::AlreadyRunning),
            last_stop_outcome: None,
            last_restore_error: None,
            last_save_error: None,
            relay_pid: Some(relay.id()),
            guest_agent_pid: Some(guest.id()),
            simulated: true,
            notes: vec!["simulated running state".to_string()],
        },
    )
    .expect("persist running state");
    for path in [
        shared_vm_control_socket_path(&temp),
        shared_vm_guest_agent_socket_path(&temp),
        shared_vm_guest_control_ready_path(&temp),
        shared_vm_saved_state_path(&temp),
    ] {
        fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
        fs::write(&path, b"x").expect("seed derived state");
    }

    let started = start_shared_vm(
        &temp,
        &runtime_root,
        &source_rootfs,
        &kernel_path,
        &initrd_path,
        "runtime-current".to_string(),
    )
    .expect("restart shared vm after writable-surface contract change");

    assert!(matches!(
        started.state,
        AvfLinuxSharedVmLifecycleState::Running
    ));
    assert!(started
        .notes
        .iter()
        .any(|note| note.contains("writable-surface contract")));
    assert!(!started
        .notes
        .iter()
        .any(|note| { note.contains("shared VM start reused an already-running") }));
    assert_eq!(
        started.writable_surface_contract_digest.as_deref(),
        Some(expected_writable_surface_digest.as_str())
    );
    let persisted = load_state(&shared_vm_state_path(&temp))
        .expect("load persisted state")
        .expect("persisted shared vm state");
    assert_eq!(
        persisted.writable_surface_contract_digest.as_deref(),
        Some(expected_writable_surface_digest.as_str())
    );
    assert!(persisted.last_saved_at.is_none());
    for path in [
        shared_vm_control_socket_path(&temp),
        shared_vm_guest_agent_socket_path(&temp),
        shared_vm_guest_control_ready_path(&temp),
        shared_vm_saved_state_path(&temp),
    ] {
        assert!(
            !path.exists(),
            "{} should be cleared before restart",
            path.display()
        );
    }
    assert!(wait_for_child_exit(&mut relay, Duration::from_secs(2)));
    assert!(wait_for_child_exit(&mut guest, Duration::from_secs(2)));

    let _ = relay.kill();
    let _ = guest.kill();
    let _ = relay.wait();
    let _ = guest.wait();
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[test]
fn shared_vm_guest_readiness_args_include_bridge_probe() {
    let rendered = shared_vm_guest_readiness_args().join(" ");
    assert!(rendered.contains("bridge_probe_failed"));
    assert!(rendered.contains("readiness phase"));
    assert!(rendered.contains("ip link add name \"$probe_bridge\" type bridge"));
    assert!(rendered.contains("writable-root-separate"));
    assert!(rendered.contains("root-on-writable-root"));
    assert!(rendered.contains("tmp-on-writable-root"));
    assert!(rendered.contains("var-tmp-on-writable-root"));
    assert!(rendered.contains("var-log-on-writable-root"));
    assert!(rendered.contains("containerd-root-on-writable-root"));
    assert!(rendered.contains("buildkit-root-on-writable-root"));
    assert!(rendered.contains("nerdctl-root-on-writable-root"));
    assert!(rendered.contains("cni-config-on-writable-root"));
    assert!(rendered.contains("cni-state-on-writable-root"));
    assert!(rendered.contains("guest-policy-masked-units"));
    assert!(rendered.contains("masked-runtime"));
    assert!(rendered.contains("systemctl is-enabled \"$unit\" 2>/dev/null || true"));
    assert!(rendered.contains("stat -fc %d"));
    assert!(rendered.contains("/root"));
    assert!(rendered.contains("/var/log"));
    assert!(rendered.contains("/var/lib/nerdctl"));
    assert!(rendered.contains("/etc/cni/net.d"));
    assert!(rendered.contains("/var/lib/cni"));
    assert!(rendered.contains(SHARED_VM_GUEST_NERDCTL_BIN));
    assert!(rendered.contains(SHARED_VM_GUEST_BUILDKITCTL_BIN));
    for unit in SHARED_VM_GUEST_POLICY_MASKED_UNITS {
        assert!(
            rendered.contains(unit),
            "missing masked readiness unit {unit}"
        );
    }
    assert!(rendered.contains(&format!(
        "timeout --kill-after=1s --preserve-status {SHARED_VM_READINESS_PHASE_TIMEOUT_SECONDS}s"
    )));
}

#[test]
fn readiness_phase_line_extraction_filters_non_phase_output() {
    let stdout = b"hello\n[ctx-avf-linux] readiness phase buildctl ok in 12ms\n";
    let stderr =
        b"noise\n[ctx-avf-linux] readiness phase bridge-probe failed with exit 41 after 3ms\n";
    let lines = extract_shared_vm_readiness_phase_lines(stdout, stderr);
    assert_eq!(
        lines,
        vec![
            "[ctx-avf-linux] readiness phase buildctl ok in 12ms".to_string(),
            "[ctx-avf-linux] readiness phase bridge-probe failed with exit 41 after 3ms"
                .to_string(),
        ]
    );
}

#[test]
fn readiness_phase_summary_strips_helper_prefix() {
    let summary = summarize_shared_vm_readiness_phase_lines(&[
        "[ctx-avf-linux] readiness phase containerd ok in 10ms".to_string(),
        "[ctx-avf-linux] readiness phase buildkit ok in 11ms".to_string(),
    ]);
    assert_eq!(summary, "containerd ok in 10ms, buildkit ok in 11ms");
}

#[test]
fn cold_boot_timeout_extends_when_rootfs_is_materialized() {
    assert_eq!(
        default_real_guest_exec_ready_timeout(),
        Duration::from_secs(30)
    );
    assert_eq!(
        cold_boot_real_guest_exec_ready_timeout(),
        Duration::from_secs(600)
    );
    assert_eq!(
        real_guest_exec_ready_timeout_for_start(None, true),
        default_real_guest_exec_ready_timeout()
    );
    assert_eq!(
        real_guest_exec_ready_timeout_for_start(
            Some("copied rootfs image into helper-managed writable path"),
            true
        ),
        cold_boot_real_guest_exec_ready_timeout()
    );
    assert_eq!(
        real_guest_exec_ready_timeout_for_start(None, false),
        cold_boot_real_guest_exec_ready_timeout()
    );
}

#[test]
fn writable_surface_readiness_failures_trigger_writable_rootfs_reset() {
    for rendered in [
        "guest exec readiness probe exited 41 (stdout='', stderr='[ctx-avf-linux] bridge_probe_failed')",
        "[ctx-avf-linux] readiness phase writable-root-separate failed with exit 1 after 2ms",
        "[ctx-avf-linux] readiness phase root-on-writable-root failed with exit 1 after 2ms",
        "[ctx-avf-linux] readiness phase tmp-on-writable-root failed with exit 1 after 2ms",
        "[ctx-avf-linux] readiness phase var-tmp-on-writable-root failed with exit 1 after 2ms",
        "[ctx-avf-linux] readiness phase containerd-root-on-writable-root failed with exit 1 after 2ms",
        "[ctx-avf-linux] readiness phase buildkit-root-on-writable-root failed with exit 1 after 2ms",
        "[ctx-avf-linux] readiness phase nerdctl-root-on-writable-root failed with exit 1 after 2ms",
        "[ctx-avf-linux] readiness phase cni-config-on-writable-root failed with exit 1 after 2ms",
        "[ctx-avf-linux] readiness phase cni-state-on-writable-root failed with exit 1 after 2ms",
        "[ctx-avf-linux] readiness phase guest-policy-masked-units failed with exit 1 after 2ms",
    ] {
        let err = anyhow::anyhow!(rendered.to_string());
        assert!(
            shared_vm_readiness_failure_requires_writable_rootfs_reset(&err),
            "expected reset-worthy readiness failure for: {rendered}"
        );
    }
    let unrelated = anyhow::anyhow!("guest exec readiness probe exited 1");
    assert!(!shared_vm_readiness_failure_requires_writable_rootfs_reset(
        &unrelated
    ));
}

#[test]
fn stop_shared_vm_owner_after_readiness_failure_waits_for_exit() {
    let temp = std::env::temp_dir().join(format!(
        "ctx-avf-owner-stop-after-readiness-failure-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    fs::create_dir_all(&temp).expect("create tempdir");

    let mut child = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("spawn owner fixture");

    stop_shared_vm_owner_after_readiness_failure(&temp, child.id())
        .expect("stop helper should wait for owner exit");
    match child.try_wait() {
        Ok(Some(_)) => {}
        Ok(None) => panic!("owner fixture should have exited after stop helper"),
        Err(err) if err.raw_os_error() == Some(libc::ECHILD) => {}
        Err(err) => panic!("unexpected owner fixture wait error: {err}"),
    }

    let log = fs::read_to_string(shared_vm_log_path(&temp)).expect("read shared vm log");
    assert!(
        log.contains("waiting up to"),
        "expected shutdown-wait log entry, got: {log}"
    );

    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[test]
fn cloud_init_applies_guest_policy_before_writable_root_setup_and_service_startup() {
    let user_data = render_shared_vm_cloud_init_user_data(
        Path::new("/tmp"),
        b"guest-agent",
        None,
        Path::new("/tmp/runtime/helpers/container-stack.tar.gz"),
        "deadbeef",
    )
    .expect("render cloud-init user-data");
    let guest_policy_index = user_data.find("bootcmd:").expect("guest-policy bootcmd");
    let daemon_reload_index = user_data
        .find("- [ systemctl, daemon-reload ]")
        .expect("daemon reload command");
    let host_data_enable_index = user_data
        .find(&format!(
            "systemctl enable --now {service_name}",
            service_name = SHARED_VM_HOST_DATA_SERVICE_NAME
        ))
        .expect("host-data enable command");
    let data_disk_enable_index = user_data
        .find("systemctl enable --now ctx-avf-data-disk.service")
        .expect("data-disk enable command");
    let prepare_guest_agent_index = user_data
        .find("preparing ctx-avf-linux-guest-agent.service")
        .expect("prepare guest-agent command");
    let containerd_enable_index = user_data
        .find("systemctl enable --now containerd.service")
        .expect("containerd enable command");
    assert!(guest_policy_index < daemon_reload_index);
    assert!(daemon_reload_index < host_data_enable_index);
    assert!(host_data_enable_index < data_disk_enable_index);
    assert!(guest_policy_index < prepare_guest_agent_index);
    assert!(data_disk_enable_index < prepare_guest_agent_index);
    assert!(host_data_enable_index < prepare_guest_agent_index);
    assert!(prepare_guest_agent_index < containerd_enable_index);
}

#[test]
fn guest_policy_script_masks_background_rootfs_mutators() {
    let script = render_shared_vm_guest_policy_script();
    assert!(script.contains("mkdir -p /etc/systemd/system"));
    assert!(script.contains("ln -s /dev/null"));
    assert!(script.contains("guest policy already masked"));
    assert!(script.contains("systemctl stop \"$unit\""));
    assert!(script.contains("systemctl reset-failed \"$unit\""));
    assert!(script.contains("systemctl daemon-reload"));
    for unit in SHARED_VM_GUEST_POLICY_MASKED_UNITS {
        assert!(script.contains(unit), "missing masked unit {unit}");
    }
}

#[test]
fn avf_vm_save_restore_timeout_exceeds_default_completion_timeout() {
    assert!(VM_SAVE_RESTORE_COMPLETION_TIMEOUT > VM_LIFECYCLE_COMPLETION_TIMEOUT);
    assert!(VM_LIFECYCLE_COMPLETION_TIMEOUT > GUEST_EXEC_CONNECT_TIMEOUT);
    assert!(SHARED_VM_SHUTDOWN_WAIT_TIMEOUT > VM_SAVE_RESTORE_COMPLETION_TIMEOUT);
}

#[test]
fn resetting_writable_runtime_state_removes_only_derived_files() {
    let temp = PathBuf::from("/tmp").join(format!(
        "ctxavf-reset-runtime-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    let control_socket = shared_vm_control_socket_path(&temp);
    let guest_agent_socket = shared_vm_guest_agent_socket_path(&temp);
    let ready_marker = shared_vm_guest_control_ready_path(&temp);
    let saved_state = shared_vm_saved_state_path(&temp);
    let rootfs = shared_vm_rootfs_path(&temp);
    let data_disk = shared_vm_data_disk_path(&temp);
    for path in [
        &control_socket,
        &guest_agent_socket,
        &ready_marker,
        &saved_state,
        &rootfs,
    ] {
        fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
        fs::write(path, b"x").expect("seed file");
    }
    fs::create_dir_all(data_disk.parent().expect("data-disk parent")).expect("create parent");
    fs::write(&data_disk, b"x").expect("seed data-disk");

    reset_writable_shared_vm_runtime_state(&temp).expect("reset runtime state");

    for path in [
        &control_socket,
        &guest_agent_socket,
        &ready_marker,
        &saved_state,
        &rootfs,
    ] {
        assert!(!path.exists(), "{} should be removed", path.display());
    }
    assert!(
        data_disk.exists(),
        "{} should be preserved",
        data_disk.display()
    );
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[test]
fn shared_vm_start_lock_times_out_while_live_holder_exists() {
    let temp = std::env::temp_dir().join(format!(
        "ctx-avf-start-lock-timeout-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    fs::create_dir_all(shared_vm_root(&temp)).expect("create shared vm root");
    fs::write(
        shared_vm_start_lock_path(&temp),
        format!("{}\n", std::process::id()),
    )
    .expect("seed live start lock");

    let err = acquire_shared_vm_start_lock(&temp, Duration::from_millis(100))
        .expect_err("live holder should block acquisition");
    assert!(err
        .to_string()
        .contains("timed out waiting for shared VM start lock"));
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[test]
fn shared_vm_start_lock_replaces_stale_holder() {
    let temp = std::env::temp_dir().join(format!(
        "ctx-avf-start-lock-stale-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    fs::create_dir_all(shared_vm_root(&temp)).expect("create shared vm root");
    fs::write(shared_vm_start_lock_path(&temp), "999999\n").expect("seed stale start lock");

    let guard = acquire_shared_vm_start_lock(&temp, Duration::from_secs(1))
        .expect("stale holder should be replaced");
    let raw = fs::read_to_string(shared_vm_start_lock_path(&temp)).expect("read lock");
    assert_eq!(
        parse_shared_vm_start_lock_pid(&raw),
        Some(std::process::id())
    );
    drop(guard);
    assert!(
        !shared_vm_start_lock_path(&temp).exists(),
        "lock should be removed when guard drops"
    );
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[test]
fn cloud_init_meta_data_changes_when_guest_payload_changes() {
    let first = render_shared_vm_cloud_init_meta_data(
        Path::new("/tmp/a"),
        b"guest-agent-a",
        Some(b"egress-proxy-a"),
        "container-stack-a",
    )
    .expect("render first cloud-init meta-data");
    let second = render_shared_vm_cloud_init_meta_data(
        Path::new("/tmp/b"),
        b"guest-agent-b",
        Some(b"egress-proxy-b"),
        "container-stack-b",
    )
    .expect("render second cloud-init meta-data");
    assert!(first.contains("instance-id: ctx-avf-linux-"));
    assert_ne!(first, second);
}

#[cfg(target_os = "macos")]
#[test]
fn transient_guest_control_connect_nserrors_retry() {
    assert!(is_transient_guest_control_connect_nserror(
        "NSPOSIXErrorDomain",
        libc::ECONNRESET as isize
    ));
    assert!(is_transient_guest_control_connect_nserror(
        "NSPOSIXErrorDomain",
        libc::ECONNREFUSED as isize
    ));
    assert!(!is_transient_guest_control_connect_nserror(
        "NSPOSIXErrorDomain",
        libc::ENOENT as isize
    ));
    assert!(!is_transient_guest_control_connect_nserror(
        "SomeOtherDomain",
        libc::ECONNRESET as isize
    ));
}

#[test]
fn wait_for_guest_control_ready_marker_observes_marker_creation() {
    let temp = std::env::temp_dir().join(format!(
        "ctx-avf-ready-marker-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    fs::create_dir_all(&temp).expect("create tempdir");
    let marker = shared_vm_guest_control_ready_path(&temp);
    let marker_for_thread = marker.clone();
    let writer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        if let Some(parent) = marker_for_thread.parent() {
            fs::create_dir_all(parent).expect("create marker parent");
        }
        fs::write(&marker_for_thread, b"ready").expect("write ready marker");
    });

    wait_for_guest_control_ready_marker(&temp, Duration::from_secs(1))
        .expect("marker should become ready");
    writer.join().expect("writer thread");
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[test]
fn wait_for_guest_control_ready_marker_times_out_without_marker() {
    let temp = std::env::temp_dir().join(format!(
        "ctx-avf-ready-marker-timeout-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    fs::create_dir_all(&temp).expect("create tempdir");

    let err = wait_for_guest_control_ready_marker(&temp, Duration::from_millis(100))
        .expect_err("missing marker should time out");
    assert!(err
        .to_string()
        .contains("timed out waiting for guest control ready marker"));
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[test]
fn wait_for_guest_control_ready_marker_surfaces_failure_marker_and_log_tail() {
    let temp = std::env::temp_dir().join(format!(
        "ctx-avf-ready-marker-failure-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    fs::create_dir_all(shared_vm_logs_root(&temp)).expect("create logs root");
    let failure_marker = shared_vm_guest_control_failed_path(&temp);
    let guest_agent_log = shared_vm_guest_agent_log_path(&temp);
    let failure_marker_for_thread = failure_marker.clone();
    let guest_agent_log_for_thread = guest_agent_log.clone();
    let writer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        if let Some(parent) = failure_marker_for_thread.parent() {
            fs::create_dir_all(parent).expect("create marker parent");
        }
        fs::write(
            &guest_agent_log_for_thread,
            b"[ctx-avf-linux] guest-agent launcher starting\n[ctx-avf-linux] guest-agent exited before ready with status 1\n",
        )
        .expect("write guest agent log");
        fs::write(
            &failure_marker_for_thread,
            b"[ctx-avf-linux] guest-agent exited before ready with status 1\n",
        )
        .expect("write failure marker");
    });

    let err = wait_for_guest_control_ready_marker(&temp, Duration::from_secs(1))
        .expect_err("failure marker should fail readiness");
    let rendered = err.to_string();
    assert!(rendered.contains("guest control failed before ready marker"));
    assert!(rendered.contains("guest-agent exited before ready with status 1"));
    assert!(rendered.contains("guest-agent log tail"));

    writer.join().expect("writer thread");
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[test]
fn shared_vm_owner_guest_probe_ready_requires_guest_control_marker() {
    let temp = std::env::temp_dir().join(format!(
        "ctx-avf-owner-probe-ready-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    fs::create_dir_all(&temp).expect("create tempdir");
    assert!(!shared_vm_owner_guest_probe_ready(&temp));
    let marker = shared_vm_guest_control_ready_path(&temp);
    if let Some(parent) = marker.parent() {
        fs::create_dir_all(parent).expect("create marker parent");
    }
    fs::write(&marker, b"ready").expect("write ready marker");
    assert!(shared_vm_owner_guest_probe_ready(&temp));
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[cfg(all(target_os = "macos", unix))]
#[test]
fn wait_for_real_guest_exec_ready_succeeds_without_guest_control_marker() {
    use std::thread;

    let temp = PathBuf::from("/tmp").join(format!(
        "ctx-avf-real-ready-no-marker-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    fs::create_dir_all(&temp).expect("create tempdir");
    let listener = bind_shared_vm_control_listener(&temp).expect("bind control socket");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept control socket");
        let frame = read_exec_frame(&mut stream)
            .expect("read request")
            .expect("request frame");
        let request = match frame {
            AvfLinuxExecFrame::Request(request) => request,
            other => panic!("expected request frame, got {other:?}"),
        };
        assert_eq!(request.command, "/bin/sh");
        assert_eq!(request.cwd, "/");
        assert_eq!(request.user.as_deref(), Some("root"));
        write_exec_frame(
            &mut stream,
            &AvfLinuxExecFrame::Stderr(
                b"[ctx-avf-linux] readiness phase containerd ok in 10ms\n[ctx-avf-linux] readiness phase buildkit ok in 11ms\n"
                    .to_vec(),
            ),
        )
        .expect("write readiness phases");
        write_exec_frame(
            &mut stream,
            &AvfLinuxExecFrame::Exit(AvfLinuxExecExit { exit_code: 0 }),
        )
        .expect("write exit frame");
    });

    let readiness =
        wait_for_real_guest_exec_ready(&temp, Duration::from_secs(1)).expect("guest ready");
    assert_eq!(readiness.attempts, 1);
    assert_eq!(
        readiness.phase_lines,
        vec![
            "[ctx-avf-linux] readiness phase containerd ok in 10ms".to_string(),
            "[ctx-avf-linux] readiness phase buildkit ok in 11ms".to_string(),
        ]
    );
    assert!(!shared_vm_owner_guest_probe_ready(&temp));

    server.join().expect("server thread");
    let control_socket = shared_vm_control_socket_path(&temp);
    if control_socket.exists() {
        fs::remove_file(&control_socket).expect("cleanup control socket");
    }
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[cfg(all(target_os = "macos", unix))]
#[test]
fn wait_for_real_guest_launch_ready_requires_guest_control_marker() {
    use std::thread;

    let temp = PathBuf::from("/tmp").join(format!(
        "ctx-avf-launch-ready-marker-required-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    fs::create_dir_all(&temp).expect("create tempdir");
    let listener = bind_shared_vm_control_listener(&temp).expect("bind control socket");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept control socket");
        let frame = read_exec_frame(&mut stream)
            .expect("read request")
            .expect("request frame");
        let request = match frame {
            AvfLinuxExecFrame::Request(request) => request,
            other => panic!("expected request frame, got {other:?}"),
        };
        assert_eq!(request.command, "/bin/sh");
        write_exec_frame(
            &mut stream,
            &AvfLinuxExecFrame::Exit(AvfLinuxExecExit { exit_code: 0 }),
        )
        .expect("write exit frame");
    });

    let err = wait_for_real_guest_launch_ready_with_owner_process(
        &temp,
        Duration::from_millis(250),
        None,
    )
    .expect_err("launch readiness should require the guest control ready marker");
    assert!(err.to_string().contains("guest control ready marker"));

    server.join().expect("server thread");
    let control_socket = shared_vm_control_socket_path(&temp);
    if control_socket.exists() {
        fs::remove_file(&control_socket).expect("cleanup control socket");
    }
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[cfg(all(target_os = "macos", unix))]
#[test]
fn wait_for_real_guest_launch_ready_backfills_ready_marker_after_restore_hit() {
    use std::thread;

    let temp = PathBuf::from("/tmp").join(format!(
        "ctx-avf-launch-ready-restore-hit-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    prepare_runtime_layout(&temp).expect("prepare runtime layout");
    persist_state(
        &shared_vm_state_path(&temp),
        &PersistedSharedVmState {
            state: AvfLinuxSharedVmLifecycleState::Running,
            guest_identity: supported_guest_identity(),
            runtime_root: None,
            rootfs_image: None,
            kernel_path: None,
            initrd_path: None,
            runtime_version: None,
            runtime_shape_digest: None,
            writable_surface_contract_digest: None,
            updated_at: Some(now_timestamp_string()),
            last_started_at: Some(now_timestamp_string()),
            last_saved_at: Some(now_timestamp_string()),
            last_stopped_at: None,
            transition_status: Some(AvfLinuxSharedVmTransitionStatus::Scaffolded),
            last_start_outcome: Some(AvfLinuxSharedVmStartOutcome::Restored),
            last_stop_outcome: Some(AvfLinuxSharedVmStopOutcome::SavedStateWritten),
            last_restore_error: None,
            last_save_error: None,
            relay_pid: Some(std::process::id()),
            guest_agent_pid: None,
            simulated: false,
            notes: vec!["restored".to_string()],
        },
    )
    .expect("persist restored state");
    let failure_marker = shared_vm_guest_control_failed_path(&temp);
    if let Some(parent) = failure_marker.parent() {
        fs::create_dir_all(parent).expect("create failure marker parent");
    }
    fs::write(&failure_marker, b"stale failure").expect("seed stale failure marker");
    let listener = bind_shared_vm_control_listener(&temp).expect("bind control socket");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept control socket");
        let frame = read_exec_frame(&mut stream)
            .expect("read request")
            .expect("request frame");
        let request = match frame {
            AvfLinuxExecFrame::Request(request) => request,
            other => panic!("expected request frame, got {other:?}"),
        };
        assert_eq!(request.command, "/bin/sh");
        write_exec_frame(
            &mut stream,
            &AvfLinuxExecFrame::Stderr(
                b"[ctx-avf-linux] readiness phase containerd ok in 1ms\n".to_vec(),
            ),
        )
        .expect("write readiness phase");
        write_exec_frame(
            &mut stream,
            &AvfLinuxExecFrame::Exit(AvfLinuxExecExit { exit_code: 0 }),
        )
        .expect("write exit frame");
    });

    let readiness =
        wait_for_real_guest_launch_ready_with_owner_process(&temp, Duration::from_secs(1), None)
            .expect("restore-hit launch readiness should succeed without a republished marker");
    assert_eq!(readiness.attempts, 1);
    assert!(shared_vm_owner_guest_probe_ready(&temp));
    let ready_marker = shared_vm_guest_control_ready_path(&temp);
    assert_eq!(
        fs::read_to_string(&ready_marker).expect("read backfilled ready marker"),
        format!("listening:{SHARED_VM_GUEST_CONTROL_VSOCK_PORT}\n"),
    );
    assert!(
        !failure_marker.exists(),
        "restore-hit backfill should clear any stale failure marker"
    );

    server.join().expect("server thread");
    let control_socket = shared_vm_control_socket_path(&temp);
    if control_socket.exists() {
        fs::remove_file(&control_socket).expect("cleanup control socket");
    }
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[test]
fn shared_vm_exec_requires_launch_ready_transition() {
    let temp = std::env::temp_dir().join(format!(
        "ctx-avf-shared-exec-launch-ready-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    prepare_runtime_layout(&temp).expect("prepare runtime layout");
    let current_pid = std::process::id();
    persist_state(
        &shared_vm_state_path(&temp),
        &PersistedSharedVmState {
            state: AvfLinuxSharedVmLifecycleState::Running,
            guest_identity: supported_guest_identity(),
            runtime_root: None,
            rootfs_image: None,
            kernel_path: None,
            initrd_path: None,
            runtime_version: None,
            runtime_shape_digest: None,
            writable_surface_contract_digest: None,
            updated_at: Some(now_timestamp_string()),
            last_started_at: Some(now_timestamp_string()),
            last_saved_at: None,
            last_stopped_at: None,
            transition_status: Some(AvfLinuxSharedVmTransitionStatus::Scaffolded),
            last_start_outcome: Some(AvfLinuxSharedVmStartOutcome::ColdBoot),
            last_stop_outcome: None,
            last_restore_error: None,
            last_save_error: None,
            relay_pid: Some(current_pid),
            guest_agent_pid: Some(current_pid),
            simulated: true,
            notes: vec!["starting".to_string()],
        },
    )
    .expect("persist running scaffolded state");

    let err = shared_vm_exec(
        &temp,
        Path::new("/"),
        "/usr/bin/true",
        &[],
        Some("root"),
        false,
        &[],
    )
    .expect_err("shared vm exec should require a launch-ready transition");
    assert!(err
        .to_string()
        .contains("must be launch-ready before shared-vm-exec"));
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[test]
fn ensure_shared_vm_launch_ready_requires_guest_probe_marker_for_real_avf() {
    let temp = std::env::temp_dir().join(format!(
        "ctx-avf-shared-exec-probe-ready-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    fs::create_dir_all(&temp).expect("create tempdir");

    let err = ensure_shared_vm_launch_ready_for_operation(
        &temp,
        &AvfLinuxSharedVmStateResponse {
            protocol_version: HELPER_PROTOCOL_VERSION,
            protocol_schema: HELPER_PROTOCOL_SCHEMA,
            state: AvfLinuxSharedVmLifecycleState::Running,
            vm_root: temp.clone(),
            logs_root: temp.join("logs"),
            state_path: temp.join("shared-vm-state.json"),
            log_path: Some(temp.join("logs/shared-vm.log")),
            saved_state_path: Some(temp.join("saved-machine-state.vzvmsave")),
            saved_state_exists: false,
            runtime_root: None,
            rootfs_image: None,
            kernel_path: None,
            initrd_path: None,
            runtime_version: None,
            runtime_shape_digest: None,
            writable_surface_contract_digest: None,
            updated_at: Some(now_timestamp_string()),
            last_started_at: Some(now_timestamp_string()),
            last_saved_at: None,
            last_stopped_at: None,
            transition_status: Some(AvfLinuxSharedVmTransitionStatus::Ready),
            last_start_outcome: Some(AvfLinuxSharedVmStartOutcome::ColdBoot),
            last_stop_outcome: None,
            last_restore_error: None,
            last_save_error: None,
            relay_pid: Some(std::process::id()),
            guest_agent_pid: None,
            simulated: false,
            notes: vec!["running".to_string()],
        },
        "shared-vm-exec",
    )
    .expect_err("real shared vm exec should require the guest probe marker");
    assert!(err
        .to_string()
        .contains("guest-control ready marker before shared-vm-exec"));
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[cfg(unix)]
#[test]
fn wait_for_real_guest_exec_ready_reports_owner_exit_log_tail() {
    let temp = PathBuf::from("/tmp").join(format!(
        "ctx-avf-real-ready-owner-exit-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    fs::create_dir_all(&temp).expect("create tempdir");
    let log_path = shared_vm_log_path(&temp);
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent).expect("create log dir");
    }

    let script = format!(
        "printf '%s\n' '[ctx-avf-linux] bridge_probe_failed' > '{}' && exit 41",
        log_path.display()
    );
    let mut owner = std::process::Command::new("sh")
        .arg("-lc")
        .arg(script)
        .spawn()
        .expect("spawn owner");

    let err = wait_for_real_guest_exec_ready_with_owner_process(
        &temp,
        Duration::from_secs(1),
        Some(&mut owner),
    )
    .expect_err("owner exit should fail readiness");
    let rendered = err.to_string();
    assert!(rendered.contains("shared AVF VM owner exited before guest exec readiness"));
    assert!(rendered.contains("bridge_probe_failed"));
    assert!(shared_vm_readiness_failure_requires_writable_rootfs_reset(
        &err
    ));

    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[cfg(unix)]
#[test]
fn guest_exec_relays_request_over_shared_vm_control_socket() {
    use std::os::unix::net::UnixListener;
    use std::thread;

    let temp = PathBuf::from("/tmp").join(format!(
        "ctxavf-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    fs::create_dir_all(&temp).expect("create tempdir");
    let runtime_root = temp.join("runtime");
    fs::create_dir_all(&runtime_root).expect("runtime dir");
    let rootfs = runtime_root.join("rootfs.img");
    let kernel = runtime_root.join("kernel");
    let initrd = runtime_root.join("initrd");
    fs::write(&rootfs, b"rootfs").expect("rootfs");
    fs::write(&kernel, b"kernel").expect("kernel");
    fs::write(&initrd, b"initrd").expect("initrd");
    start_shared_vm(
        &temp,
        &runtime_root,
        &rootfs,
        &kernel,
        &initrd,
        "test".into(),
    )
    .expect("start shared vm");

    let metadata_path = shared_vm_worktree_metadata_path(&temp, "ws-123", "wt-456");
    persist_guest_worktree_state(
        &metadata_path,
        &PersistedGuestWorktreeState {
            workspace_id: "ws-123".to_string(),
            worktree_id: "wt-456".to_string(),
            guest_identity: supported_guest_identity(),
            host_workspace_root: temp.join("repo"),
            guest_root: PathBuf::from("/ctx/ws/worktrees/wt-456"),
            host_shadow_root: temp.join("shadow-root"),
            guest_user: "ctx-ws-test".to_string(),
            base_commit_sha: "abc123".to_string(),
            branch_name: "ctx/ws-123/wt-456".to_string(),
            updated_at: now_timestamp_string(),
            simulated: true,
            notes: vec![],
        },
    )
    .expect("persist guest worktree state");

    let socket_path = shared_vm_control_socket_path(&temp);
    if let Some(parent) = socket_path.parent() {
        fs::create_dir_all(parent).expect("socket dir");
    }
    if socket_path.exists() {
        fs::remove_file(&socket_path).expect("remove stale socket");
    }
    let listener = UnixListener::bind(&socket_path).expect("bind control socket");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept control socket");
        let request = read_exec_frame(&mut stream)
            .expect("read request")
            .expect("request frame");
        let request = match request {
            AvfLinuxExecFrame::Request(request) => request,
            other => panic!("expected request frame, got {other:?}"),
        };
        assert_eq!(request.command, "/usr/bin/env");
        assert_eq!(request.args, vec!["--version".to_string()]);
        assert_eq!(request.cwd, "/ctx/ws/worktrees/wt-456/src");
        assert_eq!(request.user.as_deref(), Some("ctxagent"));
        assert!(request.pty);
        assert_eq!(
            request.env.get("TERM").map(String::as_str),
            Some("xterm-256color")
        );
        write_exec_frame(
            &mut stream,
            &AvfLinuxExecFrame::Exit(AvfLinuxExecExit { exit_code: 7 }),
        )
        .expect("write exit frame");
    });

    let exit_code = guest_exec(
        &temp,
        "ws-123",
        "wt-456",
        Path::new("/ctx/ws/worktrees/wt-456/src"),
        "/usr/bin/env",
        &["TERM=xterm-256color".to_string()],
        Some("ctxagent"),
        true,
        &["--version".to_string()],
    )
    .expect("guest exec should succeed");

    assert_eq!(exit_code, 7);
    server.join().expect("server thread");
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[cfg(unix)]
#[test]
fn wait_for_control_socket_accepts_live_listener_only() {
    use std::os::unix::net::UnixListener;
    use std::thread;

    let temp = PathBuf::from("/tmp").join(format!(
        "ctxavf-control-socket-wait-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    fs::create_dir_all(&temp).expect("create tempdir");

    let socket_path = shared_vm_control_socket_path(&temp);
    if let Some(parent) = socket_path.parent() {
        fs::create_dir_all(parent).expect("socket dir");
    }
    if socket_path.exists() {
        fs::remove_file(&socket_path).expect("remove stale socket");
    }

    let listener = UnixListener::bind(&socket_path).expect("bind control socket");
    let accept_thread = thread::spawn(move || {
        let (_stream, _) = listener.accept().expect("accept control connection");
    });

    real_vm_runtime::wait_for_socket_accepting_connections(
        &socket_path,
        Duration::from_millis(250),
        "shared VM control socket",
    )
    .expect("live listener should satisfy control socket wait");

    accept_thread.join().expect("accept thread");
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[cfg(unix)]
#[test]
fn wait_for_control_socket_rejects_stale_socket_path() {
    let temp = PathBuf::from("/tmp").join(format!(
        "ctxavf-stale-control-socket-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    fs::create_dir_all(&temp).expect("create tempdir");

    let socket_path = shared_vm_control_socket_path(&temp);
    if let Some(parent) = socket_path.parent() {
        fs::create_dir_all(parent).expect("socket dir");
    }
    if socket_path.exists() {
        fs::remove_file(&socket_path).expect("remove stale socket");
    }

    fs::write(&socket_path, b"not a socket").expect("write non-socket path");

    let err = real_vm_runtime::wait_for_socket_accepting_connections(
        &socket_path,
        Duration::from_millis(150),
        "shared VM control socket",
    )
    .expect_err("stale socket path should not satisfy control socket wait");
    let rendered = err.to_string();
    assert!(rendered.contains("shared VM control socket"));
    assert!(rendered.contains("accept connections"));

    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[cfg(unix)]
#[test]
fn guest_exec_capture_reports_explicit_error_frames() {
    use std::os::unix::net::UnixListener;
    use std::thread;

    let temp = PathBuf::from("/tmp").join(format!(
        "ctxavf-capture-error-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    fs::create_dir_all(&temp).expect("create tempdir");
    let socket_path = temp.join("shared-vm-control.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind control socket");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept control socket");
        let frame = read_exec_frame(&mut stream)
            .expect("read request")
            .expect("request frame");
        assert!(matches!(frame, AvfLinuxExecFrame::Request(_)));
        write_exec_frame(
            &mut stream,
            &AvfLinuxExecFrame::Error(AvfLinuxExecError {
                code: "guest_control_connect_failed".to_string(),
                message: "connecting to guest vsock port 47001: transient connect error"
                    .to_string(),
            }),
        )
        .expect("write explicit error frame");
    });

    let result = run_guest_exec_capture(
        &socket_path,
        Path::new("/"),
        "/usr/bin/true",
        &[],
        Some("root"),
        HashMap::new(),
        None,
    );
    let err = match result {
        Ok(_) => panic!("explicit error frame should fail the capture"),
        Err(err) => err,
    };

    let rendered = err.to_string();
    assert!(rendered.contains("guest exec failed"));
    assert!(rendered.contains("guest_control_connect_failed"));
    assert!(rendered.contains("connecting to guest vsock port 47001"));

    server.join().expect("server thread");
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[cfg(all(target_os = "macos", unix))]
#[test]
fn guest_exec_capture_with_socket_timeout_fails_when_server_never_responds() {
    use std::os::unix::net::UnixListener;
    use std::thread;

    let temp = PathBuf::from("/tmp").join(format!(
        "ctxavf-capture-timeout-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    fs::create_dir_all(&temp).expect("create tempdir");
    let socket_path = temp.join("shared-vm-control.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind control socket");
    let server = thread::spawn(move || {
        let (_stream, _) = listener.accept().expect("accept control socket");
        thread::sleep(Duration::from_millis(300));
    });

    let result = run_guest_exec_capture_with_socket_timeout(
        &socket_path,
        Path::new("/"),
        "/usr/bin/true",
        &[],
        Some("root"),
        HashMap::new(),
        None,
        Some(Duration::from_millis(100)),
    );
    let err = match result {
        Ok(_) => panic!("capture should time out when the server never responds"),
        Err(err) => err,
    };

    let rendered = format!("{err:#}");
    assert!(
        rendered.contains("timed out")
            || rendered.contains("deadline")
            || rendered.contains("Resource temporarily unavailable")
            || rendered.contains("operation would block")
    );

    server.join().expect("server thread");
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[cfg(all(target_os = "macos", unix))]
#[test]
fn shared_vm_relay_turns_truncated_guest_frames_into_explicit_error_frames() {
    use std::os::fd::{FromRawFd, IntoRawFd};
    use std::thread;

    let (mut client, relay_client) = UnixStream::pair().expect("client pair");
    let (mut guest_server, guest_relay) = UnixStream::pair().expect("guest pair");
    let relay = thread::spawn(move || {
        let guest = unsafe { File::from_raw_fd(guest_relay.into_raw_fd()) };
        relay_shared_vm_control_client(relay_client, guest)
    });

    write_exec_frame(
        &mut client,
        &AvfLinuxExecFrame::Request(AvfLinuxExecRequest::new(
            "/usr/bin/true",
            Vec::new(),
            "/",
            Some("root".to_string()),
            HashMap::new(),
            false,
        )),
    )
    .expect("write request frame");

    let guest = thread::spawn(move || {
        let frame = read_exec_frame(&mut guest_server)
            .expect("read forwarded request")
            .expect("request frame");
        assert!(matches!(frame, AvfLinuxExecFrame::Request(_)));
        guest_server
            .write_all(&[3, 0, 0, 0, 5, b'o', b'k'])
            .expect("write truncated stdout frame");
    });

    let frame = read_exec_frame(&mut client)
        .expect("read explicit transport error")
        .expect("transport error frame");
    let error = match frame {
        AvfLinuxExecFrame::Error(error) => error,
        other => panic!("expected explicit transport error frame, got {other:?}"),
    };
    assert_eq!(error.code, "guest_control_stream_closed");
    assert!(error
        .message
        .contains("reading shared VM guest response frame failed"));
    assert!(
        error.message.contains("failed to fill whole buffer")
            || error.message.contains("unexpected end of file")
    );

    let relay_err = relay
        .join()
        .expect("relay thread join")
        .expect_err("relay should fail after truncated guest frame");
    assert!(relay_err
        .to_string()
        .contains("reading shared VM guest response frame"));

    guest.join().expect("guest thread");
}

#[cfg(unix)]
#[test]
fn shared_vm_relay_restores_blocking_mode_for_nonblocking_clients() {
    use std::os::fd::{FromRawFd, IntoRawFd};
    use std::thread;
    use std::time::Duration;

    let (mut client, relay_client) = UnixStream::pair().expect("client pair");
    relay_client
        .set_nonblocking(true)
        .expect("set relay client nonblocking");
    let (mut guest_server, guest_relay) = UnixStream::pair().expect("guest pair");
    let relay = thread::spawn(move || {
        let guest = unsafe { File::from_raw_fd(guest_relay.into_raw_fd()) };
        relay_shared_vm_control_client(relay_client, guest)
    });

    write_exec_frame(
        &mut client,
        &AvfLinuxExecFrame::Request(AvfLinuxExecRequest::new(
            "/usr/bin/true",
            Vec::new(),
            "/",
            Some("root".to_string()),
            HashMap::new(),
            false,
        )),
    )
    .expect("write request frame");

    let guest = thread::spawn(move || {
        let frame = read_exec_frame(&mut guest_server)
            .expect("read forwarded request")
            .expect("request frame");
        assert!(matches!(frame, AvfLinuxExecFrame::Request(_)));

        let payload = vec![b'x'; AVF_EXEC_STREAM_FRAME_MAX_PAYLOAD];
        for _ in 0..2048 {
            write_exec_frame(
                &mut guest_server,
                &AvfLinuxExecFrame::Stdout(payload.clone()),
            )
            .expect("write stdout frame burst");
        }
        write_exec_frame(
            &mut guest_server,
            &AvfLinuxExecFrame::Exit(AvfLinuxExecExit { exit_code: 0 }),
        )
        .expect("write exit frame");
    });

    thread::sleep(Duration::from_millis(100));

    let mut received = 0usize;
    loop {
        let frame = read_exec_frame(&mut client)
            .expect("read relayed frame")
            .expect("relayed frame");
        match frame {
            AvfLinuxExecFrame::Stdout(bytes) => received += bytes.len(),
            AvfLinuxExecFrame::Exit(exit) => {
                assert_eq!(exit.exit_code, 0);
                break;
            }
            other => panic!("unexpected relayed frame: {other:?}"),
        }
    }

    assert_eq!(received, 2048 * AVF_EXEC_STREAM_FRAME_MAX_PAYLOAD);
    relay
        .join()
        .expect("relay thread join")
        .expect("relay should succeed once client starts reading");
    guest.join().expect("guest thread");
}

#[cfg(unix)]
#[test]
fn non_pty_guest_exec_cli_writes_captured_output() {
    use std::os::unix::net::UnixListener;
    use std::thread;

    let temp = PathBuf::from("/tmp").join(format!(
        "ctxavf-cli-capture-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    fs::create_dir_all(&temp).expect("create tempdir");
    let socket_path = temp.join("shared-vm-control.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind control socket");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept control socket");
        let request = read_exec_frame(&mut stream)
            .expect("read request")
            .expect("request frame");
        let request = match request {
            AvfLinuxExecFrame::Request(request) => request,
            other => panic!("expected request frame, got {other:?}"),
        };
        assert_eq!(request.command, "/bin/pwd");
        assert_eq!(request.cwd, "/ctx/ws/worktrees/wt-456");
        assert!(!request.pty);
        assert_eq!(request.user.as_deref(), Some("ctxagent"));
        write_exec_frame(
            &mut stream,
            &AvfLinuxExecFrame::Stdout(b"/ctx/ws/worktrees/wt-456\n".to_vec()),
        )
        .expect("write stdout frame");
        write_exec_frame(
            &mut stream,
            &AvfLinuxExecFrame::Stderr(b"warning: capture-path\n".to_vec()),
        )
        .expect("write stderr frame");
        write_exec_frame(
            &mut stream,
            &AvfLinuxExecFrame::Exit(AvfLinuxExecExit { exit_code: 17 }),
        )
        .expect("write exit frame");
    });

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit_code = run_guest_exec_cli(
        &socket_path,
        Path::new("/ctx/ws/worktrees/wt-456"),
        "/bin/pwd",
        &[],
        Some("ctxagent"),
        HashMap::new(),
        false,
        &mut stdout,
        &mut stderr,
    )
    .expect("non-PTY CLI guest exec should succeed");

    assert_eq!(exit_code, 17);
    assert_eq!(
        String::from_utf8(stdout).expect("stdout utf8"),
        "/ctx/ws/worktrees/wt-456\n"
    );
    assert_eq!(
        String::from_utf8(stderr).expect("stderr utf8"),
        "warning: capture-path\n"
    );
    server.join().expect("server thread");
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[cfg(unix)]
#[test]
fn non_pty_guest_exec_cli_streams_output_before_stdin_eof() {
    use std::io::Read;
    use std::os::unix::net::UnixListener;
    use std::sync::{mpsc, Arc, Condvar, Mutex};
    use std::thread;
    use std::time::Duration;

    #[derive(Default)]
    struct ReaderState {
        sent_payload: bool,
        allow_eof: bool,
    }

    struct GateReader {
        state: Arc<(Mutex<ReaderState>, Condvar)>,
    }

    impl Read for GateReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let (lock, cv) = &*self.state;
            let mut state = lock.lock().expect("lock gated reader state");
            if !state.sent_payload {
                let payload = b"session.open\n";
                buf[..payload.len()].copy_from_slice(payload);
                state.sent_payload = true;
                return Ok(payload.len());
            }
            while !state.allow_eof {
                state = cv.wait(state).expect("wait for EOF gate");
            }
            Ok(0)
        }
    }

    let temp = PathBuf::from("/tmp").join(format!(
        "ctxavf-cli-streaming-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    fs::create_dir_all(&temp).expect("create tempdir");
    let socket_path = temp.join("shared-vm-control.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind control socket");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept control socket");
        let request = read_exec_frame(&mut stream)
            .expect("read request")
            .expect("request frame");
        let request = match request {
            AvfLinuxExecFrame::Request(request) => request,
            other => panic!("expected request frame, got {other:?}"),
        };
        assert_eq!(request.command, "/usr/bin/codex-crp");
        assert!(!request.pty);

        let stdin = read_exec_frame(&mut stream)
            .expect("read stdin frame")
            .expect("stdin frame");
        let AvfLinuxExecFrame::Stdin(stdin) = stdin else {
            panic!("expected stdin frame");
        };
        assert_eq!(stdin, b"session.open\n".to_vec());

        write_exec_frame(
            &mut stream,
            &AvfLinuxExecFrame::Stdout(b"session.opened\n".to_vec()),
        )
        .expect("write stdout frame");
        write_exec_frame(
            &mut stream,
            &AvfLinuxExecFrame::Exit(AvfLinuxExecExit { exit_code: 0 }),
        )
        .expect("write exit frame");
    });

    let gate = Arc::new((Mutex::new(ReaderState::default()), Condvar::new()));
    let reader = GateReader {
        state: Arc::clone(&gate),
    };
    let (result_tx, result_rx) = mpsc::channel();
    let socket_path_for_client = socket_path.clone();
    let worker = thread::spawn(move || {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let result = run_guest_exec_cli_with_streaming_stdin(
            &socket_path_for_client,
            Path::new("/"),
            "/usr/bin/codex-crp",
            &[],
            None,
            HashMap::new(),
            Some(Box::new(reader)),
            &mut stdout,
            &mut stderr,
        );
        result_tx
            .send((result, stdout, stderr))
            .expect("send streaming exec result");
    });

    let (result, stdout, stderr) = match result_rx.recv_timeout(Duration::from_secs(2)) {
        Ok(result) => result,
        Err(err) => {
            let (lock, cv) = &*gate;
            let mut state = lock.lock().expect("lock EOF gate after timeout");
            state.allow_eof = true;
            cv.notify_all();
            panic!("timed out waiting for streaming exec result: {err}");
        }
    };

    let (lock, cv) = &*gate;
    let mut state = lock.lock().expect("lock EOF gate");
    state.allow_eof = true;
    cv.notify_all();
    drop(state);

    worker.join().expect("streaming exec worker");
    assert_eq!(result.expect("streaming exec should succeed"), 0);
    assert_eq!(
        String::from_utf8(stdout).expect("stdout utf8"),
        "session.opened\n"
    );
    assert!(stderr.is_empty());
    server.join().expect("server thread");
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[cfg(unix)]
#[test]
fn non_pty_guest_exec_cli_forwards_piped_stdin_into_capture_path() {
    use std::io::Cursor;
    use std::os::unix::net::UnixListener;
    use std::thread;

    let temp = PathBuf::from("/tmp").join(format!(
        "ctxavf-cli-stdin-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    fs::create_dir_all(&temp).expect("create tempdir");
    let socket_path = temp.join("shared-vm-control.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind control socket");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept control socket");
        let request = read_exec_frame(&mut stream)
            .expect("read request")
            .expect("request frame");
        let request = match request {
            AvfLinuxExecFrame::Request(request) => request,
            other => panic!("expected request frame, got {other:?}"),
        };
        assert_eq!(request.command, "/usr/bin/tar");
        assert_eq!(request.cwd, "/");
        assert!(!request.pty);

        let stdin = read_exec_frame(&mut stream)
            .expect("read stdin frame")
            .expect("stdin frame");
        let AvfLinuxExecFrame::Stdin(stdin) = stdin else {
            panic!("expected stdin frame");
        };
        assert_eq!(stdin, b"archive-payload".to_vec());

        let close = read_exec_frame(&mut stream)
            .expect("read close frame")
            .expect("close frame");
        assert!(matches!(close, AvfLinuxExecFrame::CloseStdin));

        write_exec_frame(
            &mut stream,
            &AvfLinuxExecFrame::Stdout(b"imported\n".to_vec()),
        )
        .expect("write stdout frame");
        write_exec_frame(
            &mut stream,
            &AvfLinuxExecFrame::Exit(AvfLinuxExecExit { exit_code: 0 }),
        )
        .expect("write exit frame");
    });

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut stdin = Cursor::new(b"archive-payload".to_vec());
    let exit_code = run_guest_exec_cli_with_capture_stdin(
        &socket_path,
        Path::new("/"),
        "/usr/bin/tar",
        &["-xf".to_string(), "-".to_string()],
        None,
        HashMap::new(),
        Some(&mut stdin),
        &mut stdout,
        &mut stderr,
    )
    .expect("non-PTY CLI guest exec with piped stdin should succeed");

    assert_eq!(exit_code, 0);
    assert_eq!(
        String::from_utf8(stdout).expect("stdout utf8"),
        "imported\n"
    );
    assert!(stderr.is_empty());
    server.join().expect("server thread");
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[cfg(unix)]
#[test]
fn guest_exec_capture_returns_exit_and_stderr_when_guest_exits_early_during_streamed_stdin() {
    use std::io::Cursor;
    use std::os::unix::net::UnixListener;
    use std::thread;

    let temp = PathBuf::from("/tmp").join(format!(
        "ctxavf-cli-early-exit-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    fs::create_dir_all(&temp).expect("create tempdir");
    let socket_path = temp.join("shared-vm-control.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind control socket");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept control socket");
        let request = read_exec_frame(&mut stream)
            .expect("read request")
            .expect("request frame");
        let request = match request {
            AvfLinuxExecFrame::Request(request) => request,
            other => panic!("expected request frame, got {other:?}"),
        };
        assert_eq!(request.command, "/usr/bin/tar");
        assert_eq!(request.cwd, "/");

        let stdin = read_exec_frame(&mut stream)
            .expect("read first stdin frame")
            .expect("stdin frame");
        assert!(matches!(stdin, AvfLinuxExecFrame::Stdin(_)));

        write_exec_frame(
            &mut stream,
            &AvfLinuxExecFrame::Stderr(b"tar: Unexpected EOF in archive\n".to_vec()),
        )
        .expect("write stderr frame");
        write_exec_frame(
            &mut stream,
            &AvfLinuxExecFrame::Exit(AvfLinuxExecExit { exit_code: 2 }),
        )
        .expect("write exit frame");
    });

    let mut stdin = Cursor::new(vec![b'x'; AVF_EXEC_STREAM_FRAME_MAX_PAYLOAD * 4]);
    let result = run_guest_exec_capture(
        &socket_path,
        Path::new("/"),
        "/usr/bin/tar",
        &["-xpf".to_string(), "-".to_string()],
        None,
        HashMap::new(),
        Some(&mut stdin),
    )
    .expect("capture path should surface the guest exit instead of a broken pipe");

    assert_eq!(result.exit_code, 2);
    assert!(result.stdout.is_empty());
    assert_eq!(
        String::from_utf8(result.stderr).expect("stderr utf8"),
        "tar: Unexpected EOF in archive\n"
    );

    server.join().expect("server thread");
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[cfg(unix)]
#[test]
fn guest_exec_capture_over_connected_stream_returns_output_without_reentering_relay() {
    use std::thread;

    let (mut client, mut server) = UnixStream::pair().expect("unix stream pair");
    let guest = thread::spawn(move || {
        let request = read_exec_frame(&mut server)
            .expect("read request")
            .expect("request frame");
        let request = match request {
            AvfLinuxExecFrame::Request(request) => request,
            other => panic!("expected request frame, got {other:?}"),
        };
        assert_eq!(request.command, "/bin/sh");
        assert_eq!(request.cwd, "/");

        let close = read_exec_frame(&mut server)
            .expect("read close stdin")
            .expect("close stdin frame");
        assert!(matches!(close, AvfLinuxExecFrame::CloseStdin));

        write_exec_frame(&mut server, &AvfLinuxExecFrame::Stdout(b"12345\n".to_vec()))
            .expect("write stdout");
        write_exec_frame(&mut server, &AvfLinuxExecFrame::Stderr(b"warn\n".to_vec()))
            .expect("write stderr");
        write_exec_frame(
            &mut server,
            &AvfLinuxExecFrame::Exit(AvfLinuxExecExit { exit_code: 0 }),
        )
        .expect("write exit");
    });

    let result = run_guest_exec_capture_over_connected_stream(
        &mut client,
        Path::new("/"),
        "/bin/sh",
        &["-lc".to_string(), "echo 12345".to_string()],
        Some("root"),
        HashMap::new(),
    )
    .expect("capture over connected stream should succeed");

    assert_eq!(result.exit_code, 0);
    assert_eq!(
        String::from_utf8(result.stdout).expect("stdout utf8"),
        "12345\n"
    );
    assert_eq!(
        String::from_utf8(result.stderr).expect("stderr utf8"),
        "warn\n"
    );
    guest.join().expect("guest thread");
}

#[cfg(unix)]
#[test]
fn exec_stream_payload_budget_stays_within_shared_vm_safe_limit() {
    const {
        assert!(
            AVF_EXEC_STREAM_FRAME_MAX_PAYLOAD <= 1024,
            "shared-VM exec transport truncated larger stdin frames in live tar-import repros",
        );
    }
}

#[cfg(unix)]
#[test]
fn relay_child_output_splits_large_stdout_frames_below_transport_limit() {
    use std::io::Cursor;

    let payload = vec![b'x'; AVF_EXEC_STREAM_FRAME_MAX_PAYLOAD + 33];
    let mut reader = Cursor::new(payload);
    let (mut client, server) = UnixStream::pair().expect("stdout relay pair");

    relay_child_output(&mut reader, Arc::new(Mutex::new(server)), true);

    let first = read_exec_frame(&mut client)
        .expect("read first stdout frame")
        .expect("first stdout frame present");
    let second = read_exec_frame(&mut client)
        .expect("read second stdout frame")
        .expect("second stdout frame present");
    let eof = read_exec_frame(&mut client).expect("read eof");

    let first = match first {
        AvfLinuxExecFrame::Stdout(bytes) => bytes,
        other => panic!("expected first stdout frame, got {other:?}"),
    };
    let second = match second {
        AvfLinuxExecFrame::Stdout(bytes) => bytes,
        other => panic!("expected second stdout frame, got {other:?}"),
    };

    assert_eq!(first.len(), AVF_EXEC_STREAM_FRAME_MAX_PAYLOAD);
    assert_eq!(second.len(), 33);
    assert!(eof.is_none());
}

#[cfg(unix)]
#[test]
fn shared_vm_control_connection_proxies_to_guest_agent() {
    use std::os::unix::net::UnixListener;
    use std::thread;

    let temp = PathBuf::from("/tmp").join(format!(
        "ctxavf-proxy-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    fs::create_dir_all(&temp).expect("create tempdir");

    let metadata_path = shared_vm_worktree_metadata_path(&temp, "ws-123", "wt-456");
    let host_shadow_root = temp.join("shadow-root");
    fs::create_dir_all(host_shadow_root.join("src")).expect("shadow root");
    persist_guest_worktree_state(
        &metadata_path,
        &PersistedGuestWorktreeState {
            workspace_id: "ws-123".to_string(),
            worktree_id: "wt-456".to_string(),
            guest_identity: supported_guest_identity(),
            host_workspace_root: temp.join("repo"),
            guest_root: PathBuf::from("/ctx/ws/worktrees/wt-456"),
            host_shadow_root: host_shadow_root.clone(),
            guest_user: "ctx-ws-test".to_string(),
            base_commit_sha: "abc123".to_string(),
            branch_name: "ctx/ws-123/wt-456".to_string(),
            updated_at: now_timestamp_string(),
            simulated: true,
            notes: vec![],
        },
    )
    .expect("persist guest worktree state");

    let agent_socket = shared_vm_guest_agent_socket_path(&temp);
    if let Some(parent) = agent_socket.parent() {
        fs::create_dir_all(parent).expect("socket dir");
    }
    if agent_socket.exists() {
        fs::remove_file(&agent_socket).expect("remove stale guest-agent socket");
    }
    let listener = UnixListener::bind(&agent_socket).expect("bind guest-agent socket");
    let agent = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept guest-agent connection");
        let request = read_exec_frame(&mut stream)
            .expect("read proxied request")
            .expect("request frame");
        let request = match request {
            AvfLinuxExecFrame::Request(request) => request,
            other => panic!("expected proxied request frame, got {other:?}"),
        };
        assert_eq!(request.command, "/usr/bin/env");
        assert_eq!(
            request.cwd,
            host_shadow_root.join("src").display().to_string()
        );
        assert_eq!(request.args, vec!["--version".to_string()]);
        assert_eq!(
            request.env.get("TERM").map(String::as_str),
            Some("xterm-256color")
        );
        write_exec_frame(
            &mut stream,
            &AvfLinuxExecFrame::Exit(AvfLinuxExecExit { exit_code: 13 }),
        )
        .expect("write exit frame");
    });

    let (mut client, server) = UnixStream::pair().expect("unix stream pair");
    let temp_for_server = temp.clone();
    let relay = thread::spawn(move || {
        handle_shared_vm_control_connection(&temp_for_server, server)
            .expect("proxy shared vm control connection");
    });

    write_exec_frame(
        &mut client,
        &AvfLinuxExecFrame::Request(AvfLinuxExecRequest::new(
            "/usr/bin/env",
            vec!["--version".to_string()],
            "/ctx/ws/worktrees/wt-456/src",
            Some("ctxagent".to_string()),
            HashMap::from([("TERM".to_string(), "xterm-256color".to_string())]),
            false,
        )),
    )
    .expect("write client request");

    let frame = read_exec_frame(&mut client)
        .expect("read proxied exit")
        .expect("exit frame");
    assert_eq!(
        frame,
        AvfLinuxExecFrame::Exit(AvfLinuxExecExit { exit_code: 13 })
    );

    relay.join().expect("relay thread");
    agent.join().expect("agent thread");
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[cfg(unix)]
#[test]
fn guest_agent_exec_emits_exit_without_waiting_for_close_stdin() {
    use std::os::unix::net::UnixStream;
    use std::thread;
    use std::time::Duration;

    let temp_home = std::env::temp_dir().join(format!(
        "ctx-avf-test-home-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time after epoch")
            .as_nanos()
    ));
    fs::create_dir_all(&temp_home).expect("create temp home");
    let (mut client, server) = UnixStream::pair().expect("unix stream pair");
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set client read timeout");
    let relay = thread::spawn(move || {
        handle_guest_agent_control_connection(Path::new("/tmp"), server)
            .expect("handle guest-agent control connection");
    });

    write_exec_frame(
        &mut client,
        &AvfLinuxExecFrame::Request(AvfLinuxExecRequest::new(
            "/bin/sh",
            vec!["-lc".to_string(), "printf ready".to_string()],
            "/",
            None,
            HashMap::from([("HOME".to_string(), temp_home.to_string_lossy().to_string())]),
            false,
        )),
    )
    .expect("write client request");

    let stdout = read_exec_frame(&mut client)
        .expect("read stdout frame")
        .expect("stdout frame");
    let stdout = match stdout {
        AvfLinuxExecFrame::Stdout(bytes) => bytes,
        other => panic!("expected stdout frame, got {other:?}"),
    };
    assert_eq!(stdout, b"ready".to_vec());

    let exit = read_exec_frame(&mut client)
        .expect("read exit frame")
        .expect("exit frame");
    assert_eq!(
        exit,
        AvfLinuxExecFrame::Exit(AvfLinuxExecExit { exit_code: 0 })
    );

    relay.join().expect("guest-agent relay thread");
    fs::remove_dir_all(&temp_home).expect("cleanup temp home");
}
