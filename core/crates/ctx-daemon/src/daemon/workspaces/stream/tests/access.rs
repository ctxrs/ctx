use super::*;

use crate::test_support::TestDaemon;
use ctx_core::ids::WorkspaceId;
use ctx_core::models::VcsKind;
use ctx_route_contracts::workspaces::{WorkspaceStreamRouteErrorKind, WorkspaceStreamRouteParams};
use tempfile::tempdir;

#[tokio::test]
async fn workspace_stream_access_rejects_missing_workspace() {
    let temp = tempdir().expect("tempdir");
    let daemon = TestDaemon::new_for_test(
        temp.path().to_path_buf(),
        "http://127.0.0.1:4567".to_string(),
    )
    .await
    .expect("test daemon");
    let workspace_id = WorkspaceId(uuid::Uuid::new_v4());

    let active_error = daemon
        .workspace_stream_handle_for_test()
        .require_workspace_active_stream_access(workspace_id)
        .await
        .expect_err("missing workspace should reject active stream access");
    assert!(matches!(active_error, WorkspaceStreamAccessError::NotFound));

    let vcs_error = daemon
        .workspace_vcs_stream_handle_for_test()
        .require_workspace_vcs_stream_access(workspace_id)
        .await
        .expect_err("missing workspace should reject VCS stream access");
    assert!(matches!(vcs_error, WorkspaceStreamAccessError::NotFound));
}

#[tokio::test]
async fn workspace_stream_route_admission_rejects_invalid_workspace_id() {
    let temp = tempdir().expect("tempdir");
    let daemon = TestDaemon::new_for_test(
        temp.path().to_path_buf(),
        "http://127.0.0.1:4567".to_string(),
    )
    .await
    .expect("test daemon");

    let active_error = daemon
        .workspace_stream_handle_for_test()
        .admit_workspace_active_stream_for_route(WorkspaceStreamRouteParams::new("not-a-workspace"))
        .await
        .expect_err("invalid workspace id should reject active stream route admission");
    assert_eq!(
        active_error.kind(),
        WorkspaceStreamRouteErrorKind::BadRequest
    );
    assert_eq!(active_error.message(), "invalid workspace id");

    let vcs_error = daemon
        .workspace_vcs_stream_handle_for_test()
        .admit_workspace_vcs_stream_for_route(WorkspaceStreamRouteParams::new("not-a-workspace"))
        .await
        .expect_err("invalid workspace id should reject VCS stream route admission");
    assert_eq!(vcs_error.kind(), WorkspaceStreamRouteErrorKind::BadRequest);
    assert_eq!(vcs_error.message(), "invalid workspace id");
}

#[tokio::test]
async fn workspace_stream_access_allows_existing_workspace() {
    let temp = tempdir().expect("tempdir");
    let daemon = TestDaemon::new_for_test(
        temp.path().to_path_buf(),
        "http://127.0.0.1:4567".to_string(),
    )
    .await
    .expect("test daemon");
    let workspace = daemon
        .global_store()
        .create_workspace(
            "workspace".to_string(),
            daemon
                .data_root()
                .join("workspace")
                .to_string_lossy()
                .to_string(),
            VcsKind::Git,
        )
        .await
        .expect("create workspace");

    daemon
        .workspace_stream_handle_for_test()
        .require_workspace_active_stream_access(workspace.id)
        .await
        .expect("existing workspace should allow active stream access");
    daemon
        .workspace_vcs_stream_handle_for_test()
        .require_workspace_vcs_stream_access(workspace.id)
        .await
        .expect("existing workspace should allow VCS stream access");

    let active_admission = daemon
        .workspace_stream_handle_for_test()
        .admit_workspace_active_stream_for_route(WorkspaceStreamRouteParams::new(
            workspace.id.0.to_string(),
        ))
        .await
        .expect("existing workspace should allow active stream route admission");
    assert_eq!(active_admission.workspace_id(), workspace.id);

    let vcs_admission = daemon
        .workspace_vcs_stream_handle_for_test()
        .admit_workspace_vcs_stream_for_route(WorkspaceStreamRouteParams::new(
            workspace.id.0.to_string(),
        ))
        .await
        .expect("existing workspace should allow VCS stream route admission");
    assert_eq!(vcs_admission.workspace_id(), workspace.id);
}
