use serde_json::{Map, Value};

use super::CountBucket;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonStartModeV1 {
    Manual,
    Auto,
}

impl DaemonStartModeV1 {
    fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Auto => "auto",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonSupervisorV1 {
    User,
    CliAutostart,
}

impl DaemonSupervisorV1 {
    fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::CliAutostart => "cli_autostart",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonTriggerV1 {
    Setup,
    Import,
    Search,
    Semantic,
}

impl DaemonTriggerV1 {
    fn as_str(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Import => "import",
            Self::Search => "search",
            Self::Semantic => "semantic",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DaemonRunFactsV1 {
    start_mode: DaemonStartModeV1,
    supervisor: DaemonSupervisorV1,
    trigger: Option<DaemonTriggerV1>,
}

impl DaemonRunFactsV1 {
    pub fn new(
        start_mode: DaemonStartModeV1,
        supervisor: DaemonSupervisorV1,
        trigger: Option<DaemonTriggerV1>,
    ) -> Self {
        Self {
            start_mode,
            supervisor,
            trigger,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonCycleResultV1 {
    Work,
    NoWork,
    Failure,
}

impl DaemonCycleResultV1 {
    fn as_str(self) -> &'static str {
        match self {
            Self::Work => "work",
            Self::NoWork => "no_work",
            Self::Failure => "failure",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonHistoryFreshnessV1 {
    Current,
    Backoff,
    Failed,
    Unknown,
}

impl DaemonHistoryFreshnessV1 {
    fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Backoff => "backoff",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonBacklogV1 {
    Unknown,
}

impl DaemonBacklogV1 {
    fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonCoverageV1 {
    Unknown,
}

impl DaemonCoverageV1 {
    fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonBackoffV1 {
    None,
    History,
}

impl DaemonBackoffV1 {
    fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::History => "history",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DaemonCycleStateV1 {
    history_freshness: DaemonHistoryFreshnessV1,
    semantic_backlog: DaemonBacklogV1,
    semantic_coverage: DaemonCoverageV1,
    retry_backoff: DaemonBackoffV1,
}

impl DaemonCycleStateV1 {
    pub fn new(
        history_freshness: DaemonHistoryFreshnessV1,
        semantic_backlog: DaemonBacklogV1,
        semantic_coverage: DaemonCoverageV1,
        retry_backoff: DaemonBackoffV1,
    ) -> Self {
        Self {
            history_freshness,
            semantic_backlog,
            semantic_coverage,
            retry_backoff,
        }
    }

    pub fn unknown() -> Self {
        Self::new(
            DaemonHistoryFreshnessV1::Unknown,
            DaemonBacklogV1::Unknown,
            DaemonCoverageV1::Unknown,
            DaemonBackoffV1::None,
        )
    }

    pub fn retry_backoff(self) -> DaemonBackoffV1 {
        self.retry_backoff
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DaemonCycleFactsV1 {
    run: DaemonRunFactsV1,
    result: DaemonCycleResultV1,
    coalesced_cycles: CountBucket,
    state: DaemonCycleStateV1,
}

impl DaemonCycleFactsV1 {
    pub fn new(
        run: DaemonRunFactsV1,
        result: DaemonCycleResultV1,
        coalesced_cycles: CountBucket,
        state: DaemonCycleStateV1,
    ) -> Self {
        Self {
            run,
            result,
            coalesced_cycles,
            state,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DaemonRuntimeSnapshotV1 {
    run: DaemonRunFactsV1,
    state: DaemonCycleStateV1,
}

impl DaemonRuntimeSnapshotV1 {
    pub fn new(run: DaemonRunFactsV1, state: DaemonCycleStateV1) -> Self {
        Self { run, state }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonOperationV1 {
    Enable,
    Disable,
    Status,
}

impl DaemonOperationV1 {
    pub fn name(self) -> &'static str {
        match self {
            Self::Enable => "enable",
            Self::Disable => "disable",
            Self::Status => "status",
        }
    }

    pub fn insert_properties(self, _properties: &mut Map<String, Value>) {}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DaemonRuntimeKindV1 {
    Ready,
    Stopped,
    Recovered,
    Failed,
    Cycle,
    Liveness,
}

impl DaemonRuntimeKindV1 {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Stopped => "stopped",
            Self::Recovered => "recovered",
            Self::Failed => "failed",
            Self::Cycle => "cycle",
            Self::Liveness => "liveness",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DaemonRuntimePayloadV1 {
    Ready(DaemonRunFactsV1),
    Stopped(DaemonRuntimeSnapshotV1),
    Recovered(DaemonRuntimeSnapshotV1),
    Failed(DaemonRuntimeSnapshotV1),
    Cycle(DaemonCycleFactsV1),
    Liveness(DaemonRuntimeSnapshotV1),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DaemonRuntimeObservationV1(DaemonRuntimePayloadV1);

impl DaemonRuntimeObservationV1 {
    pub fn ready(run: DaemonRunFactsV1) -> Self {
        Self(DaemonRuntimePayloadV1::Ready(run))
    }

    pub fn stopped(snapshot: DaemonRuntimeSnapshotV1) -> Self {
        Self(DaemonRuntimePayloadV1::Stopped(snapshot))
    }

    pub fn recovered(snapshot: DaemonRuntimeSnapshotV1) -> Self {
        Self(DaemonRuntimePayloadV1::Recovered(snapshot))
    }

    pub fn failed(snapshot: DaemonRuntimeSnapshotV1) -> Self {
        Self(DaemonRuntimePayloadV1::Failed(snapshot))
    }

    pub fn cycle(cycle: DaemonCycleFactsV1) -> Self {
        Self(DaemonRuntimePayloadV1::Cycle(cycle))
    }

    pub fn liveness(snapshot: DaemonRuntimeSnapshotV1) -> Self {
        Self(DaemonRuntimePayloadV1::Liveness(snapshot))
    }

    pub fn name(self) -> &'static str {
        match self.0 {
            DaemonRuntimePayloadV1::Ready(_) => DaemonRuntimeKindV1::Ready.as_str(),
            DaemonRuntimePayloadV1::Stopped(_) => DaemonRuntimeKindV1::Stopped.as_str(),
            DaemonRuntimePayloadV1::Recovered(_) => DaemonRuntimeKindV1::Recovered.as_str(),
            DaemonRuntimePayloadV1::Failed(_) => DaemonRuntimeKindV1::Failed.as_str(),
            DaemonRuntimePayloadV1::Cycle(_) => DaemonRuntimeKindV1::Cycle.as_str(),
            DaemonRuntimePayloadV1::Liveness(_) => DaemonRuntimeKindV1::Liveness.as_str(),
        }
    }

    pub fn insert_properties(self, properties: &mut Map<String, Value>) {
        match self.0 {
            DaemonRuntimePayloadV1::Ready(run) => insert_run_properties(properties, run),
            DaemonRuntimePayloadV1::Stopped(snapshot)
            | DaemonRuntimePayloadV1::Recovered(snapshot)
            | DaemonRuntimePayloadV1::Failed(snapshot)
            | DaemonRuntimePayloadV1::Liveness(snapshot) => {
                insert_snapshot_properties(properties, snapshot)
            }
            DaemonRuntimePayloadV1::Cycle(cycle) => {
                insert_run_properties(properties, cycle.run);
                insert_state_properties(properties, cycle.state);
                insert_str(properties, "cycle_result", cycle.result.as_str());
                insert_str(
                    properties,
                    "coalesced_cycles_bucket",
                    cycle.coalesced_cycles.as_str(),
                );
            }
        }
    }
}

fn insert_snapshot_properties(
    properties: &mut Map<String, Value>,
    snapshot: DaemonRuntimeSnapshotV1,
) {
    insert_run_properties(properties, snapshot.run);
    insert_state_properties(properties, snapshot.state);
}

fn insert_run_properties(properties: &mut Map<String, Value>, run: DaemonRunFactsV1) {
    insert_str(properties, "start_mode", run.start_mode.as_str());
    insert_str(properties, "supervisor", run.supervisor.as_str());
    if let Some(trigger) = run.trigger {
        insert_str(properties, "trigger_command", trigger.as_str());
    }
}

fn insert_state_properties(properties: &mut Map<String, Value>, state: DaemonCycleStateV1) {
    insert_str(
        properties,
        "history_freshness",
        state.history_freshness.as_str(),
    );
    insert_str(
        properties,
        "semantic_backlog_bucket",
        state.semantic_backlog.as_str(),
    );
    insert_str(
        properties,
        "semantic_coverage",
        state.semantic_coverage.as_str(),
    );
    insert_str(properties, "retry_backoff", state.retry_backoff.as_str());
}

fn insert_str(properties: &mut Map<String, Value>, key: &'static str, value: &'static str) {
    properties.insert(key.to_owned(), Value::String(value.to_owned()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auto_search_run() -> DaemonRunFactsV1 {
        DaemonRunFactsV1::new(
            DaemonStartModeV1::Auto,
            DaemonSupervisorV1::CliAutostart,
            Some(DaemonTriggerV1::Search),
        )
    }

    #[test]
    fn daemon_operation_properties_are_closed_and_constant_keyed() {
        for (operation, expected) in [
            (DaemonOperationV1::Enable, "enable"),
            (DaemonOperationV1::Disable, "disable"),
            (DaemonOperationV1::Status, "status"),
        ] {
            let mut properties = Map::new();
            operation.insert_properties(&mut properties);
            assert_eq!(operation.name(), expected);
            assert!(properties.is_empty());
        }
    }

    #[test]
    fn daemon_cycle_properties_are_aggregate_and_bucketed() {
        let state = DaemonCycleStateV1::new(
            DaemonHistoryFreshnessV1::Backoff,
            DaemonBacklogV1::Unknown,
            DaemonCoverageV1::Unknown,
            DaemonBackoffV1::History,
        );
        let cycle = DaemonRuntimeObservationV1::cycle(DaemonCycleFactsV1::new(
            auto_search_run(),
            DaemonCycleResultV1::NoWork,
            CountBucket::SixToTwenty,
            state,
        ));
        let mut properties = Map::new();
        cycle.insert_properties(&mut properties);
        assert_eq!(cycle.name(), "cycle");
        assert_eq!(properties["cycle_result"], "no_work");
        assert_eq!(properties["coalesced_cycles_bucket"], "6-20");
        assert_eq!(properties["history_freshness"], "backoff");
        assert_eq!(properties["semantic_backlog_bucket"], "unknown");
        assert_eq!(properties["semantic_coverage"], "unknown");
        assert_eq!(properties["retry_backoff"], "history");
        assert_eq!(properties.len(), 9);
    }
}
