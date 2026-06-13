use anyhow::Context;
use ctx_core::models::SandboxBinding;
use serde::Deserialize;
use serde_json::Value;

use crate::{
    normalize_container_execution_settings, ContainerMountMode, ContainerRuntimeKind,
    ExecutionMode, ExecutionSettings, UbuntuSandboxSubstrate,
};

pub const SANDBOX_BINDING_EXECUTION_SETTINGS_SCHEMA_V1: i64 = 1;

#[derive(Debug, Deserialize)]
struct VersionedSandboxBindingExecutionSettings {
    schema_version: i64,
    execution_settings: Value,
}

pub fn sandbox_execution_settings_from_binding(
    binding: &SandboxBinding,
) -> anyhow::Result<ExecutionSettings> {
    if !binding.uses_workspace_mapped_sandbox_instance() {
        let expected = ctx_core::models::sandbox_instance_id_for_workspace(binding.workspace_id);
        return Err(anyhow::anyhow!(
            "sandbox binding {} maps workspace {} to unsupported sandbox_instance_id {}; expected {}",
            binding.worktree_id.0,
            binding.workspace_id.0,
            binding.sandbox_instance_id.0,
            expected.0
        ));
    }
    let substrate = UbuntuSandboxSubstrate::from_binding(binding)?;
    if let Some(raw) = binding.execution_settings_json.as_deref() {
        return parse_sandbox_binding_execution_settings(raw)
            .and_then(|settings| validate_sandbox_binding_execution_settings(binding, settings))
            .context("parsing sandbox binding execution settings");
    }

    let mut settings = ExecutionSettings {
        mode: ExecutionMode::Sandbox,
        ..ExecutionSettings::default()
    };
    settings.container.runtime = substrate.runtime_kind();
    settings.container.mount_mode = ContainerMountMode::DiskIsolated;
    Ok(settings)
}

fn validate_sandbox_binding_execution_settings(
    binding: &SandboxBinding,
    settings: ExecutionSettings,
) -> anyhow::Result<ExecutionSettings> {
    if !matches!(settings.mode, ExecutionMode::Sandbox) {
        return Err(anyhow::anyhow!(
            "sandbox binding execution settings snapshot must keep mode=sandbox"
        ));
    }

    let expected_runtime = UbuntuSandboxSubstrate::from_binding(binding)?.runtime_kind();
    if settings.container.runtime != expected_runtime {
        let observed = match settings.container.runtime {
            ContainerRuntimeKind::NativeContainer => "native_container",
            ContainerRuntimeKind::SharedVmContainer => "shared_vm_container",
        };
        let expected = match expected_runtime {
            ContainerRuntimeKind::NativeContainer => "native_container",
            ContainerRuntimeKind::SharedVmContainer => "shared_vm_container",
        };
        return Err(anyhow::anyhow!(
            "sandbox binding execution settings snapshot runtime {observed} does not match binding substrate {expected}"
        ));
    }

    Ok(settings)
}

fn parse_sandbox_binding_execution_settings(raw: &str) -> anyhow::Result<ExecutionSettings> {
    let value: Value =
        serde_json::from_str(raw).context("parsing sandbox binding execution settings JSON")?;
    parse_sandbox_binding_execution_settings_value(value)
}

fn parse_sandbox_binding_execution_settings_value(
    value: Value,
) -> anyhow::Result<ExecutionSettings> {
    match value {
        Value::Object(map) if map.contains_key("schema_version") => {
            let versioned: VersionedSandboxBindingExecutionSettings =
                serde_json::from_value(Value::Object(map))
                    .context("parsing versioned sandbox binding execution settings")?;
            match versioned.schema_version {
                SANDBOX_BINDING_EXECUTION_SETTINGS_SCHEMA_V1 => {
                    parse_and_normalize_execution_settings(versioned.execution_settings)
                }
                other => Err(anyhow::anyhow!(
                    "unsupported sandbox binding execution settings schema version {other}"
                )),
            }
        }
        Value::Object(map) => parse_and_normalize_execution_settings(Value::Object(map)),
        other => Err(anyhow::anyhow!(
            "sandbox binding execution settings snapshot must be a JSON object, found {other}"
        )),
    }
}

fn parse_and_normalize_execution_settings(value: Value) -> anyhow::Result<ExecutionSettings> {
    let mut settings: ExecutionSettings =
        serde_json::from_value(value).context("parsing execution settings payload")?;
    normalize_container_execution_settings(&mut settings.container);
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use ctx_core::ids::{SandboxInstanceId, WorkspaceId, WorktreeId};
    use ctx_core::models::{SandboxGuestIdentity, SandboxProfile, SandboxSubstrate};
    use uuid::Uuid;

    use super::*;
    use crate::ContainerNetworkMode;

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

    #[test]
    fn binding_snapshot_plain_json_is_normalized() {
        let raw = serde_json::json!({
            "mode": "sandbox",
            "container": {
                "runtime": "shared_vm_container",
                "mount_mode": "legacy",
                "network_mode": "allowlist",
                "allowlist": ["github.com"],
                "image": "registry.example/sandbox:v1"
            }
        });
        let binding = test_binding(SandboxSubstrate::SharedVmContainer, Some(raw.to_string()));

        let parsed = sandbox_execution_settings_from_binding(&binding).expect("parse binding");

        assert_eq!(parsed.mode, ExecutionMode::Sandbox);
        assert_eq!(
            parsed.container.runtime,
            ContainerRuntimeKind::SharedVmContainer
        );
        assert_eq!(
            parsed.container.mount_mode,
            ContainerMountMode::DiskIsolated
        );
        assert_eq!(
            parsed.container.network_mode,
            ContainerNetworkMode::Allowlist
        );
        assert_eq!(parsed.container.allowlist, vec!["github.com".to_string()]);
    }

    #[test]
    fn binding_snapshot_rejects_unknown_schema_version() {
        let raw = serde_json::json!({
            "schema_version": 99,
            "execution_settings": {
                "mode": "sandbox",
                "container": {
                    "runtime": "shared_vm_container",
                    "mount_mode": "disk_isolated"
                }
            }
        });
        let binding = test_binding(SandboxSubstrate::SharedVmContainer, Some(raw.to_string()));

        let err = sandbox_execution_settings_from_binding(&binding)
            .expect_err("unknown schema version should fail");

        assert!(format!("{err:#}")
            .contains("unsupported sandbox binding execution settings schema version 99"));
    }

    #[test]
    fn binding_snapshot_rejects_host_mode() {
        let raw = serde_json::json!({
            "mode": "host",
            "container": {
                "runtime": "native_container",
                "mount_mode": "disk_isolated",
                "network_mode": "all",
                "allowlist": [],
                "image": null
            }
        });
        let binding = test_binding(SandboxSubstrate::NativeContainer, Some(raw.to_string()));

        let err = sandbox_execution_settings_from_binding(&binding)
            .expect_err("host-mode binding snapshot should fail closed");

        assert!(format!("{err:#}")
            .contains("sandbox binding execution settings snapshot must keep mode=sandbox"));
    }

    #[test]
    fn binding_snapshot_rejects_substrate_mismatch() {
        let raw = serde_json::json!({
            "mode": "sandbox",
            "container": {
                "runtime": "shared_vm_container",
                "mount_mode": "disk_isolated",
                "network_mode": "all",
                "allowlist": [],
                "image": null
            }
        });
        let binding = test_binding(SandboxSubstrate::NativeContainer, Some(raw.to_string()));

        let err = sandbox_execution_settings_from_binding(&binding)
            .expect_err("runtime-family mismatch should fail closed");

        assert!(format!("{err:#}")
            .contains("sandbox binding execution settings snapshot runtime shared_vm_container does not match binding substrate native_container"));
    }

    #[test]
    fn binding_snapshot_rejects_non_workspace_mapped_sandbox_instance() {
        let raw = serde_json::json!({
            "mode": "sandbox",
            "container": {
                "runtime": "native_container",
                "mount_mode": "disk_isolated",
                "network_mode": "all",
                "allowlist": [],
                "image": null
            }
        });
        let mut binding = test_binding(SandboxSubstrate::NativeContainer, Some(raw.to_string()));
        binding.sandbox_instance_id = SandboxInstanceId(Uuid::new_v4());

        let err = sandbox_execution_settings_from_binding(&binding)
            .expect_err("non-workspace-mapped sandbox instance should fail closed");

        assert!(format!("{err:#}").contains("unsupported sandbox_instance_id"));
    }
}
