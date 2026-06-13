use ctx_desktop_ipc::{
    DesktopWebviewRecoveryAutomationSnapshot, DesktopWebviewRecoveryWindowAutomationSnapshot,
};

use super::super::policy::prune_recent_incidents;
use super::*;

impl DesktopWebviewRecoveryController {
    pub(in crate::desktop_webview_recovery) fn automation_snapshot(
        &self,
    ) -> DesktopWebviewRecoveryAutomationSnapshot {
        let Ok(mut guard) = self.inner.lock() else {
            return DesktopWebviewRecoveryAutomationSnapshot {
                windows: Vec::new(),
                pending_incident_count: 0,
            };
        };
        let now = now_ms();
        let mut windows = Vec::with_capacity(guard.windows.len());
        for state in guard.windows.values_mut() {
            let recent_incident_count =
                prune_recent_incidents(&mut state.recent_incident_timestamps_ms, now);
            windows.push(DesktopWebviewRecoveryWindowAutomationSnapshot {
                window_label: state.window_label.clone(),
                window_surface: state.surface,
                route: state.route.clone(),
                last_heartbeat_at_ms: state.last_heartbeat_at_ms,
                startup_completed_at_ms: state.startup_completed_at_ms,
                recovery_in_progress: state.recovery_in_progress,
                consecutive_recovery_count: recent_incident_count as u32,
                pending_heartbeat_timeout: state.pending_heartbeat_timeout,
                daemon_health: state.daemon_health,
                recent_incident_count: recent_incident_count as u32,
            });
        }
        windows.sort_by(|left, right| left.window_label.cmp(&right.window_label));
        let pending_incident_count = windows
            .iter()
            .filter(|window| window.pending_heartbeat_timeout || window.recovery_in_progress)
            .count() as u32;
        DesktopWebviewRecoveryAutomationSnapshot {
            windows,
            pending_incident_count,
        }
    }
}
