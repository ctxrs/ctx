mod automatic;
mod command;
mod config;
mod diagnostics;
pub(crate) mod ports;

use crate::config::{AppConfig, AutoUpgradeMode};

pub(crate) use automatic::{maybe_spawn_automatic, wait_for_invoking_parent};
pub use command::run;
pub use ctx_cli_presentation::upgrade::UpgradeArgs;
pub(crate) use diagnostics::upgrade_diagnostics;

pub(crate) fn effective_auto_upgrade_mode(config: &AppConfig) -> AutoUpgradeMode {
    if !config.auto_upgrade_enabled() {
        return AutoUpgradeMode::Off;
    }
    match ctx_upgrade_engine::managed_install_marker_for_current_exe() {
        Ok(ctx_upgrade_engine::ManagedInstallMarker::Valid(_)) => AutoUpgradeMode::Apply,
        Ok(ctx_upgrade_engine::ManagedInstallMarker::Absent)
        | Ok(ctx_upgrade_engine::ManagedInstallMarker::Invalid { .. })
        | Err(_) => AutoUpgradeMode::Off,
    }
}

/// Cheap foreground eligibility hint. A structurally usable marker is fully
/// revalidated by the automatic worker or daemon before any upgrade attempt;
/// this path never hashes the binary.
pub(crate) fn automatic_upgrade_eligible_hint(config: &AppConfig) -> bool {
    config.auto_upgrade_enabled()
        && ctx_upgrade_engine::current_exe_has_managed_install_marker_hint()
}
