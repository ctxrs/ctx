use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const MAX_PROVIDER_IMPORT_FAILURE_SAMPLES: usize = 32;
const MAX_PROVIDER_IMPORT_MAINTENANCE_SAMPLES: usize = 8;

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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub maintenance_warnings: Vec<ProviderImportMaintenanceWarning>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderImportFailure {
    pub line: usize,
    pub error: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderImportMaintenanceKind {
    WalCheckpoint,
    TransactionContinuation,
    ImportInterruptedAfterCommit,
    EventSearchFinalization,
    EventSearchFinalizationPending,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderImportMaintenanceWarning {
    pub kind: ProviderImportMaintenanceKind,
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

    pub fn requires_maintenance(&self) -> bool {
        !self.maintenance_warnings.is_empty()
    }

    pub(crate) fn has_checkpoint_blocking_maintenance(&self) -> bool {
        self.maintenance_warnings.iter().any(|warning| {
            warning.kind != ProviderImportMaintenanceKind::EventSearchFinalizationPending
        })
    }

    pub(crate) fn push_maintenance_warning(
        &mut self,
        kind: ProviderImportMaintenanceKind,
        error: impl Into<String>,
    ) {
        if self.maintenance_warnings.len() < MAX_PROVIDER_IMPORT_MAINTENANCE_SAMPLES {
            self.maintenance_warnings
                .push(ProviderImportMaintenanceWarning {
                    kind,
                    error: error.into(),
                });
        }
    }

    pub(crate) fn sample_failure(&mut self, failure: ProviderImportFailure) {
        if self.failures.len() < MAX_PROVIDER_IMPORT_FAILURE_SAMPLES {
            self.failures.push(failure);
        }
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
        let remaining_failures =
            MAX_PROVIDER_IMPORT_FAILURE_SAMPLES.saturating_sub(self.failures.len());
        self.failures
            .extend(other.failures.into_iter().take(remaining_failures));
        let remaining_warnings =
            MAX_PROVIDER_IMPORT_MAINTENANCE_SAMPLES.saturating_sub(self.maintenance_warnings.len());
        self.maintenance_warnings.extend(
            other
                .maintenance_warnings
                .into_iter()
                .take(remaining_warnings),
        );
    }

    pub(crate) fn merge(&mut self, other: ProviderImportSummary) {
        self.merge_from(other);
    }
}

impl CatalogSummary {
    pub(crate) fn sample_failure(&mut self, failure: ProviderImportFailure) {
        if self.failures.len() < MAX_PROVIDER_IMPORT_FAILURE_SAMPLES {
            self.failures.push(failure);
        }
    }

    pub(crate) fn merge_from(&mut self, other: CatalogSummary) {
        self.source_files = self.source_files.saturating_add(other.source_files);
        self.source_bytes = self.source_bytes.saturating_add(other.source_bytes);
        self.cataloged_sessions = self
            .cataloged_sessions
            .saturating_add(other.cataloged_sessions);
        self.cached_sessions = self.cached_sessions.saturating_add(other.cached_sessions);
        self.parsed_sessions = self.parsed_sessions.saturating_add(other.parsed_sessions);
        self.skipped_sessions = self.skipped_sessions.saturating_add(other.skipped_sessions);
        self.failed_sessions = self.failed_sessions.saturating_add(other.failed_sessions);
        let remaining = MAX_PROVIDER_IMPORT_FAILURE_SAMPLES.saturating_sub(self.failures.len());
        self.failures
            .extend(other.failures.into_iter().take(remaining));
    }
}

impl SpoolImportSummary {
    pub(crate) fn sample_failure(&mut self, failure: SpoolImportFailure) {
        if self.failures.len() < MAX_PROVIDER_IMPORT_FAILURE_SAMPLES {
            self.failures.push(failure);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CatalogSummary, ProviderImportFailure, ProviderImportSummary, SpoolImportFailure,
        SpoolImportSummary, MAX_PROVIDER_IMPORT_FAILURE_SAMPLES,
    };
    use std::path::PathBuf;

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

    #[test]
    fn failure_totals_are_exact_while_samples_are_bounded_and_deterministic() {
        let mut summary = ProviderImportSummary::default();
        for line in 0..(MAX_PROVIDER_IMPORT_FAILURE_SAMPLES * 4) {
            let mut batch = ProviderImportSummary {
                failed: 1,
                ..ProviderImportSummary::default()
            };
            batch.sample_failure(ProviderImportFailure {
                line,
                error: format!("failure-{line}"),
            });
            summary.merge_from(batch);
        }

        assert_eq!(summary.failed, MAX_PROVIDER_IMPORT_FAILURE_SAMPLES * 4);
        assert_eq!(summary.failures.len(), MAX_PROVIDER_IMPORT_FAILURE_SAMPLES);
        assert_eq!(summary.failures.first().unwrap().line, 0);
        assert_eq!(
            summary.failures.last().unwrap().line,
            MAX_PROVIDER_IMPORT_FAILURE_SAMPLES - 1
        );
    }

    #[test]
    fn catalog_and_spool_failure_samples_share_the_same_bound() {
        let mut catalog = CatalogSummary::default();
        let mut spool = SpoolImportSummary::default();
        for index in 0..(MAX_PROVIDER_IMPORT_FAILURE_SAMPLES * 3) {
            let mut catalog_batch = CatalogSummary {
                failed_sessions: 1,
                ..CatalogSummary::default()
            };
            catalog_batch.sample_failure(ProviderImportFailure {
                line: index,
                error: format!("catalog-{index}"),
            });
            catalog.merge_from(catalog_batch);
            spool.failed_files += 1;
            spool.sample_failure(SpoolImportFailure {
                path: PathBuf::from(format!("spool-{index}")),
                error: format!("spool-{index}"),
            });
        }

        assert_eq!(
            catalog.failed_sessions,
            MAX_PROVIDER_IMPORT_FAILURE_SAMPLES * 3
        );
        assert_eq!(catalog.failures.len(), MAX_PROVIDER_IMPORT_FAILURE_SAMPLES);
        assert_eq!(spool.failed_files, MAX_PROVIDER_IMPORT_FAILURE_SAMPLES * 3);
        assert_eq!(spool.failures.len(), MAX_PROVIDER_IMPORT_FAILURE_SAMPLES);
        assert_eq!(
            catalog.failures.last().unwrap().line,
            MAX_PROVIDER_IMPORT_FAILURE_SAMPLES - 1
        );
        assert_eq!(
            spool.failures.last().unwrap().path,
            PathBuf::from(format!("spool-{}", MAX_PROVIDER_IMPORT_FAILURE_SAMPLES - 1))
        );
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
