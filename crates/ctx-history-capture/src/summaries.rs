use serde::{Deserialize, Serialize};

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
        self.imported_events > 0 || self.imported_edges > 0
    }

    pub fn work_result(&self) -> ProviderImportWorkResult {
        if self.imported > 0 || (self.skipped == 0 && self.has_accepted_content()) {
            ProviderImportWorkResult::Changed
        } else {
            ProviderImportWorkResult::NoOp
        }
    }
}
