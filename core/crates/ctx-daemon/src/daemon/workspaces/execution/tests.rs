use super::*;

use chrono::Utc;
use ctx_core::ids::{WorkspaceId, WorktreeId};
use ctx_core::models::{SandboxBinding, SandboxGuestIdentity, SandboxProfile, SandboxSubstrate};
use ctx_sandbox_contract::sandbox_execution_settings_from_binding;
use ctx_settings_model::ExecutionMode;
use uuid::Uuid;

mod binding_snapshot;
mod resolve_existing;

fn test_binding(substrate: SandboxSubstrate, raw: Option<String>) -> SandboxBinding {
    let workspace_id = WorkspaceId(Uuid::new_v4());
    SandboxBinding {
        worktree_id: WorktreeId(Uuid::new_v4()),
        workspace_id,
        sandbox_instance_id: ctx_core::models::sandbox_instance_id_for_workspace(workspace_id),
        substrate,
        guest_identity: SandboxGuestIdentity::linux_container_ubuntu(),
        profile: SandboxProfile::Standard,
        live_workspace_root: "/ctx/ws".to_string(),
        live_worktree_root: "/ctx/wt".to_string(),
        execution_settings_json: raw,
        container_name: Some("ctx-test".to_string()),
        host_materialization_root: Some("/tmp/shadow".to_string()),
        created_at: Utc::now(),
    }
}
