use std::{path::PathBuf, time::Duration};

use ctx_history_capture_model::{ProviderImportSummary, ProviderSource};
use ctx_history_core::CaptureProvider;
use ctx_history_refresh::{ExplicitSourceCatalogAuthority, SourceBackedRefreshCurrent};

use crate::{HistorySourcePluginSource, ImportIndexFacts, ImportTotals, SourceStats};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestStatus {
    Published,
    Partial,
    Failure,
    Rejection,
}

impl IngestStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Published => "published",
            Self::Partial => "partial",
            Self::Failure => "failure",
            Self::Rejection => "rejection",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestFailureScope {
    None,
    Record,
    Source,
    RecordAndSource,
}

impl IngestFailureScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Record => "record",
            Self::Source => "source",
            Self::RecordAndSource => "record_and_source",
        }
    }

    pub const fn from_failures(source: bool, record: bool) -> Self {
        match (source, record) {
            (false, false) => Self::None,
            (false, true) => Self::Record,
            (true, false) => Self::Source,
            (true, true) => Self::RecordAndSource,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestFailureType {
    None,
    RecordRejection,
    SourceFailure,
    RecordRejectionAndSourceFailure,
    UnsupportedSchema,
    Other,
}

impl IngestFailureType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::RecordRejection => "record_rejection",
            Self::SourceFailure => "source_failure",
            Self::RecordRejectionAndSourceFailure => "record_rejection_and_source_failure",
            Self::UnsupportedSchema => "unsupported_schema",
            Self::Other => "other",
        }
    }

    pub const fn from_failures(source: bool, record: bool) -> Self {
        match (source, record) {
            (false, false) => Self::None,
            (false, true) => Self::RecordRejection,
            (true, false) => Self::SourceFailure,
            (true, true) => Self::RecordRejectionAndSourceFailure,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestChange {
    Changed,
    NoOp,
}

impl IngestChange {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Changed => "changed",
            Self::NoOp => "no_op",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestTerminalOutcome {
    Completed,
    CompletedWithRejections,
    CompletedWithSourceFailures,
    CompletedWithRejectionsAndSourceFailures,
}

impl IngestTerminalOutcome {
    pub const fn from_failures(source: bool, record: bool) -> Self {
        match (source, record) {
            (false, false) => Self::Completed,
            (false, true) => Self::CompletedWithRejections,
            (true, false) => Self::CompletedWithSourceFailures,
            (true, true) => Self::CompletedWithRejectionsAndSourceFailures,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::CompletedWithRejections => "completed_with_rejections",
            Self::CompletedWithSourceFailures => "completed_with_source_failures",
            Self::CompletedWithRejectionsAndSourceFailures => {
                "completed_with_rejections_and_source_failures"
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFailureOutcome {
    pub status: IngestStatus,
    pub failure_scope: IngestFailureScope,
    pub failure_type: IngestFailureType,
    pub source_identity: String,
    pub provider: String,
    pub source_failure_class: String,
    pub carried_forward: bool,
    pub source_selector: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordRejectionOutcome {
    pub source_identity: String,
    pub provider: String,
    pub source_selector: String,
    pub line: u64,
    pub payload_type: String,
    pub class: String,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct AutomaticPublicationOutcome {
    pub status: IngestStatus,
    pub failure_scope: IngestFailureScope,
    pub failure_type: IngestFailureType,
    pub terminal_outcome: IngestTerminalOutcome,
    pub change: IngestChange,
    pub previous_generation: Option<String>,
    pub published_generation: String,
    pub generation_changed: bool,
    pub scanned_routes: usize,
    pub successful_routes: usize,
    pub source_failure_total: usize,
    pub source_failures_omitted: usize,
    pub rejected_record_total: u64,
    pub sources_completed_with_rejections: usize,
    pub rejection_diagnostics_reported: usize,
    pub rejection_diagnostics_omitted: u64,
    pub current: SourceBackedRefreshCurrent,
    pub policy_schema_hash: String,
    pub request_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ExactPublicationOutcome {
    pub status: IngestStatus,
    pub failure_scope: IngestFailureScope,
    pub failure_type: IngestFailureType,
    pub provider: CaptureProvider,
    pub path: PathBuf,
    pub source_format: &'static str,
    pub stats: SourceStats,
    pub route_identity: String,
    pub catalog_lineage: String,
    pub request_overlay: ExplicitSourceCatalogAuthority,
    pub previous_generation: Option<String>,
    pub published_generation: String,
    pub generation_changed: bool,
    pub scanned_routes: usize,
    pub successful_routes: usize,
    pub source_failure_total: usize,
    pub route_source_failure_total: usize,
    pub rejected_record_total: u64,
    pub rejection_diagnostics: Vec<RecordRejectionOutcome>,
    pub request_id: Option<String>,
    pub change: IngestChange,
    pub current: SourceBackedRefreshCurrent,
    pub requested_failure: Option<SourceFailureOutcome>,
    pub requested_failure_class: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PluginPublicationOutcome {
    pub status: IngestStatus,
    pub failure_scope: IngestFailureScope,
    pub failure_type: IngestFailureType,
    pub plugin_source: HistorySourcePluginSource,
    pub route_source: ProviderSource,
    pub stats: SourceStats,
    pub catalog_lineage: String,
    pub catalog_authority: ExplicitSourceCatalogAuthority,
    pub previous_generation: Option<String>,
    pub published_generation: String,
    pub generation_changed: bool,
    pub rejected_record_total: u64,
    pub rejection_diagnostics: Vec<RecordRejectionOutcome>,
    pub request_id: Option<String>,
    pub change: IngestChange,
    pub current: SourceBackedRefreshCurrent,
}

#[derive(Debug, Clone)]
pub enum IngestSourceOutcome {
    Automatic(AutomaticPublicationOutcome),
    Exact(ExactPublicationOutcome),
    Plugin(PluginPublicationOutcome),
    SourceFailure(SourceFailureOutcome),
    Rejection(RecordRejectionOutcome),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderRefreshModeFact {
    ExplicitPath,
    ExplicitFormat,
    HistorySourcePlugin,
}

#[derive(Debug, Clone)]
pub struct ProviderRefreshFacts {
    pub provider: CaptureProvider,
    pub mode: ProviderRefreshModeFact,
    pub summary: ProviderImportSummary,
    pub stats: SourceStats,
    pub duration: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CorePublicationFacts {
    pub generation_changed: bool,
    pub source_failure_total: usize,
    pub rejected_record_total: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IngestTelemetryFacts {
    pub sources_seen: u64,
    pub source_files: u64,
    pub source_bytes: u64,
    pub failed_sources: u64,
}

#[derive(Debug, Clone)]
pub struct IngestReport {
    pub resume: bool,
    pub totals: ImportTotals,
    pub sources: Vec<IngestSourceOutcome>,
    pub telemetry: Option<IngestTelemetryFacts>,
    pub provider_refresh: Option<ProviderRefreshFacts>,
    pub core_publication: Option<CorePublicationFacts>,
}

impl IngestReport {
    pub fn first_failure_detail(&self) -> Option<(&str, IngestFailureType, &str)> {
        self.sources.iter().find_map(|source| match source {
            IngestSourceOutcome::SourceFailure(failure) => Some((
                failure.source_selector.as_str(),
                failure.failure_type,
                failure.detail.as_str(),
            )),
            IngestSourceOutcome::Exact(exact) if exact.route_source_failure_total != 0 => {
                exact.requested_failure.as_ref().map_or_else(
                    || {
                        Some((
                            "",
                            exact.failure_type,
                            "source failure detail omitted from bounded diagnostics",
                        ))
                    },
                    |failure| {
                        Some((
                            failure.source_selector.as_str(),
                            exact.failure_type,
                            failure.detail.as_str(),
                        ))
                    },
                )
            }
            _ => None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct IngestPublication {
    pub request_id: Option<String>,
    pub request_previous_generation: Option<String>,
    pub request_generation_changed: bool,
    pub scanned_routes: Option<usize>,
    pub pinned_generation: String,
    pub policy_schema_hash: Option<String>,
    pub catalog_content: std::collections::BTreeMap<String, (bool, bool)>,
    pub index_facts: Option<ImportIndexFacts>,
    pub receipt: Option<ctx_history_refresh::SourceBackedRefreshReceipt>,
}
