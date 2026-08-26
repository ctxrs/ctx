//! Transport-neutral application policy for history-source discovery and ingest.
//!
//! This crate owns request routing, bounded source inventory, manifest-backed
//! source discovery, and source-list assembly. Provider parsers, durable
//! source admission, refresh execution, command rendering, and telemetry
//! delivery remain behind coarse borrowed ports in their owning layers.

mod diagnostics;
mod inventory;
mod lifecycle;
#[cfg(test)]
mod lifecycle_tests;
mod listing;
mod outcome;
mod plugins;
mod routing;
mod totals;

pub use ctx_history_refresh::RefreshSelection;
pub use diagnostics::{ImportPathMissingDuringRefresh, ImportPathNotFound};
pub use inventory::{source_stats, SourceStats};
pub use lifecycle::run_ingest;
pub use listing::{
    assemble_source_listing, history_source_plugin_report, merge_sources, source_identity,
    source_is_visible, HistorySourcePluginReport, HistorySourcePluginReportingStatus,
    SourceListing, SourceListingRequest,
};
pub use outcome::{
    AutomaticPublicationOutcome, CorePublicationFacts, ExactPublicationOutcome, IngestChange,
    IngestFailureScope, IngestFailureType, IngestPublication, IngestReport, IngestSourceOutcome,
    IngestStatus, IngestTelemetryFacts, IngestTerminalOutcome, PluginPublicationOutcome,
    ProviderRefreshFacts, ProviderRefreshModeFact, RecordRejectionOutcome, SourceFailureOutcome,
};
pub use plugins::{
    discover_history_source_plugins, discover_history_source_plugins_with_diagnostics,
    select_history_source_plugin, HistorySourcePluginDiscovery, HistorySourcePluginManifestFailure,
    HistorySourcePluginRefresh, HistorySourcePluginSource, COMMAND_ONLY_UNSUPPORTED_REASON,
};
pub use routing::{
    automatic_source_preflight, validate_ingest_request, AutomaticSourcePreflight,
    CaptureAdmissionPort, IngestProgressPort, IngestRefreshPort, IngestRequest, IngestRoute,
    ProviderSelectionGuidance, SourceDiscoveryPort,
};
pub use totals::{ImportFailureScope, ImportFailureType, ImportOutcome, ImportTotals};
