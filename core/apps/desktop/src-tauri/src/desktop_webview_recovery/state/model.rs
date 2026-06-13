use std::collections::{HashMap, VecDeque};

use ctx_desktop_ipc::{
    DesktopWebviewRecoveryAction, DesktopWebviewRecoveryDaemonHealth,
    DesktopWebviewRecoveryIncident, DesktopWebviewSurface,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeartbeatTimeoutEvaluation {
    Skip,
    AwaitConfirmation,
    Ready,
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedRecoveryIncident {
    pub incident: DesktopWebviewRecoveryIncident,
    pub action: DesktopWebviewRecoveryAction,
}

#[derive(Debug, Default)]
pub(crate) struct DesktopWebviewRecoveryState {
    pub(crate) windows: HashMap<String, RecoveryWindowState>,
}

#[derive(Debug, Clone)]
pub(crate) struct RecoveryWindowState {
    pub(crate) created_at_ms: u64,
    pub(crate) daemon_health: DesktopWebviewRecoveryDaemonHealth,
    pub(crate) exists: bool,
    pub(crate) last_suppressed_at_ms: Option<u64>,
    pub(crate) last_heartbeat_at_ms: Option<u64>,
    pub(crate) pending_heartbeat_timeout: bool,
    pub(crate) recent_incident_timestamps_ms: VecDeque<u64>,
    pub(crate) recovery_in_progress: bool,
    pub(crate) route: String,
    pub(crate) stale_detected_at_ms: Option<u64>,
    pub(crate) startup_completed_at_ms: Option<u64>,
    pub(crate) surface: DesktopWebviewSurface,
    pub(crate) window_label: String,
}

impl RecoveryWindowState {
    pub(crate) fn new(
        window_label: &str,
        surface: DesktopWebviewSurface,
        route: &str,
        created_at_ms: u64,
    ) -> Self {
        Self {
            created_at_ms,
            daemon_health: DesktopWebviewRecoveryDaemonHealth::Unknown,
            exists: true,
            last_suppressed_at_ms: None,
            last_heartbeat_at_ms: None,
            pending_heartbeat_timeout: false,
            recent_incident_timestamps_ms: VecDeque::new(),
            recovery_in_progress: false,
            route: normalize_route(route),
            stale_detected_at_ms: None,
            startup_completed_at_ms: None,
            surface,
            window_label: window_label.to_string(),
        }
    }
}

pub(crate) fn normalize_route(route: &str) -> String {
    let trimmed = route.trim();
    if trimmed.is_empty() {
        "/".to_string()
    } else {
        trimmed.to_string()
    }
}
