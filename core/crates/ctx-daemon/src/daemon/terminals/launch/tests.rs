use super::{infer_terminal_worktree, worktree::resolve_terminal_worktree, TerminalLaunchHost};
use crate::daemon::{DaemonState, ProtectedWorkspaceStoreLookup};
use chrono::Utc;
use ctx_core::ids::{SessionId, TaskId, WorkspaceId, WorktreeId};
use ctx_core::models::{ExecutionEnvironment, VcsKind, Workspace, Worktree};
use ctx_sandbox_contract::{container_worktree_root, sandbox_worktree_root};
use ctx_store::StoreManager;
use ctx_transport_runtime::terminal_launch::TerminalLaunchErrorKind;
use ctx_worktree_vcs_service::managed_worktree_path;
use std::path::PathBuf;
use std::sync::Arc;

fn sample_workspace(root_path: &str) -> Workspace {
    Workspace {
        id: WorkspaceId(uuid::Uuid::new_v4()),
        root_path: root_path.to_string(),
        name: "sample".to_string(),
        created_at: Utc::now(),
        vcs_kind: Some(VcsKind::Git),
    }
}

fn sample_worktree(workspace: &Workspace, root_path: PathBuf) -> Worktree {
    Worktree {
        id: WorktreeId(uuid::Uuid::new_v4()),
        workspace_id: workspace.id,
        root_path: root_path.to_string_lossy().to_string(),
        base_commit_sha: String::new(),
        git_branch: None,
        vcs_kind: workspace.vcs_kind.clone(),
        base_revision: None,
        vcs_ref: None,
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
    }
}

async fn test_state(data_root: &std::path::Path) -> Arc<DaemonState> {
    Arc::new(DaemonState::new(
        data_root.to_path_buf(),
        StoreManager::open(data_root).await.expect("open stores"),
        std::collections::HashMap::new(),
        "http://127.0.0.1:4310".to_string(),
        None,
    ))
}

fn test_terminal_launch_host(state: &Arc<DaemonState>) -> TerminalLaunchHost {
    TerminalLaunchHost::new(
        state.global_store().clone(),
        ProtectedWorkspaceStoreLookup::new(
            state.core.stores.clone(),
            Arc::clone(&state.sessions),
            Arc::clone(&state.transport.merge_queue),
        ),
        state.core.data_root.clone(),
        state.core.daemon_url.clone(),
        Arc::clone(&state.execution.harness),
        Arc::clone(&state.transport.terminals),
    )
}

#[test]
fn sandbox_worktree_root_maps_managed_host_worktree_to_container_root() {
    let data_root = tempfile::tempdir().unwrap();
    let workspace = sample_workspace("/host/ws");
    let worktree_id = WorktreeId(uuid::Uuid::new_v4());
    let managed_root = managed_worktree_path(data_root.path(), workspace.id, worktree_id);
    let mut worktree = sample_worktree(&workspace, managed_root);
    worktree.id = worktree_id;

    assert_eq!(
        sandbox_worktree_root(&workspace, &worktree),
        container_worktree_root(worktree_id)
    );
}

#[tokio::test]
async fn infer_terminal_worktree_returns_not_found_for_unknown_session_without_fallback() {
    let data_root = tempfile::tempdir().expect("tempdir");
    let state = test_state(data_root.path()).await;
    let workspace = state
        .global_store()
        .create_workspace(
            "ws".to_string(),
            data_root
                .path()
                .join("workspace")
                .to_string_lossy()
                .to_string(),
            VcsKind::Git,
        )
        .await
        .expect("create workspace");
    let store = state
        .store_for_workspace(workspace.id)
        .await
        .expect("workspace store");
    let _worktree = store
        .insert_worktree(sample_worktree(
            &workspace,
            data_root.path().join("workspace").join("wt-existing"),
        ))
        .await
        .expect("insert worktree");

    let host = test_terminal_launch_host(&state);
    let err = infer_terminal_worktree(
        &host,
        workspace.id,
        Some(SessionId(uuid::Uuid::new_v4())),
        None,
    )
    .await
    .expect_err("unknown explicit session target should 404");

    assert_eq!(err.kind(), TerminalLaunchErrorKind::NotFound);
}

#[tokio::test]
async fn infer_terminal_worktree_returns_not_found_for_unknown_task_without_fallback() {
    let data_root = tempfile::tempdir().expect("tempdir");
    let state = test_state(data_root.path()).await;
    let workspace = state
        .global_store()
        .create_workspace(
            "ws".to_string(),
            data_root
                .path()
                .join("workspace")
                .to_string_lossy()
                .to_string(),
            VcsKind::Git,
        )
        .await
        .expect("create workspace");
    let store = state
        .store_for_workspace(workspace.id)
        .await
        .expect("workspace store");
    let _worktree = store
        .insert_worktree(sample_worktree(
            &workspace,
            data_root.path().join("workspace").join("wt-existing"),
        ))
        .await
        .expect("insert worktree");

    let host = test_terminal_launch_host(&state);
    let err = infer_terminal_worktree(
        &host,
        workspace.id,
        None,
        Some(TaskId(uuid::Uuid::new_v4())),
    )
    .await
    .expect_err("unknown explicit task target should 404");

    assert_eq!(err.kind(), TerminalLaunchErrorKind::NotFound);
}

#[tokio::test]
async fn resolve_terminal_worktree_rejects_explicit_worktree_from_other_workspace() {
    let data_root = tempfile::tempdir().expect("tempdir");
    let state = test_state(data_root.path()).await;
    let workspace_a = state
        .global_store()
        .create_workspace(
            "ws-a".to_string(),
            data_root
                .path()
                .join("workspace-a")
                .to_string_lossy()
                .to_string(),
            VcsKind::Git,
        )
        .await
        .expect("create workspace a");
    let workspace_b = state
        .global_store()
        .create_workspace(
            "ws-b".to_string(),
            data_root
                .path()
                .join("workspace-b")
                .to_string_lossy()
                .to_string(),
            VcsKind::Git,
        )
        .await
        .expect("create workspace b");
    let store_b = state
        .store_for_workspace(workspace_b.id)
        .await
        .expect("workspace b store");
    let worktree_b = store_b
        .insert_worktree(sample_worktree(
            &workspace_b,
            data_root.path().join("workspace-b").join("wt-b"),
        ))
        .await
        .expect("insert worktree b");
    state
        .global_store()
        .upsert_workspace_worktree_index(worktree_b.id, workspace_b.id)
        .await
        .expect("index worktree b");

    let host = test_terminal_launch_host(&state);
    let err = resolve_terminal_worktree(&host, workspace_a.id, Some(worktree_b.id), None, None)
        .await
        .expect_err("cross-workspace worktree should 404");

    assert_eq!(err.kind(), TerminalLaunchErrorKind::NotFound);
}

#[tokio::test]
async fn infer_terminal_worktree_rejects_session_from_other_workspace() {
    let data_root = tempfile::tempdir().expect("tempdir");
    let state = test_state(data_root.path()).await;
    let workspace_a = state
        .global_store()
        .create_workspace(
            "ws-a".to_string(),
            data_root
                .path()
                .join("workspace-a")
                .to_string_lossy()
                .to_string(),
            VcsKind::Git,
        )
        .await
        .expect("create workspace a");
    let workspace_b = state
        .global_store()
        .create_workspace(
            "ws-b".to_string(),
            data_root
                .path()
                .join("workspace-b")
                .to_string_lossy()
                .to_string(),
            VcsKind::Git,
        )
        .await
        .expect("create workspace b");
    let store_b = state
        .store_for_workspace(workspace_b.id)
        .await
        .expect("workspace b store");
    let worktree_b = store_b
        .insert_worktree(sample_worktree(
            &workspace_b,
            data_root.path().join("workspace-b").join("wt-b"),
        ))
        .await
        .expect("insert worktree b");
    let task_b = store_b
        .create_task(workspace_b.id, "task-b".to_string(), None)
        .await
        .expect("create task b");
    let session_b = store_b
        .create_session(
            task_b.id,
            workspace_b.id,
            worktree_b.id,
            ExecutionEnvironment::Host,
            "codex".to_string(),
            "gpt-5.4".to_string(),
            "primary".to_string(),
            None,
            None,
            None,
        )
        .await
        .expect("create session b");
    state
        .global_store()
        .upsert_workspace_session_index(session_b.id, workspace_b.id)
        .await
        .expect("index session b");

    let host = test_terminal_launch_host(&state);
    let err = infer_terminal_worktree(&host, workspace_a.id, Some(session_b.id), None)
        .await
        .expect_err("cross-workspace session should 404");

    assert_eq!(err.kind(), TerminalLaunchErrorKind::NotFound);
}

#[tokio::test]
async fn infer_terminal_worktree_rejects_task_from_other_workspace() {
    let data_root = tempfile::tempdir().expect("tempdir");
    let state = test_state(data_root.path()).await;
    let workspace_a = state
        .global_store()
        .create_workspace(
            "ws-a".to_string(),
            data_root
                .path()
                .join("workspace-a")
                .to_string_lossy()
                .to_string(),
            VcsKind::Git,
        )
        .await
        .expect("create workspace a");
    let workspace_b = state
        .global_store()
        .create_workspace(
            "ws-b".to_string(),
            data_root
                .path()
                .join("workspace-b")
                .to_string_lossy()
                .to_string(),
            VcsKind::Git,
        )
        .await
        .expect("create workspace b");
    let store_b = state
        .store_for_workspace(workspace_b.id)
        .await
        .expect("workspace b store");
    let task_b = store_b
        .create_task(workspace_b.id, "task-b".to_string(), None)
        .await
        .expect("create task b");
    state
        .global_store()
        .upsert_workspace_task_index(task_b.id, workspace_b.id)
        .await
        .expect("index task b");

    let host = test_terminal_launch_host(&state);
    let err = infer_terminal_worktree(&host, workspace_a.id, None, Some(task_b.id))
        .await
        .expect_err("cross-workspace task should 404");

    assert_eq!(err.kind(), TerminalLaunchErrorKind::NotFound);
}
