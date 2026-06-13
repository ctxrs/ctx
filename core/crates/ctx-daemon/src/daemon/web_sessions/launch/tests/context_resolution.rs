use super::super::context::resolve_web_session_launch_context;
use super::fixtures::{
    sample_worktree, sandbox_binding_for, test_state, test_web_session_launch_host, EnvVarGuard,
};
use crate::daemon::DaemonState;
use ctx_core::ids::{SessionId, WorktreeId};
use ctx_core::models::{ExecutionEnvironment, Session, VcsKind, Worktree};
use ctx_settings_model::{ExecutionMode, ExecutionSettings, Settings};
use ctx_settings_service::{CTX_HOST_EXECUTION_POLICY_ENV, EXECUTION_POLICY_TEST_ENV_LOCK};
use ctx_store::Store;
use std::sync::Arc;

struct SeededWebSessionScope {
    store: Store,
    worktree: Worktree,
    session: Session,
}

async fn seed_host_session_scope(
    state: &Arc<DaemonState>,
    data_root: &std::path::Path,
    label: &str,
) -> SeededWebSessionScope {
    seed_session_scope(state, data_root, label, ExecutionEnvironment::Host).await
}

async fn seed_session_scope(
    state: &Arc<DaemonState>,
    data_root: &std::path::Path,
    label: &str,
    execution_environment: ExecutionEnvironment,
) -> SeededWebSessionScope {
    let workspace_root = data_root.join(format!("workspace-{label}"));
    let workspace = state
        .global_store()
        .create_workspace(
            format!("ws-{label}"),
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
            workspace_root.join(format!("worktree-{label}")),
        ))
        .await
        .expect("insert worktree");
    state
        .global_store()
        .upsert_workspace_worktree_index(worktree.id, workspace.id)
        .await
        .expect("index worktree");
    let task = store
        .create_task(workspace.id, format!("task-{label}"), None)
        .await
        .expect("create task");
    let session = store
        .create_session(
            task.id,
            workspace.id,
            worktree.id,
            execution_environment,
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
    SeededWebSessionScope {
        store,
        worktree,
        session,
    }
}

#[tokio::test]
async fn launch_context_infers_worktree_from_session_scope() {
    let _env_lock = EXECUTION_POLICY_TEST_ENV_LOCK.lock().await;
    let _policy = EnvVarGuard::set(CTX_HOST_EXECUTION_POLICY_ENV, "allow_host");
    let data_root = tempfile::tempdir().expect("tempdir");
    let state = test_state(data_root.path()).await;
    let host = test_web_session_launch_host(&state);
    let seeded = seed_host_session_scope(&state, data_root.path(), "session").await;

    let context = resolve_web_session_launch_context(&host, Some(seeded.session.id), None)
        .await
        .expect("session launch context");

    assert_eq!(
        context.work_dir,
        Some(std::path::PathBuf::from(seeded.worktree.root_path))
    );
}

#[tokio::test]
async fn launch_context_uses_explicit_worktree_scope() {
    let _env_lock = EXECUTION_POLICY_TEST_ENV_LOCK.lock().await;
    let _policy = EnvVarGuard::set(CTX_HOST_EXECUTION_POLICY_ENV, "allow_host");
    let data_root = tempfile::tempdir().expect("tempdir");
    let state = test_state(data_root.path()).await;
    let host = test_web_session_launch_host(&state);
    let seeded = seed_host_session_scope(&state, data_root.path(), "worktree").await;

    let context = resolve_web_session_launch_context(&host, None, Some(seeded.worktree.id))
        .await
        .expect("worktree launch context");

    assert_eq!(
        context.work_dir,
        Some(std::path::PathBuf::from(seeded.worktree.root_path))
    );
}

#[tokio::test]
async fn launch_context_preserves_explicit_worktree_precedence_with_dual_scope() {
    let _env_lock = EXECUTION_POLICY_TEST_ENV_LOCK.lock().await;
    let _policy = EnvVarGuard::set(CTX_HOST_EXECUTION_POLICY_ENV, "allow_host");
    let data_root = tempfile::tempdir().expect("tempdir");
    let state = test_state(data_root.path()).await;
    let host = test_web_session_launch_host(&state);
    let session_scope = seed_host_session_scope(&state, data_root.path(), "session-scope").await;
    let explicit_scope = seed_host_session_scope(&state, data_root.path(), "explicit-scope").await;

    let context = resolve_web_session_launch_context(
        &host,
        Some(session_scope.session.id),
        Some(explicit_scope.worktree.id),
    )
    .await
    .expect("dual-scope launch context");

    assert_eq!(
        context.work_dir,
        Some(std::path::PathBuf::from(explicit_scope.worktree.root_path))
    );
}

#[tokio::test]
async fn launch_context_rejects_sandbox_session_even_with_explicit_host_worktree() {
    let _env_lock = EXECUTION_POLICY_TEST_ENV_LOCK.lock().await;
    let _policy = EnvVarGuard::set(CTX_HOST_EXECUTION_POLICY_ENV, "allow_host");
    let data_root = tempfile::tempdir().expect("tempdir");
    let state = test_state(data_root.path()).await;
    let host = test_web_session_launch_host(&state);
    let session_scope = seed_session_scope(
        &state,
        data_root.path(),
        "sandbox-session",
        ExecutionEnvironment::Sandbox,
    )
    .await;
    let explicit_scope = seed_host_session_scope(&state, data_root.path(), "explicit-host").await;

    let err = resolve_web_session_launch_context(
        &host,
        Some(session_scope.session.id),
        Some(explicit_scope.worktree.id),
    )
    .await
    .expect_err("sandbox session should reject before explicit host worktree launch");

    assert!(format!("{err:#}").contains("disabled for sandbox sessions"));
}

#[tokio::test]
async fn launch_context_rejects_sandbox_bound_session_worktree_even_with_explicit_host_worktree() {
    let _env_lock = EXECUTION_POLICY_TEST_ENV_LOCK.lock().await;
    let _policy = EnvVarGuard::set(CTX_HOST_EXECUTION_POLICY_ENV, "allow_host");
    let data_root = tempfile::tempdir().expect("tempdir");
    let state = test_state(data_root.path()).await;
    let host = test_web_session_launch_host(&state);
    let session_scope =
        seed_host_session_scope(&state, data_root.path(), "sandbox-bound-session").await;
    let explicit_scope = seed_host_session_scope(&state, data_root.path(), "explicit-host").await;
    session_scope
        .store
        .upsert_sandbox_binding(sandbox_binding_for(&session_scope.worktree))
        .await
        .expect("seed session worktree sandbox binding");

    let err = resolve_web_session_launch_context(
        &host,
        Some(session_scope.session.id),
        Some(explicit_scope.worktree.id),
    )
    .await
    .expect_err(
        "sandbox-bound session worktree should reject before explicit host worktree launch",
    );

    assert!(format!("{err:#}").contains("disabled for sandbox worktrees"));
}

#[tokio::test]
async fn launch_context_rejects_unknown_session_without_fallback() {
    let _env_lock = EXECUTION_POLICY_TEST_ENV_LOCK.lock().await;
    let _policy = EnvVarGuard::set(CTX_HOST_EXECUTION_POLICY_ENV, "allow_host");
    let data_root = tempfile::tempdir().expect("tempdir");
    let state = test_state(data_root.path()).await;
    let host = test_web_session_launch_host(&state);
    let unknown_session = SessionId(uuid::Uuid::new_v4());

    let err = resolve_web_session_launch_context(&host, Some(unknown_session), None)
        .await
        .expect_err("unknown session should reject");

    assert!(format!("{err:#}").contains("workspace missing for session"));
}

#[tokio::test]
async fn launch_context_rejects_unknown_worktree_without_fallback() {
    let _env_lock = EXECUTION_POLICY_TEST_ENV_LOCK.lock().await;
    let _policy = EnvVarGuard::set(CTX_HOST_EXECUTION_POLICY_ENV, "allow_host");
    let data_root = tempfile::tempdir().expect("tempdir");
    let state = test_state(data_root.path()).await;
    let host = test_web_session_launch_host(&state);
    let unknown_worktree = WorktreeId(uuid::Uuid::new_v4());

    let err = resolve_web_session_launch_context(&host, None, Some(unknown_worktree))
        .await
        .expect_err("unknown worktree should reject");

    assert!(format!("{err:#}").contains("workspace missing for worktree"));
}

#[tokio::test]
async fn launch_context_rejects_sandbox_worktree_binding_before_worker_prep() {
    let _env_lock = EXECUTION_POLICY_TEST_ENV_LOCK.lock().await;
    let _policy = EnvVarGuard::set(CTX_HOST_EXECUTION_POLICY_ENV, "allow_host");
    let data_root = tempfile::tempdir().expect("tempdir");
    let state = test_state(data_root.path()).await;
    let host = test_web_session_launch_host(&state);
    let seeded = seed_host_session_scope(&state, data_root.path(), "sandbox-binding").await;
    seeded
        .store
        .upsert_sandbox_binding(sandbox_binding_for(&seeded.worktree))
        .await
        .expect("seed sandbox binding");

    let err = resolve_web_session_launch_context(&host, None, Some(seeded.worktree.id))
        .await
        .expect_err("sandbox-bound worktree should reject");

    assert!(format!("{err:#}").contains("disabled for sandbox worktrees"));
}

#[tokio::test]
async fn launch_context_rejects_sandbox_workspace_mode_before_worker_prep() {
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
    let seeded = seed_host_session_scope(&state, data_root.path(), "sandbox-workspace").await;

    let err = resolve_web_session_launch_context(&host, None, Some(seeded.worktree.id))
        .await
        .expect_err("sandbox workspace mode should reject");

    assert!(format!("{err:#}").contains("disabled for sandbox workspaces"));
}
