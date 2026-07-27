use serde::{Deserialize, Serialize};

pub(crate) const MAX_RETAINED_PROVIDER_FAILURES: usize = 64;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderImportSummary {
    pub imported: usize,
    pub skipped: usize,
    pub failed: usize,
    pub imported_sessions: usize,
    pub skipped_sessions: usize,
    pub imported_events: usize,
    pub skipped_events: usize,
    pub imported_edges: usize,
    pub skipped_edges: usize,
    #[serde(skip)]
    pub(crate) accepted_content_records: usize,
    #[serde(skip)]
    pub(crate) work_result: Option<ProviderImportWorkResult>,
    #[serde(skip)]
    pub(crate) terminal_outcome: ProviderImportTerminalOutcome,
    #[serde(skip)]
    pub work_remaining: bool,
    pub failures: Vec<ProviderImportFailure>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ProviderImportWorkResult {
    Changed,
    #[default]
    NoOp,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ProviderImportTerminalOutcome {
    #[default]
    None,
    CoreCursorCommitted,
}

impl ProviderImportWorkResult {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Changed => "changed",
            Self::NoOp => "no_op",
        }
    }

    pub fn merge(self, other: Self) -> Self {
        if self == Self::Changed || other == Self::Changed {
            Self::Changed
        } else {
            Self::NoOp
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderImportFailure {
    pub line: usize,
    pub error: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogSummary {
    pub source_files: usize,
    pub source_bytes: u64,
    pub cataloged_sessions: usize,
    pub cached_sessions: usize,
    pub parsed_sessions: usize,
    pub skipped_sessions: usize,
    pub failed_sessions: usize,
    pub failures: Vec<ProviderImportFailure>,
}

impl ProviderImportSummary {
    pub fn has_accepted_content(&self) -> bool {
        self.accepted_content_records > 0 || self.imported_events > 0 || self.imported_edges > 0
    }

    pub fn work_result(&self) -> ProviderImportWorkResult {
        self.work_result.unwrap_or_else(|| {
            if self.imported > 0 || (self.skipped == 0 && self.has_accepted_content()) {
                ProviderImportWorkResult::Changed
            } else {
                ProviderImportWorkResult::NoOp
            }
        })
    }

    pub fn terminal_outcome(&self) -> ProviderImportTerminalOutcome {
        self.terminal_outcome
    }

    pub(crate) fn set_work_result(&mut self, work_result: ProviderImportWorkResult) {
        self.work_result = Some(work_result);
    }

    pub(crate) fn set_terminal_outcome(&mut self, outcome: ProviderImportTerminalOutcome) {
        self.terminal_outcome = outcome;
    }

    pub fn merge_from(&mut self, other: ProviderImportSummary) {
        let work_result = self.work_result().merge(other.work_result());
        self.imported = self.imported.saturating_add(other.imported);
        self.skipped = self.skipped.saturating_add(other.skipped);
        self.failed = self.failed.saturating_add(other.failed);
        self.imported_sessions = self
            .imported_sessions
            .saturating_add(other.imported_sessions);
        self.skipped_sessions = self.skipped_sessions.saturating_add(other.skipped_sessions);
        self.imported_events = self.imported_events.saturating_add(other.imported_events);
        self.skipped_events = self.skipped_events.saturating_add(other.skipped_events);
        self.imported_edges = self.imported_edges.saturating_add(other.imported_edges);
        self.skipped_edges = self.skipped_edges.saturating_add(other.skipped_edges);
        self.accepted_content_records = self
            .accepted_content_records
            .saturating_add(other.accepted_content_records);
        self.work_remaining |= other.work_remaining;
        if other.terminal_outcome == ProviderImportTerminalOutcome::CoreCursorCommitted {
            self.terminal_outcome = ProviderImportTerminalOutcome::CoreCursorCommitted;
        }
        let remaining = MAX_RETAINED_PROVIDER_FAILURES.saturating_sub(self.failures.len());
        self.failures
            .extend(other.failures.into_iter().take(remaining));
        self.work_result = Some(work_result);
    }

    pub(crate) fn record_failure(&mut self, failure: ProviderImportFailure) {
        self.failed = self.failed.saturating_add(1);
        if self.failures.len() < MAX_RETAINED_PROVIDER_FAILURES {
            self.failures.push(failure);
        }
    }

    pub(crate) fn merge(&mut self, other: ProviderImportSummary) {
        self.merge_from(other);
    }
}

#[cfg(test)]
mod tests {
    use super::ProviderImportSummary;

    #[test]
    fn merge_saturates_all_summary_counters() {
        let mut summary = ProviderImportSummary {
            imported: usize::MAX,
            skipped: usize::MAX,
            failed: usize::MAX,
            imported_sessions: usize::MAX,
            skipped_sessions: usize::MAX,
            imported_events: usize::MAX,
            skipped_events: usize::MAX,
            imported_edges: usize::MAX,
            skipped_edges: usize::MAX,
            accepted_content_records: usize::MAX,
            ..ProviderImportSummary::default()
        };
        let increment = ProviderImportSummary {
            imported: 1,
            skipped: 1,
            failed: 1,
            imported_sessions: 1,
            skipped_sessions: 1,
            imported_events: 1,
            skipped_events: 1,
            imported_edges: 1,
            skipped_edges: 1,
            accepted_content_records: 1,
            ..ProviderImportSummary::default()
        };

        summary.merge_from(increment);

        assert_eq!(summary.imported, usize::MAX);
        assert_eq!(summary.skipped, usize::MAX);
        assert_eq!(summary.failed, usize::MAX);
        assert_eq!(summary.imported_sessions, usize::MAX);
        assert_eq!(summary.skipped_sessions, usize::MAX);
        assert_eq!(summary.imported_events, usize::MAX);
        assert_eq!(summary.skipped_events, usize::MAX);
        assert_eq!(summary.imported_edges, usize::MAX);
        assert_eq!(summary.skipped_edges, usize::MAX);
        assert_eq!(summary.accepted_content_records, usize::MAX);
    }
}
