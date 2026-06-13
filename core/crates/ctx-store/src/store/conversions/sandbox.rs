pub(super) fn sandbox_substrate_to_str(value: &SandboxSubstrate) -> &'static str {
    match value {
        SandboxSubstrate::NativeContainer => "native_container",
        SandboxSubstrate::SharedVmContainer => "shared_vm_container",
    }
}

pub(super) fn parse_sandbox_substrate(value: &str) -> SandboxSubstrate {
    match value {
        "shared_vm_container" => SandboxSubstrate::SharedVmContainer,
        _ => SandboxSubstrate::NativeContainer,
    }
}

pub(super) fn sandbox_guest_platform_to_str(value: SandboxGuestPlatform) -> &'static str {
    match value {
        SandboxGuestPlatform::Linux => "linux",
    }
}

pub(super) fn parse_sandbox_guest_platform(value: &str) -> SandboxGuestPlatform {
    match value {
        "linux" => SandboxGuestPlatform::Linux,
        _ => SandboxGuestPlatform::Linux,
    }
}

pub(super) fn sandbox_isolation_kind_to_str(value: SandboxIsolationKind) -> &'static str {
    match value {
        SandboxIsolationKind::Container => "container",
    }
}

pub(super) fn parse_sandbox_isolation_kind(value: &str) -> SandboxIsolationKind {
    match value {
        "container" => SandboxIsolationKind::Container,
        _ => SandboxIsolationKind::Container,
    }
}

pub(super) fn sandbox_guest_runtime_to_str(value: SandboxGuestRuntime) -> &'static str {
    match value {
        SandboxGuestRuntime::Ubuntu => "ubuntu",
    }
}

pub(super) fn parse_sandbox_guest_runtime(value: &str) -> SandboxGuestRuntime {
    match value {
        "ubuntu" => SandboxGuestRuntime::Ubuntu,
        _ => SandboxGuestRuntime::Ubuntu,
    }
}

pub(super) fn sandbox_profile_to_str(value: &SandboxProfile) -> &'static str {
    match value {
        SandboxProfile::Standard => "standard",
        SandboxProfile::Strict => "strict",
    }
}

pub(super) fn parse_sandbox_profile(value: &str) -> SandboxProfile {
    match value {
        "strict" => SandboxProfile::Strict,
        _ => SandboxProfile::Standard,
    }
}

pub(super) fn map_sandbox_binding(row: SqliteRow) -> Option<SandboxBinding> {
    let worktree_id: String = row.try_get("worktree_id").ok()?;
    let workspace_id: String = row.try_get("workspace_id").ok()?;
    let sandbox_instance_id: String = row.try_get("sandbox_instance_id").ok()?;
    let runtime_family: String = row.try_get("runtime_family").ok()?;
    let guest_platform: String = row.try_get("guest_platform").ok()?;
    let isolation_kind: String = row.try_get("isolation_kind").ok()?;
    let guest_runtime: String = row.try_get("guest_runtime").ok()?;
    let profile: String = row.try_get("profile").ok()?;
    let created_at: String = row.try_get("created_at").ok()?;
    Some(SandboxBinding {
        worktree_id: WorktreeId(uuid::Uuid::parse_str(&worktree_id).ok()?),
        workspace_id: WorkspaceId(uuid::Uuid::parse_str(&workspace_id).ok()?),
        sandbox_instance_id: SandboxInstanceId(uuid::Uuid::parse_str(&sandbox_instance_id).ok()?),
        substrate: parse_sandbox_substrate(&runtime_family),
        guest_identity: SandboxGuestIdentity {
            platform: parse_sandbox_guest_platform(&guest_platform),
            isolation_kind: parse_sandbox_isolation_kind(&isolation_kind),
            runtime: parse_sandbox_guest_runtime(&guest_runtime),
        },
        profile: parse_sandbox_profile(&profile),
        live_workspace_root: row.try_get("live_workspace_root").ok()?,
        live_worktree_root: row.try_get("live_worktree_root").ok()?,
        execution_settings_json: row.try_get("execution_settings_json").ok(),
        container_name: row.try_get("container_name").ok()?,
        host_materialization_root: row.try_get("host_projection_root").ok()?,
        created_at: parse_dt(&created_at).ok()?,
    })
}
