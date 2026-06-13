use std::path::Path;

use axum::http::StatusCode;
use serde_json::Value;

use ctx_core::ids::WorkspaceId;

pub(super) fn assert_storage_admission_rejected_before_copy(
    status: StatusCode,
    body: Value,
    log: &str,
    workspace_id: WorkspaceId,
    container_name: &str,
    data_root: &Path,
) {
    assert_eq!(
        status,
        StatusCode::INSUFFICIENT_STORAGE,
        "unexpected task creation response: {body:#?}\nsandbox log:\n{log}"
    );
    let error = body
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        error.contains("failed to create default session"),
        "expected default session failure after storage admission rejection, got: {body:#?}"
    );

    assert_sandbox_preflight_happened(log, workspace_id, container_name);
    assert_disk_isolated_copy_did_not_start(log, data_root);
}

fn assert_sandbox_preflight_happened(log: &str, workspace_id: WorkspaceId, container_name: &str) {
    assert!(
        log.contains(&format!("volume inspect ctx-ws-{}", workspace_id.0)),
        "expected workspace volume preflight in sandbox log:\n{log}"
    );
    assert!(
        log.contains(&format!("volume create ctx-ws-{}", workspace_id.0)),
        "expected workspace volume creation in sandbox log:\n{log}"
    );
    assert!(
        log.contains(&format!("container inspect {container_name}")),
        "expected container existence check in sandbox log:\n{log}"
    );
    assert!(
        log.contains(&format!(
            "container inspect --format {{{{.State.Running}}}} {container_name}"
        )),
        "expected running-container check in sandbox log:\n{log}"
    );
    assert!(
        log.contains(&format!("inspect {container_name}")),
        "expected disk-isolated mount verification in sandbox log:\n{log}"
    );
}

fn assert_disk_isolated_copy_did_not_start(log: &str, data_root: &Path) {
    assert!(
        !log.contains(" tar -xf -"),
        "disk-isolated copy should not stream into the container after admission failure:\n{log}"
    );
    assert!(
        !log.contains(" mkdir -p -- /ctx/ws/worktrees/"),
        "disk-isolated worktree root should not be created after admission failure:\n{log}"
    );
    assert!(
        !log.contains(" git checkout -B "),
        "disk-isolated checkout should not run after admission failure:\n{log}"
    );
    assert!(
        !data_root.join("disk-isolated").join("staging").exists(),
        "host-side staging root should not be created when admission rejects early"
    );
}
