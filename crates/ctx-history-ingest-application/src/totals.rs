use ctx_history_capture_model::{ProviderImportSummary, ProviderImportWorkResult};

use crate::SourceStats;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportIndexDelta {
    pub sessions: i64,
    pub searchable_events: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportIndexFacts {
    pub current_sessions: u64,
    pub delta: Option<ImportIndexDelta>,
}

impl ImportIndexFacts {
    pub fn from_cardinalities(
        current_sessions: u64,
        current_searchable_events: u64,
        previous: Option<(u64, u64)>,
    ) -> Self {
        let delta = previous.and_then(|(previous_sessions, previous_searchable_events)| {
            Some(ImportIndexDelta {
                sessions: signed_count_delta(current_sessions, previous_sessions)?,
                searchable_events: signed_count_delta(
                    current_searchable_events,
                    previous_searchable_events,
                )?,
            })
        });
        Self {
            current_sessions,
            delta,
        }
    }
}

fn signed_count_delta(current: u64, previous: u64) -> Option<i64> {
    if current >= previous {
        i64::try_from(current - previous).ok()
    } else {
        i64::try_from(previous - current).ok()?.checked_neg()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportOutcome {
    Success,
    Failure,
    CompletedWithRejections,
    CompletedWithSourceFailures,
    CompletedWithRejectionsAndSourceFailures,
}

impl ImportOutcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::CompletedWithRejections => "completed_with_rejections",
            Self::CompletedWithSourceFailures => "completed_with_source_failures",
            Self::CompletedWithRejectionsAndSourceFailures => {
                "completed_with_rejections_and_source_failures"
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportFailureScope {
    None,
    Record,
    Source,
    RecordAndSource,
}

impl ImportFailureScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Record => "record",
            Self::Source => "source",
            Self::RecordAndSource => "record_and_source",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportFailureType {
    None,
    RecordRejection,
    SourceFailure,
    RecordRejectionAndSourceFailure,
}

impl ImportFailureType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::RecordRejection => "record_rejection",
            Self::SourceFailure => "source_failure",
            Self::RecordRejectionAndSourceFailure => "record_rejection_and_source_failure",
        }
    }
}

/// Typed outcome totals. `index_delta` is the only explicit Core cardinality
/// delta; imported, skipped, source, and current-generation counters do not
/// imply a per-record or per-run delta.
#[derive(Debug, Clone, Default)]
pub struct ImportTotals {
    pub per_run_counts_available: bool,
    pub terminal_route_counts_available: bool,
    pub source_files: usize,
    pub source_bytes: u64,
    pub imported_sources: usize,
    pub sources_completed_with_rejections: usize,
    pub failed_sources: usize,
    pub imported_sessions: usize,
    pub imported_events: usize,
    pub imported_edges: usize,
    pub skipped_sessions: usize,
    pub skipped_events: usize,
    pub skipped_edges: usize,
    pub skipped: usize,
    pub failed: usize,
    pub current_source_count: Option<usize>,
    pub current_indexed_sessions: Option<u64>,
    pub current_indexed_documents: Option<u64>,
    pub index_delta: Option<ImportIndexDelta>,
    pub current_complete_records: Option<u64>,
    pub current_retained_records: Option<u64>,
    pub current_rejected_records: Option<u64>,
    pub current_ignored_records: Option<u64>,
    pub current_certified_source_bytes: Option<u64>,
    pub current_sources_with_rejections: Option<usize>,
    pub removed_source_count: Option<usize>,
    pub request_records_attempted: Option<bool>,
    pub request_has_usable_records: Option<bool>,
    pub capture_work_remaining: bool,
    pub work_result: ProviderImportWorkResult,
}

impl ImportTotals {
    fn has_usable_current_generation(&self) -> bool {
        self.current_retained_records.map_or_else(
            || self.imported_sessions > 0 || self.imported_events > 0 || self.imported_edges > 0,
            |retained| retained > 0,
        )
    }

    pub fn has_usable_source_result(&self) -> bool {
        self.request_has_usable_records.unwrap_or_else(|| {
            self.current_retained_records.map_or_else(
                || {
                    self.imported_sessions > 0
                        || self.imported_events > 0
                        || self.imported_edges > 0
                },
                |retained| retained > 0,
            )
        })
    }
    pub fn has_attempted_source_records(&self) -> bool {
        self.request_records_attempted.unwrap_or_else(|| {
            self.current_complete_records
                .is_some_and(|records| records > 0)
        }) || self.failed > 0
    }
    pub fn reported_source_failures(&self) -> usize {
        self.failed_sources
    }
    pub fn outcome(&self) -> (ImportOutcome, ImportFailureScope) {
        let has_usable_history = if self.failed_sources > 0 {
            self.has_usable_current_generation()
        } else {
            self.has_usable_source_result()
        };
        if !has_usable_history && (self.failed_sources > 0 || self.has_attempted_source_records()) {
            return (
                ImportOutcome::Failure,
                match (self.failed_sources > 0, self.failed > 0) {
                    (false, true) => ImportFailureScope::Record,
                    (true, false) => ImportFailureScope::Source,
                    (true, true) => ImportFailureScope::RecordAndSource,
                    (false, false) => ImportFailureScope::None,
                },
            );
        }
        match (self.failed_sources > 0, self.failed > 0) {
            (false, false) => (ImportOutcome::Success, ImportFailureScope::None),
            (false, true) => (
                ImportOutcome::CompletedWithRejections,
                ImportFailureScope::Record,
            ),
            (true, false) => (
                ImportOutcome::CompletedWithSourceFailures,
                ImportFailureScope::Source,
            ),
            (true, true) => (
                ImportOutcome::CompletedWithRejectionsAndSourceFailures,
                ImportFailureScope::RecordAndSource,
            ),
        }
    }
    pub fn failure_type(&self) -> ImportFailureType {
        match (self.failed_sources > 0, self.failed > 0) {
            (false, false) => ImportFailureType::None,
            (false, true) => ImportFailureType::RecordRejection,
            (true, false) => ImportFailureType::SourceFailure,
            (true, true) => ImportFailureType::RecordRejectionAndSourceFailure,
        }
    }
    pub fn add(&mut self, summary: &ProviderImportSummary, stats: &SourceStats) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_facts_omit_a_cold_or_unrepresentable_delta() {
        assert_eq!(
            ImportIndexFacts::from_cardinalities(3, 8, None),
            ImportIndexFacts {
                current_sessions: 3,
                delta: None,
            }
        );
        assert_eq!(
            ImportIndexFacts::from_cardinalities(u64::MAX, 8, Some((0, 7))).delta,
            None
        );
    }

    #[test]
    fn index_facts_report_positive_zero_and_negative_net_change() {
        for (current, previous, expected) in [
            ((3, 8), (1, 4), (2, 4)),
            ((3, 8), (3, 8), (0, 0)),
            ((1, 4), (3, 8), (-2, -4)),
        ] {
            let facts = ImportIndexFacts::from_cardinalities(current.0, current.1, Some(previous));
            assert_eq!(
                facts.delta,
                Some(ImportIndexDelta {
                    sessions: expected.0,
                    searchable_events: expected.1,
                })
            );
        }
    }

    #[test]
    fn aggregate_outcomes_preserve_machine_compatibility_and_genuine_failures() {
        for (failed_sources, rejected, outcome, scope, failure_type) in [
            (0, 0, "success", "none", "none"),
            (
                0,
                2,
                "completed_with_rejections",
                "record",
                "record_rejection",
            ),
            (
                2,
                0,
                "completed_with_source_failures",
                "source",
                "source_failure",
            ),
            (
                2,
                3,
                "completed_with_rejections_and_source_failures",
                "record_and_source",
                "record_rejection_and_source_failure",
            ),
        ] {
            let totals = ImportTotals {
                failed_sources,
                failed: rejected,
                current_retained_records: Some(1),
                ..ImportTotals::default()
            };
            let (actual_outcome, actual_scope) = totals.outcome();
            assert_eq!(actual_outcome.as_str(), outcome);
            assert_eq!(actual_scope.as_str(), scope);
            assert_eq!(totals.failure_type().as_str(), failure_type);
        }
        let failed = ImportTotals {
            failed_sources: 1,
            ..ImportTotals::default()
        };
        assert_eq!(failed.outcome().0, ImportOutcome::Failure);

        let all_rejected = ImportTotals {
            failed: 3,
            current_source_count: Some(2),
            current_complete_records: Some(8),
            current_retained_records: Some(5),
            current_rejected_records: Some(3),
            request_records_attempted: Some(true),
            request_has_usable_records: Some(false),
            ..ImportTotals::default()
        };
        assert_eq!(
            all_rejected.outcome(),
            (ImportOutcome::Failure, ImportFailureScope::Record)
        );

        let all_ignored = ImportTotals {
            current_source_count: Some(1),
            current_complete_records: Some(2),
            current_retained_records: Some(0),
            current_ignored_records: Some(2),
            ..ImportTotals::default()
        };
        assert_eq!(
            all_ignored.outcome(),
            (ImportOutcome::Failure, ImportFailureScope::None)
        );
    }
}
