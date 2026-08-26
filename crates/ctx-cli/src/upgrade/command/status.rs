use std::path::Path;

use anyhow::Result;
use ctx_upgrade_engine::{
    invalid_install_marker_recovery_guidance, managed_install_marker_for_current_exe,
    read_state_json, unmanaged_install_conversion_guidance, ManagedInstallMarker,
    STATE_SCHEMA_VERSION,
};
use serde_json::json;

use crate::{config::AppConfig, ui::Ui};

pub(super) fn render_status(
    _data_root: &Path,
    config: &AppConfig,
    json_output: bool,
    ui: &mut Ui,
) -> Result<()> {
    let state = read_state_json().unwrap_or_else(|| {
        json!({
            "schema_version": STATE_SCHEMA_VERSION,
            "status": "never_checked"
        })
    });
    let current_version = super::super::ports::product_identity().version();
    let marker_result = managed_install_marker_for_current_exe();
    let valid_marker = match &marker_result {
        Ok(ManagedInstallMarker::Valid(marker)) => Some(marker),
        _ => None,
    };
    let state = ctx_cli_presentation::upgrade::reconcile_scheduled_state(state, valid_marker);
    let install = match marker_result {
        Ok(ManagedInstallMarker::Valid(marker)) => json!({
            "managed": true,
            "marker": "valid",
            "install_path": marker.install_path,
            "platform": marker.platform,
            "channel": marker.channel,
            "version": marker.version,
            "sha256": marker.sha256,
        }),
        Ok(ManagedInstallMarker::Absent) => json!({
            "managed": false,
            "marker": "absent",
            "reason": "ctx was not installed by the hosted installer",
            "action": unmanaged_install_conversion_guidance(),
        }),
        Ok(ManagedInstallMarker::Invalid { reason }) => json!({
            "managed": false,
            "marker": "corrupt",
            "reason": reason,
            "action": invalid_install_marker_recovery_guidance(),
        }),
        Err(error) => json!({
            "managed": false,
            "marker": "unavailable",
            "reason": format!("{error:#}"),
        }),
    };
    let auto_mode = super::super::effective_auto_upgrade_mode(config);
    ctx_cli_presentation::upgrade::render_status(
        ctx_cli_presentation::upgrade::UpgradeStatusView {
            current_version,
            auto_upgrade: auto_mode.as_str(),
            auto_enabled: auto_mode.enabled(),
            state: &state,
            install: &install,
        },
        json_output,
        ui,
    )
}
