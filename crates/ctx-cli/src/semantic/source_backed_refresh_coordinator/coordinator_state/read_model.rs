use super::*;

/// Verified terminal receipt for one daemon-owned source refresh.
#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct SourceBackedRefreshReceipt {
    pub(crate) previous_generation: Option<String>,
    pub(crate) published_generation: String,
    pub(crate) generation_changed: bool,
    pub(crate) published_explicit_source_catalog: ExplicitSourceCatalogAuthority,
    pub(crate) current: SourceBackedRefreshCurrent,
    pub(crate) scanned_routes: usize,
    pub(crate) successful_routes: usize,
    pub(crate) source_failures: SourceBackedRefreshSourceFailures,
}

impl SourceBackedRefreshReceipt {
    pub(crate) fn terminal_outcome(&self) -> &'static str {
        if self.source_failures.is_empty() {
            "completed"
        } else {
            "completed_with_source_failures"
        }
    }

    pub(crate) fn to_json(&self) -> Value {
        compact_json(json!({
            "previous_generation": self.previous_generation,
            "published_generation": self.published_generation,
            "generation_changed": self.generation_changed,
            "published_explicit_source_catalog": self
                .published_explicit_source_catalog
                .to_json(),
            "current": self.current.to_json(),
            "outcome": self.terminal_outcome(),
            "scanned_routes": self.scanned_routes,
            "successful_routes": self.successful_routes,
            "source_failures": self.source_failures.to_json(),
        }))
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum SourceBackedRefreshSourceFailureClass {
    Unavailable,
    SourceChanged,
    Unreadable,
    Incompatible,
}

impl SourceBackedRefreshSourceFailureClass {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::SourceChanged => "source_changed",
            Self::Unreadable => "unreadable",
            Self::Incompatible => "incompatible",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "unavailable" => Some(Self::Unavailable),
            "source_changed" => Some(Self::SourceChanged),
            "unreadable" => Some(Self::Unreadable),
            "incompatible" => Some(Self::Incompatible),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct SourceBackedRefreshSourceFailure {
    pub(crate) source_identity: String,
    pub(crate) provider: String,
    pub(crate) class: SourceBackedRefreshSourceFailureClass,
    pub(crate) carried_forward: bool,
    pub(crate) source_selector: String,
    pub(crate) detail: String,
}

impl SourceBackedRefreshSourceFailure {
    pub(crate) fn to_json(&self) -> Value {
        json!({
            "source_identity": self.source_identity,
            "provider": self.provider,
            "class": self.class.as_str(),
            "carried_forward": self.carried_forward,
            "source_selector": self.source_selector,
            "detail": self.detail,
        })
    }
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub(crate) struct SourceBackedRefreshSourceFailures {
    pub(crate) failures: Vec<SourceBackedRefreshSourceFailure>,
    pub(crate) omitted: usize,
}

impl SourceBackedRefreshSourceFailures {
    pub(crate) fn total(&self) -> usize {
        self.failures.len().saturating_add(self.omitted)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.total() == 0
    }

    pub(crate) fn to_json(&self) -> Value {
        json!({
            "failures": self.failures.iter()
                .map(SourceBackedRefreshSourceFailure::to_json)
                .collect::<Vec<_>>(),
            "omitted": self.omitted,
            "total": self.total(),
        })
    }
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub(crate) struct SourceBackedRefreshTimings {
    pub(crate) discovery_us: u64,
    pub(crate) scan_stage_us: u64,
    pub(crate) commit_us: u64,
}

impl SourceBackedRefreshTimings {
    fn to_json(self) -> Value {
        json!({
            "discovery": self.discovery_us,
            "scan_stage": self.scan_stage_us,
            "commit": self.commit_us,
        })
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum SourceBackedRefreshState {
    Queued,
    Running,
    Published,
    Failed,
}

impl SourceBackedRefreshState {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Published => "published",
            Self::Failed => "failed",
        }
    }

    pub(super) fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Running)
    }
}

#[derive(Debug, Clone)]
pub(super) struct SourceBackedRefreshProgress {
    pub(super) phase: String,
    pub(super) completed_sources: usize,
    pub(super) total_sources: usize,
    pub(super) current_source: Option<String>,
}

impl Default for SourceBackedRefreshProgress {
    fn default() -> Self {
        Self {
            phase: "queued".to_owned(),
            completed_sources: 0,
            total_sources: 0,
            current_source: None,
        }
    }
}

impl SourceBackedRefreshProgress {
    fn to_json(&self) -> Value {
        compact_json(json!({
            "phase": self.phase,
            "completed_sources": self.completed_sources,
            "total_sources": self.total_sources,
            "current_source": self.current_source,
        }))
    }
}

#[derive(Debug, Clone)]
pub(super) struct SourceBackedRefreshAttempt {
    pub(super) request_id: String,
    pub(super) state: SourceBackedRefreshState,
    pub(super) requested_at_ms: i64,
    pub(super) started_at_ms: Option<i64>,
    pub(super) finished_at_ms: Option<i64>,
    pub(super) previous_generation: Option<String>,
    pub(super) published_generation: Option<String>,
    pub(super) requested_explicit_source_catalog: Option<ExplicitSourceCatalogAuthority>,
    pub(super) published_explicit_source_catalog: Option<ExplicitSourceCatalogAuthority>,
    pub(super) coalesced_requests: u64,
    pub(super) progress: SourceBackedRefreshProgress,
    pub(super) scanned_routes: Option<usize>,
    pub(super) unsupported_routes: Option<usize>,
    pub(super) certified_source_count: Option<usize>,
    pub(super) certified_source_bytes: Option<u64>,
    pub(super) receipt: Option<SourceBackedRefreshReceipt>,
    pub(super) timings: Option<SourceBackedRefreshTimings>,
    pub(super) publication_probe_us: u64,
    pub(super) daemon_mode: DaemonMode,
    pub(super) trigger: &'static str,
    pub(super) trigger_provenance: &'static str,
    pub(super) failure_type: Option<&'static str>,
    pub(super) last_error: Option<String>,
    pub(super) post_publication_error: Option<String>,
}

impl SourceBackedRefreshAttempt {
    fn failure_code(&self) -> Option<&'static str> {
        self.last_error
            .as_deref()
            .filter(|error| error.contains(TERMINAL_COVERAGE_ERROR_CODE))
            .map(|_| TERMINAL_COVERAGE_ERROR_CODE)
    }

    fn failure_reason(&self) -> Option<&'static str> {
        self.failure_code()
            .map(|_| "provider_terminal_coverage_unavailable")
    }

    pub(super) fn to_json(&self) -> Value {
        compact_json(json!({
            "ok": true,
            "schema_version": 1,
            "owner": "daemon",
            "request_id": self.request_id,
            "request_state": self.state.as_str(),
            "requested_at_ms": self.requested_at_ms,
            "started_at_ms": self.started_at_ms,
            "finished_at_ms": self.finished_at_ms,
            "previous_generation": self.previous_generation,
            "published_generation": self.published_generation,
            "requested_explicit_source_catalog": self.requested_explicit_source_catalog
                .as_ref()
                .map(ExplicitSourceCatalogAuthority::to_json),
            "published_explicit_source_catalog": self.published_explicit_source_catalog
                .as_ref()
                .map(ExplicitSourceCatalogAuthority::to_json),
            "generation_changed": self.receipt.as_ref().map(|receipt| receipt.generation_changed),
            "outcome": self.receipt.as_ref().map(SourceBackedRefreshReceipt::terminal_outcome),
            "successful_routes": self.receipt.as_ref().map(|receipt| receipt.successful_routes),
            "source_failure_total": self.receipt.as_ref()
                .map(|receipt| receipt.source_failures.total()),
            "source_failures_omitted": self.receipt.as_ref()
                .map(|receipt| receipt.source_failures.omitted),
            "receipt": self.receipt.as_ref().map(SourceBackedRefreshReceipt::to_json),
            "coalesced_requests": self.coalesced_requests,
            "progress": self.progress.to_json(),
            "scanned_routes": self.scanned_routes,
            "unsupported_routes": self.unsupported_routes,
            "certified_source_count": self.certified_source_count,
            "certified_source_bytes": self.certified_source_bytes,
            "timings_us": self.timings_json(),
            "daemon_mode": self.daemon_mode.as_str(),
            "trigger": self.trigger,
            "trigger_provenance": self.trigger_provenance,
            "failure_type": self.failure_type,
            "error_code": self.failure_code(),
            "reason": self.failure_reason(),
            "last_error": self.last_error,
            "post_publication_error": self.post_publication_error,
        }))
    }

    pub(super) fn job_json(&self) -> Value {
        let status = match self.state {
            SourceBackedRefreshState::Published => "completed",
            SourceBackedRefreshState::Failed => "failed",
            SourceBackedRefreshState::Queued | SourceBackedRefreshState::Running => "running",
        };
        compact_json(json!({
            "mode": SourceBackedRefreshMode::Background.as_str(),
            "owner": "daemon",
            "kind": "core_refresh",
            "status": status,
            "request_id": self.request_id,
            "request_state": self.state.as_str(),
            "source_count": self.progress.total_sources,
            "last_run_at_ms": self.started_at_ms.unwrap_or(self.requested_at_ms),
            "previous_generation": self.previous_generation,
            "published_generation": self.published_generation,
            "requested_explicit_source_catalog": self.requested_explicit_source_catalog
                .as_ref()
                .map(ExplicitSourceCatalogAuthority::to_json),
            "published_explicit_source_catalog": self.published_explicit_source_catalog
                .as_ref()
                .map(ExplicitSourceCatalogAuthority::to_json),
            "generation_changed": self.receipt.as_ref().map(|receipt| receipt.generation_changed),
            "outcome": self.receipt.as_ref().map(SourceBackedRefreshReceipt::terminal_outcome),
            "successful_routes": self.receipt.as_ref().map(|receipt| receipt.successful_routes),
            "source_failure_total": self.receipt.as_ref()
                .map(|receipt| receipt.source_failures.total()),
            "source_failures_omitted": self.receipt.as_ref()
                .map(|receipt| receipt.source_failures.omitted),
            "receipt": self.receipt.as_ref().map(SourceBackedRefreshReceipt::to_json),
            "coalesced_requests": self.coalesced_requests,
            "progress": self.progress.to_json(),
            "scanned_routes": self.scanned_routes,
            "unsupported_routes": self.unsupported_routes,
            "certified_source_count": self.certified_source_count,
            "certified_source_bytes": self.certified_source_bytes,
            "timings_us": self.timings_json(),
            "daemon_mode": self.daemon_mode.as_str(),
            "trigger": self.trigger,
            "trigger_provenance": self.trigger_provenance,
            "failure_type": self.failure_type,
            "error_code": self.failure_code(),
            "reason": self.failure_reason(),
            "last_error": self.last_error,
            "post_publication_error": self.post_publication_error,
        }))
    }

    fn timings_json(&self) -> Option<Value> {
        self.timings.map(|timings| {
            let mut timings = timings.to_json();
            timings["publication_probe"] = json!(self.publication_probe_us);
            timings
        })
    }
}
