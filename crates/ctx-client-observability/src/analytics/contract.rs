use super::{
    GraphCompletedV1, HostedOperationCompletedV1, OperationCompletedV1, ProductRuntimeV1,
    ProviderRefreshCompletedV1, RemoteCompletedV1, RuntimeObservationV1, ServerRequestCompletedV1,
    ServerSummaryV1, SharingSummaryV1, SiftSummaryV1,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    Cli,
    Mcp,
    Daemon,
    Server,
}

impl Surface {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Mcp => "mcp",
            Self::Daemon => "daemon",
            Self::Server => "server",
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
    GraphCompleted(GraphCompletedV1),
    RemoteCompleted(RemoteCompletedV1),
    ServerRequestCompleted(ServerRequestCompletedV1),
    ProductRuntime(ProductRuntimeV1),
    SiftSummary(SiftSummaryV1),
    ServerSummary(ServerSummaryV1),
    SharingSummary(SharingSummaryV1),
    OperationCompleted(OperationCompletedV1),
    /// A finite hosted command, serialized as ordinary `operation_completed@1`.
    HostedOperationCompleted(HostedOperationCompletedV1),
    ProviderRefreshCompleted(ProviderRefreshCompletedV1),
    RuntimeObservation(RuntimeObservationV1),
}
