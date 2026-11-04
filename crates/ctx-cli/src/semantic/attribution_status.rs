//! Recovery advice follows existing automatic maintenance and writer activity.
use ctx_app_config::AppConfig;
use ctx_attribution_model::{BlameDiagnostic, BlameDiagnosticReason};
use std::path::Path;

pub(crate) fn upgrade_blame_diagnostic(
    data_root: &Path,
    diagnostic: BlameDiagnostic,
) -> BlameDiagnostic {
    if diagnostic.error_code != "needs_rebuild"
        || diagnostic.reason != BlameDiagnosticReason::ProjectionIncompatible
    {
        return diagnostic;
    }
    let Ok(config) = AppConfig::load(data_root) else {
        return diagnostic;
    };
    if config.automatic_indexing_enabled() && config.blame.enabled {
        let progress = ctx_attribution::materialization_progress(data_root)
            .ok()
            .flatten();
        // Startup and publication schedule the existing worker. A stopped daemon
        // without a live writer cannot promise automatic completion.
        let running = progress.is_some()
            || ctx_daemon_cli::source_epoch_status_report(
                data_root,
                &super::daemon_cli_config(&config),
            )
            .is_ok_and(|source| source.report["daemon"]["running"] == true);
        if running {
            return ctx_attribution::rebuilding_after_upgrade(diagnostic, progress);
        }
    }
    diagnostic
}

#[cfg(test)]
#[path = "attribution_status_tests.rs"]
mod tests;
