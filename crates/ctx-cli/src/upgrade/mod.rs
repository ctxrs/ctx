mod command;
mod diagnostics;
pub(crate) mod ports;

use ctx_app_config::{AppConfig, AutoUpgradeMode};

pub use command::run;
pub use ctx_cli_presentation::upgrade::UpgradeArgs;
pub(crate) use diagnostics::upgrade_diagnostics;

pub(crate) fn effective_auto_upgrade_mode(config: &AppConfig) -> AutoUpgradeMode {
    let marker = ctx_upgrade_engine::managed_install_marker_for_current_exe().ok();
    let marker = match marker.as_ref() {
        Some(ctx_upgrade_engine::ManagedInstallMarker::Valid(marker)) => Some(marker),
        _ => None,
    };
    effective_auto_upgrade_mode_for_marker(config, marker)
}

fn effective_auto_upgrade_mode_for_marker(
    config: &AppConfig,
    marker: Option<&ctx_upgrade_engine::InstallMarker>,
) -> AutoUpgradeMode {
    if config.auto_upgrade_enabled() && marker.is_some_and(|marker| !marker.staging_dogfood) {
        AutoUpgradeMode::Apply
    } else {
        AutoUpgradeMode::Off
    }
}

/// Cheap daemon-start eligibility hint. A structurally usable marker is fully
/// revalidated by the daemon-owned scheduler before any upgrade attempt; this
/// path never hashes the binary.
pub(crate) fn automatic_upgrade_eligible_hint(config: &AppConfig) -> bool {
    automatic_upgrade_eligible_for_marker_hint(
        config,
        ctx_upgrade_engine::current_exe_has_managed_install_marker_hint(),
        ctx_upgrade_engine::current_exe_is_staging_dogfood(),
    )
}

pub(crate) fn automatic_upgrade_eligible_for_marker_hint(
    config: &AppConfig,
    managed: bool,
    staging_dogfood: bool,
) -> bool {
    config.auto_upgrade_enabled() && managed && !staging_dogfood
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn marker(staging_dogfood: bool) -> ctx_upgrade_engine::InstallMarker {
        ctx_upgrade_engine::InstallMarker {
            install_path: PathBuf::from("ctx"),
            platform: "linux-x64".to_owned(),
            channel: "stable".to_owned(),
            version: "1.0.0".to_owned(),
            sha256: "0".repeat(64),
            staging_dogfood,
        }
    }

    #[test]
    fn status_effective_mode_disables_staging_dogfood_and_keeps_managed_apply() {
        let config = AppConfig::default();
        let ordinary = marker(false);
        let staging = marker(true);

        assert_eq!(
            effective_auto_upgrade_mode_for_marker(&config, Some(&ordinary)),
            AutoUpgradeMode::Apply
        );
        assert_eq!(
            effective_auto_upgrade_mode_for_marker(&config, Some(&staging)),
            AutoUpgradeMode::Off
        );
    }
}
