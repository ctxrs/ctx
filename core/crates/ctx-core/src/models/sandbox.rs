use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::*;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SandboxSubstrate {
    NativeContainer,
    SharedVmContainer,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SandboxGuestPlatform {
    #[default]
    Linux,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SandboxIsolationKind {
    #[default]
    Container,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SandboxGuestRuntime {
    #[default]
    Ubuntu,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SandboxGuestIdentity {
    #[serde(default)]
    pub platform: SandboxGuestPlatform,
    #[serde(default)]
    pub isolation_kind: SandboxIsolationKind,
    #[serde(default)]
    pub runtime: SandboxGuestRuntime,
}

impl SandboxGuestIdentity {
    pub const fn linux_container_ubuntu() -> Self {
        Self {
            platform: SandboxGuestPlatform::Linux,
            isolation_kind: SandboxIsolationKind::Container,
            runtime: SandboxGuestRuntime::Ubuntu,
        }
    }
}

pub fn sandbox_instance_id_for_workspace(workspace_id: WorkspaceId) -> SandboxInstanceId {
    SandboxInstanceId(workspace_id.0)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SandboxProfile {
    #[default]
    Standard,
    Strict,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxBinding {
    pub worktree_id: WorktreeId,
    pub workspace_id: WorkspaceId,
    pub sandbox_instance_id: SandboxInstanceId,
    #[serde(alias = "runtime_family")]
    pub substrate: SandboxSubstrate,
    #[serde(default)]
    pub guest_identity: SandboxGuestIdentity,
    #[serde(default)]
    pub profile: SandboxProfile,
    pub live_workspace_root: String,
    pub live_worktree_root: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_settings_json: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_name: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "host_projection_root"
    )]
    pub host_materialization_root: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl SandboxBinding {
    pub fn expected_sandbox_instance_id(&self) -> SandboxInstanceId {
        sandbox_instance_id_for_workspace(self.workspace_id)
    }

    pub fn uses_workspace_mapped_sandbox_instance(&self) -> bool {
        self.sandbox_instance_id == self.expected_sandbox_instance_id()
    }
}
