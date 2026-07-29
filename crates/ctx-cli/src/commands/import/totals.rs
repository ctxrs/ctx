use ctx_history_capture::{ProviderImportSummary, ProviderImportWorkResult};

use super::SourceStats;

#[derive(Debug, Clone, Default)]
pub(crate) struct ImportTotals {
    pub(crate) per_run_counts_available: bool,
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
    pub(crate) current_source_count: Option<usize>,
    pub(crate) current_indexed_documents: Option<u64>,
    pub(crate) current_complete_records: Option<u64>,
    pub(crate) current_retained_records: Option<u64>,
    pub(crate) current_rejected_records: Option<u64>,
    pub(crate) current_ignored_records: Option<u64>,
    pub(crate) current_certified_source_bytes: Option<u64>,
    pub(crate) current_sources_with_rejections: Option<usize>,
    pub(crate) removed_source_count: Option<usize>,
    pub(crate) capture_work_remaining: bool,
    pub(crate) work_result: ProviderImportWorkResult,
}

impl ImportTotals {
    pub(crate) fn add(&mut self, summary: &ProviderImportSummary, stats: &SourceStats) {
        self.per_run_counts_available = true;
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
}
