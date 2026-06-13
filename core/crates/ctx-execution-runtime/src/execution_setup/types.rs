use super::*;

#[cfg(test)]
pub(super) fn sandbox_cli_env_test_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

pub(super) fn runtime_prewarm_ready_phase_message(
    runtime_requested: bool,
    runtime_kind: &crate::ContainerRuntimeKind,
    launch_ready: bool,
) -> &'static str {
    if runtime_requested {
        ctx_harness_runtime::runtime_prewarm_ready_message(runtime_kind, launch_ready)
    } else {
        "container builder is ready"
    }
}

#[cfg(test)]
mod ready_message_tests {
    use super::runtime_prewarm_ready_phase_message;
    use crate::ContainerRuntimeKind;

    #[test]
    fn runtime_prewarm_ready_phase_message_uses_runtime_specific_semantics() {
        assert_eq!(
            runtime_prewarm_ready_phase_message(
                true,
                &ContainerRuntimeKind::SharedVmContainer,
                false,
            ),
            "shared VM runtime artifacts are ready; launch image loads when the shared VM starts"
        );
        assert_eq!(
            runtime_prewarm_ready_phase_message(
                true,
                &ContainerRuntimeKind::SharedVmContainer,
                true,
            ),
            "shared VM substrate and launch image are ready"
        );
        assert_eq!(
            runtime_prewarm_ready_phase_message(true, &ContainerRuntimeKind::NativeContainer, true),
            "local sandbox runtime and launch image are ready"
        );
    }

    #[test]
    fn runtime_prewarm_ready_phase_message_preserves_builder_message() {
        assert_eq!(
            runtime_prewarm_ready_phase_message(
                false,
                &ContainerRuntimeKind::SharedVmContainer,
                false,
            ),
            "container builder is ready"
        );
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionSetupJobKind {
    StartupPrewarm,
    WorkspaceLaunch,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RuntimePrewarmScope {
    #[default]
    Runtime,
    LaunchReady,
    Builder,
    All,
}

impl RuntimePrewarmScope {
    pub(crate) fn includes_runtime(self) -> bool {
        matches!(self, Self::Runtime | Self::LaunchReady | Self::All)
    }

    pub(crate) fn includes_builder(self) -> bool {
        matches!(self, Self::Builder | Self::All)
    }

    pub(crate) fn requires_launch_ready_runtime(self) -> bool {
        matches!(self, Self::LaunchReady | Self::All)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionLaunchState {
    Running,
    Ready,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutionLaunchLogLine {
    pub seq: u64,
    pub ts: String,
    pub phase: HarnessSetupPhase,
    pub level: HarnessSetupLogLevel,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutionLaunchPhaseStatus {
    pub phase: HarnessSetupPhase,
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionLaunchSnapshot {
    pub job_id: String,
    pub workspace_id: String,
    pub kind: ExecutionSetupJobKind,
    pub state: ExecutionLaunchState,
    pub created_at: String,
    pub started_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_phase: Option<HarnessSetupPhase>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_step_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress_pct: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eta_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_download: Option<HarnessSetupDownloadStatus>,
    pub phases: Vec<ExecutionLaunchPhaseStatus>,
    pub logs: Vec<ExecutionLaunchLogLine>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExecutionLaunchStreamEvent {
    LaunchSnapshot {
        snapshot: ExecutionLaunchSnapshot,
    },
    LaunchLog {
        job_id: String,
        line: ExecutionLaunchLogLine,
    },
    LaunchComplete {
        snapshot: ExecutionLaunchSnapshot,
    },
    LaunchError {
        snapshot: ExecutionLaunchSnapshot,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum StartupPrewarmState {
    #[default]
    Idle,
    Running,
    Ready,
    Error,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StartupPrewarmSnapshot {
    pub state: StartupPrewarmState,
    pub target_image: String,
    pub needs_prewarm: bool,
    pub machine_ready: bool,
    pub image_present: bool,
    pub image_ref_changed: bool,
    pub bundled_image_digest_changed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_attempt_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_success_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
