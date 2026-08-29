use std::path::Path;

use anyhow::{bail, Context, Result};
use ctx_history_refresh::{RefreshOutcomeCode, RefreshSelection};

use crate::{
    progress::ProgressReporter,
    semantic::{
        complete_import_semantic, coordinate_import_source_backed_refresh_with_progress,
        ImportSemanticCompletion, SourceBackedRefreshMode, SourceBackedRefreshObservation,
    },
};

/// Applies import-specific policy around the one Core refresh control path.
///
/// Import may start the daemon and waits only for authoritative Core publication.
pub(super) fn wait_for_import_core_refresh(
    data_root: &Path,
    no_daemon: bool,
    selection: RefreshSelection,
    semantic_completion: &ImportSemanticCompletion,
    progress: &mut ProgressReporter<'_>,
) -> Result<SourceBackedRefreshObservation> {
    let mut deferred_terminal_core_success = None;
    let mut report_progress = |update: &crate::semantic::RefreshStatus| {
        if import_rerenders_terminal_missing_path(update)? {
            return Ok(());
        }
        if semantic_completion.is_enabled() && import_terminal_core_success(update)? {
            // Core is durable at this point, but import owns the final terminal
            // progress frame until semantic completion has reached its own
            // terminal outcome.
            deferred_terminal_core_success = Some(update.clone());
            return Ok(());
        }
        progress.source_refresh(update).map_err(anyhow::Error::new)
    };
    let mut refresh = coordinate_import_source_backed_refresh_with_progress(
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
    if semantic_completion.is_enabled() {
        progress
            .message("semantic", "Reconciling semantic search.")
            .map_err(anyhow::Error::new)?;
    }
    refresh.pin = match complete_import_semantic(semantic_completion, data_root, refresh.pin) {
        Ok(pin) => pin,
        Err(error) => {
            progress
                .failure("semantic", error.to_string())
                .map_err(anyhow::Error::new)?;
            return Err(error.into());
        }
    };
    if let Some(status) = deferred_terminal_core_success {
        progress
            .source_refresh(&status)
            .map_err(anyhow::Error::new)?;
    }
    Ok(refresh)
}

fn import_terminal_core_success(status: &crate::semantic::RefreshStatus) -> Result<bool> {
    Ok(status
        .kind()?
        .terminal_outcome()
        .is_some_and(|outcome| !outcome.code.is_failure()))
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
        let successful = code.starts_with("completed");
        let state = if successful { "published" } else { "failed" };
        let phase = if successful { "published" } else { "failed" };
        let whole_run_stage = if successful { "complete" } else { "failed" };
        crate::semantic::RefreshStatus::parse_schema_v1(json!({
            "request_id": "logical-request",
            "request_state": state,
            "logical_request_id": "logical-request",
            "logical_phase": "terminal",
            "physical_attempt_id": "physical-attempt",
            "physical_attempt_state": state,
            "progress_owner_request_id": "physical-attempt",
            "progress_owner_attempt_state": state,
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
                "phase": phase,
                "completed_sources": 0,
                "total_sources": 1,
                "total_sources_known": true
            },
            "whole_run_stage": whole_run_stage
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
    fn semantic_completion_defers_only_a_successful_core_terminal_frame() {
        assert!(import_terminal_core_success(&terminal_status("completed", "completed")).unwrap());
        assert!(import_terminal_core_success(&terminal_status(
            "completed_with_rejections",
            "completed"
        ))
        .unwrap());
        assert!(!import_terminal_core_success(&terminal_status(
            "source_unavailable",
            "unavailable"
        ))
        .unwrap());
    }

    #[test]
    fn semantic_phase_is_announced_after_the_exact_core_gate_and_before_completion() {
        let source = include_str!("core_refresh.rs");
        let exact_core_gate = source
            .find("Core refresh receipt names generation")
            .expect("Core receipt/pin equality gate");
        let semantic_phase = source
            .find(".message(\"semantic\", \"Reconciling semantic search.\")")
            .expect("nonterminal semantic phase");
        let completion = source
            .find("complete_import_semantic(semantic_completion")
            .expect("semantic completion");

        assert!(exact_core_gate < semantic_phase);
        assert!(semantic_phase < completion);
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
