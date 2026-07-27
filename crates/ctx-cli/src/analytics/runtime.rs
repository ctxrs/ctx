use std::time::Duration;

use serde_json::{Map, Value};

use super::{
    duration_bucket, DaemonRuntimeObservationV1, DurationBucket, McpRuntimeObservationV1, Outcome,
    Surface,
};

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeObservationKindV1 {
    Daemon(DaemonRuntimeObservationV1),
    Mcp(McpRuntimeObservationV1),
}

impl RuntimeObservationKindV1 {
    pub(crate) fn surface(self) -> Surface {
        match self {
            Self::Daemon(_) => Surface::Daemon,
            Self::Mcp(_) => Surface::Mcp,
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Daemon(observation) => observation.name(),
            Self::Mcp(observation) => observation.name(),
        }
    }

    pub(crate) fn insert_properties(self, properties: &mut Map<String, Value>) {
        match self {
            Self::Daemon(observation) => observation.insert_properties(properties),
            Self::Mcp(observation) => observation.insert_properties(properties),
        }
    }
}

#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct RuntimeObservationV1 {
    pub(crate) kind: RuntimeObservationKindV1,
    pub(crate) outcome: Outcome,
    pub(crate) duration: DurationBucket,
}

#[allow(dead_code)]
impl RuntimeObservationV1 {
    pub(crate) fn daemon(
        kind: DaemonRuntimeObservationV1,
        outcome: Outcome,
        duration: Duration,
    ) -> Self {
        Self {
            kind: RuntimeObservationKindV1::Daemon(kind),
            outcome,
            duration: duration_bucket(duration),
        }
    }

    pub(crate) fn mcp(kind: McpRuntimeObservationV1, outcome: Outcome, duration: Duration) -> Self {
        Self {
            kind: RuntimeObservationKindV1::Mcp(kind),
            outcome,
            duration: duration_bucket(duration),
        }
    }
}
