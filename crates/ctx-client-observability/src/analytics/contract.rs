use super::{
    HostedOperationCompletedV1, OperationCompletedV1, ProviderRefreshCompletedV1,
    RuntimeObservationV1,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    Cli,
    Mcp,
    Daemon,
}

impl Surface {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Mcp => "mcp",
            Self::Daemon => "daemon",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Success,
    Failure,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputKind {
    Human,
    Json,
}

impl OutputKind {
    pub fn from_json_output(json_output: bool) -> Self {
        if json_output {
            Self::Json
        } else {
            Self::Human
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Json => "json",
        }
    }
}

#[derive(Debug)]
pub enum PublicEventV1 {
    OperationCompleted(OperationCompletedV1),
    /// A finite hosted command, serialized as ordinary `operation_completed@1`.
    HostedOperationCompleted(HostedOperationCompletedV1),
    ProviderRefreshCompleted(ProviderRefreshCompletedV1),
    RuntimeObservation(RuntimeObservationV1),
}
