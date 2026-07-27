use super::{OperationCompletedV1, ProviderRefreshCompletedV1, RuntimeObservationV1};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Surface {
    Cli,
    Mcp,
    ProHost,
    Daemon,
}

impl Surface {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Mcp => "mcp",
            Self::ProHost => "pro_host",
            Self::Daemon => "daemon",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    Success,
    Failure,
}

impl Outcome {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutputKind {
    Human,
    Json,
}

impl OutputKind {
    pub(crate) fn from_json_output(json_output: bool) -> Self {
        if json_output {
            Self::Json
        } else {
            Self::Human
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Json => "json",
        }
    }
}

#[derive(Debug)]
pub(crate) enum PublicEventV1 {
    OperationCompleted(OperationCompletedV1),
    ProviderRefreshCompleted(ProviderRefreshCompletedV1),
    RuntimeObservation(RuntimeObservationV1),
}
