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
    pub imported_sessions: usize,
    pub skipped_sessions: usize,
    pub imported_events: usize,
    pub skipped_events: usize,
    pub imported_edges: usize,
    pub skipped_edges: usize,
    #[serde(skip)]
    pub(crate) accepted_content_records: usize,
    #[serde(skip)]
    retained_existing_content: bool,
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
    pub failures: Vec<ProviderImportFailure>,
}

impl ProviderImportSummary {
    pub fn has_accepted_content(&self) -> bool {
        self.accepted_content_records > 0
            || self.imported_events > 0
            || self.imported_edges > 0
            || self.retained_existing_content
    }

    pub fn mark_retained_existing_content(&mut self) {
        self.retained_existing_content = true;
    }

    pub fn merge_from(&mut self, other: ProviderImportSummary) {
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
        self.retained_existing_content |= other.retained_existing_content;
        self.failures.extend(other.failures);
    }

    pub(crate) fn merge(&mut self, other: ProviderImportSummary) {
        self.merge_from(other);
    }
}

#[cfg(test)]
mod tests {
    use super::ProviderImportSummary;

    #[test]
    fn retained_existing_content_changes_outcome_without_synthesizing_counts() {
        let mut summary = ProviderImportSummary {
            failed: 1,
            ..ProviderImportSummary::default()
        };
        summary.mark_retained_existing_content();

        assert!(summary.has_accepted_content());
        assert_eq!(summary.accepted_content_records, 0);
        assert_eq!(summary.imported_sessions, 0);
        assert_eq!(summary.imported_events, 0);
        assert_eq!(summary.imported_edges, 0);

        let mut merged = ProviderImportSummary::default();
        merged.merge_from(summary);
        assert!(merged.has_accepted_content());
        assert_eq!(merged.accepted_content_records, 0);
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
