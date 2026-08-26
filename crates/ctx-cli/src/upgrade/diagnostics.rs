use crate::config::AppConfig;

pub(crate) fn upgrade_diagnostics(
    config: &AppConfig,
) -> ctx_cli_presentation::upgrade::UpgradeDiagnostics {
    let mode = super::effective_auto_upgrade_mode(config);
    let diagnostics = ctx_upgrade_engine::upgrade_diagnostics();
    ctx_cli_presentation::upgrade::present_upgrade_diagnostics(
        mode.as_str(),
        mode.enabled(),
        diagnostics,
    )
}
