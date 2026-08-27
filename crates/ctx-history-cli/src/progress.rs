//! Neutral refresh-progress conversion and terminal reporting.

use ctx_history_core::CaptureProvider;
use ctx_history_refresh::{
    RefreshLogicalPhase as EngineLogicalPhase, RefreshRequestState as EngineRequestState,
    RefreshStatus, RefreshStatusKind as EngineStatusKind,
    SourceBackedCurrentSourceProgress as EngineCurrentSourceProgress,
    SourceBackedCurrentSourceProgressStage as EngineCurrentSourceProgressStage,
    SourceBackedRefreshStage as EngineWholeRunStage,
};
use ctx_terminal::{
    RefreshCurrentSourceProgress, RefreshCurrentSourceProgressStage, RefreshLogicalPhase,
    RefreshLogicalStatus, RefreshProgress, RefreshProgressSnapshot, RefreshRequestState,
    RefreshStatusKind, RefreshStructuredOutcome, RefreshWholeRunStage, Ui,
};

pub use ctx_terminal::{format_bytes, format_count, ProgressWriterError};

use crate::ProgressMode;

/// Converts validated engine refresh status into the terminal crate's neutral
/// snapshot before output is rendered.
pub struct ProgressReporter<'a>(ctx_terminal::ProgressReporter<'a>);

impl<'a> ProgressReporter<'a> {
    pub fn new(
        ui: &'a mut Ui,
        mode: ProgressMode,
        json_output: bool,
        operation: &'static str,
        total_bytes: u64,
    ) -> Self {
        Self(ctx_terminal::ProgressReporter::new(
            ui,
            match mode {
                ProgressMode::Auto => ctx_terminal::ProgressMode::Auto,
                ProgressMode::Plain => ctx_terminal::ProgressMode::Plain,
                ProgressMode::Json => ctx_terminal::ProgressMode::Json,
                ProgressMode::None => ctx_terminal::ProgressMode::None,
            },
            json_output,
            operation,
            total_bytes,
        ))
    }

    pub fn new_with_live_json_stderr(
        ui: &'a mut Ui,
        mode: ProgressMode,
        json_output: bool,
        operation: &'static str,
        total_bytes: u64,
        allow_live_json_stderr: bool,
    ) -> Self {
        Self(ctx_terminal::ProgressReporter::new_with_live_json_stderr(
            ui,
            match mode {
                ProgressMode::Auto => ctx_terminal::ProgressMode::Auto,
                ProgressMode::Plain => ctx_terminal::ProgressMode::Plain,
                ProgressMode::Json => ctx_terminal::ProgressMode::Json,
                ProgressMode::None => ctx_terminal::ProgressMode::None,
            },
            json_output,
            operation,
            total_bytes,
            allow_live_json_stderr,
        ))
    }

    pub fn message(
        &mut self,
        phase: &'static str,
        message: impl Into<String>,
    ) -> Result<(), ProgressWriterError> {
        self.0.message(phase, message)
    }

    pub fn failure(
        &mut self,
        phase: &'static str,
        message: impl Into<String>,
    ) -> Result<(), ProgressWriterError> {
        self.0.failure(phase, message)
    }

    pub fn notice(
        &mut self,
        phase: &'static str,
        lines: &[&str],
    ) -> Result<(), ProgressWriterError> {
        self.0.notice(phase, lines)
    }

    pub fn is_enabled(&self) -> bool {
        self.0.is_enabled()
    }

    pub fn source_refresh(&mut self, status: &RefreshStatus) -> Result<(), ProgressWriterError> {
        let snapshot = presentation_snapshot(status).map_err(|error| {
            ProgressWriterError::from(std::io::Error::new(std::io::ErrorKind::InvalidData, error))
        })?;
        self.0.source_refresh(snapshot)
    }

    pub fn source_refresh_with_published_index(
        &mut self,
        status: &RefreshStatus,
        index: &ctx_history_index::VerifiedIndex,
    ) -> anyhow::Result<()> {
        let mut snapshot = presentation_snapshot(status)?;
        snapshot.set_terminal_history_totals(
            index.session_count()?,
            index.event_type_count("message")?,
            index.event_type_count("tool_call")?,
            index.manifest().certified_source_bytes,
        );
        self.0.source_refresh(snapshot).map_err(anyhow::Error::new)
    }
}

pub fn presentation_snapshot(status: &RefreshStatus) -> anyhow::Result<RefreshProgressSnapshot> {
    let kind = match status.kind()? {
        EngineStatusKind::Legacy { request_state } => RefreshStatusKind::Legacy {
            request_state: presentation_request_state(request_state),
        },
        EngineStatusKind::BackgroundMaintenanceWake(_) => {
            RefreshStatusKind::BackgroundMaintenanceWake
        }
        EngineStatusKind::Logical(logical) => RefreshStatusKind::Logical(RefreshLogicalStatus {
            request_state: presentation_request_state(logical.request_state),
            logical_phase: presentation_logical_phase(logical.logical_phase),
            physical_attempt_id: logical.physical_attempt_id,
            physical_attempt_state: presentation_request_state(logical.physical_attempt_state),
            progress_owner_request_id: logical.progress_owner_request_id,
            progress_owner_attempt_state: presentation_request_state(
                logical.progress_owner_attempt_state,
            ),
            structured_outcome: logical.structured_outcome.map(|outcome| {
                Box::new(RefreshStructuredOutcome {
                    code: outcome.code.as_str().to_owned(),
                    class: outcome.class.as_str().to_owned(),
                    retryable: outcome.retryable,
                    affected_routes: outcome
                        .affected_routes
                        .iter()
                        .map(|route| route.as_str().to_owned())
                        .collect(),
                    retryable_routes: outcome
                        .retryable_routes
                        .iter()
                        .map(|route| route.as_str().to_owned())
                        .collect(),
                    blocked_routes: outcome
                        .blocked_routes
                        .iter()
                        .map(|route| route.as_str().to_owned())
                        .collect(),
                    physical_attempt_id: outcome.physical_attempt_id,
                    retained_generation: outcome.retained_generation,
                    published_generation: outcome.published_generation,
                    retry_advice: outcome
                        .retry_advice
                        .map(|advice| advice.as_str().to_owned()),
                    detail: outcome.detail,
                    failure: outcome.code.is_failure(),
                })
            }),
        }),
    };
    let progress = status.progress()?;
    let whole_run_stage = presentation_whole_run_stage(status.whole_run_stage()?);
    let estimated_remaining_millis = status.estimated_remaining_millis()?;
    Ok(RefreshProgressSnapshot::new(
        status.request_id().map(ToOwned::to_owned),
        kind,
        RefreshProgress {
            phase: progress.phase,
            completed_sources: progress.completed_sources as u64,
            total_sources: progress.total_sources as u64,
            current_source: progress.current_source,
            completed_records: progress.completed_records,
            completed_bytes: progress.completed_bytes,
            agent_histories: progress
                .providers
                .iter()
                .map(|provider| provider_display_name(provider))
                .collect(),
            processed_sessions: progress.processed_sessions,
            processed_messages: progress.processed_messages,
            processed_tool_calls: progress.processed_tool_calls,
            processed_bytes: progress.processed_bytes,
            elapsed_millis: progress.elapsed_millis,
            whole_run_stage,
            estimated_remaining_millis,
            current_source_progress: progress
                .current_source_progress
                .map(presentation_current_source_progress),
        },
        status.total_sources_known()?,
    ))
}

fn presentation_whole_run_stage(value: EngineWholeRunStage) -> RefreshWholeRunStage {
    match value {
        EngineWholeRunStage::Preparing => RefreshWholeRunStage::Preparing,
        EngineWholeRunStage::Reading => RefreshWholeRunStage::Reading,
        EngineWholeRunStage::Merging => RefreshWholeRunStage::Merging,
        EngineWholeRunStage::Syncing => RefreshWholeRunStage::Syncing,
        EngineWholeRunStage::PhysicalVerification => RefreshWholeRunStage::PhysicalVerification,
        EngineWholeRunStage::LogicalVerification => RefreshWholeRunStage::LogicalVerification,
        EngineWholeRunStage::Activation => RefreshWholeRunStage::Activation,
        EngineWholeRunStage::Complete => RefreshWholeRunStage::Complete,
        EngineWholeRunStage::Failed => RefreshWholeRunStage::Failed,
    }
}

pub fn provider_display_name(provider: &str) -> String {
    provider.parse::<CaptureProvider>().map_or_else(
        |_| provider.replace('_', " "),
        |provider| provider.display_name().to_owned(),
    )
}

fn presentation_request_state(value: EngineRequestState) -> RefreshRequestState {
    match value {
        EngineRequestState::AdmissionPending => RefreshRequestState::AdmissionPending,
        EngineRequestState::Queued => RefreshRequestState::Queued,
        EngineRequestState::Running => RefreshRequestState::Running,
        EngineRequestState::Published => RefreshRequestState::Published,
        EngineRequestState::Failed => RefreshRequestState::Failed,
    }
}

fn presentation_logical_phase(value: EngineLogicalPhase) -> RefreshLogicalPhase {
    match value {
        EngineLogicalPhase::Waiting => RefreshLogicalPhase::Waiting,
        EngineLogicalPhase::Attached => RefreshLogicalPhase::Attached,
        EngineLogicalPhase::CoverageCheck => RefreshLogicalPhase::CoverageCheck,
        EngineLogicalPhase::ExactSuccessor => RefreshLogicalPhase::ExactSuccessor,
        EngineLogicalPhase::Direct => RefreshLogicalPhase::Direct,
        EngineLogicalPhase::Terminal => RefreshLogicalPhase::Terminal,
    }
}

fn presentation_current_source_progress(
    value: EngineCurrentSourceProgress,
) -> RefreshCurrentSourceProgress {
    RefreshCurrentSourceProgress {
        stage: match value.stage {
            EngineCurrentSourceProgressStage::SourceFamilyCopy => {
                RefreshCurrentSourceProgressStage::SourceFamilyCopy
            }
            EngineCurrentSourceProgressStage::OnlineBackup => {
                RefreshCurrentSourceProgressStage::OnlineBackup
            }
            EngineCurrentSourceProgressStage::LogicalFingerprint => {
                RefreshCurrentSourceProgressStage::LogicalFingerprint
            }
            EngineCurrentSourceProgressStage::LogicalScan => {
                RefreshCurrentSourceProgressStage::LogicalScan
            }
            EngineCurrentSourceProgressStage::Parsing => RefreshCurrentSourceProgressStage::Parsing,
            EngineCurrentSourceProgressStage::IndexWriting => {
                RefreshCurrentSourceProgressStage::IndexWriting
            }
        },
        snapshot_pages_completed: value.snapshot_pages_completed,
        snapshot_pages_total: value.snapshot_pages_total,
        snapshot_bytes_completed: value.snapshot_bytes_completed,
        snapshot_bytes_total: value.snapshot_bytes_total,
        logical_rows_scanned: value.logical_rows_scanned,
        logical_certified_bytes: value.logical_certified_bytes,
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{self, Write},
        sync::{Arc, Mutex},
    };

    use serde_json::json;

    use super::*;
    use ctx_terminal::{RenderContext, StreamKind, TestContext};

    #[derive(Clone, Default)]
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl SharedWriter {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    impl Write for SharedWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn typed_status(progress: serde_json::Value) -> RefreshStatus {
        RefreshStatus::parse_schema_v1(json!({
            "request_id": "logical-request",
            "request_state": "running",
            "logical_request_id": "logical-request",
            "logical_phase": "exact_successor",
            "physical_attempt_id": "published-predecessor",
            "physical_attempt_state": "published",
            "progress_owner_request_id": "published-predecessor",
            "progress_owner_attempt_state": "published",
            "progress": progress,
        }))
        .unwrap()
    }

    #[test]
    fn logical_request_state_remains_authoritative_over_published_attempt() {
        let snapshot = presentation_snapshot(&typed_status(json!({
            "phase": "committed",
            "completed_sources": 2,
            "total_sources": 2,
        })))
        .unwrap();

        assert_eq!(
            snapshot.kind().request_state(),
            RefreshRequestState::Running
        );
        assert!(!snapshot.is_terminal());
        assert_eq!(snapshot.phase(), "committed");
    }

    #[test]
    fn provider_names_use_products_and_preserve_unknown_fallbacks() {
        for (provider, expected) in [
            ("claude", "Claude Code"),
            ("gemini", "Gemini"),
            ("copilot_cli", "GitHub Copilot"),
            ("kiro_cli", "Kiro"),
            ("kimi_code_cli", "Kimi Code"),
            ("custom_provider", "custom provider"),
        ] {
            assert_eq!(provider_display_name(provider), expected);
        }
    }

    #[test]
    fn legacy_nonzero_total_without_known_field_remains_known() {
        let snapshot = presentation_snapshot(&typed_status(json!({
            "phase": "refreshing",
            "completed_sources": 1,
            "total_sources": 2,
        })))
        .unwrap();

        assert!(snapshot.total_sources_known());
    }

    #[test]
    fn typed_adapter_drops_additive_current_source_progress_fields() {
        let status = typed_status(json!({
            "phase": "copying",
            "completed_sources": 1,
            "total_sources": 2,
            "total_sources_known": true,
            "current_source": "/history.sqlite",
            "completed_records": 8,
            "completed_bytes": 256,
            "current_source_progress": {
                "stage": "online_backup",
                "snapshot_pages_completed": 2,
                "snapshot_pages_total": 4,
                "snapshot_bytes_completed": 256,
                "snapshot_bytes_total": 512,
                "future_additive_field": "must-not-leak"
            }
        }));
        let stdout = SharedWriter::default();
        let stderr = SharedWriter::default();
        let stderr_capture = stderr.clone();
        let mut ui = Ui::with_writers(
            stdout,
            RenderContext::for_test(TestContext::pipe(StreamKind::Stdout)),
            stderr,
            RenderContext::for_test(TestContext::pipe(StreamKind::Stderr)),
        );

        ProgressReporter::new(&mut ui, ProgressMode::Json, false, "import", 0)
            .source_refresh(&status)
            .unwrap();

        let event: serde_json::Value = serde_json::from_str(stderr_capture.text().trim()).unwrap();
        assert_eq!(
            event["current_source_progress"],
            json!({
                "stage": "online_backup",
                "snapshot_pages_completed": 2,
                "snapshot_pages_total": 4,
                "snapshot_bytes_completed": 256,
                "snapshot_bytes_total": 512,
            })
        );
    }

    #[test]
    fn typed_adapter_carries_every_whole_run_stage_and_unknown_eta() {
        for expected in [
            RefreshWholeRunStage::Preparing,
            RefreshWholeRunStage::Reading,
            RefreshWholeRunStage::Merging,
            RefreshWholeRunStage::Syncing,
            RefreshWholeRunStage::PhysicalVerification,
            RefreshWholeRunStage::LogicalVerification,
            RefreshWholeRunStage::Activation,
            RefreshWholeRunStage::Complete,
            RefreshWholeRunStage::Failed,
        ] {
            let snapshot = presentation_snapshot(&typed_status(json!({
                "phase": "refreshing",
                "whole_run_stage": expected.as_str(),
                "estimated_remaining_millis": null,
                "completed_sources": 0,
                "total_sources": 0,
            })))
            .unwrap();

            assert_eq!(snapshot.whole_run_stage(), expected);
            assert_eq!(snapshot.estimated_remaining_millis(), None);
        }

        let numeric = presentation_snapshot(&typed_status(json!({
            "phase": "refreshing",
            "whole_run_stage": "reading",
            "estimated_remaining_millis": 1_234,
            "completed_sources": 0,
            "total_sources": 0,
        })))
        .unwrap();
        assert_eq!(numeric.estimated_remaining_millis(), Some(1_234));
    }
}
