use ctx_desktop_ipc::{
    DesktopWebviewRecoveryAction, DesktopWebviewRecoveryDaemonHealth,
    DesktopWebviewRecoveryIncident, DesktopWebviewRecoverySuppressionReason,
    DesktopWebviewRecoveryTriggerKind,
};

use super::super::policy::{
    decide_recovery_action, prune_recent_incidents, HEARTBEAT_TIMEOUT_MS, STARTUP_GRACE_MS,
};
use super::model::PreparedRecoveryIncident;
use super::*;

impl DesktopWebviewRecoveryController {
    pub(in crate::desktop_webview_recovery) fn prepare_incident(
        &self,
        window_label: &str,
        trigger_kind: DesktopWebviewRecoveryTriggerKind,
        daemon_health: DesktopWebviewRecoveryDaemonHealth,
        suppression_reason: Option<DesktopWebviewRecoverySuppressionReason>,
        created_at_ms: u64,
        force: bool,
    ) -> Option<PreparedRecoveryIncident> {
        let Ok(mut guard) = self.inner.lock() else {
            return None;
        };
        let entry = guard.windows.get_mut(window_label)?;
        if !entry.exists || entry.recovery_in_progress {
            return None;
        }

        if trigger_kind == DesktopWebviewRecoveryTriggerKind::HeartbeatTimeout && !force {
            if !entry.pending_heartbeat_timeout {
                return None;
            }
            if entry.startup_completed_at_ms.is_none()
                && created_at_ms.saturating_sub(entry.created_at_ms) < STARTUP_GRACE_MS
            {
                entry.pending_heartbeat_timeout = false;
                entry.stale_detected_at_ms = None;
                return None;
            }
            let last_heartbeat_at_ms = entry.last_heartbeat_at_ms.unwrap_or(entry.created_at_ms);
            if created_at_ms.saturating_sub(last_heartbeat_at_ms) < HEARTBEAT_TIMEOUT_MS {
                entry.pending_heartbeat_timeout = false;
                entry.stale_detected_at_ms = None;
                return None;
            }
        }
        let recent_incident_count =
            prune_recent_incidents(&mut entry.recent_incident_timestamps_ms, created_at_ms);
        let decision_daemon_health = match trigger_kind {
            DesktopWebviewRecoveryTriggerKind::NativeProcessTermination => {
                DesktopWebviewRecoveryDaemonHealth::Ok
            }
            DesktopWebviewRecoveryTriggerKind::HeartbeatTimeout => daemon_health,
        };
        let action = decide_recovery_action(
            recent_incident_count,
            decision_daemon_health,
            suppression_reason,
        );
        entry.daemon_health = daemon_health;
        if trigger_kind == DesktopWebviewRecoveryTriggerKind::HeartbeatTimeout {
            entry.pending_heartbeat_timeout = true;
        }
        if action != DesktopWebviewRecoveryAction::Noop {
            entry.recovery_in_progress = true;
            entry.recent_incident_timestamps_ms.push_back(created_at_ms);
        }
        let incident = DesktopWebviewRecoveryIncident {
            incident_id: uuid::Uuid::new_v4().to_string(),
            window_label: entry.window_label.clone(),
            window_surface: entry.surface,
            route: entry.route.clone(),
            trigger_kind,
            action,
            daemon_health,
            suppression_reason,
            created_at_ms,
        };
        Some(PreparedRecoveryIncident { incident, action })
    }

    pub(in crate::desktop_webview_recovery) fn finish_recovery_action(
        &self,
        window_label: &str,
        trigger_kind: DesktopWebviewRecoveryTriggerKind,
        action: DesktopWebviewRecoveryAction,
        finished_at_ms: u64,
    ) {
        let Ok(mut guard) = self.inner.lock() else {
            return;
        };
        let Some(entry) = guard.windows.get_mut(window_label) else {
            return;
        };
        entry.recovery_in_progress = false;
        entry.pending_heartbeat_timeout = false;
        match (trigger_kind, action) {
            (
                DesktopWebviewRecoveryTriggerKind::HeartbeatTimeout,
                DesktopWebviewRecoveryAction::Noop,
            ) => {
                entry.last_suppressed_at_ms = Some(finished_at_ms);
                entry.stale_detected_at_ms = None;
            }
            (_, DesktopWebviewRecoveryAction::Noop) => {}
            (_, _) => {
                entry.created_at_ms = finished_at_ms;
                entry.last_suppressed_at_ms = None;
                entry.last_heartbeat_at_ms = None;
                entry.stale_detected_at_ms = None;
                entry.startup_completed_at_ms = None;
            }
        }
    }

    pub(in crate::desktop_webview_recovery) fn fail_recovery_action(&self, window_label: &str) {
        let Ok(mut guard) = self.inner.lock() else {
            return;
        };
        let Some(entry) = guard.windows.get_mut(window_label) else {
            return;
        };
        entry.recovery_in_progress = false;
        entry.pending_heartbeat_timeout = false;
    }

    pub(in crate::desktop_webview_recovery) fn current_window_labels(&self) -> Vec<String> {
        let Ok(guard) = self.inner.lock() else {
            return Vec::new();
        };
        guard
            .windows
            .iter()
            .filter_map(|(label, state)| state.exists.then(|| label.clone()))
            .collect()
    }
}
