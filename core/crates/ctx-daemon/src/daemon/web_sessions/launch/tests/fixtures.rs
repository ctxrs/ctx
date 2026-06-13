use super::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::Utc;
use ctx_core::models::{
    sandbox_instance_id_for_workspace, SandboxBinding, SandboxGuestIdentity, SandboxProfile,
    SandboxSubstrate, VcsKind, Worktree,
};
use ctx_store::StoreManager;

use crate::daemon::web_sessions::WebSessionWorkerRuntimeHost;
use crate::daemon::{DaemonState, ProtectedWorkspaceStoreLookup};

pub(super) struct EnvVarGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvVarGuard {
    pub(super) fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        // SAFETY: Guarded by EXECUTION_POLICY_TEST_ENV_LOCK in every test using this helper.
        unsafe { std::env::set_var(key, value) };
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.as_ref() {
            // SAFETY: Guarded by EXECUTION_POLICY_TEST_ENV_LOCK in every test using this helper.
            unsafe { std::env::set_var(self.key, previous) };
        } else {
            // SAFETY: Guarded by EXECUTION_POLICY_TEST_ENV_LOCK in every test using this helper.
            unsafe { std::env::remove_var(self.key) };
        }
    }
}

pub(super) async fn test_state(data_root: &Path) -> Arc<DaemonState> {
    Arc::new(DaemonState::new(
        data_root.to_path_buf(),
        StoreManager::open(data_root).await.expect("open stores"),
        HashMap::new(),
        "http://127.0.0.1:4399".to_string(),
        None,
    ))
}

pub(super) fn test_web_session_launch_host(state: &Arc<DaemonState>) -> WebSessionLaunchHost {
    WebSessionLaunchHost::new(
        state.global_store().clone(),
        ProtectedWorkspaceStoreLookup::new(
            state.core.stores.clone(),
            Arc::clone(&state.sessions),
            Arc::clone(&state.transport.merge_queue),
        ),
        state.core.data_root.clone(),
        WebSessionWorkerRuntimeHost::new(
            state.core.data_root.clone(),
            Arc::clone(&state.providers),
            state.telemetry.ops_events.clone(),
        ),
        Arc::clone(&state.transport.web_sessions),
    )
}

pub(super) fn sample_worktree(
    workspace_id: ctx_core::ids::WorkspaceId,
    root_path: PathBuf,
) -> Worktree {
    Worktree {
        id: WorktreeId(uuid::Uuid::new_v4()),
        workspace_id,
        root_path: root_path.to_string_lossy().to_string(),
        base_commit_sha: String::new(),
        git_branch: None,
        vcs_kind: Some(VcsKind::Git),
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

pub(super) fn sandbox_binding_for(worktree: &Worktree) -> SandboxBinding {
    SandboxBinding {
        worktree_id: worktree.id,
        workspace_id: worktree.workspace_id,
        sandbox_instance_id: sandbox_instance_id_for_workspace(worktree.workspace_id),
        substrate: SandboxSubstrate::NativeContainer,
        guest_identity: SandboxGuestIdentity::linux_container_ubuntu(),
        profile: SandboxProfile::Standard,
        live_workspace_root: "/ctx/workspace".to_string(),
        live_worktree_root: "/ctx/worktree".to_string(),
        execution_settings_json: None,
        container_name: Some("ctx-test".to_string()),
        host_materialization_root: Some("/tmp/ctx-test".to_string()),
        created_at: Utc::now(),
    }
}

pub(super) fn assert_launch_error(
    error: &WebSessionLaunchError,
    kind: WebSessionLaunchErrorKind,
    message_contains: &str,
) {
    assert_eq!(error.kind(), kind);
    assert!(
        error.message().contains(message_contains),
        "expected {:?} to contain {:?}",
        error.message(),
        message_contains
    );
}
