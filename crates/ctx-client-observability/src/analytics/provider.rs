#[cfg(any(test, feature = "test-support"))]
use std::time::Duration;

use ctx_history_core::CaptureProvider;

#[cfg(any(test, feature = "test-support"))]
use super::duration_bucket;
use super::{
    bytes_bucket, count_bucket, BytesBucket, CountBucket, DurationBucket, Outcome, Surface,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderRefreshTrigger {
    Setup,
    Import,
    Search,
    Daemon,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderRefreshConfiguredIndexingMode {
    Automatic,
    Manual,
}

impl ProviderRefreshConfiguredIndexingMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::Manual => "manual",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderRefreshDaemonTriggerKind {
    DaemonWatch,
    StartupCatchUp,
    PeriodicReconciliation,
}

impl ProviderRefreshDaemonTriggerKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DaemonWatch => "daemon_watch",
            Self::StartupCatchUp => "startup_catch_up",
            Self::PeriodicReconciliation => "periodic_reconciliation",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderRefreshReconciliationDemand {
    Incremental,
    Exhaustive,
}

impl ProviderRefreshReconciliationDemand {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Incremental => "incremental",
            Self::Exhaustive => "exhaustive",
        }
    }
}

impl ProviderRefreshTrigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Import => "import",
            Self::Search => "search",
            Self::Daemon => "daemon",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderRefreshSourceMode {
    Discovered,
    ExplicitPath,
    ExplicitFormat,
    HistorySourcePlugin,
}

impl ProviderRefreshSourceMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Discovered => "discovered",
            Self::ExplicitPath => "explicit_path",
            Self::ExplicitFormat => "explicit_format",
            Self::HistorySourcePlugin => "history_source_plugin",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderRefreshChange {
    Changed,
    NoOp,
}

impl ProviderRefreshChange {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Changed => "changed",
            Self::NoOp => "no_op",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderRefreshContentEvidence {
    None,
    Accepted,
    Mixed,
    Unknown,
}

impl ProviderRefreshContentEvidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Accepted => "accepted",
            Self::Mixed => "mixed",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderRefreshWorkKind {
    NoOp,
    Fresh,
    #[cfg(any(test, feature = "test-support"))]
    Append,
    #[cfg(any(test, feature = "test-support"))]
    Rewrite,
    #[cfg(any(test, feature = "test-support"))]
    Truncate,
    #[cfg(any(test, feature = "test-support"))]
    Replace,
    #[cfg(any(test, feature = "test-support"))]
    Retire,
    Mixed,
}

impl ProviderRefreshWorkKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoOp => "no_op",
            Self::Fresh => "fresh",
            #[cfg(any(test, feature = "test-support"))]
            Self::Append => "append",
            #[cfg(any(test, feature = "test-support"))]
            Self::Rewrite => "rewrite",
            #[cfg(any(test, feature = "test-support"))]
            Self::Truncate => "truncate",
            #[cfg(any(test, feature = "test-support"))]
            Self::Replace => "replace",
            #[cfg(any(test, feature = "test-support"))]
            Self::Retire => "retire",
            Self::Mixed => "mixed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderRefreshResult {
    Complete,
    Partial,
    Failure,
}

impl ProviderRefreshResult {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::Failure => "failure",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderCoreResult {
    NoOp,
    Complete,
    Partial,
    Failure,
    Unknown,
}

impl ProviderCoreResult {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoOp => "no_op",
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::Failure => "failure",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderRefreshFailureScope {
    None,
    Record,
    Source,
    System,
    Mixed,
    Unknown,
}

impl ProviderRefreshFailureScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Record => "record",
            Self::Source => "source",
            Self::System => "system",
            Self::Mixed => "mixed",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderRefreshFailureType {
    None,
    RecordRejection,
    UnsupportedSchema,
    #[cfg(any(test, feature = "test-support"))]
    NotFound,
    #[cfg(any(test, feature = "test-support"))]
    Permission,
    #[cfg(any(test, feature = "test-support"))]
    SourceDatabase,
    MalformedSource,
    #[cfg(any(test, feature = "test-support"))]
    Store,
    #[cfg(any(test, feature = "test-support"))]
    WorkerPanic,
    #[cfg(any(test, feature = "test-support"))]
    SystemIo,
    System,
    #[cfg(any(test, feature = "test-support"))]
    Other,
    Mixed,
    Unknown,
}

impl ProviderRefreshFailureType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::RecordRejection => "record_rejection",
            Self::UnsupportedSchema => "unsupported_schema",
            #[cfg(any(test, feature = "test-support"))]
            Self::NotFound => "not_found",
            #[cfg(any(test, feature = "test-support"))]
            Self::Permission => "permission",
            #[cfg(any(test, feature = "test-support"))]
            Self::SourceDatabase => "source_database",
            Self::MalformedSource => "malformed_source",
            #[cfg(any(test, feature = "test-support"))]
            Self::Store => "store",
            #[cfg(any(test, feature = "test-support"))]
            Self::WorkerPanic => "worker_panic",
            #[cfg(any(test, feature = "test-support"))]
            Self::SystemIo => "system_io",
            Self::System => "system",
            #[cfg(any(test, feature = "test-support"))]
            Self::Other => "other",
            Self::Mixed => "mixed",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderRefreshCountsV1 {
    pub sources: Option<CountBucket>,
    pub source_files: Option<CountBucket>,
    pub sessions: Option<CountBucket>,
    pub events: Option<CountBucket>,
    pub edges: Option<CountBucket>,
    pub skips: Option<CountBucket>,
    pub rejections: Option<CountBucket>,
    pub failures: Option<CountBucket>,
    pub bytes: Option<BytesBucket>,
}

impl ProviderRefreshCountsV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        sources: u64,
        source_files: u64,
        sessions: u64,
        events: u64,
        edges: u64,
        skips: u64,
        rejections: u64,
        failures: u64,
        bytes: u64,
    ) -> Self {
        Self {
            sources: Some(count_bucket(sources)),
            source_files: Some(count_bucket(source_files)),
            sessions: Some(count_bucket(sessions)),
            events: Some(count_bucket(events)),
            edges: Some(count_bucket(edges)),
            skips: Some(count_bucket(skips)),
            rejections: Some(count_bucket(rejections)),
            failures: Some(count_bucket(failures)),
            bytes: Some(bytes_bucket(bytes)),
        }
    }

    /// Builds sparse per-run facts when the refresh receipt does not own all
    /// foreground import counters.
    pub fn from_refresh_receipt(
        sources: u64,
        sessions: u64,
        rejections: u64,
        failures: u64,
        bytes: u64,
    ) -> Self {
        Self {
            sources: Some(count_bucket(sources)),
            source_files: None,
            sessions: Some(count_bucket(sessions)),
            events: None,
            edges: None,
            skips: None,
            rejections: Some(count_bucket(rejections)),
            failures: Some(count_bucket(failures)),
            bytes: Some(bytes_bucket(bytes)),
        }
    }

    /// Builds sparse daemon-run facts. Unknown values remain absent instead
    /// of being projected from current-generation cardinalities.
    pub fn sparse_refresh_receipt(
        sources: Option<u64>,
        sessions: Option<u64>,
        rejections: Option<u64>,
        failures: Option<u64>,
        bytes: Option<u64>,
    ) -> Self {
        Self {
            sources: sources.map(count_bucket),
            source_files: None,
            sessions: sessions.map(count_bucket),
            events: None,
            edges: None,
            skips: None,
            rejections: rejections.map(count_bucket),
            failures: failures.map(count_bucket),
            bytes: bytes.map(bytes_bucket),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderRefreshPerformanceV1 {
    pub cpu_duration: DurationBucket,
    pub observed_process_peak_rss: Option<BytesBucket>,
}

impl ProviderRefreshPerformanceV1 {
    #[cfg(any(test, feature = "test-support"))]
    pub fn new(cpu_duration: Duration, observed_process_peak_rss_bytes: Option<u64>) -> Self {
        Self {
            cpu_duration: duration_bucket(cpu_duration),
            observed_process_peak_rss: observed_process_peak_rss_bytes.map(bytes_bucket),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ForegroundProviderRefreshV1 {
    /// Absent only for one provider-neutral, all-provider source publication.
    pub provider: Option<CaptureProvider>,
    pub trigger: ProviderRefreshTrigger,
    /// Absent when a global publication may contain discovered and explicit
    /// catalog routes together.
    pub source_mode: Option<ProviderRefreshSourceMode>,
    pub change: ProviderRefreshChange,
    pub content_evidence: ProviderRefreshContentEvidence,
    pub work_kind: Option<ProviderRefreshWorkKind>,
    pub refresh_result: ProviderRefreshResult,
    pub core_result: ProviderCoreResult,
    pub failure_scope: ProviderRefreshFailureScope,
    pub failure_type: ProviderRefreshFailureType,
    pub work_remaining: bool,
    pub retired_records: Option<CountBucket>,
    /// Per-run counts are omitted when only current generation cardinalities
    /// are authoritative.
    pub counts: Option<ProviderRefreshCountsV1>,
    pub performance: Option<ProviderRefreshPerformanceV1>,
}

/// Optional bucketed health facts from the daemon's durable terminal job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderRefreshTerminalHealthV1 {
    pub configured_indexing_mode: Option<ProviderRefreshConfiguredIndexingMode>,
    /// Further classifies the existing `Daemon` trigger without duplicating
    /// setup, search, or import trigger authority.
    pub daemon_trigger_kind: Option<ProviderRefreshDaemonTriggerKind>,
    pub reconciliation_demand: Option<ProviderRefreshReconciliationDemand>,
    /// Present only for failed terminal jobs. Successful publication is
    /// already authoritative in the event's changed/no-op result.
    pub retained_previous_generation: Option<bool>,
    pub queue_wait_duration: Option<DurationBucket>,
    pub discovery_duration: Option<DurationBucket>,
    pub scan_stage_duration: Option<DurationBucket>,
    pub commit_duration: Option<DurationBucket>,
    pub coalesced_request_count: Option<CountBucket>,
    pub successor_pending: bool,
    pub processed_sessions: Option<CountBucket>,
    pub processed_messages: Option<CountBucket>,
    pub processed_tool_calls: Option<CountBucket>,
    pub processed_bytes: Option<BytesBucket>,
}

/// Sparse current-generation stock from an exact published refresh receipt.
///
/// This is a best-effort, non-retryable sampled observation attached to the
/// existing terminal event. It is emitted only for a successfully changed
/// daemon publication, and is neither a generation census nor a delivery or
/// coverage denominator. Its absence means the terminal outcome/change did
/// not provide this exact receipt-backed observation; it never means zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderRefreshCorpusStockV1 {
    /// Logical events currently retained as indexed lexical documents.
    pub indexed_documents: CountBucket,
    pub retained_records: CountBucket,
    pub rejected_records: CountBucket,
    pub certified_source_bytes: BytesBucket,
    /// Transition-local removed-source count, not retired logical records.
    pub removed_source_count: CountBucket,
}

#[derive(Debug)]
pub struct ProviderRefreshCompletedV1 {
    pub surface: Surface,
    pub outcome: Outcome,
    pub duration: DurationBucket,
    pub foreground: Option<ForegroundProviderRefreshV1>,
    pub terminal_health: Option<ProviderRefreshTerminalHealthV1>,
    pub corpus_stock: Option<ProviderRefreshCorpusStockV1>,
}

impl ProviderRefreshCompletedV1 {
    #[cfg(any(test, feature = "test-support"))]
    pub fn new(surface: Surface, outcome: Outcome, duration: Duration) -> Self {
        Self {
            surface,
            outcome,
            duration: duration_bucket(duration),
            foreground: None,
            terminal_health: None,
            corpus_stock: None,
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn foreground(
        outcome: Outcome,
        duration: Duration,
        foreground: ForegroundProviderRefreshV1,
    ) -> Self {
        Self {
            surface: Surface::Cli,
            outcome,
            duration: duration_bucket(duration),
            foreground: Some(foreground),
            terminal_health: None,
            corpus_stock: None,
        }
    }

    pub fn foreground_bucketed(
        outcome: Outcome,
        duration: DurationBucket,
        foreground: ForegroundProviderRefreshV1,
    ) -> Self {
        Self::bucketed(Surface::Cli, outcome, duration, foreground)
    }

    pub fn bucketed(
        surface: Surface,
        outcome: Outcome,
        duration: DurationBucket,
        foreground: ForegroundProviderRefreshV1,
    ) -> Self {
        Self {
            surface,
            outcome,
            duration,
            foreground: Some(foreground),
            terminal_health: None,
            corpus_stock: None,
        }
    }

    pub fn with_terminal_health(
        mut self,
        terminal_health: ProviderRefreshTerminalHealthV1,
    ) -> Self {
        self.terminal_health = Some(terminal_health);
        self
    }

    pub fn with_corpus_stock(mut self, corpus_stock: ProviderRefreshCorpusStockV1) -> Self {
        self.corpus_stock = Some(corpus_stock);
        self
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::analytics::{sender::serialize_event, PublicEventV1};

    #[test]
    fn foreground_provider_refresh_serializes_only_closed_content_free_fields() {
        let event =
            PublicEventV1::ProviderRefreshCompleted(ProviderRefreshCompletedV1::foreground(
                Outcome::Success,
                Duration::from_secs(2),
                ForegroundProviderRefreshV1 {
                    provider: Some(CaptureProvider::Custom),
                    trigger: ProviderRefreshTrigger::Import,
                    source_mode: Some(ProviderRefreshSourceMode::HistorySourcePlugin),
                    change: ProviderRefreshChange::Changed,
                    content_evidence: ProviderRefreshContentEvidence::Mixed,
                    work_kind: Some(ProviderRefreshWorkKind::Append),
                    refresh_result: ProviderRefreshResult::Partial,
                    core_result: ProviderCoreResult::Partial,
                    failure_scope: ProviderRefreshFailureScope::Record,
                    failure_type: ProviderRefreshFailureType::RecordRejection,
                    work_remaining: true,
                    retired_records: Some(count_bucket(42)),
                    counts: Some(ProviderRefreshCountsV1::new(2, 12, 3, 8, 1, 5, 1, 1, 2048)),
                    performance: Some(ProviderRefreshPerformanceV1::new(
                        Duration::from_millis(800),
                        Some(512 * 1024 * 1024),
                    )),
                },
            ));
        let occurred_at = chrono::DateTime::parse_from_rfc3339("2026-07-22T12:34:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let serialized = serialize_event(&event, occurred_at, None, None);

        assert_eq!(serialized["event_name"], "provider_refresh_completed");
        assert_eq!(serialized["operation"], "refresh");
        assert_eq!(serialized["surface"], "cli");
        assert_eq!(
            serialized["properties"],
            json!({
                "provider": "custom",
                "trigger": "import",
                "source_mode": "history_source_plugin",
                "change": "changed",
                "content_evidence": "mixed",
                "work_kind": "append",
                "refresh_result": "partial",
                "core_result": "partial",
                "failure_scope": "record",
                "failure_type": "record_rejection",
                "work_remaining": true,
                "retired_records_bucket": "21-100",
                "sources_bucket": "2-5",
                "source_files_bucket": "6-20",
                "sessions_bucket": "2-5",
                "events_bucket": "6-20",
                "edges_bucket": "1",
                "skips_bucket": "2-5",
                "rejections_bucket": "1",
                "failures_bucket": "1",
                "bytes_bucket": "lt_100kb",
                "cpu_duration_bucket": "lt_1s",
                "observed_process_peak_rss_bucket": "100mb-1gb",
            })
        );
        let properties = serialized["properties"].as_object().unwrap();
        for forbidden in [
            "content",
            "path",
            "source_id",
            "session_id",
            "record_id",
            "locator",
            "cursor",
            "provider_key",
            "source_format",
            "ingestion_mode",
            "ingestion_engine",
            "rewrite_reason",
            "error",
            "error_message",
            "duration_ms",
            "peak_rss_bucket",
            "bytes",
            "observed_process_peak_rss_bytes",
            "corpus_stock_indexed_documents_bucket",
        ] {
            assert!(!properties.contains_key(forbidden));
        }
    }

    #[test]
    fn daemon_setup_refresh_serializes_sparse_receipt_facts_and_present_zero_health() {
        let event = PublicEventV1::ProviderRefreshCompleted(
            ProviderRefreshCompletedV1::bucketed(
                Surface::Daemon,
                Outcome::Success,
                duration_bucket(Duration::from_secs(2)),
                ForegroundProviderRefreshV1 {
                    provider: Some(CaptureProvider::Codex),
                    trigger: ProviderRefreshTrigger::Setup,
                    source_mode: Some(ProviderRefreshSourceMode::Discovered),
                    change: ProviderRefreshChange::Changed,
                    content_evidence: ProviderRefreshContentEvidence::Unknown,
                    work_kind: Some(ProviderRefreshWorkKind::Fresh),
                    refresh_result: ProviderRefreshResult::Complete,
                    core_result: ProviderCoreResult::Complete,
                    failure_scope: ProviderRefreshFailureScope::None,
                    failure_type: ProviderRefreshFailureType::None,
                    work_remaining: false,
                    retired_records: None,
                    counts: Some(ProviderRefreshCountsV1::from_refresh_receipt(
                        2, 7, 0, 0, 4096,
                    )),
                    performance: None,
                },
            )
            .with_terminal_health(ProviderRefreshTerminalHealthV1 {
                configured_indexing_mode: Some(ProviderRefreshConfiguredIndexingMode::Automatic),
                daemon_trigger_kind: None,
                reconciliation_demand: Some(ProviderRefreshReconciliationDemand::Exhaustive),
                retained_previous_generation: None,
                queue_wait_duration: Some(duration_bucket(Duration::ZERO)),
                discovery_duration: None,
                scan_stage_duration: Some(duration_bucket(Duration::from_secs(2))),
                commit_duration: Some(duration_bucket(Duration::from_secs(6))),
                coalesced_request_count: Some(count_bucket(0)),
                successor_pending: true,
                processed_sessions: Some(count_bucket(7)),
                processed_messages: Some(count_bucket(19)),
                processed_tool_calls: Some(count_bucket(4)),
                processed_bytes: Some(bytes_bucket(4096)),
            }),
        );
        let occurred_at = chrono::DateTime::parse_from_rfc3339("2026-07-22T12:34:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let serialized = serialize_event(&event, occurred_at, None, None);

        assert_eq!(serialized["surface"], "daemon");
        assert_eq!(serialized["properties"]["trigger"], "setup");
        assert_eq!(serialized["properties"]["source_mode"], "discovered");
        assert_eq!(serialized["properties"]["work_kind"], "fresh");
        assert_eq!(
            serialized["properties"]["refresh_configured_indexing_mode"],
            "automatic"
        );
        assert_eq!(
            serialized["properties"]["refresh_reconciliation_demand"],
            "exhaustive"
        );
        assert_eq!(serialized["properties"]["sessions_bucket"], "6-20");
        assert_eq!(serialized["properties"]["bytes_bucket"], "lt_100kb");
        assert_eq!(
            serialized["properties"]["refresh_queue_wait_duration_bucket"],
            "lt_100ms"
        );
        assert_eq!(
            serialized["properties"]["refresh_scan_stage_duration_bucket"],
            "lt_5s"
        );
        assert_eq!(
            serialized["properties"]["refresh_commit_duration_bucket"],
            "lt_30s"
        );
        assert_eq!(
            serialized["properties"]["refresh_coalesced_request_count_bucket"],
            "0"
        );
        assert_eq!(serialized["properties"]["refresh_successor_pending"], true);
        assert_eq!(
            serialized["properties"]["refresh_processed_sessions_bucket"],
            "6-20"
        );
        assert_eq!(
            serialized["properties"]["refresh_processed_messages_bucket"],
            "6-20"
        );
        assert_eq!(
            serialized["properties"]["refresh_processed_tool_calls_bucket"],
            "2-5"
        );
        assert_eq!(
            serialized["properties"]["refresh_processed_bytes_bucket"],
            "lt_100kb"
        );
        let properties = serialized["properties"].as_object().unwrap();
        for absent in [
            "source_files_bucket",
            "events_bucket",
            "edges_bucket",
            "skips_bucket",
            "retired_records_bucket",
            "cpu_duration_bucket",
            "observed_process_peak_rss_bucket",
            "refresh_discovery_duration_bucket",
            "refresh_daemon_trigger_kind",
            "refresh_retained_previous_generation",
        ] {
            assert!(!properties.contains_key(absent));
        }
    }

    #[test]
    fn best_effort_corpus_stock_serializes_only_bucketed_current_receipt_facts() {
        let event = PublicEventV1::ProviderRefreshCompleted(
            ProviderRefreshCompletedV1::bucketed(
                Surface::Daemon,
                Outcome::Success,
                duration_bucket(Duration::from_secs(2)),
                ForegroundProviderRefreshV1 {
                    provider: None,
                    trigger: ProviderRefreshTrigger::Daemon,
                    source_mode: None,
                    change: ProviderRefreshChange::Changed,
                    content_evidence: ProviderRefreshContentEvidence::Unknown,
                    work_kind: None,
                    refresh_result: ProviderRefreshResult::Complete,
                    core_result: ProviderCoreResult::Complete,
                    failure_scope: ProviderRefreshFailureScope::None,
                    failure_type: ProviderRefreshFailureType::None,
                    work_remaining: false,
                    retired_records: None,
                    counts: None,
                    performance: None,
                },
            )
            .with_corpus_stock(ProviderRefreshCorpusStockV1 {
                indexed_documents: count_bucket(21),
                retained_records: count_bucket(7),
                rejected_records: count_bucket(3),
                certified_source_bytes: bytes_bucket(4096),
                removed_source_count: count_bucket(1),
            }),
        );
        let occurred_at = chrono::DateTime::parse_from_rfc3339("2026-07-22T12:34:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let serialized = serialize_event(&event, occurred_at, None, None);
        let properties = serialized["properties"].as_object().unwrap();

        assert_eq!(
            properties["corpus_stock_indexed_documents_bucket"],
            "21-100"
        );
        assert_eq!(properties["corpus_stock_retained_records_bucket"], "6-20");
        assert_eq!(properties["corpus_stock_rejected_records_bucket"], "2-5");
        assert_eq!(
            properties["corpus_stock_certified_source_bytes_bucket"],
            "lt_100kb"
        );
        assert_eq!(properties["corpus_transition_removed_sources_bucket"], "1");
        for forbidden in [
            "generation_id",
            "session_id",
            "message_id",
            "tool_call_id",
            "relationship",
            "copy",
            "logical_added",
            "logical_changed",
            "logical_retired",
            "corpus_stock_indexed_documents",
            "corpus_stock_certified_source_bytes",
            "corpus_transition_removed_sources",
        ] {
            assert!(!properties.contains_key(forbidden));
        }
    }

    #[test]
    fn provider_refresh_triggers_are_a_closed_contract() {
        assert_eq!(
            [
                ProviderRefreshTrigger::Setup,
                ProviderRefreshTrigger::Import,
                ProviderRefreshTrigger::Search,
                ProviderRefreshTrigger::Daemon,
            ]
            .map(ProviderRefreshTrigger::as_str),
            ["setup", "import", "search", "daemon"]
        );
        assert_eq!(
            [
                ProviderRefreshSourceMode::Discovered,
                ProviderRefreshSourceMode::ExplicitPath,
                ProviderRefreshSourceMode::ExplicitFormat,
                ProviderRefreshSourceMode::HistorySourcePlugin,
            ]
            .map(ProviderRefreshSourceMode::as_str),
            [
                "discovered",
                "explicit_path",
                "explicit_format",
                "history_source_plugin",
            ]
        );
        assert_eq!(
            [ProviderRefreshChange::Changed, ProviderRefreshChange::NoOp,]
                .map(ProviderRefreshChange::as_str),
            ["changed", "no_op"]
        );
    }

    #[test]
    fn provider_refresh_decision_enums_are_closed_contracts() {
        assert_eq!(
            [
                ProviderRefreshConfiguredIndexingMode::Automatic,
                ProviderRefreshConfiguredIndexingMode::Manual,
            ]
            .map(ProviderRefreshConfiguredIndexingMode::as_str),
            ["automatic", "manual"]
        );
        assert_eq!(
            [
                ProviderRefreshDaemonTriggerKind::DaemonWatch,
                ProviderRefreshDaemonTriggerKind::StartupCatchUp,
                ProviderRefreshDaemonTriggerKind::PeriodicReconciliation,
            ]
            .map(ProviderRefreshDaemonTriggerKind::as_str),
            [
                "daemon_watch",
                "startup_catch_up",
                "periodic_reconciliation"
            ]
        );
        assert_eq!(
            [
                ProviderRefreshReconciliationDemand::Incremental,
                ProviderRefreshReconciliationDemand::Exhaustive,
            ]
            .map(ProviderRefreshReconciliationDemand::as_str),
            ["incremental", "exhaustive"]
        );
        assert_eq!(
            [
                ProviderRefreshContentEvidence::None,
                ProviderRefreshContentEvidence::Accepted,
                ProviderRefreshContentEvidence::Mixed,
                ProviderRefreshContentEvidence::Unknown,
            ]
            .map(ProviderRefreshContentEvidence::as_str),
            ["none", "accepted", "mixed", "unknown"]
        );
        assert_eq!(
            [
                ProviderRefreshWorkKind::NoOp,
                ProviderRefreshWorkKind::Fresh,
                ProviderRefreshWorkKind::Append,
                ProviderRefreshWorkKind::Rewrite,
                ProviderRefreshWorkKind::Truncate,
                ProviderRefreshWorkKind::Replace,
                ProviderRefreshWorkKind::Retire,
                ProviderRefreshWorkKind::Mixed,
            ]
            .map(ProviderRefreshWorkKind::as_str),
            ["no_op", "fresh", "append", "rewrite", "truncate", "replace", "retire", "mixed",]
        );
        assert_eq!(
            [
                ProviderRefreshResult::Complete,
                ProviderRefreshResult::Partial,
                ProviderRefreshResult::Failure,
            ]
            .map(ProviderRefreshResult::as_str),
            ["complete", "partial", "failure"]
        );
        assert_eq!(
            [
                ProviderCoreResult::NoOp,
                ProviderCoreResult::Complete,
                ProviderCoreResult::Partial,
                ProviderCoreResult::Failure,
                ProviderCoreResult::Unknown,
            ]
            .map(ProviderCoreResult::as_str),
            ["no_op", "complete", "partial", "failure", "unknown"]
        );
        assert_eq!(
            [
                ProviderRefreshFailureScope::None,
                ProviderRefreshFailureScope::Record,
                ProviderRefreshFailureScope::Source,
                ProviderRefreshFailureScope::System,
                ProviderRefreshFailureScope::Mixed,
                ProviderRefreshFailureScope::Unknown,
            ]
            .map(ProviderRefreshFailureScope::as_str),
            ["none", "record", "source", "system", "mixed", "unknown"]
        );
        assert_eq!(
            [
                ProviderRefreshFailureType::None,
                ProviderRefreshFailureType::RecordRejection,
                ProviderRefreshFailureType::UnsupportedSchema,
                ProviderRefreshFailureType::NotFound,
                ProviderRefreshFailureType::Permission,
                ProviderRefreshFailureType::SourceDatabase,
                ProviderRefreshFailureType::MalformedSource,
                ProviderRefreshFailureType::Store,
                ProviderRefreshFailureType::WorkerPanic,
                ProviderRefreshFailureType::SystemIo,
                ProviderRefreshFailureType::System,
                ProviderRefreshFailureType::Other,
                ProviderRefreshFailureType::Mixed,
                ProviderRefreshFailureType::Unknown,
            ]
            .map(ProviderRefreshFailureType::as_str),
            [
                "none",
                "record_rejection",
                "unsupported_schema",
                "not_found",
                "permission",
                "source_database",
                "malformed_source",
                "store",
                "worker_panic",
                "system_io",
                "system",
                "other",
                "mixed",
                "unknown",
            ]
        );
    }

    #[test]
    fn every_capture_provider_matches_the_versioned_telemetry_vocabulary() {
        let manifest: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../contracts/telemetry-v1/providers-v1.json"
        ))
        .unwrap();
        assert_eq!(manifest["schema_version"], 1);
        assert_eq!(manifest["vocabulary"], "ctx-telemetry-provider");

        let providers = manifest["providers"].as_array().unwrap();
        let current = providers
            .iter()
            .filter(|entry| entry["status"] == "current")
            .map(|entry| entry["id"].as_str().unwrap())
            .collect::<Vec<_>>();
        let retired = providers
            .iter()
            .filter(|entry| entry["status"] == "retired")
            .map(|entry| entry["id"].as_str().unwrap())
            .collect::<Vec<_>>();
        let unique = providers
            .iter()
            .map(|entry| entry["id"].as_str().unwrap())
            .collect::<std::collections::HashSet<_>>();

        assert_eq!(current, CaptureProvider::variants());
        assert_eq!(retired, ["windsurf", "trae"]);
        assert_eq!(unique.len(), providers.len());
    }
}
