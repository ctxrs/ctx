use std::path::Path;

use anyhow::{bail, Context, Result};
use ctx_history_refresh::{RefreshOutcomeCode, RefreshSelection};

use crate::{
    progress::ProgressReporter,
    semantic::{
        coordinate_import_source_backed_refresh_with_progress, SourceBackedRefreshMode,
        SourceBackedRefreshObservation,
    },
};

/// Applies import-specific policy around the one Core refresh control path.
///
/// Import may start the daemon and waits only for authoritative Core publication.
pub(super) fn wait_for_import_core_refresh(
    data_root: &Path,
    no_daemon: bool,
    selection: RefreshSelection,
    progress: &mut ProgressReporter<'_>,
) -> Result<SourceBackedRefreshObservation> {
    let mut report_progress = |update: &crate::semantic::RefreshStatus| {
        if import_rerenders_terminal_missing_path(update)? {
            return Ok(());
        }
        progress.source_refresh(update).map_err(anyhow::Error::new)
    };
    let refresh = coordinate_import_source_backed_refresh_with_progress(
        data_root,
        SourceBackedRefreshMode::Wait,
        selection,
        !no_daemon,
        &mut report_progress,
    )
    .context("publish provider inputs through the Core refresh engine")?;

    let receipt = refresh
        .receipt
        .as_ref()
        .context("Core refresh completed without an authoritative publication receipt")?;
    if refresh.pin.generation_id() != receipt.published_generation {
        bail!(
            "Core refresh receipt names generation {}, but the verified publication pin carries {}",
            receipt.published_generation,
            refresh.pin.generation_id()
        );
    }
    Ok(refresh)
}

/// The import application turns this one Core terminal outcome into its
/// path-aware diagnostic. Keep that final host as the sole terminal reporter.
fn import_rerenders_terminal_missing_path(status: &crate::semantic::RefreshStatus) -> Result<bool> {
    Ok(status
        .kind()?
        .terminal_outcome()
        .is_some_and(|outcome| outcome.code == RefreshOutcomeCode::ExplicitSourcePathMissing))
}

pub(super) fn is_terminal_missing_import_path(error: &anyhow::Error) -> bool {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<crate::semantic::SourceBackedRefreshTerminalError>())
        .and_then(|terminal| terminal.code.parse::<RefreshOutcomeCode>().ok())
        == Some(RefreshOutcomeCode::ExplicitSourcePathMissing)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn terminal_status(code: &str, class: &str) -> crate::semantic::RefreshStatus {
        crate::semantic::RefreshStatus::parse_schema_v1(json!({
            "request_id": "logical-request",
            "request_state": "failed",
            "logical_request_id": "logical-request",
            "logical_phase": "terminal",
            "physical_attempt_id": "physical-attempt",
            "physical_attempt_state": "failed",
            "progress_owner_request_id": "physical-attempt",
            "progress_owner_attempt_state": "failed",
            "structured_outcome": {
                "code": code,
                "class": class,
                "retryable": true,
                "affected_routes": [],
                "retryable_routes": [],
                "blocked_routes": [],
                "physical_attempt_id": "physical-attempt",
                "retry_advice": "inspect_sources",
                "detail": "content-bearing raw terminal detail"
            },
            "progress": {
                "phase": "failed",
                "completed_sources": 0,
                "total_sources": 1,
                "total_sources_known": true
            },
            "whole_run_stage": "failed"
        }))
        .unwrap()
    }

    #[test]
    fn import_claims_only_the_terminal_failure_it_renders_with_the_requested_path() {
        assert!(import_rerenders_terminal_missing_path(&terminal_status(
            "explicit_source_path_missing",
            "unavailable"
        ))
        .unwrap());
        assert!(!import_rerenders_terminal_missing_path(&terminal_status(
            "source_unavailable",
            "unavailable"
        ))
        .unwrap());
    }

    #[test]
    fn import_control_contains_no_ingestion_provider_read_or_sidecar_implementation() {
        let source = include_str!("core_refresh.rs");
        for forbidden in [
            ["ctx_history_", "capture"].concat(),
            ["ImportCore", "RefreshRequest"].concat(),
            ["SourceBackedRefresh", "Selector"].concat(),
            ["SourceBackedRefresh", "Executor"].concat(),
            ["VerifiedIndex", "::open"].concat(),
            ["Store", "::open"].concat(),
        ] {
            assert!(
                !source.contains(&forbidden),
                "import Core control contains forbidden foreground implementation `{forbidden}`"
            );
        }
    }
}
