use super::*;

pub(super) fn build_probe() -> Result<AvfLinuxHelperProbe> {
    let identity = current_build_identity()?;
    let mut notes = Vec::new();
    let save_restore_supported = shared_vm_save_restore_supported();
    if cfg!(target_os = "macos") {
        if let Some(version) = macos_product_version() {
            notes.push(format!("host macOS {version}"));
        }
        notes.push(
            "shared AVF Linux VM backend is enabled for managed guest artifact prefetch"
                .to_string(),
        );
    } else {
        notes.push("AVF Linux helper is only usable on macOS hosts".to_string());
    }
    if cfg!(target_arch = "aarch64") {
        notes.push(
            "Apple silicon host detected; the host satisfies AVF save/restore prerequisites, but each VM configuration still needs runtime-time validation".to_string(),
        );
        notes.push(
            "probe save/restore support is scoped to host prerequisites only; actual restore success or failure is reported by the shared VM lifecycle outcomes".to_string(),
        );
    } else {
        notes.push(
            "Intel Mac host detected; save/restore is expected to remain unavailable".to_string(),
        );
    }

    Ok(AvfLinuxHelperProbe {
        protocol_version: HELPER_PROTOCOL_VERSION,
        protocol_schema: HELPER_PROTOCOL_SCHEMA,
        helper_version: identity.exact_version.clone(),
        exact_version: identity.exact_version,
        build_id: identity.build_id,
        compatibility_token: identity.compatibility_token,
        host_os: std::env::consts::OS,
        host_arch: std::env::consts::ARCH,
        supported: cfg!(target_os = "macos"),
        save_restore_supported,
        save_restore_capability_scope: if save_restore_supported {
            AvfLinuxSaveRestoreCapabilityScope::HostPrerequisitesOnly
        } else {
            AvfLinuxSaveRestoreCapabilityScope::Unsupported
        },
        rosetta_supported: cfg!(all(target_os = "macos", target_arch = "aarch64")),
        notes,
    })
}

pub(super) fn shared_vm_save_restore_supported() -> bool {
    cfg!(all(target_os = "macos", target_arch = "aarch64")) && macos_major_version_at_least(14)
}

pub(super) fn macos_major_version_at_least(required_major: u64) -> bool {
    let Some(version) = macos_product_version() else {
        return false;
    };
    let Some(major) = version
        .split('.')
        .next()
        .and_then(|segment| segment.parse::<u64>().ok())
    else {
        return false;
    };
    major >= required_major
}

#[cfg(target_os = "macos")]
pub(super) fn file_url_for_path(path: &Path) -> Retained<NSURL> {
    NSURL::fileURLWithPath(&NSString::from_str(path.to_string_lossy().as_ref()))
}

#[cfg(target_os = "macos")]
pub(super) fn format_nserror(error: &NSError) -> String {
    format!(
        "{} (domain: {}, code: {})",
        error.localizedDescription(),
        error.domain(),
        error.code()
    )
}

pub(super) fn macos_product_version() -> Option<String> {
    let output = Command::new("sw_vers")
        .arg("-productVersion")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let version = String::from_utf8(output.stdout).ok()?;
    let trimmed = version.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}
