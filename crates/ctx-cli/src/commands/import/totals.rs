use ctx_history_capture::{ProviderImportSummary, ProviderImportWorkResult};

use super::SourceStats;

#[derive(Debug, Clone, Default)]
pub(crate) struct ImportTotals {
    pub(crate) source_files: usize,
    pub(crate) source_bytes: u64,
    pub(crate) imported_sources: usize,
    pub(crate) sources_completed_with_rejections: usize,
    pub(crate) failed_sources: usize,
    pub(crate) imported_sessions: usize,
    pub(crate) imported_events: usize,
    pub(crate) imported_edges: usize,
    pub(crate) skipped_sessions: usize,
    pub(crate) skipped_events: usize,
    pub(crate) skipped_edges: usize,
    pub(crate) skipped: usize,
    pub(crate) failed: usize,
    pub(crate) capture_work_remaining: bool,
    pub(crate) work_result: ProviderImportWorkResult,
}

impl ImportTotals {
    pub(crate) fn add(&mut self, summary: &ProviderImportSummary, stats: &SourceStats) {
        self.source_files += stats.files;
        self.source_bytes = self.source_bytes.saturating_add(stats.bytes);
        self.imported_sources += 1;
        self.sources_completed_with_rejections += usize::from(summary.failed > 0);
        self.imported_sessions += summary.imported_sessions;
        self.imported_events += summary.imported_events;
        self.imported_edges += summary.imported_edges;
        self.skipped_sessions += summary.skipped_sessions;
        self.skipped_events += summary.skipped_events;
        self.skipped_edges += summary.skipped_edges;
        self.skipped += summary.skipped;
        self.failed += summary.failed;
        self.capture_work_remaining |= summary.work_remaining;
        self.work_result = self.work_result.merge(summary.work_result());
    }

    pub(crate) fn add_source_failure(&mut self, stats: &SourceStats) {
        self.source_files += stats.files;
        self.source_bytes = self.source_bytes.saturating_add(stats.bytes);
        self.failed_sources += 1;
    }

    pub(crate) fn add_rejected_source(
        &mut self,
        summary: &ProviderImportSummary,
        stats: &SourceStats,
    ) {
        self.add_source_failure(stats);
        self.skipped_sessions = self
            .skipped_sessions
            .saturating_add(summary.skipped_sessions);
        self.skipped_events = self.skipped_events.saturating_add(summary.skipped_events);
        self.skipped_edges = self.skipped_edges.saturating_add(summary.skipped_edges);
        self.skipped = self.skipped.saturating_add(summary.skipped);
        self.failed = self.failed.saturating_add(summary.failed);
        self.capture_work_remaining |= summary.work_remaining;
        self.work_result = self.work_result.merge(summary.work_result());
    }
}
