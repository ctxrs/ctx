use std::collections::HashMap;

use super::{AgentServerCommand, ManagedInstallMetadata};
use crate::expected_managed_dependency_version;
use ctx_provider_install::install_state::InstallTarget;

pub(super) fn install_target_bucket_key(target: InstallTarget) -> &'static str {
    target.as_str()
}

pub(super) fn requested_target_or_host(target: Option<InstallTarget>) -> InstallTarget {
    target.unwrap_or(InstallTarget::Host)
}

pub(super) fn legacy_managed_metadata_matches_target(
    meta: &ManagedInstallMetadata,
    requested_target: Option<InstallTarget>,
) -> bool {
    meta.target.unwrap_or(InstallTarget::Host) == requested_target_or_host(requested_target)
}

pub(super) fn managed_dependency_target_from_id(entry_id: &str) -> Option<InstallTarget> {
    expected_managed_dependency_version(entry_id)?;
    let suffix = entry_id
        .trim()
        .strip_prefix("runtime-node-")
        .or_else(|| entry_id.trim().strip_prefix("runtime-python-"))?;
    match suffix {
        "host" => Some(InstallTarget::Host),
        "container" => Some(InstallTarget::Container),
        "linux-aarch64" => Some(InstallTarget::LinuxAarch64),
        "linux-x86_64" => Some(InstallTarget::LinuxX8664),
        _ => None,
    }
}

pub(super) fn infer_legacy_managed_target(
    entry_id: &str,
    target: Option<InstallTarget>,
) -> InstallTarget {
    target
        .or_else(|| managed_dependency_target_from_id(entry_id))
        .unwrap_or(InstallTarget::Host)
}

pub(super) fn migrate_managed_provider_command_args(
    provider_id: &str,
    command: &mut AgentServerCommand,
) -> bool {
    if command.managed.is_none() {
        return false;
    }
    if provider_id == "kimi" && command.args == ["--acp"] {
        command.args = vec!["acp".to_string()];
        return true;
    }
    false
}

pub(super) fn target_bucket_lookup<'a, T>(
    buckets: &'a HashMap<String, HashMap<String, T>>,
    provider_id: &str,
    requested_target: Option<InstallTarget>,
) -> Option<&'a T> {
    let target_key = install_target_bucket_key(requested_target_or_host(requested_target));
    buckets.get(provider_id)?.get(target_key)
}
