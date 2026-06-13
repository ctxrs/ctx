use super::fixtures::{
    assert_launch_error, sample_worktree, sandbox_binding_for, test_state,
    test_web_session_launch_host, EnvVarGuard,
};
use super::*;
use ctx_core::models::{ExecutionEnvironment, VcsKind};
use ctx_settings_model::{ExecutionMode, ExecutionSettings, Settings};
use ctx_settings_service::{CTX_HOST_EXECUTION_POLICY_ENV, EXECUTION_POLICY_TEST_ENV_LOCK};

#[tokio::test]
async fn create_web_session_rejects_sandbox_only_before_host_runtime_setup() {
    let _env_lock = EXECUTION_POLICY_TEST_ENV_LOCK.lock().await;
    let _policy = EnvVarGuard::set(CTX_HOST_EXECUTION_POLICY_ENV, "sandbox_only");
    let data_root = tempfile::tempdir().expect("tempdir");
    let state = test_state(data_root.path()).await;
    let host = test_web_session_launch_host(&state);

    let err = create_web_session(
        &host,
        WebSessionLaunchRequest {
            session_id: None,
            worktree_id: None,
            url: "https://example.com".to_string(),
            viewport: None,
            fps: None,
        },
    )
    .await
    .expect_err("sandbox-only policy should reject host web sessions");

    assert_launch_error(
        &err,
        WebSessionLaunchErrorKind::Forbidden,
        "web sessions currently run on the host",
    );
    assert!(err.message().contains("host execution is disabled"));
}

#[tokio::test]
async fn create_web_session_rejects_sandbox_session_before_host_runtime_setup() {
    let _env_lock = EXECUTION_POLICY_TEST_ENV_LOCK.lock().await;
    let _policy = EnvVarGuard::set(CTX_HOST_EXECUTION_POLICY_ENV, "allow_host");
    let data_root = tempfile::tempdir().expect("tempdir");
    let state = test_state(data_root.path()).await;
    let host = test_web_session_launch_host(&state);
    let workspace_root = data_root.path().join("workspace");
    let workspace = state
        .global_store()
        .create_workspace(
            "ws".to_string(),
            workspace_root.to_string_lossy().to_string(),
            VcsKind::Git,
        )
        .await
        .expect("create workspace");
    let store = state
        .store_for_workspace(workspace.id)
        .await
        .expect("workspace store");
    let worktree = store
        .insert_worktree(sample_worktree(
            workspace.id,
            workspace_root.join("worktree"),
        ))
        .await
        .expect("insert worktree");
    let task = store
        .create_task(workspace.id, "task".to_string(), None)
        .await
        .expect("create task");
    let session = store
        .create_session(
            task.id,
            workspace.id,
            worktree.id,
            ExecutionEnvironment::Sandbox,
            "codex".to_string(),
            "gpt-5.4".to_string(),
            "primary".to_string(),
            None,
            None,
            None,
        )
        .await
        .expect("create session");
    state
        .global_store()
        .upsert_workspace_session_index(session.id, workspace.id)
        .await
        .expect("index session");

    let err = create_web_session(
        &host,
        WebSessionLaunchRequest {
            session_id: Some(session.id),
            worktree_id: None,
            url: "https://example.com".to_string(),
            viewport: None,
            fps: None,
        },
    )
    .await
    .expect_err("sandbox session should reject host web sessions");

    assert_launch_error(
        &err,
        WebSessionLaunchErrorKind::Forbidden,
        "disabled for sandbox sessions",
    );
}

#[tokio::test]
async fn create_web_session_rejects_sandbox_bound_worktree_before_host_runtime_setup() {
    let _env_lock = EXECUTION_POLICY_TEST_ENV_LOCK.lock().await;
    let _policy = EnvVarGuard::set(CTX_HOST_EXECUTION_POLICY_ENV, "allow_host");
    let data_root = tempfile::tempdir().expect("tempdir");
    let state = test_state(data_root.path()).await;
    let host = test_web_session_launch_host(&state);
    let workspace_root = data_root.path().join("workspace");
    let workspace = state
        .global_store()
        .create_workspace(
            "ws".to_string(),
            workspace_root.to_string_lossy().to_string(),
            VcsKind::Git,
        )
        .await
        .expect("create workspace");
    let store = state
        .store_for_workspace(workspace.id)
        .await
        .expect("workspace store");
    let worktree = store
        .insert_worktree(sample_worktree(
            workspace.id,
            workspace_root.join("worktree"),
        ))
        .await
        .expect("insert worktree");
    state
        .global_store()
        .upsert_workspace_worktree_index(worktree.id, workspace.id)
        .await
        .expect("index worktree");
    store
        .upsert_sandbox_binding(sandbox_binding_for(&worktree))
        .await
        .expect("seed sandbox binding");

    let err = create_web_session(
        &host,
        WebSessionLaunchRequest {
            session_id: None,
            worktree_id: Some(worktree.id),
            url: "https://example.com".to_string(),
            viewport: None,
            fps: None,
        },
    )
    .await
    .expect_err("sandbox-bound worktree should reject before worker prep");

    assert_launch_error(
        &err,
        WebSessionLaunchErrorKind::Forbidden,
        "disabled for sandbox worktrees",
    );
}

#[tokio::test]
async fn create_web_session_rejects_sandbox_workspace_mode_before_host_runtime_setup() {
    let _env_lock = EXECUTION_POLICY_TEST_ENV_LOCK.lock().await;
    let _policy = EnvVarGuard::set(CTX_HOST_EXECUTION_POLICY_ENV, "allow_host");
    let data_root = tempfile::tempdir().expect("tempdir");
    let state = test_state(data_root.path()).await;
    ctx_settings_service::save_settings(
        state.global_store(),
        &Settings {
            execution: Some(ExecutionSettings {
                mode: ExecutionMode::Sandbox,
                ..ExecutionSettings::default()
            }),
            ..Settings::default()
        },
    )
    .await
    .expect("save sandbox execution setting");
    let host = test_web_session_launch_host(&state);
    let workspace_root = data_root.path().join("workspace");
    let workspace = state
        .global_store()
        .create_workspace(
            "ws".to_string(),
            workspace_root.to_string_lossy().to_string(),
            VcsKind::Git,
        )
        .await
        .expect("create workspace");
    let store = state
        .store_for_workspace(workspace.id)
        .await
        .expect("workspace store");
    let worktree = store
        .insert_worktree(sample_worktree(
            workspace.id,
            workspace_root.join("worktree"),
        ))
        .await
        .expect("insert worktree");
    state
        .global_store()
        .upsert_workspace_worktree_index(worktree.id, workspace.id)
        .await
        .expect("index worktree");

    let err = create_web_session(
        &host,
        WebSessionLaunchRequest {
            session_id: None,
            worktree_id: Some(worktree.id),
            url: "https://example.com".to_string(),
            viewport: None,
            fps: None,
        },
    )
    .await
    .expect_err("sandbox workspace mode should reject before worker prep");

    assert_launch_error(
        &err,
        WebSessionLaunchErrorKind::Forbidden,
        "disabled for sandbox workspaces",
    );
}

#[tokio::test]
async fn create_web_session_rejects_unscoped_host_launches() {
    let _env_lock = EXECUTION_POLICY_TEST_ENV_LOCK.lock().await;
    let _policy = EnvVarGuard::set(CTX_HOST_EXECUTION_POLICY_ENV, "allow_host");
    let data_root = tempfile::tempdir().expect("tempdir");
    let state = test_state(data_root.path()).await;
    let host = test_web_session_launch_host(&state);

    let err = create_web_session(
        &host,
        WebSessionLaunchRequest {
            session_id: None,
            worktree_id: None,
            url: "https://example.com".to_string(),
            viewport: None,
            fps: None,
        },
    )
    .await
    .expect_err("unscoped web sessions should be rejected");

    assert_launch_error(
        &err,
        WebSessionLaunchErrorKind::BadRequest,
        "session_id or worktree_id",
    );
}
