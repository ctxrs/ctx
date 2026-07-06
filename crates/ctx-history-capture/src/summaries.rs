use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpoolCounts {
    pub pending: usize,
    pub tmp: usize,
    pub processing: usize,
    pub done: usize,
    pub failed: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpoolImportFailure {
    pub path: PathBuf,
    pub error: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpoolImportSummary {
    pub processed_files: usize,
    pub skipped_files: usize,
    pub imported_records: usize,
    pub failed_files: usize,
    pub failures: Vec<SpoolImportFailure>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderImportSummary {
    pub imported: usize,
    pub skipped: usize,
    pub failed: usize,
    pub redacted: usize,
    pub imported_sessions: usize,
    pub skipped_sessions: usize,
    pub imported_events: usize,
    pub skipped_events: usize,
    pub imported_edges: usize,
    pub skipped_edges: usize,
    pub failures: Vec<ProviderImportFailure>,
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
}

impl ProviderImportSummary {
    pub(crate) fn merge(&mut self, other: ProviderImportSummary) {
        self.imported += other.imported;
        self.skipped += other.skipped;
        self.failed += other.failed;
        self.redacted += other.redacted;
        self.imported_sessions += other.imported_sessions;
        self.skipped_sessions += other.skipped_sessions;
        self.imported_events += other.imported_events;
        self.skipped_events += other.skipped_events;
        self.imported_edges += other.imported_edges;
        self.skipped_edges += other.skipped_edges;
        self.failures.extend(other.failures);
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpoolRepairSummary {
    pub retried_files: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ArchiveCounts {
    pub(crate) records: usize,
}

impl ArchiveCounts {
    pub(crate) fn add(&mut self, other: Self) {
        self.records += other.records;
    }
}
