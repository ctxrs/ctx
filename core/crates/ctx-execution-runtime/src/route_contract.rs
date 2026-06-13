use crate::{ExecutionSetupJobKind, RuntimePrewarmScope};

pub use ctx_linux_sandbox_runtime::{
    LinuxSandboxActivationMode, LinuxSandboxRuntimePrepareResult, LinuxSandboxRuntimeStatus,
};

#[derive(Debug, serde::Deserialize)]
pub struct StartExecutionLaunchRequest {
    #[serde(default)]
    pub kind: Option<ExecutionSetupJobKind>,
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub prewarm_scope: RuntimePrewarmScope,
}

#[derive(Debug)]
pub enum StartExecutionLaunchError {
    MissingWorkspaceId,
    InvalidWorkspaceId,
    WorkspaceNotFound,
    MaintenanceActive {
        message: String,
    },
    InvalidWorkspaceExecutionSettings {
        message: String,
        policy_denial: bool,
    },
    Internal {
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxSandboxRuntimeOperation {
    Status,
    Stage,
    Prepare,
}

impl LinuxSandboxRuntimeOperation {
    pub fn user_message(self) -> &'static str {
        match self {
            Self::Status => "Linux sandbox runtime status check failed",
            Self::Stage => "Linux sandbox runtime downloads failed to stage",
            Self::Prepare => "Preparing Linux sandbox runtime failed",
        }
    }
}

#[derive(Debug)]
pub enum LinuxSandboxRuntimeError {
    Runtime {
        operation: LinuxSandboxRuntimeOperation,
        message: String,
    },
    PrepareAlreadyActive,
    PrepareActivityUnavailable {
        message: String,
    },
    PrepareSandboxWorkActive,
}

impl LinuxSandboxRuntimeError {
    pub fn message(&self) -> &str {
        match self {
            Self::Runtime { message, .. } => message,
            Self::PrepareAlreadyActive => {
                "Linux sandbox runtime prepare is already in progress. Retry when current maintenance completes."
            }
            Self::PrepareActivityUnavailable { message } => message,
            Self::PrepareSandboxWorkActive => {
                "Preparing Linux sandbox runtime is blocked while sandbox work is active. Retry when sandbox turns, terminals, containers, and runtime operations are idle."
            }
        }
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct LinuxSandboxRuntimePrepareRequest {
    #[serde(default)]
    pub activation_mode: Option<LinuxSandboxActivationMode>,
    #[serde(default)]
    pub sudo_password: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_sandbox_operation_messages_are_stable() {
        assert_eq!(
            LinuxSandboxRuntimeOperation::Status.user_message(),
            "Linux sandbox runtime status check failed"
        );
        assert_eq!(
            LinuxSandboxRuntimeOperation::Stage.user_message(),
            "Linux sandbox runtime downloads failed to stage"
        );
        assert_eq!(
            LinuxSandboxRuntimeOperation::Prepare.user_message(),
            "Preparing Linux sandbox runtime failed"
        );
    }

    #[test]
    fn linux_sandbox_error_messages_are_stable() {
        assert_eq!(
            LinuxSandboxRuntimeError::Runtime {
                operation: LinuxSandboxRuntimeOperation::Status,
                message: LinuxSandboxRuntimeOperation::Status
                    .user_message()
                    .to_string(),
            }
            .message(),
            "Linux sandbox runtime status check failed"
        );
        assert!(LinuxSandboxRuntimeError::PrepareAlreadyActive
            .message()
            .contains("already in progress"));
        assert!(LinuxSandboxRuntimeError::PrepareSandboxWorkActive
            .message()
            .contains("sandbox work is active"));
    }
}
