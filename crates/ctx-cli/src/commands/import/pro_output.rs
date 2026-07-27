use std::path::Path;

use anyhow::{anyhow, Result};

use ctx_history_capture::{CaptureWorkLimit, ProviderImportSummary, ProviderImportWorkResult};
#[cfg(test)]
use ctx_history_store::Store;

use crate::analytics::{ImportTelemetry, ProviderRefreshTrigger};
use crate::progress::{ProgressArg, ProgressReporter};
use crate::ImportArgs;

use super::provider_refresh::ProviderRefreshCollector;
use super::{run_import_internal_with_pro_output, ImportRunOptions, ImportTotals};

pub(super) enum ProOutputSelection {
    Automatic,
    Disabled,
    Connected(crate::pro::ProOutputImport),
}

impl ProOutputSelection {
    pub(super) fn is_automatic(&self) -> bool {
        matches!(self, Self::Automatic)
    }

    pub(super) fn begin(self, data_root: &Path) -> (Option<crate::pro::ProOutputImport>, bool) {
        match self {
            Self::Automatic => (
                crate::pro::ProOutputImport::begin_if_available(data_root),
                false,
            ),
            Self::Disabled => (None, false),
            Self::Connected(output) => (Some(output), true),
        }
    }
}

pub(crate) trait CanonicalProSourceProgression {
    fn progress_to_committed_core_frontier(&mut self);
}

impl CanonicalProSourceProgression for crate::pro::ProOutputImport {
    fn progress_to_committed_core_frontier(&mut self) {
        crate::pro::ProOutputImport::note_core_source_committed(self);
    }
}

/// Advances canonical Pro after a Core source attempt that either reported a
/// committed change or returned an error after possibly committing one or more
/// NativePath pages. A successful no-op cannot have advanced the journal and is
/// skipped. Output-Pro remains independently replayable.
pub(crate) fn progress_canonical_pro_after_core_source_attempt<
    P: CanonicalProSourceProgression + ?Sized,
>(
    pro_output: Option<&mut P>,
    successful_summary: Option<&ProviderImportSummary>,
) {
    if successful_summary
        .is_some_and(|summary| summary.work_result() != ProviderImportWorkResult::Changed)
    {
        return;
    }
    if let Some(pro_output) = pro_output {
        pro_output.progress_to_committed_core_frontier();
    }
}

/// Runs custom-history NativePath imports one committed Core group at a time.
/// Foreground callers still drain the source, while background callers retain
/// their one-group scheduling bound. Bounded attempts also give terminal
/// upstream-cursor commits an independent bulk-search lifecycle.
///
/// Each bounded attempt advances canonical Pro after Core returns, including
/// errors that may follow a durable Core commit. Successful no-ops skip the
/// frontier check. The progression contract is intentionally best-effort, so
/// neither canonical-Pro nor output-Pro failure can replace the Core result.
pub(crate) fn import_custom_history_with_canonical_pro_progression<
    P: CanonicalProSourceProgression + ?Sized,
    F,
>(
    requested_work_limit: CaptureWorkLimit,
    pro_output: Option<&mut P>,
    mut import_attempt: F,
) -> Result<ProviderImportSummary>
where
    F: FnMut(CaptureWorkLimit) -> Result<ProviderImportSummary>,
{
    let attempt_work_limit = CaptureWorkLimit::OneSafeGroup;
    let drain_bounded_attempts = requested_work_limit == CaptureWorkLimit::Drain;
    let mut pro_output = pro_output;
    let mut aggregate = None::<ProviderImportSummary>;

    loop {
        let result = import_attempt(attempt_work_limit);
        progress_canonical_pro_after_core_source_attempt(
            pro_output.as_deref_mut(),
            result.as_ref().ok(),
        );
        let mut summary = result?;
        let work_remaining = summary.work_remaining;
        if let Some(aggregate) = aggregate.as_mut() {
            // Validation failures describe the complete input and repeat on
            // every bounded parse. Keep the latest snapshot while merging
            // only page-local Core accounting.
            let failed = summary.failed;
            let failures = std::mem::take(&mut summary.failures);
            summary.failed = 0;
            aggregate.work_remaining = false;
            aggregate.merge_from(summary);
            aggregate.failed = failed;
            aggregate.failures = failures;
            aggregate.work_remaining = work_remaining;
        } else {
            aggregate = Some(summary);
        }
        if !drain_bounded_attempts || !work_remaining {
            break;
        }
    }

    Ok(aggregate.unwrap_or_default())
}

pub(crate) fn prepare_core_for_pro_materialization(data_root: &Path) -> Result<()> {
    run_pro_materialization_import(data_root, ProOutputSelection::Disabled)
}

pub(crate) fn catch_up_pro_outputs(
    data_root: &Path,
    output: crate::pro::ProOutputImport,
) -> Result<()> {
    run_pro_materialization_import(data_root, ProOutputSelection::Connected(output))
}

fn run_pro_materialization_import(
    data_root: &Path,
    pro_output_selection: ProOutputSelection,
) -> Result<()> {
    let args = ImportArgs {
        provider: None,
        path: None,
        history_source: None,
        history_source_manifest: Vec::new(),
        reset_cursor: false,
        input_format: None,
        all: true,
        resume: false,
        partial: false,
        no_daemon: true,
        format: crate::output::JsonOutputFormat::Text,
        progress: ProgressArg::None,
    };
    let config = crate::config::AppConfig::load(data_root)?;
    let mut telemetry = ImportTelemetry::from_args(&args);
    let mut provider_refreshes = ProviderRefreshCollector::default();
    run_import_internal_with_pro_output(
        &args,
        data_root.to_path_buf(),
        &mut telemetry,
        &mut provider_refreshes,
        ProviderRefreshTrigger::Setup,
        &config,
        ImportRunOptions {
            progress: ProgressArg::None,
            json: false,
            print_human: false,
            allow_empty_sources: true,
            include_history_source_plugins: false,
            operation: "pro-materialization",
        },
        pro_output_selection,
    )
    .map(|_| ())
}

pub(crate) fn output_inventory_can_finish(discovery_complete: bool, totals: &ImportTotals) -> bool {
    discovery_complete && totals.failed_sources == 0 && !totals.capture_work_remaining
}

pub(crate) fn finish_pro_output_inventory(
    output: Option<crate::pro::ProOutputImport>,
    progress: &ProgressReporter,
) {
    let Some(output) = output else {
        return;
    };
    if let Err(error) = output.finish() {
        let warning = crate::pro::ProOutputImport::finish_warning(&error);
        if progress.is_enabled() {
            progress.warning(&warning);
        } else {
            eprintln!("warning: {warning}");
        }
    }
}

pub(super) fn complete_pro_output_inventory(
    output: Option<crate::pro::ProOutputImport>,
    progress: &ProgressReporter,
    required: bool,
) -> Result<()> {
    if !required {
        finish_pro_output_inventory(output, progress);
        return Ok(());
    }
    let output = output.ok_or_else(|| {
        anyhow!("not_materialized: connected Pro output materialization session is missing")
    })?;
    output.finish().map(|_| ())
}

#[cfg(test)]
mod pro_output_inventory_tests {
    use super::*;
    use std::io::Cursor;

    use ctx_history_capture::{
        import_custom_history_jsonl_v1_reader, CustomHistoryJsonlV1ImportOptions,
    };
    use serde_json::json;
    use tempfile::tempdir;

    #[derive(Default)]
    struct TestCanonicalProProgression {
        frontier_checks: usize,
        fail: bool,
        behind: bool,
    }

    impl CanonicalProSourceProgression for TestCanonicalProProgression {
        fn progress_to_committed_core_frontier(&mut self) {
            self.frontier_checks += 1;
            if self.fail {
                self.behind = true;
            }
        }
    }

    #[test]
    fn empty_full_inventory_can_finish() {
        assert!(output_inventory_can_finish(true, &ImportTotals::default()));
    }

    #[test]
    fn failed_or_incomplete_inventory_cannot_finish() {
        let failed = ImportTotals {
            failed_sources: 1,
            ..ImportTotals::default()
        };
        assert!(!output_inventory_can_finish(true, &failed));

        let incomplete = ImportTotals {
            capture_work_remaining: true,
            ..ImportTotals::default()
        };
        assert!(!output_inventory_can_finish(true, &incomplete));
        assert!(!output_inventory_can_finish(
            false,
            &ImportTotals::default()
        ));
    }

    #[test]
    fn explicit_import_progresses_after_changed_or_failed_core_attempts() {
        let mut changed = ProviderImportSummary::default();
        changed.imported = 1;
        let no_op = ProviderImportSummary::default();
        let mut progression = TestCanonicalProProgression::default();

        progress_canonical_pro_after_core_source_attempt(Some(&mut progression), Some(&changed));
        progress_canonical_pro_after_core_source_attempt(Some(&mut progression), Some(&no_op));
        progress_canonical_pro_after_core_source_attempt(Some(&mut progression), None);

        assert_eq!(progression.frontier_checks, 2);
    }

    #[test]
    fn custom_foreground_import_progresses_each_bounded_core_page() {
        let mut progression = TestCanonicalProProgression::default();
        let mut attempts = 0_usize;
        let mut work_limits = Vec::new();

        let summary = import_custom_history_with_canonical_pro_progression(
            CaptureWorkLimit::Drain,
            Some(&mut progression),
            |work_limit| {
                work_limits.push(work_limit);
                attempts += 1;
                let mut summary = ProviderImportSummary::default();
                summary.imported = 1;
                summary.work_remaining = attempts == 1;
                Ok(summary)
            },
        )
        .unwrap();

        assert_eq!(attempts, 2);
        assert_eq!(
            work_limits,
            vec![
                CaptureWorkLimit::OneSafeGroup,
                CaptureWorkLimit::OneSafeGroup
            ]
        );
        assert_eq!(progression.frontier_checks, 2);
        assert_eq!(summary.imported, 2);
        assert!(!summary.work_remaining);
    }

    #[test]
    fn custom_nativepath_multi_page_import_progresses_after_each_core_page() {
        let temp = tempdir().unwrap();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let mut records = vec![
            json!({
                "record_type": "manifest",
                "schema_version": "ctx-history-jsonl-v1",
            })
            .to_string(),
            json!({
                "record_type": "source",
                "source_id": "multi-page",
                "provider_key": "multi-page-agent",
                "source_format": "multi-page-v1",
            })
            .to_string(),
            json!({
                "record_type": "session",
                "source_id": "multi-page",
                "session_id": "session",
                "started_at": "2026-07-25T12:00:00Z",
            })
            .to_string(),
        ];
        records.extend((0_u64..130).map(|event_index| {
            json!({
                "record_type": "event",
                "source_id": "multi-page",
                "session_id": "session",
                "event_index": event_index,
                "event_type": "message",
                "role": "assistant",
                "occurred_at": "2026-07-25T12:00:01Z",
                "payload": {"text": format!("event {event_index}")},
            })
            .to_string()
        }));
        let input = records.join("\n");
        let options = CustomHistoryJsonlV1ImportOptions {
            source_path: Some(temp.path().join("multi-page.jsonl")),
            ..CustomHistoryJsonlV1ImportOptions::default()
        };
        let mut progression = TestCanonicalProProgression::default();

        let summary = import_custom_history_with_canonical_pro_progression(
            CaptureWorkLimit::Drain,
            Some(&mut progression),
            |capture_work_limit| {
                import_custom_history_jsonl_v1_reader(
                    Cursor::new(input.as_bytes()),
                    &mut store,
                    CustomHistoryJsonlV1ImportOptions {
                        capture_work_limit,
                        ..options.clone()
                    },
                )
                .map_err(anyhow::Error::from)
            },
        )
        .unwrap();

        assert_eq!(summary.imported_sessions, 1);
        assert_eq!(summary.imported_events, 130);
        assert!(progression.frontier_checks > 1);
        assert!(!summary.work_remaining);
    }

    #[test]
    fn custom_import_no_op_skips_canonical_pro_progression() {
        let mut progression = TestCanonicalProProgression::default();

        let summary = import_custom_history_with_canonical_pro_progression(
            CaptureWorkLimit::Drain,
            Some(&mut progression),
            |work_limit| {
                assert_eq!(work_limit, CaptureWorkLimit::OneSafeGroup);
                Ok(ProviderImportSummary::default())
            },
        )
        .unwrap();

        assert_eq!(summary.work_result(), ProviderImportWorkResult::NoOp);
        assert_eq!(progression.frontier_checks, 0);
    }

    #[test]
    fn custom_import_preserves_core_success_when_pro_progression_fails() {
        let mut progression = TestCanonicalProProgression {
            fail: true,
            ..TestCanonicalProProgression::default()
        };

        let summary = import_custom_history_with_canonical_pro_progression(
            CaptureWorkLimit::Drain,
            Some(&mut progression),
            |_| {
                let mut summary = ProviderImportSummary::default();
                summary.imported = 1;
                Ok(summary)
            },
        )
        .unwrap();

        assert_eq!(summary.imported, 1);
        assert_eq!(progression.frontier_checks, 1);
        assert!(progression.behind);
    }

    #[test]
    fn custom_import_checks_canonical_frontier_after_failed_core_attempt() {
        let mut progression = TestCanonicalProProgression::default();

        let error = import_custom_history_with_canonical_pro_progression(
            CaptureWorkLimit::Drain,
            Some(&mut progression),
            |_| Err(anyhow!("injected post-commit failure")),
        )
        .unwrap_err();

        assert_eq!(error.to_string(), "injected post-commit failure");
        assert_eq!(progression.frontier_checks, 1);
    }
}
