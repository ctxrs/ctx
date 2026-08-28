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

/// Stable content-free reason for a terminal Core refresh failure.
///
/// This deliberately mirrors only the closed structured outcome vocabulary.
/// Raw error detail, retry advice, route identifiers, and source coordinates
/// are never represented here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderRefreshFailureCode {
    None,
    SourceUnavailable,
    ExplicitSourcePathMissing,
    SourceChanged,
    MalformedSource,
    UnsupportedSchema,
    SourceFailures,
    LogicalSourceFailures,
    SourceUnclaimed,
    SourceRefreshFailed,
    SourceRefreshInternal,
    ResourceUnavailable,
    IndexIncompatible,
    IndexCorruption,
    SourceRefreshAdmissionFailed,
    AllProviderTerminalCoverageUnavailable,
    Unknown,
}

impl ProviderRefreshFailureCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::SourceUnavailable => "source_unavailable",
            Self::ExplicitSourcePathMissing => "explicit_source_path_missing",
            Self::SourceChanged => "source_changed",
            Self::MalformedSource => "malformed_source",
            Self::UnsupportedSchema => "unsupported_schema",
            Self::SourceFailures => "source_failures",
            Self::LogicalSourceFailures => "logical_source_failures",
            Self::SourceUnclaimed => "source_unclaimed",
            Self::SourceRefreshFailed => "source_refresh_failed",
            Self::SourceRefreshInternal => "source_refresh_internal",
            Self::ResourceUnavailable => "resource_unavailable",
            Self::IndexIncompatible => "index_incompatible",
            Self::IndexCorruption => "index_corruption",
            Self::SourceRefreshAdmissionFailed => "source_refresh_admission_failed",
            Self::AllProviderTerminalCoverageUnavailable => {
                "all_provider_terminal_coverage_unavailable"
            }
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderRefreshCountsV1 {
    pub records: Option<CountBucket>,
    pub logical_bytes: Option<BytesBucket>,
}

impl ProviderRefreshCountsV1 {
    pub fn new(records: u64, logical_bytes: u64) -> Self {
        Self {
            records: Some(count_bucket(records)),
            logical_bytes: Some(bytes_bucket(logical_bytes)),
        }
    }

    pub fn sparse(records: Option<u64>, logical_bytes: Option<u64>) -> Self {
        Self {
            records: records.map(count_bucket),
            logical_bytes: logical_bytes.map(bytes_bucket),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ForegroundProviderRefreshV1 {
    /// Absent only for one provider-neutral, all-provider publication.
    pub provider: Option<CaptureProvider>,
    pub trigger: ProviderRefreshTrigger,
    pub change: ProviderRefreshChange,
    pub refresh_result: ProviderRefreshResult,
    pub core_result: ProviderCoreResult,
    pub failure_scope: ProviderRefreshFailureScope,
    pub failure_type: ProviderRefreshFailureType,
    pub failure_code: ProviderRefreshFailureCode,
    pub retryable: bool,
    pub work_remaining: bool,
    pub counts: Option<ProviderRefreshCountsV1>,
}

/// Optional continuity facts from the daemon's durable terminal job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderRefreshTerminalHealthV1 {
    /// Present only for failed terminal jobs. Successful publication is
    /// already authoritative in the event's changed/no-op result.
    pub retained_previous_generation: Option<bool>,
    pub successor_pending: bool,
}

#[derive(Debug)]
pub struct ProviderRefreshCompletedV1 {
    pub surface: Surface,
    pub outcome: Outcome,
    pub duration: DurationBucket,
    pub foreground: Option<ForegroundProviderRefreshV1>,
    pub terminal_health: Option<ProviderRefreshTerminalHealthV1>,
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
        }
    }

    pub fn with_terminal_health(
        mut self,
        terminal_health: ProviderRefreshTerminalHealthV1,
    ) -> Self {
        self.terminal_health = Some(terminal_health);
        self
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::analytics::{sender::serialize_event, PublicEventV1};

    #[test]
    fn provider_refresh_serializes_only_terminal_decisions_and_coarse_work() {
        let event =
            PublicEventV1::ProviderRefreshCompleted(ProviderRefreshCompletedV1::foreground(
                Outcome::Success,
                Duration::from_secs(2),
                ForegroundProviderRefreshV1 {
                    provider: Some(CaptureProvider::Custom),
                    trigger: ProviderRefreshTrigger::Import,
                    change: ProviderRefreshChange::Changed,
                    refresh_result: ProviderRefreshResult::Partial,
                    core_result: ProviderCoreResult::Partial,
                    failure_scope: ProviderRefreshFailureScope::Record,
                    failure_type: ProviderRefreshFailureType::RecordRejection,
                    failure_code: ProviderRefreshFailureCode::None,
                    retryable: false,
                    work_remaining: true,
                    counts: Some(ProviderRefreshCountsV1::new(8, 2048)),
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
                "change": "changed",
                "refresh_result": "partial",
                "core_result": "partial",
                "failure_scope": "record",
                "failure_type": "record_rejection",
                "failure_code": "none",
                "retryable": false,
                "work_remaining": true,
                "records_bucket": "6-20",
                "logical_bytes_bucket": "lt_100kb",
            })
        );
    }

    #[test]
    fn daemon_refresh_serializes_only_continuity_health() {
        let event = PublicEventV1::ProviderRefreshCompleted(
            ProviderRefreshCompletedV1::bucketed(
                Surface::Daemon,
                Outcome::Failure,
                duration_bucket(Duration::from_secs(2)),
                ForegroundProviderRefreshV1 {
                    provider: Some(CaptureProvider::Codex),
                    trigger: ProviderRefreshTrigger::Daemon,
                    change: ProviderRefreshChange::NoOp,
                    refresh_result: ProviderRefreshResult::Failure,
                    core_result: ProviderCoreResult::Failure,
                    failure_scope: ProviderRefreshFailureScope::System,
                    failure_type: ProviderRefreshFailureType::System,
                    failure_code: ProviderRefreshFailureCode::IndexCorruption,
                    retryable: true,
                    work_remaining: true,
                    counts: None,
                },
            )
            .with_terminal_health(ProviderRefreshTerminalHealthV1 {
                retained_previous_generation: Some(true),
                successor_pending: true,
            }),
        );
        let occurred_at = chrono::DateTime::parse_from_rfc3339("2026-07-22T12:34:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let serialized = serialize_event(&event, occurred_at, None, None);

        assert_eq!(
            serialized["properties"]["refresh_retained_previous_generation"],
            true
        );
        assert_eq!(serialized["properties"]["refresh_successor_pending"], true);
        let properties = serialized["properties"].as_object().unwrap();
        for removed in [
            "source_mode",
            "content_evidence",
            "work_kind",
            "retired_records_bucket",
            "sources_bucket",
            "source_files_bucket",
            "sessions_bucket",
            "events_bucket",
            "edges_bucket",
            "skips_bucket",
            "rejections_bucket",
            "failures_bucket",
            "bytes_bucket",
            "cpu_duration_bucket",
            "observed_process_peak_rss_bucket",
            "refresh_configured_indexing_mode",
            "refresh_daemon_trigger_kind",
            "refresh_reconciliation_demand",
            "refresh_queue_wait_duration_bucket",
            "refresh_discovery_duration_bucket",
            "refresh_scan_stage_duration_bucket",
            "refresh_commit_duration_bucket",
            "refresh_coalesced_request_count_bucket",
            "refresh_processed_sessions_bucket",
            "refresh_processed_messages_bucket",
            "refresh_processed_tool_calls_bucket",
            "refresh_processed_bytes_bucket",
        ] {
            assert!(
                !properties.contains_key(removed),
                "retained removed field: {removed}"
            );
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
