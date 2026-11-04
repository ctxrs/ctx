//! Recovery advice follows existing automatic maintenance and writer activity.
use ctx_app_config::AppConfig;
use ctx_attribution_model::{BlameDiagnostic, BlameDiagnosticReason};
use std::path::Path;

pub(crate) fn upgrade_blame_diagnostic(
    data_root: &Path,
    diagnostic: BlameDiagnostic,
) -> BlameDiagnostic {
    let maintenance = ctx_daemon_cli::attribution_maintenance_status(data_root);
    with_maintenance(data_root, diagnostic, maintenance.as_ref())
}

pub(super) fn with_maintenance(
    data_root: &Path,
    diagnostic: BlameDiagnostic,
    maintenance: Option<&ctx_daemon_cli::AttributionMaintenanceStatus>,
) -> BlameDiagnostic {
    if diagnostic.error_code != "needs_rebuild"
        || diagnostic.reason != BlameDiagnosticReason::ProjectionIncompatible
    {
        return diagnostic;
    }
    let Ok(config) = AppConfig::load(data_root) else {
        return diagnostic;
    };
    if config.automatic_indexing_enabled()
        && config.blame.enabled
        && matches!(
            maintenance,
            Some(ctx_daemon_cli::AttributionMaintenanceStatus::Running)
        )
    {
        let progress = ctx_attribution::materialization_progress(data_root)
            .ok()
            .flatten();
        return ctx_attribution::rebuilding_after_upgrade(diagnostic, progress);
    }
    diagnostic
}

#[cfg(test)]
#[path = "attribution_status_tests.rs"]
mod tests;
