use super::route_error_kind_for_internal_error;
use crate::test_support::TestDaemon;
use ctx_core::models::ExecutionEnvironment;
use ctx_core::models::VcsKind;
use ctx_route_contracts::tasks::{TaskRouteErrorKind, TaskRouteParams};

#[test]
fn route_error_kind_preserves_non_http_internal_status_categories() {
    let storage_error =
        anyhow::anyhow!("Insufficient storage capacity for creating an isolated task worktree");
    assert_eq!(
        route_error_kind_for_internal_error(&storage_error),
        TaskRouteErrorKind::InsufficientStorage
    );

    let policy_error = ctx_settings_service::HostExecutionPolicy::SandboxOnly
        .validate_execution_environment(ExecutionEnvironment::Host)
        .expect_err("host execution should be denied");
    assert_eq!(
        route_error_kind_for_internal_error(&policy_error),
        TaskRouteErrorKind::Forbidden
    );

    assert_eq!(
        route_error_kind_for_internal_error(&anyhow::anyhow!("plain internal failure")),
        TaskRouteErrorKind::Internal
    );
}

#[tokio::test]
async fn task_session_listing_store_lookup_failure_is_internal_not_not_found() {
    let temp = tempfile::tempdir().expect("tempdir");
    let daemon = TestDaemon::new_for_test(
        temp.path().join("data"),
        "http://127.0.0.1:4310".to_string(),
    )
    .await
    .expect("daemon");
    let workspace_root = temp.path().join("repo");
    std::fs::create_dir_all(&workspace_root).expect("workspace root");
    let workspace = daemon
        .seed_task_lifecycle_workspace_for_test("ws", &workspace_root, VcsKind::Git)
        .await
        .expect("workspace");
    let task = daemon
        .seed_task_lifecycle_task_for_test(workspace.id, "task")
        .await
        .expect("task");
    daemon
        .cache_rehydration_make_workspace_store_unopenable_for_test(workspace.id)
        .await
        .expect("make workspace store unavailable");

    let error = daemon
        .task_session_listing_handle_for_test()
        .list_task_sessions_for_route(TaskRouteParams::new(task.id.0.to_string()))
        .await
        .expect_err("store lookup failure should not become a 404");

    assert_eq!(error.kind(), TaskRouteErrorKind::Internal);
}
