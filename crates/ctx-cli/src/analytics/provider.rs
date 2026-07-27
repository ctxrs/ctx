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
pub(crate) struct ProviderRefreshCountsV1 {
    pub(crate) sources: CountBucket,
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
pub(crate) struct ForegroundProviderRefreshV1 {
    pub(crate) provider: CaptureProvider,
    pub(crate) trigger: ProviderRefreshTrigger,
    pub(crate) source_mode: ProviderRefreshSourceMode,
    pub(crate) change: ProviderRefreshChange,
    pub(crate) work_remaining: bool,
    pub(crate) counts: ProviderRefreshCountsV1,
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
                    provider: CaptureProvider::Custom,
                    trigger: ProviderRefreshTrigger::Import,
                    source_mode: ProviderRefreshSourceMode::HistorySourcePlugin,
                    change: ProviderRefreshChange::Changed,
                    work_remaining: true,
                    counts: ProviderRefreshCountsV1::new(2, 3, 8, 1, 5, 1, 1, 2048),
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
                "work_remaining": true,
                "sources_bucket": "2-5",
                "sessions_bucket": "2-5",
                "events_bucket": "6-20",
                "edges_bucket": "1",
                "skips_bucket": "2-5",
                "rejections_bucket": "1",
                "failures_bucket": "1",
                "bytes_bucket": "lt_100kb",
            })
        );
        let properties = serialized["properties"].as_object().unwrap();
        for forbidden in [
            "content",
            "path",
            "source_id",
            "session_id",
            "record_id",
            "provider_key",
            "source_format",
            "ingestion_mode",
            "rewrite_reason",
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
    }
}
