use super::wire::count;
use super::*;
use serde_json::json;

vocabulary!(ServerFailure { Unauthorized => "unauthorized", Forbidden => "forbidden", NotFound => "not_found", Conflict => "conflict", Cancelled => "cancelled", Expired => "expired", Invalid => "invalid", Capacity => "capacity", RequestCapacity => "request_capacity", WorkCapacity => "work_capacity", Timeout => "timeout", Interrupted => "interrupted", Body => "body", BodyTooLarge => "body_too_large", Method => "method", Unavailable => "unavailable", Index => "index", Io => "io", Catalog => "catalog", Json => "json", Core => "core", Identity => "identity", Archive => "archive", Other => "other" });
vocabulary!(ServerBodyOutcome { Suppressed => "suppressed", Complete => "complete", Failed => "failed", Dropped => "dropped" });
vocabulary!(ServerPopulation { Request => "request", Execution => "execution", Read => "read", Upload => "upload", Publication => "publication", Index => "index" });
vocabulary!(ServerPublicationKind { Published => "published", Withdrawn => "withdrawn", Removed => "removed", Cancelled => "cancelled", AlreadyAccepted => "already_accepted" });
vocabulary!(SharingOperation { WorkerStarted => "worker_started", WorkerStartFailed => "worker_start_failed", WorkerStopped => "worker_stopped", Tick => "tick", Selection => "selection", Queued => "queued", Transfer => "transfer", Accepted => "accepted", Settled => "settled", Retry => "retry" });
vocabulary!(SharingPhase { Settings => "settings", Admission => "admission", Capture => "capture", BeginUpload => "begin_upload", UploadStatus => "upload_status", UploadChunk => "upload_chunk", Publish => "publish", Receipt => "receipt", Settlement => "settlement", Checkpoint => "checkpoint" });
vocabulary!(SharingTick { Disabled => "disabled", Paused => "paused", Idle => "idle", Progress => "progress", Failed => "failed" });
vocabulary!(SharingFailure { Configuration => "configuration", Credentials => "credentials", State => "state", NotConnected => "not_connected", DestinationChanged => "destination_changed", PolicyConflict => "policy_conflict", PolicyDenied => "policy_denied", Busy => "busy", Unavailable => "unavailable", Unauthorized => "unauthorized", Forbidden => "forbidden", NotFound => "not_found", Conflict => "conflict", StagingExpired => "staging_expired", TooLarge => "too_large", RateLimited => "rate_limited", Protocol => "protocol", HttpRejected => "http_rejected", Archive => "archive" });
vocabulary!(SharingSelection { Selected => "selected", UnselectedSource => "unselected_source", ChangedProfile => "changed_profile", OutsideWorkRoots => "outside_work_roots", UnknownWorkRoot => "unknown_work_root", BackfillExcluded => "backfill_excluded", FutureExcluded => "future_excluded", NeedsReview => "needs_review" });

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowCounts {
    pub observed: u64,
    pub failed: u64,
    /// Measured samples only, in NATIVE_DURATION_BUCKETS order.
    pub latency: Option<[u64; 14]>,
}
impl WindowCounts {
    pub fn is_valid(&self) -> bool {
        self.observed > 0
            && self.failed <= self.observed
            && self
                .latency
                .is_none_or(|b| histogram_total(b).is_some_and(|n| n <= self.observed))
    }
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffCounts {
    pub complete: u64,
    pub failed: u64,
    pub unknown: u64,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerReadTotals {
    pub observed: u64,
    pub returned: u64,
    pub nonempty: u64,
    pub bytes: Option<MeasuredTotal>,
    pub continuation_requested: u64,
    pub has_more: Option<MeasuredTotal>,
    pub complete: Option<MeasuredTotal>,
    pub exhaustive: Option<MeasuredTotal>,
    pub response_limited: Option<MeasuredTotal>,
    pub snippets_truncated: Option<MeasuredTotal>,
    pub coverage_lag: Option<MeasuredTotal>,
    pub query_latency: Option<[u64; 14]>,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerIndexTotals {
    pub processed_operations: Option<MeasuredTotal>,
    pub records: Option<MeasuredTotal>,
    pub bytes: Option<MeasuredTotal>,
    pub coverage_lag: Option<MeasuredTotal>,
    pub reads_available: Option<MeasuredTotal>,
    pub activated: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerSummaryFacts {
    /// These optional populations are correlated with these requests at source.
    Request {
        handoff: HandoffCounts,
        body: Option<ServerBodyOutcome>,
        response_bytes: Option<MeasuredTotal>,
        execution: Option<WindowCounts>,
        read: Option<ServerReadTotals>,
    },
    Execution,
    Read(ServerReadTotals),
    Upload {
        bytes: u64,
        replayed: u64,
    },
    Publication {
        kind: ServerPublicationKind,
        replayed: u64,
        bytes: Option<MeasuredTotal>,
        records: Option<MeasuredTotal>,
    },
    Index(ServerIndexTotals),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerSummaryV1 {
    pub operation: ServerOperation,
    pub window: Duration,
    pub counts: WindowCounts,
    pub facts: ServerSummaryFacts,
    pub failure: Option<ServerFailure>,
    pub response_class: Option<ResponseClass>,
    pub collection_limited: bool,
}
impl ServerSummaryV1 {
    pub fn new(operation: ServerOperation, window: Duration, facts: ServerSummaryFacts) -> Self {
        Self {
            operation,
            window,
            counts: WindowCounts::default(),
            facts,
            failure: None,
            response_class: None,
            collection_limited: false,
        }
    }
    pub fn into_event(self) -> Option<PublicEventV1> {
        if !self.counts.is_valid() {
            return None;
        }
        if self.failure.is_some() && self.counts.failed != self.counts.observed {
            return None;
        }
        if self.response_class.is_some()
            && !matches!(self.facts, ServerSummaryFacts::Request { .. })
        {
            return None;
        }
        let n = self.counts.observed;
        let valid = match self.facts {
            ServerSummaryFacts::Request {
                handoff: h,
                body: _,
                response_bytes,
                execution,
                read,
            } => {
                h.complete.checked_add(h.failed)?.checked_add(h.unknown)? == n
                    && valid_total(response_bytes, n, false)
                    && execution.is_none_or(|x| x.is_valid() && x.observed <= n)
                    && read.is_none_or(|x| x.valid(n))
            }
            ServerSummaryFacts::Read(r) => r.observed == n && r.valid(n),
            ServerSummaryFacts::Execution => true,
            ServerSummaryFacts::Upload { replayed, .. } => replayed <= n,
            ServerSummaryFacts::Publication {
                replayed,
                bytes,
                records,
                ..
            } => replayed <= n && valid_total(bytes, n, false) && valid_total(records, n, false),
            ServerSummaryFacts::Index(i) => {
                i.activated <= n
                    && [i.processed_operations, i.records, i.bytes, i.coverage_lag]
                        .into_iter()
                        .all(|v| valid_total(v, n, false))
                    && valid_total(i.reads_available, n, true)
            }
        };
        valid.then_some(PublicEventV1::ServerSummary(self))
    }
    pub(super) fn properties(self) -> Map<String, Value> {
        let mut p = window(self.window, self.collection_limited, self.counts);
        if let Some(v) = self.failure {
            p.insert("server_failure".into(), json!(v.as_str()));
        }
        if let Some(v) = self.response_class {
            p.insert("response_class".into(), json!(v.as_str()));
        }

        p.insert("server_operation".into(), json!(self.operation.as_str()));
        let population = match self.facts {
            ServerSummaryFacts::Request {
                handoff: h,
                body,
                response_bytes,
                execution,
                read,
            } => {
                if let Some(v) = body {
                    p.insert("server_body_outcome".into(), json!(v.as_str()));
                }
                total(&mut p, "response_bytes", response_bytes, true);
                for (key, value) in [
                    ("handoff_complete", h.complete),
                    ("handoff_failed", h.failed),
                    ("handoff_unknown", h.unknown),
                ] {
                    count(&mut p, &format!("{key}_count_bucket"), Some(value));
                }
                if let Some(x) = execution {
                    counts(&mut p, "execution_", x);
                }
                if let Some(x) = read {
                    x.insert(&mut p);
                }
                ServerPopulation::Request
            }
            ServerSummaryFacts::Execution => ServerPopulation::Execution,
            ServerSummaryFacts::Read(r) => {
                r.insert(&mut p);
                ServerPopulation::Read
            }
            ServerSummaryFacts::Upload { bytes, replayed } => {
                byte_count(&mut p, "upload_bytes_bucket", bytes);
                count(&mut p, "replayed_count_bucket", Some(replayed));
                ServerPopulation::Upload
            }
            ServerSummaryFacts::Publication {
                kind,
                replayed,
                bytes,
                records,
            } => {
                p.insert("publication_kind".into(), json!(kind.as_str()));
                count(&mut p, "replayed_count_bucket", Some(replayed));
                total(&mut p, "publication_bytes", bytes, true);
                total(&mut p, "publication_records", records, false);
                ServerPopulation::Publication
            }
            ServerSummaryFacts::Index(i) => {
                for (name, value, bytes) in [
                    ("index_processed_operations", i.processed_operations, false),
                    ("index_records", i.records, false),
                    ("index_bytes", i.bytes, true),
                    ("index_coverage_lag", i.coverage_lag, false),
                    ("index_reads_available", i.reads_available, false),
                ] {
                    total(&mut p, name, value, bytes);
                }
                count(&mut p, "index_activated_count_bucket", Some(i.activated));
                ServerPopulation::Index
            }
        };
        p.insert("server_population".into(), json!(population.as_str()));
        p
    }
}
impl ServerReadTotals {
    fn valid(self, n: u64) -> bool {
        self.observed > 0
            && self.observed <= n
            && self.nonempty <= self.observed
            && self.nonempty <= self.returned
            && self.continuation_requested <= self.observed
            && [self.bytes, self.snippets_truncated, self.coverage_lag]
                .into_iter()
                .all(|v| valid_total(v, self.observed, false))
            && [
                self.has_more,
                self.complete,
                self.exhaustive,
                self.response_limited,
            ]
            .into_iter()
            .all(|v| valid_total(v, self.observed, true))
            && self
                .query_latency
                .is_none_or(|v| histogram_total(v).is_some_and(|n| n <= self.observed))
    }
    fn insert(self, p: &mut Map<String, Value>) {
        for (name, value) in [
            ("read_observed", self.observed),
            ("read_returned", self.returned),
            ("read_nonempty", self.nonempty),
            ("read_continuation_requested", self.continuation_requested),
        ] {
            count(p, &format!("{name}_count_bucket"), Some(value));
        }
        for (name, value, bytes) in [
            ("read_bytes", self.bytes, true),
            ("read_has_more", self.has_more, false),
            ("read_complete", self.complete, false),
            ("read_exhaustive", self.exhaustive, false),
            ("read_response_limited", self.response_limited, false),
            ("read_snippets_truncated", self.snippets_truncated, false),
            ("read_coverage_lag", self.coverage_lag, false),
        ] {
            total(p, name, value, bytes);
        }
        histogram(p, "read_query_", self.query_latency);
    }
}
/// One closed operation/phase/outcome cohort per window, never identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharingSummaryV1 {
    pub operation: SharingOperation,
    pub window: Duration,
    pub counts: WindowCounts,
    pub phase: Option<SharingPhase>,
    pub tick: Option<SharingTick>,
    pub failure: Option<SharingFailure>,
    pub selection: Option<SharingSelection>,
    pub selected_count: Option<u64>,
    pub selection_complete: Option<bool>,
    pub bytes: Option<MeasuredTotal>,
    pub records: Option<MeasuredTotal>,
    pub recovered_receipts: Option<u64>,
    pub already_accepted: Option<u64>,
    pub progress_after_failure: Option<u64>,
    pub retry_attempts: Option<MeasuredTotal>,
    pub retry_delay: Option<[u64; 14]>,
    pub collection_limited: bool,
}
impl SharingSummaryV1 {
    pub fn new(operation: SharingOperation, window: Duration) -> Self {
        Self {
            operation,
            window,
            counts: WindowCounts::default(),
            phase: None,
            tick: None,
            failure: None,
            selection: None,
            selected_count: None,
            selection_complete: None,
            bytes: None,
            records: None,
            recovered_receipts: None,
            already_accepted: None,
            progress_after_failure: None,
            retry_attempts: None,
            retry_delay: None,
            collection_limited: false,
        }
    }
    pub fn into_event(self) -> Option<PublicEventV1> {
        if !self.counts.is_valid()
            || ![self.bytes, self.records, self.retry_attempts]
                .into_iter()
                .all(|x| valid_total(x, self.counts.observed, false))
            || [
                self.recovered_receipts,
                self.already_accepted,
                self.progress_after_failure,
            ]
            .into_iter()
            .flatten()
            .any(|n| n > self.counts.observed)
            || self
                .retry_delay
                .is_some_and(|b| histogram_total(b).is_none_or(|n| n > self.counts.observed))
        {
            return None;
        }
        let tick = self.operation == SharingOperation::Tick;
        let retry = self.operation == SharingOperation::Retry;
        let selection = self.operation == SharingOperation::Selection;
        if (tick
            && (self.phase.is_none()
                || self.tick.is_none()
                || (self.tick == Some(SharingTick::Failed)) != self.failure.is_some()))
            || (!tick && (self.tick.is_some() || self.progress_after_failure.is_some()))
            || (retry && (self.phase.is_none() || self.failure.is_none()))
            || (!retry && (self.retry_attempts.is_some() || self.retry_delay.is_some()))
            || (selection
                && (self.selection.is_none()
                    || self.selected_count.is_none()
                    || self.selection_complete.is_none()))
            || (!selection
                && (self.selection.is_some()
                    || self.selected_count.is_some()
                    || self.selection_complete.is_some()))
            || (self.recovered_receipts.is_some() && self.operation != SharingOperation::Accepted)
            || (self.already_accepted.is_some() && self.operation != SharingOperation::Settled)
        {
            return None;
        }
        Some(PublicEventV1::SharingSummary(self))
    }
    pub(super) fn properties(self) -> Map<String, Value> {
        let mut p = window(self.window, self.collection_limited, self.counts);
        p.insert("sharing_operation".into(), json!(self.operation.as_str()));
        for (name, value) in [
            ("sharing_phase", self.phase.map(|v| v.as_str())),
            ("sharing_tick", self.tick.map(|v| v.as_str())),
            ("sharing_failure", self.failure.map(|v| v.as_str())),
            ("sharing_selection", self.selection.map(|v| v.as_str())),
        ] {
            if let Some(v) = value {
                p.insert(name.into(), json!(v));
            }
        }
        if let Some(v) = self.selection_complete {
            p.insert("selection_complete".into(), json!(v));
        }
        for (name, value) in [
            ("selection", self.selected_count),
            ("recovered_receipts", self.recovered_receipts),
            ("already_accepted", self.already_accepted),
            ("progress_after_failure", self.progress_after_failure),
        ] {
            count(&mut p, &format!("{name}_count_bucket"), value);
        }
        total(&mut p, "sharing_bytes", self.bytes, true);
        total(&mut p, "sharing_records", self.records, false);
        total(&mut p, "retry_attempts", self.retry_attempts, false);
        histogram(&mut p, "retry_delay_", self.retry_delay);
        p
    }
}
pub(super) fn valid_total(v: Option<MeasuredTotal>, n: u64, boolean: bool) -> bool {
    v.is_none_or(|v| v.samples > 0 && v.samples <= n && (!boolean || v.total <= v.samples))
}
pub(super) fn histogram_total(b: [u64; 14]) -> Option<u64> {
    b.into_iter().try_fold(0u64, u64::checked_add)
}
pub(super) fn histogram(p: &mut Map<String, Value>, prefix: &str, bins: Option<[u64; 14]>) {
    if let Some(b) = bins {
        count(
            p,
            &format!("{prefix}latency_measured_count_bucket"),
            Some(b.into_iter().fold(0u64, u64::saturating_add)),
        );
        for (i, n) in b.into_iter().enumerate() {
            count(p, &format!("{prefix}latency_{i}_count_bucket"), Some(n));
        }
    }
}
fn counts(p: &mut Map<String, Value>, prefix: &str, c: WindowCounts) {
    count(
        p,
        &format!("{prefix}observed_count_bucket"),
        Some(c.observed),
    );
    count(p, &format!("{prefix}failed_count_bucket"), Some(c.failed));
    histogram(p, prefix, c.latency);
}
fn window(duration: Duration, limited: bool, c: WindowCounts) -> Map<String, Value> {
    let mut p = Map::new();
    p.insert(
        "observation_window_bucket".into(),
        json!(duration_bucket(duration).as_str()),
    );
    p.insert("collection_scope".into(), json!("best_effort_observed"));
    p.insert("collection_limited".into(), json!(limited));
    counts(&mut p, "", c);
    p
}
fn byte_count(p: &mut Map<String, Value>, key: &str, n: u64) {
    p.insert(
        key.into(),
        json!(crate::analytics::bytes_bucket(n).as_str()),
    );
}
pub(super) fn total(p: &mut Map<String, Value>, name: &str, v: Option<MeasuredTotal>, bytes: bool) {
    if let Some(v) = v {
        count(p, &format!("{name}_measured_count_bucket"), Some(v.samples));
        if bytes {
            byte_count(p, &format!("{name}_bucket"), v.total);
        } else {
            count(p, &format!("{name}_bucket"), Some(v.total));
        }
    }
}
