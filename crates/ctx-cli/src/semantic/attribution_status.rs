//! Recovery advice follows existing automatic maintenance and writer activity.
use ctx_app_config::AppConfig;
use ctx_attribution_model::{BlameDiagnostic, BlameDiagnosticReason};
use std::path::Path;

pub(crate) fn maintenance_blame_diagnostic(
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
    if !matches!(
        (diagnostic.error_code, diagnostic.reason),
        (
            "needs_rebuild",
            BlameDiagnosticReason::ProjectionIncompatible
        ) | ("stale_source", BlameDiagnosticReason::ProjectionStale)
    ) {
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
        let mut diagnostic = ctx_attribution::rebuilding_after_upgrade(diagnostic, progress);
        if diagnostic.reason == BlameDiagnosticReason::ProjectionStale {
            diagnostic.message =
                "Blame data is catching up with history. Retry when background indexing finishes.";
        }
        return diagnostic;
    }
    diagnostic
}

#[cfg(test)]
#[path = "attribution_status_tests.rs"]
mod tests;
