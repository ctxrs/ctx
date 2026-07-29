use std::time::Duration;

use ctx_history_core::CaptureProvider;

use super::{
    bytes_bucket, count_bucket, duration_bucket, BytesBucket, CountBucket, DurationBucket, Outcome,
    Surface,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderRefreshTrigger {
    Setup,
    Import,
    Search,
    Daemon,
}

impl ProviderRefreshTrigger {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Import => "import",
            Self::Search => "search",
            Self::Daemon => "daemon",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderRefreshSourceMode {
    Discovered,
    ExplicitPath,
    ExplicitFormat,
    HistorySourcePlugin,
}

impl ProviderRefreshSourceMode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Discovered => "discovered",
            Self::ExplicitPath => "explicit_path",
            Self::ExplicitFormat => "explicit_format",
            Self::HistorySourcePlugin => "history_source_plugin",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderRefreshChange {
    Changed,
    NoOp,
}

impl ProviderRefreshChange {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Changed => "changed",
            Self::NoOp => "no_op",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderRefreshContentEvidence {
    None,
    Accepted,
    Mixed,
    Unknown,
}

impl ProviderRefreshContentEvidence {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Accepted => "accepted",
            Self::Mixed => "mixed",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "the public telemetry vocabulary retains privacy-safe work kinds even when the current NativePath runtime cannot distinguish every kind"
)]
pub(crate) enum ProviderRefreshWorkKind {
    NoOp,
    Fresh,
    Append,
    Rewrite,
    Truncate,
    Replace,
    Retire,
    Mixed,
}

impl ProviderRefreshWorkKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::NoOp => "no_op",
            Self::Fresh => "fresh",
            Self::Append => "append",
            Self::Rewrite => "rewrite",
            Self::Truncate => "truncate",
            Self::Replace => "replace",
            Self::Retire => "retire",
            Self::Mixed => "mixed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderRefreshResult {
    Complete,
    Partial,
    Failure,
}

impl ProviderRefreshResult {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::Failure => "failure",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderCoreResult {
    NoOp,
    Complete,
    Partial,
    Failure,
    Unknown,
}

impl ProviderCoreResult {
    pub(crate) fn as_str(self) -> &'static str {
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
#[allow(
    dead_code,
    reason = "the public telemetry vocabulary retains Pro lifecycle outcomes used by released event consumers"
)]
pub(crate) enum ProviderProResult {
    NotRequested,
    Unavailable,
    NoOp,
    Complete,
    Partial,
    Behind,
    Failure,
    Unknown,
}

impl ProviderProResult {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::NotRequested => "not_requested",
            Self::Unavailable => "unavailable",
            Self::NoOp => "no_op",
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::Behind => "behind",
            Self::Failure => "failure",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderRefreshFailureScope {
    None,
    Record,
    Source,
    System,
    Mixed,
    Unknown,
}

impl ProviderRefreshFailureScope {
    pub(crate) fn as_str(self) -> &'static str {
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
pub(crate) enum ProviderRefreshFailureType {
    None,
    RecordRejection,
    UnsupportedSchema,
    NotFound,
    Permission,
    SourceDatabase,
    MalformedSource,
    Store,
    WorkerPanic,
    SystemIo,
    System,
    Other,
    Mixed,
    Unknown,
}

impl ProviderRefreshFailureType {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::RecordRejection => "record_rejection",
            Self::UnsupportedSchema => "unsupported_schema",
            Self::NotFound => "not_found",
            Self::Permission => "permission",
            Self::SourceDatabase => "source_database",
            Self::MalformedSource => "malformed_source",
            Self::Store => "store",
            Self::WorkerPanic => "worker_panic",
            Self::SystemIo => "system_io",
            Self::System => "system",
            Self::Other => "other",
            Self::Mixed => "mixed",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProviderRefreshCountsV1 {
    pub(crate) sources: CountBucket,
    pub(crate) source_files: CountBucket,
    pub(crate) sessions: CountBucket,
    pub(crate) events: CountBucket,
    pub(crate) edges: CountBucket,
    pub(crate) skips: CountBucket,
    pub(crate) rejections: CountBucket,
    pub(crate) failures: CountBucket,
    pub(crate) bytes: BytesBucket,
}

impl ProviderRefreshCountsV1 {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
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
            sources: count_bucket(sources),
            source_files: count_bucket(source_files),
            sessions: count_bucket(sessions),
            events: count_bucket(events),
            edges: count_bucket(edges),
            skips: count_bucket(skips),
            rejections: count_bucket(rejections),
            failures: count_bucket(failures),
            bytes: bytes_bucket(bytes),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProviderRefreshPerformanceV1 {
    pub(crate) cpu_duration: DurationBucket,
    pub(crate) observed_process_peak_rss: Option<BytesBucket>,
}

impl ProviderRefreshPerformanceV1 {
    pub(crate) fn new(
        cpu_duration: Duration,
        observed_process_peak_rss_bytes: Option<u64>,
    ) -> Self {
        Self {
            cpu_duration: duration_bucket(cpu_duration),
            observed_process_peak_rss: observed_process_peak_rss_bytes.map(bytes_bucket),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ForegroundProviderRefreshV1 {
    /// Absent only for one provider-neutral, all-provider source publication.
    pub(crate) provider: Option<CaptureProvider>,
    pub(crate) trigger: ProviderRefreshTrigger,
    /// Absent when a global publication may contain discovered and explicit
    /// catalog routes together.
    pub(crate) source_mode: Option<ProviderRefreshSourceMode>,
    pub(crate) change: ProviderRefreshChange,
    pub(crate) content_evidence: ProviderRefreshContentEvidence,
    pub(crate) work_kind: Option<ProviderRefreshWorkKind>,
    pub(crate) refresh_result: ProviderRefreshResult,
    pub(crate) core_result: ProviderCoreResult,
    pub(crate) canonical_pro_result: ProviderProResult,
    pub(crate) output_pro_result: ProviderProResult,
    pub(crate) failure_scope: ProviderRefreshFailureScope,
    pub(crate) failure_type: ProviderRefreshFailureType,
    pub(crate) work_remaining: bool,
    pub(crate) retired_records: Option<CountBucket>,
    /// Per-run counts are omitted when only current generation cardinalities
    /// are authoritative.
    pub(crate) counts: Option<ProviderRefreshCountsV1>,
    pub(crate) performance: Option<ProviderRefreshPerformanceV1>,
}

#[derive(Debug)]
pub(crate) struct ProviderRefreshCompletedV1 {
    pub(crate) surface: Surface,
    pub(crate) outcome: Outcome,
    pub(crate) duration: DurationBucket,
    pub(crate) foreground: Option<ForegroundProviderRefreshV1>,
}

impl ProviderRefreshCompletedV1 {
    #[cfg(test)]
    pub(crate) fn new(surface: Surface, outcome: Outcome, duration: Duration) -> Self {
        Self {
            surface,
            outcome,
            duration: duration_bucket(duration),
            foreground: None,
        }
    }

    #[allow(
        dead_code,
        reason = "kept as the exact-duration constructor used by telemetry contract tests"
    )]
    pub(crate) fn foreground(
        outcome: Outcome,
        duration: Duration,
        foreground: ForegroundProviderRefreshV1,
    ) -> Self {
        Self {
            surface: Surface::Cli,
            outcome,
            duration: duration_bucket(duration),
            foreground: Some(foreground),
        }
    }

    pub(crate) fn foreground_bucketed(
        outcome: Outcome,
        duration: DurationBucket,
        foreground: ForegroundProviderRefreshV1,
    ) -> Self {
        Self {
            surface: Surface::Cli,
            outcome,
            duration,
            foreground: Some(foreground),
        }
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
                    canonical_pro_result: ProviderProResult::Complete,
                    output_pro_result: ProviderProResult::Behind,
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
                "canonical_pro_result": "complete",
                "output_pro_result": "behind",
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
                ProviderProResult::NotRequested,
                ProviderProResult::Unavailable,
                ProviderProResult::NoOp,
                ProviderProResult::Complete,
                ProviderProResult::Partial,
                ProviderProResult::Behind,
                ProviderProResult::Failure,
                ProviderProResult::Unknown,
            ]
            .map(ProviderProResult::as_str),
            [
                "not_requested",
                "unavailable",
                "no_op",
                "complete",
                "partial",
                "behind",
                "failure",
                "unknown",
            ]
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
    fn every_capture_provider_has_an_unsuppressed_closed_wire_name() {
        let providers = [
            CaptureProvider::Codex,
            CaptureProvider::Claude,
            CaptureProvider::Pi,
            CaptureProvider::OpenCode,
            CaptureProvider::Kilo,
            CaptureProvider::KiroCli,
            CaptureProvider::Antigravity,
            CaptureProvider::Gemini,
            CaptureProvider::Tabnine,
            CaptureProvider::Cursor,
            CaptureProvider::Windsurf,
            CaptureProvider::Zed,
            CaptureProvider::CopilotCli,
            CaptureProvider::FactoryAiDroid,
            CaptureProvider::QwenCode,
            CaptureProvider::KimiCodeCli,
            CaptureProvider::Auggie,
            CaptureProvider::Junie,
            CaptureProvider::Firebender,
            CaptureProvider::ForgeCode,
            CaptureProvider::DeepAgents,
            CaptureProvider::MistralVibe,
            CaptureProvider::Mux,
            CaptureProvider::RovoDev,
            CaptureProvider::OpenClaw,
            CaptureProvider::Hermes,
            CaptureProvider::NanoClaw,
            CaptureProvider::AstrBot,
            CaptureProvider::Shelley,
            CaptureProvider::Continue,
            CaptureProvider::OpenHands,
            CaptureProvider::Cline,
            CaptureProvider::RooCode,
            CaptureProvider::Crush,
            CaptureProvider::Goose,
            CaptureProvider::Lingma,
            CaptureProvider::Qoder,
            CaptureProvider::Warp,
            CaptureProvider::CodeBuddy,
            CaptureProvider::Trae,
            CaptureProvider::Shell,
            CaptureProvider::Git,
            CaptureProvider::Jj,
            CaptureProvider::Gh,
            CaptureProvider::Custom,
            CaptureProvider::Unknown,
            CaptureProvider::MiMoCode,
        ];

        assert_eq!(providers.len(), 47);
        for provider in providers {
            assert!(!provider.as_str().is_empty());
        }
    }
}
