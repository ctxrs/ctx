use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum DesktopWebviewSurface {
    Main,
    Workbench,
    Launcher,
    Settings,
    FilePreview,
    WorkspaceSetup,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum DesktopWebviewRecoveryTriggerKind {
    NativeProcessTermination,
    HeartbeatTimeout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum DesktopWebviewRecoveryAction {
    Noop,
    Reload,
    Recreate,
    PromptRestart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum DesktopWebviewRecoveryDaemonHealth {
    Unknown,
    Ok,
    Down,
    Mismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum DesktopWebviewRecoverySuppressionReason {
    RecoveryInProgress,
    WindowNotVisible,
    WindowNotFocused,
    StartupGrace,
    NoHeartbeatYet,
    DaemonDown,
    DaemonMismatch,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
pub struct DesktopWebviewRecoveryIncident {
    pub incident_id: String,
    pub window_label: String,
    pub window_surface: DesktopWebviewSurface,
    pub route: String,
    pub trigger_kind: DesktopWebviewRecoveryTriggerKind,
    pub action: DesktopWebviewRecoveryAction,
    pub daemon_health: DesktopWebviewRecoveryDaemonHealth,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub suppression_reason: Option<DesktopWebviewRecoverySuppressionReason>,
    #[ts(type = "number")]
    pub created_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopWebviewRecoveryHeartbeatReq {
    pub route: String,
    #[serde(default)]
    pub document_visible: bool,
    #[serde(default)]
    pub window_focused: bool,
    #[serde(default)]
    pub startup_ready: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum DesktopWebviewRecoveryFaultKind {
    NativeProcessTermination,
    HeartbeatTimeout,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopWebviewRecoveryFaultReq {
    #[serde(default)]
    #[ts(optional = nullable)]
    pub window_label: Option<String>,
    pub kind: DesktopWebviewRecoveryFaultKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopWebviewRecoveryWindowAutomationSnapshot {
    pub window_label: String,
    pub window_surface: DesktopWebviewSurface,
    pub route: String,
    #[serde(default)]
    #[ts(type = "number", optional = nullable)]
    pub last_heartbeat_at_ms: Option<u64>,
    #[serde(default)]
    #[ts(type = "number", optional = nullable)]
    pub startup_completed_at_ms: Option<u64>,
    pub recovery_in_progress: bool,
    pub consecutive_recovery_count: u32,
    pub pending_heartbeat_timeout: bool,
    pub daemon_health: DesktopWebviewRecoveryDaemonHealth,
    pub recent_incident_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopWebviewRecoveryAutomationSnapshot {
    pub windows: Vec<DesktopWebviewRecoveryWindowAutomationSnapshot>,
    pub pending_incident_count: u32,
}
