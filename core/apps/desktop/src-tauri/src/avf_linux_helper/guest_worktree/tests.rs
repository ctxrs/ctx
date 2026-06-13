use super::*;

fn test_tempdir(prefix: &str) -> PathBuf {
    let temp = PathBuf::from("/tmp").join(format!(
        "{prefix}-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    fs::create_dir_all(&temp).expect("create tempdir");
    temp
}

fn persist_running_simulated_shared_vm_state(data_root: &Path) {
    persist_state(
        &shared_vm_state_path(data_root),
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
            last_start_outcome: Some(AvfLinuxSharedVmStartOutcome::AlreadyRunning),
            last_stop_outcome: None,
            last_restore_error: None,
            last_save_error: None,
            relay_pid: None,
            guest_agent_pid: None,
            simulated: true,
            notes: vec!["simulated running state".to_string()],
        },
    )
    .expect("persist shared vm state");
}

fn create_host_workspace_root(root: &Path) -> PathBuf {
    let host_workspace_root = root.join("workspace");
    fs::create_dir_all(host_workspace_root.join(".git")).expect("create .git dir");
    fs::write(host_workspace_root.join("README.md"), b"hello\n").expect("write readme");
    host_workspace_root
}

#[test]
fn guest_import_requires_existing_shadow_root() {
    let temp = PathBuf::from("/tmp").join(format!(
        "ctxavf-guest-shadow-missing-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    fs::create_dir_all(&temp).expect("create tempdir");
    let shadow_root = temp.join("missing-shadow-root");

    let err = ensure_shadow_root_ready_for_guest_import(&shadow_root)
        .expect_err("missing shadow root should fail");

    assert!(err
        .to_string()
        .contains("host shadow root is missing before guest rematerialization"));
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[test]
fn guest_import_requires_standalone_git_directory() {
    let temp = PathBuf::from("/tmp").join(format!(
        "ctxavf-guest-shadow-git-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    fs::create_dir_all(&temp).expect("create tempdir");
    let shadow_root = temp.join("shadow-root");
    fs::create_dir_all(&shadow_root).expect("create shadow root");
    fs::write(shadow_root.join(".git"), b"not-a-directory").expect("seed invalid .git");

    let err = ensure_shadow_root_ready_for_guest_import(&shadow_root)
        .expect_err("shadow root without .git should fail");

    assert!(err.to_string().contains("standalone .git directory"));
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[test]
fn stage_shadow_root_rejects_invalid_git_pointer_file() {
    let temp = PathBuf::from("/tmp").join(format!(
        "ctxavf-stage-shadow-git-{}-{}",
        std::process::id(),
        now_timestamp_string()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    fs::create_dir_all(&temp).expect("create tempdir");
    let host_workspace_root = temp.join("workspace-root");
    fs::create_dir_all(&host_workspace_root).expect("create workspace root");
    fs::write(host_workspace_root.join(".git"), b"not-a-directory")
        .expect("seed invalid host .git");

    let err =
        stage_shadow_root_from_host_workspace(&host_workspace_root, &temp.join("shadow-root"))
            .expect_err("host workspace with invalid .git pointer should fail");

    assert!(err.to_string().contains("invalid .git file"));
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[test]
fn prepare_guest_worktree_repairs_corrupt_metadata_by_restaging_shadow_root() {
    let temp = test_tempdir("ctxavf-repair-corrupt-metadata");
    persist_running_simulated_shared_vm_state(&temp);
    let host_workspace_root = create_host_workspace_root(&temp);
    let metadata_path = shared_vm_worktree_metadata_path(&temp, "ws-123", "wt-456");
    fs::create_dir_all(metadata_path.parent().expect("metadata parent"))
        .expect("create metadata parent");
    fs::write(&metadata_path, b"{not-json").expect("write corrupt metadata");

    let response = prepare_guest_worktree(
        &temp,
        "ws-123",
        "wt-456",
        &host_workspace_root,
        "abc123",
        "ctx/test-branch",
    )
    .expect("repair corrupt metadata");

    assert_eq!(response.status, AvfLinuxGuestWorktreeStatus::Prepared);
    assert!(response.simulated);
    assert!(response.host_shadow_root.join(".git").is_dir());
    assert!(response
        .notes
        .iter()
        .any(|note| note.contains("discarded corrupt guest worktree metadata")));
    let persisted = load_guest_worktree_state(&metadata_path)
        .expect("load repaired metadata")
        .expect("repaired metadata present");
    assert_eq!(persisted.branch_name, "ctx/test-branch");
    assert_eq!(persisted.base_commit_sha, "abc123");
    assert!(persisted.host_shadow_root.join(".git").is_dir());
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[test]
fn prepare_guest_worktree_restages_shadow_root_when_existing_state_loses_git_dir() {
    let temp = test_tempdir("ctxavf-restage-shadow-root");
    persist_running_simulated_shared_vm_state(&temp);
    let host_workspace_root = create_host_workspace_root(&temp);
    let workspace_id = "ws-123";
    let worktree_id = "wt-456";
    let host_shadow_root = shared_vm_worktree_shadow_root(&temp, workspace_id, worktree_id);
    fs::create_dir_all(&host_shadow_root).expect("create stale shadow root");
    fs::write(host_shadow_root.join("stale.txt"), b"stale").expect("write stale file");
    let metadata_path = shared_vm_worktree_metadata_path(&temp, workspace_id, worktree_id);
    persist_guest_worktree_state(
        &metadata_path,
        &PersistedGuestWorktreeState {
            workspace_id: workspace_id.to_string(),
            worktree_id: worktree_id.to_string(),
            guest_identity: supported_guest_identity(),
            host_workspace_root: host_workspace_root.clone(),
            guest_root: guest_worktree_root(worktree_id),
            guest_user: guest_workspace_user(workspace_id),
            host_shadow_root: host_shadow_root.clone(),
            base_commit_sha: "abc123".to_string(),
            branch_name: "ctx/test-branch".to_string(),
            updated_at: now_timestamp_string(),
            simulated: true,
            notes: vec!["stale state".to_string()],
        },
    )
    .expect("persist guest metadata");

    let response = prepare_guest_worktree(
        &temp,
        workspace_id,
        worktree_id,
        &host_workspace_root,
        "abc123",
        "ctx/test-branch",
    )
    .expect("restage missing shadow root git dir");

    assert_eq!(response.status, AvfLinuxGuestWorktreeStatus::Prepared);
    assert!(response.simulated);
    assert!(response.host_shadow_root.join(".git").is_dir());
    let persisted = load_guest_worktree_state(&metadata_path)
        .expect("load repaired metadata")
        .expect("metadata present");
    assert_eq!(persisted.host_shadow_root, host_shadow_root);
    assert!(persisted.host_shadow_root.join(".git").is_dir());
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}
