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
    pub work_remaining: bool,
    pub failures: Vec<ProviderImportFailure>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ProviderImportWorkResult {
    Changed,
    #[default]
    NoOp,
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

    pub(crate) fn set_work_result(&mut self, work_result: ProviderImportWorkResult) {
        self.work_result = Some(work_result);
    }

    pub fn merge_from(&mut self, other: ProviderImportSummary) {
        let work_result = self.work_result().merge(other.work_result());
        self.imported += other.imported;
        self.skipped += other.skipped;
        self.failed += other.failed;
        self.imported_sessions += other.imported_sessions;
        self.skipped_sessions += other.skipped_sessions;
        self.imported_events += other.imported_events;
        self.skipped_events += other.skipped_events;
        self.imported_edges += other.imported_edges;
        self.skipped_edges += other.skipped_edges;
        self.accepted_content_records += other.accepted_content_records;
        self.work_remaining |= other.work_remaining;
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
