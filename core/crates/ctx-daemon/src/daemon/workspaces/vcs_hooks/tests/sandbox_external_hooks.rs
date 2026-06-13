use super::fixtures::{
    git, init_git_workspace, save_test_execution_settings, test_state, write_sandbox_exec_shim,
    EnvVarGuard,
};
use chrono::Utc;
use ctx_core::ids::{TaskId, WorktreeId};
use ctx_core::models::{
    sandbox_instance_id_for_workspace, SandboxBinding, SandboxGuestIdentity, SandboxProfile,
    SandboxSubstrate, VcsKind, Worktree,
};
use ctx_settings_model::{
    ContainerExecutionSettings, ContainerNetworkMode, ContainerRuntimeKind, ExecutionMode,
    ExecutionSettings,
};
use ctx_workspace_container::workspace_container_name;
use ctx_worktree_vcs_service::managed_worktree_path;
use ctx_worktree_vcs_service::{
    cleanup_worktree_hooks, ensure_task_commit_hook, get_git_config, set_git_config,
    worktree_hooks_dir, CORE_HOOKS_PATH_KEY, CTX_PREV_HOOKS_PATH_KEY, CTX_TASK_ID_KEY,
};

#[tokio::test]
async fn sandbox_hooks_live_under_external_vcs_hooks_root_and_cleanup_restores_config() {
    let _serial = crate::test_support::sandbox_cli_env_test_lock()
        .lock()
        .await;
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_root = temp.path().join("repo");
    std::fs::create_dir_all(&repo_root).expect("create repo");
    let base_commit = init_git_workspace(&repo_root);
    let state = test_state(temp.path()).await;
    let workspace = state
        .global_store()
        .create_workspace(
            "ws".to_string(),
            repo_root.to_string_lossy().to_string(),
            VcsKind::Git,
        )
        .await
        .expect("create workspace");
    let store = state
        .store_for_workspace(workspace.id)
        .await
        .expect("workspace store");
    let task_id = TaskId::new();
    let worktree_id = WorktreeId::new();
    let managed_root = managed_worktree_path(temp.path(), workspace.id, worktree_id);
    let branch_name = format!("ctx/{}/{}", task_id.0, worktree_id.0);
    git(
        &[
            "worktree",
            "add",
            "-b",
            &branch_name,
            managed_root.to_string_lossy().as_ref(),
            &base_commit,
        ],
        &repo_root,
    );
    let worktree = Worktree {
        id: worktree_id,
        workspace_id: workspace.id,
        root_path: managed_root.to_string_lossy().to_string(),
        base_commit_sha: base_commit.clone(),
        git_branch: Some(branch_name.clone()),
        vcs_kind: Some(VcsKind::Git),
        base_revision: Some(base_commit.clone()),
        vcs_ref: Some(branch_name),
        created_at: Utc::now(),
        bootstrap_status: None,
        bootstrap_started_at: None,
        bootstrap_finished_at: None,
        bootstrap_exit_code: None,
        bootstrap_timeout_sec: None,
        bootstrap_error: None,
        bootstrap_log_path: None,
        bootstrap_log_truncated: None,
        bootstrap_command: None,
        bootstrap_script_path: None,
    };
    let worktree = store
        .insert_worktree(worktree)
        .await
        .expect("insert worktree");
    let execution = ExecutionSettings {
        mode: ExecutionMode::Sandbox,
        container: ContainerExecutionSettings {
            runtime: ContainerRuntimeKind::NativeContainer,
            network_mode: ContainerNetworkMode::All,
            ..Default::default()
        },
    };
    save_test_execution_settings(&state, execution.clone()).await;
    store
        .upsert_sandbox_binding(SandboxBinding {
            worktree_id,
            workspace_id: workspace.id,
            sandbox_instance_id: sandbox_instance_id_for_workspace(workspace.id),
            substrate: SandboxSubstrate::NativeContainer,
            guest_identity: SandboxGuestIdentity::linux_container_ubuntu(),
            profile: SandboxProfile::Standard,
            live_workspace_root: ctx_sandbox_contract::CTX_CONTAINER_WORKSPACE_ROOT.to_string(),
            live_worktree_root: managed_root.to_string_lossy().to_string(),
            execution_settings_json: Some(
                serde_json::to_string(&execution).expect("serialize execution snapshot"),
            ),
            container_name: Some(workspace_container_name(workspace.id)),
            host_materialization_root: None,
            created_at: Utc::now(),
        })
        .await
        .expect("insert sandbox binding");

    let original_hooks = temp.path().join("original-hooks");
    set_git_config(
        &managed_root,
        CORE_HOOKS_PATH_KEY,
        &original_hooks.to_string_lossy(),
    )
    .await
    .expect("set original hooks path");

    let sandbox_cli = write_sandbox_exec_shim(
        temp.path(),
        &workspace_container_name(workspace.id),
        &format!("ctx-ws-{}", workspace.id.0),
    );
    let _sandbox_cli_guard = EnvVarGuard::set(
        "CTX_HARNESS_SANDBOX_CLI_PATH",
        &sandbox_cli.to_string_lossy(),
    );

    ensure_task_commit_hook(state.as_ref(), &workspace, &worktree, task_id)
        .await
        .expect("install sandbox task hook");

    let hooks_dir = worktree_hooks_dir(temp.path(), workspace.id, worktree.id);
    let hooks_path = hooks_dir.to_string_lossy().to_string();
    assert!(
        tokio::fs::metadata(hooks_dir.join("commit-msg"))
            .await
            .is_ok(),
        "expected external commit-msg hook at {}",
        hooks_dir.display()
    );
    assert!(
        tokio::fs::metadata(managed_root.join(".ctx-hooks"))
            .await
            .is_err(),
        "sandbox worktree should not contain .ctx-hooks"
    );
    assert_eq!(
        get_git_config(&managed_root, CORE_HOOKS_PATH_KEY)
            .await
            .expect("load hooksPath"),
        Some(hooks_path.clone())
    );
    assert_eq!(
        get_git_config(&managed_root, CTX_TASK_ID_KEY)
            .await
            .expect("load task id"),
        Some(task_id.0.to_string())
    );
    assert_eq!(
        get_git_config(&managed_root, CTX_PREV_HOOKS_PATH_KEY)
            .await
            .expect("load prev hooks path"),
        Some(original_hooks.to_string_lossy().to_string())
    );

    cleanup_worktree_hooks(state.as_ref(), &workspace, &worktree)
        .await
        .expect("cleanup sandbox task hook");

    assert!(
        tokio::fs::metadata(&hooks_dir).await.is_err(),
        "expected hook dir cleanup at {}",
        hooks_dir.display()
    );
    assert_eq!(
        get_git_config(&managed_root, CORE_HOOKS_PATH_KEY)
            .await
            .expect("load restored hooksPath"),
        Some(original_hooks.to_string_lossy().to_string())
    );
    assert_eq!(
        get_git_config(&managed_root, CTX_TASK_ID_KEY)
            .await
            .expect("load cleared task id"),
        None
    );
    assert_eq!(
        get_git_config(&managed_root, CTX_PREV_HOOKS_PATH_KEY)
            .await
            .expect("load cleared prev hooks path"),
        None
    );
}
