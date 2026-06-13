use super::*;

use ctx_core::ids::SandboxInstanceId;
use ctx_sandbox_contract::SANDBOX_BINDING_EXECUTION_SETTINGS_SCHEMA_V1;
use ctx_settings_model::{ContainerMountMode, ContainerNetworkMode, ContainerRuntimeKind};

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
    assert_eq!(
        parsed.container.image,
        Some("registry.example/sandbox:v1".to_string())
    );
}

#[test]
fn binding_snapshot_versioned_payload_is_supported() {
    let raw = serde_json::json!({
        "schema_version": SANDBOX_BINDING_EXECUTION_SETTINGS_SCHEMA_V1,
        "execution_settings": {
            "mode": "sandbox",
            "container": {
                "runtime": "native_container",
                "mount_mode": "disk_isolated",
                "network_mode": "llm_only",
                "allowlist": [],
                "image": null
            }
        }
    });
    let binding = test_binding(SandboxSubstrate::NativeContainer, Some(raw.to_string()));

    let parsed = sandbox_execution_settings_from_binding(&binding).expect("parse binding");

    assert_eq!(parsed.mode, ExecutionMode::Sandbox);
    assert_eq!(
        parsed.container.runtime,
        ContainerRuntimeKind::NativeContainer
    );
    assert_eq!(
        parsed.container.mount_mode,
        ContainerMountMode::DiskIsolated
    );
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

    assert!(format!("{err:#}").contains("sandbox binding execution settings snapshot runtime shared_vm_container does not match binding substrate native_container"));
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
