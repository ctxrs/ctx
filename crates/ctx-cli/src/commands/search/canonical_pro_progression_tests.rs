use super::*;
use crate::commands::import::import_custom_history_with_canonical_pro_progression;

#[derive(Default)]
struct TestCanonicalProProgression {
    frontier_checks: usize,
}

impl CanonicalProSourceProgression for TestCanonicalProProgression {
    fn progress_to_committed_core_frontier(&mut self) {
        self.frontier_checks += 1;
    }
}

#[test]
fn search_refresh_progresses_after_changed_or_failed_core_attempts() {
    let mut changed = ProviderImportSummary::default();
    changed.imported = 1;
    let no_op = ProviderImportSummary::default();
    let mut progression = TestCanonicalProProgression::default();

    progress_search_refresh_canonical_pro(Some(&mut progression), Some(&changed));
    progress_search_refresh_canonical_pro(Some(&mut progression), Some(&no_op));
    progress_search_refresh_canonical_pro(Some(&mut progression), None);

    assert_eq!(progression.frontier_checks, 2);
}

#[test]
fn search_wait_plugin_refresh_drains_with_per_page_pro_progression() {
    let mut progression = TestCanonicalProProgression::default();
    let mut attempts = 0_usize;

    let summary = import_custom_history_with_canonical_pro_progression(
        history_source_plugin_work_limit(RefreshArg::Wait),
        Some(&mut progression),
        |work_limit| {
            assert_eq!(work_limit, CaptureWorkLimit::OneSafeGroup);
            attempts += 1;
            let mut summary = ProviderImportSummary::default();
            summary.imported = 1;
            summary.work_remaining = attempts == 1;
            Ok(summary)
        },
    )
    .unwrap();

    assert_eq!(attempts, 2);
    assert_eq!(progression.frontier_checks, 2);
    assert!(!summary.work_remaining);
}

#[test]
fn background_plugin_refresh_commits_one_page_for_daemon_followup() {
    let mut progression = TestCanonicalProProgression::default();
    let mut attempts = 0_usize;

    let summary = import_custom_history_with_canonical_pro_progression(
        history_source_plugin_work_limit(RefreshArg::Background),
        Some(&mut progression),
        |work_limit| {
            assert_eq!(work_limit, CaptureWorkLimit::OneSafeGroup);
            attempts += 1;
            let mut summary = ProviderImportSummary::default();
            summary.imported = 1;
            summary.work_remaining = true;
            Ok(summary)
        },
    )
    .unwrap();

    assert_eq!(attempts, 1);
    assert_eq!(progression.frontier_checks, 1);
    assert!(summary.work_remaining);
}
