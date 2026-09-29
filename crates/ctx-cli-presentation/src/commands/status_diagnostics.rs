//! Closed, content-free projection of daemon observations for sharing.

use std::env::consts::{ARCH, OS};

use ctx_history_core::CaptureProvider;
use ctx_upgrade_engine::ProductBuildIdentity;
use serde_json::{json, Value};

mod render;
pub use render::render_status_diagnostics;
#[cfg(test)]
mod tests;

// Strings from persisted records never become presentation text. Each code is
// converted to a closed enum before either text or JSON serialization.
macro_rules! codes {
    ($name:ident { $($variant:ident => $code:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum $name { Unknown, $($variant),+ }
        impl $name {
            fn parse(value: &Value) -> Self {
                match value.as_str() { $(Some($code) => Self::$variant,)+ _ => Self::Unknown }
            }
            fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $code,)+ Self::Unknown => "unknown" }
            }
        }
    };
}

codes!(DaemonState { Running => "running", Stopped => "stopped" });
codes!(RefreshState {
    AdmissionPending => "admission_pending", Queued => "queued", Running => "running",
    Published => "published", Failed => "failed", Skipped => "skipped", Disabled => "disabled",
});
codes!(Trigger {
    Setup => "setup", Import => "import", Search => "search", Periodic => "periodic",
    Recovery => "recovery",
});
// Refresh admission/runtime metadata and terminal recovery own the job codes.
// import_presentation also publishes the longer automatic-refresh and plugin
// codes in its request metadata; keep both producer spellings recognizable.
codes!(Provenance {
    Manual => "manual", Autostart => "autostart", Setup => "setup_command",
    Import => "import_command", AutomaticProvider => "automatic_provider",
    Scheduler => "daemon_scheduler", ExplicitSources => "explicit_source_catalog",
    CommitPayload => "commit_payload",
    AutomaticProviderRefresh => "automatic_provider_refresh", HistorySourcePlugin => "history_source_plugin",
});
codes!(Stage {
    Preparing => "preparing", Reading => "reading", Merging => "merging",
    Syncing => "syncing", PhysicalVerification => "physical_verification",
    LogicalVerification => "logical_verification", Activation => "activation",
    Complete => "complete", Failed => "failed",
});
codes!(RefreshReason {
    Backoff => "retry_backoff", Deadline => "daemon_deadline", Disabled => "daemon_disabled",
    Paused => "automatic_retry_paused", PartiallyPaused => "automatic_retry_partially_paused",
    Confirming => "automatic_retry_confirming",
});
codes!(MeasurementStatus {
    Observed => "observed", Unavailable => "unavailable", Unsupported => "unsupported",
    PermissionDenied => "permission_denied", ProcessExited => "process_exited",
    DaemonRestarted => "daemon_restarted", CounterReset => "counter_reset",
    IdentityUnknown => "identity_unknown", InvalidRecord => "invalid_record",
});
// materializer::MaterializationPhase uses these serde snake_case names.
codes!(BlamePhase {
    SnapshotUnavailable => "snapshot_unavailable", WaitingForWriter => "waiting_for_writer",
    Preparing => "preparing", Indexing => "indexing", Publishing => "publishing", Complete => "complete",
});
// daemon_worker emits these statuses for retained semantic job receipts.
codes!(SemanticJobStatus {
    Disabled => "disabled", Ready => "ready", Skipped => "skipped",
    BudgetExhausted => "budget_exhausted", Failed => "failed", ResourceDeferred => "resource_deferred",
});

const BLAME_COUNTERS: [&str; 3] = ["completed_sources", "total_sources", "applied_changes"];

const TIMINGS: [&str; 4] = ["discovery", "scan_stage", "commit", "publication_probe"];
const COUNTERS: [&str; 7] = [
    "filesystem_signals",
    "ipc_signals",
    "timeout_wakeups",
    "scheduled_retry_wakeups",
    "scheduled_refresh_wakeups",
    "work_cycles",
    "no_work_cycles",
];

fn counter(value: &Value) -> Option<u64> {
    value.as_u64().filter(|value| *value < (1_u64 << 53))
}

fn finite_number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .filter(|value| value.is_finite() && *value >= 0.0)
}

#[derive(Debug)]
pub struct RefreshActivity {
    state: RefreshState,
    trigger: Trigger,
    provenance: Provenance,
    stage: Stage,
    reason: RefreshReason,
    providers: Option<Vec<CaptureProvider>>,
    processed_records: Option<u64>,
    processed_bytes: Option<u64>,
    scanned_routes: Option<u64>,
    certified_source_count: Option<u64>,
    certified_source_bytes: Option<u64>,
    generation_changed: Option<bool>,
    timings: [Option<u64>; 4],
}

impl RefreshActivity {
    pub fn from_job(job: &Value) -> Self {
        let mut state = RefreshState::parse(&job["request_state"]);
        // Scheduler-only receipts have no request state.
        if state == RefreshState::Unknown && job.get("request_state").is_none() {
            state = match job["status"].as_str() {
                Some("completed") => RefreshState::Published,
                Some("failed") => RefreshState::Failed,
                Some("skipped") => RefreshState::Skipped,
                Some("disabled") => RefreshState::Disabled,
                _ => RefreshState::Unknown,
            };
        }
        let progress = &job["progress"];
        let providers = progress["providers"].as_array().and_then(|values| {
            // The full list must be recognized; filtering would imply complete
            // coverage when an older CLI cannot name one participating provider.
            let mut providers = values
                .iter()
                .map(|value| value.as_str()?.parse::<CaptureProvider>().ok())
                .collect::<Option<Vec<_>>>()?;
            providers.sort_unstable_by_key(|provider| provider.as_str());
            providers.dedup();
            Some(providers)
        });
        Self {
            state,
            trigger: Trigger::parse(&job["trigger"]),
            provenance: Provenance::parse(&job["trigger_provenance"]),
            stage: Stage::parse(&progress["whole_run_stage"]),
            reason: RefreshReason::parse(&job["reason"]),
            providers,
            processed_records: (state == RefreshState::Running)
                .then(|| counter(&progress["completed_records"]))
                .flatten(),
            processed_bytes: (state == RefreshState::Running)
                .then(|| counter(&progress["completed_bytes"]))
                .flatten(),
            scanned_routes: counter(&job["scanned_routes"]),
            certified_source_count: counter(&job["certified_source_count"]),
            certified_source_bytes: counter(&job["certified_source_bytes"]),
            generation_changed: job["generation_changed"].as_bool(),
            timings: TIMINGS.map(|key| counter(&job["timings_us"][key])),
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "state": self.state.as_str(), "trigger": self.trigger.as_str(),
            "trigger_provenance": self.provenance.as_str(), "stage": self.stage.as_str(),
            "reason": self.reason.as_str(), "providers": self.providers,
            "processed_records": self.processed_records, "processed_bytes": self.processed_bytes,
            "scanned_routes": self.scanned_routes, "certified_source_count": self.certified_source_count,
            "certified_source_bytes": self.certified_source_bytes, "generation_changed": self.generation_changed,
            "timings_us": TIMINGS.into_iter().zip(self.timings).map(|(key, value)| (key.to_owned(), json!(value))).collect::<serde_json::Map<_, _>>(),
        })
    }

    pub fn active(&self) -> bool {
        matches!(
            self.state,
            RefreshState::AdmissionPending | RefreshState::Queued | RefreshState::Running
        )
    }

    pub fn work_description(&self) -> String {
        let stage = match self.state {
            RefreshState::AdmissionPending => "waiting for admission".to_owned(),
            RefreshState::Queued => "queued".to_owned(),
            _ => self.stage.as_str().replace('_', " "),
        };
        let cause = match self.provenance {
            Provenance::Setup => "setup command",
            Provenance::Import => "import command",
            Provenance::AutomaticProvider | Provenance::AutomaticProviderRefresh => {
                "automatic provider refresh"
            }
            Provenance::ExplicitSources => "explicit source selection",
            Provenance::HistorySourcePlugin => "history source plugin",
            Provenance::CommitPayload => "publication recovery",
            Provenance::Autostart => "automatic daemon start",
            Provenance::Scheduler => "scheduled refresh",
            _ => match self.trigger {
                Trigger::Setup => "setup request",
                Trigger::Import => "import request",
                Trigger::Search => "search request",
                Trigger::Periodic => "scheduled refresh",
                Trigger::Recovery => "recovery request",
                Trigger::Unknown => "cause unknown",
            },
        };
        format!("{stage}; {cause}")
    }

    pub fn provider_names(&self) -> Option<String> {
        self.providers.as_ref().map(|providers| {
            providers
                .iter()
                .map(|provider| provider.display_name())
                .collect::<Vec<_>>()
                .join(", ")
        })
    }
}

/// No paths, identifiers, arbitrary strings, or raw JSON survive this boundary.
#[derive(Debug)]
pub struct StatusDiagnosticReport {
    ctx_version: &'static str,
    requested_seconds: u64,
    elapsed_ms: Option<u64>,
    daemon: DaemonState,
    heartbeat_age_ms: Option<u64>,
    refresh: RefreshActivity,
    blame_writer_observed: Option<bool>,
    blame_phase: BlamePhase,
    blame_counters: [Option<u64>; 3],
    semantic_status: SemanticJobStatus,
    semantic_work_remaining: Option<bool>,
    cpu_status: MeasurementStatus,
    cpu_elapsed_ms: Option<u64>,
    cpu_ms: Option<f64>,
    cpu_percent: Option<f64>,
    scheduler_status: MeasurementStatus,
    counters: [Option<u64>; 7],
    cycles_per_second: Option<f64>,
}

impl StatusDiagnosticReport {
    pub fn from_observation(requested_seconds: u64, observation: &Value) -> Self {
        let cpu = &observation["observed"]["cpu"];
        let scheduler = &observation["observed"]["scheduler"];
        let cpu_status = MeasurementStatus::parse(&cpu["status"]);
        let scheduler_status = MeasurementStatus::parse(&scheduler["status"]);
        let cpu_observed = cpu_status == MeasurementStatus::Observed;
        let scheduler_observed = scheduler_status == MeasurementStatus::Observed;
        let blame = &observation["blame"];
        let blame_writer_observed = blame["writer_observed"].as_bool();
        let blame_phase = if blame_writer_observed == Some(true) {
            BlamePhase::parse(&blame["phase"])
        } else {
            BlamePhase::Unknown
        };
        let blame_snapshot = !matches!(
            blame_phase,
            BlamePhase::Unknown | BlamePhase::SnapshotUnavailable
        );
        Self {
            ctx_version: "unknown",
            requested_seconds,
            elapsed_ms: counter(&observation["elapsed_ms"]),
            daemon: DaemonState::parse(&observation["daemon"]["state"]),
            heartbeat_age_ms: counter(&observation["daemon"]["heartbeat_age_ms"]),
            refresh: RefreshActivity::from_job(&observation["refresh"]),
            blame_writer_observed,
            blame_phase,
            blame_counters: BLAME_COUNTERS
                .map(|key| blame_snapshot.then(|| counter(&blame[key])).flatten()),
            semantic_status: SemanticJobStatus::parse(&observation["semantic"]["status"]),
            semantic_work_remaining: observation["semantic"]["source_work_remaining"].as_bool(),
            cpu_status,
            cpu_elapsed_ms: cpu_observed.then(|| counter(&cpu["elapsed_ms"])).flatten(),
            cpu_ms: cpu_observed
                .then(|| finite_number(&cpu["cpu_ms"]))
                .flatten(),
            cpu_percent: cpu_observed
                .then(|| finite_number(&cpu["percent_one_core"]))
                .flatten(),
            scheduler_status,
            counters: COUNTERS.map(|key| {
                scheduler_observed
                    .then(|| counter(&scheduler[key]))
                    .flatten()
            }),
            cycles_per_second: (scheduler_observed && counter(&scheduler["work_cycles"]).is_some())
                .then(|| finite_number(&scheduler["work_cycles_per_second"]))
                .flatten(),
        }
    }

    /// The reporting CLI supplies its compiled product version, never a value
    /// from persisted daemon metadata or this library's own package version.
    pub fn with_product_identity(mut self, identity: ProductBuildIdentity) -> Self {
        self.ctx_version = identity.version();
        self
    }

    pub fn to_json(&self) -> Value {
        let mut scheduler: serde_json::Map<String, Value> = COUNTERS
            .into_iter()
            .zip(self.counters)
            .map(|(key, value)| (key.to_owned(), json!(value)))
            .collect();
        scheduler.insert("status".to_owned(), json!(self.scheduler_status.as_str()));
        scheduler.insert(
            "work_cycles_per_second".to_owned(),
            json!(self.cycles_per_second),
        );
        json!({
            "schema_version": 1,
            "ctx_version": self.ctx_version, "os": OS, "arch": ARCH,
            "sample": {"requested_seconds": self.requested_seconds, "elapsed_ms": self.elapsed_ms, "observations": 2},
            "daemon": {"state": self.daemon.as_str(), "heartbeat_age_ms": self.heartbeat_age_ms},
            "refresh": self.refresh.to_json(),
            "blame": {
                "writer_observed": self.blame_writer_observed, "phase": self.blame_phase.as_str(),
                "completed_sources": self.blame_counters[0], "total_sources": self.blame_counters[1],
                "applied_changes": self.blame_counters[2],
            },
            "semantic": {"last_recorded_status": self.semantic_status.as_str(), "work_remaining": self.semantic_work_remaining},
            "observed": {
                "cpu": {"status": self.cpu_status.as_str(), "elapsed_ms": self.cpu_elapsed_ms, "cpu_ms": self.cpu_ms, "percent_one_core": self.cpu_percent},
                "scheduler": scheduler,
            },
        })
    }
}
