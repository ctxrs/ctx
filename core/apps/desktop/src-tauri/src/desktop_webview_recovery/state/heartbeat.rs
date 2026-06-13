use ctx_desktop_ipc::{DesktopWebviewRecoveryHeartbeatReq, DesktopWebviewSurface};

use super::super::policy::{HEARTBEAT_CONFIRMATION_MS, HEARTBEAT_TIMEOUT_MS, STARTUP_GRACE_MS};
use super::model::{normalize_route, HeartbeatTimeoutEvaluation, RecoveryWindowState};
use super::*;

impl DesktopWebviewRecoveryController {
    pub(in crate::desktop_webview_recovery) fn register_window(
        &self,
        window_label: &str,
        surface: DesktopWebviewSurface,
        route: &str,
        created_at_ms: u64,
    ) {
        let Ok(mut guard) = self.inner.lock() else {
            return;
        };
        let entry = guard
            .windows
            .entry(window_label.to_string())
            .or_insert_with(|| {
                RecoveryWindowState::new(window_label, surface, route, created_at_ms)
            });
        entry.window_label = window_label.to_string();
        entry.surface = surface;
        entry.route = normalize_route(route);
        entry.created_at_ms = created_at_ms;
        entry.daemon_health = ctx_desktop_ipc::DesktopWebviewRecoveryDaemonHealth::Unknown;
        entry.exists = true;
        entry.last_suppressed_at_ms = None;
        entry.last_heartbeat_at_ms = None;
        entry.pending_heartbeat_timeout = false;
        entry.recovery_in_progress = false;
        entry.stale_detected_at_ms = None;
        entry.startup_completed_at_ms = None;
    }

    pub(in crate::desktop_webview_recovery) fn update_route(
        &self,
        window_label: &str,
        surface: DesktopWebviewSurface,
        route: &str,
    ) {
        let Ok(mut guard) = self.inner.lock() else {
            return;
        };
        let entry = guard
            .windows
            .entry(window_label.to_string())
            .or_insert_with(|| RecoveryWindowState::new(window_label, surface, route, now_ms()));
        entry.window_label = window_label.to_string();
        entry.surface = surface;
        entry.route = normalize_route(route);
        entry.exists = true;
    }

    pub(in crate::desktop_webview_recovery) fn note_window_destroyed(&self, window_label: &str) {
        let Ok(mut guard) = self.inner.lock() else {
            return;
        };
        let Some(entry) = guard.windows.get_mut(window_label) else {
            return;
        };
        entry.exists = false;
        entry.pending_heartbeat_timeout = false;
        entry.recovery_in_progress = false;
        entry.stale_detected_at_ms = None;
    }

    pub(in crate::desktop_webview_recovery) fn note_heartbeat(
        &self,
        window_label: &str,
        req: &DesktopWebviewRecoveryHeartbeatReq,
    ) {
        let Ok(mut guard) = self.inner.lock() else {
            return;
        };
        let now = now_ms();
        let entry = guard
            .windows
            .entry(window_label.to_string())
            .or_insert_with(|| {
                RecoveryWindowState::new(
                    window_label,
                    DesktopWebviewSurface::Unknown,
                    &req.route,
                    now,
                )
            });
        entry.route = normalize_route(&req.route);
        entry.exists = true;
        entry.last_suppressed_at_ms = None;
        entry.last_heartbeat_at_ms = Some(now);
        entry.pending_heartbeat_timeout = false;
        entry.stale_detected_at_ms = None;
        if req.startup_ready {
            entry.startup_completed_at_ms = Some(now);
        }
    }

    pub(crate) fn rearm_heartbeat_detection(&self, window_label: &str) {
        let Ok(mut guard) = self.inner.lock() else {
            return;
        };
        let Some(entry) = guard.windows.get_mut(window_label) else {
            return;
        };
        if entry.recovery_in_progress {
            return;
        }
        entry.last_suppressed_at_ms = None;
        entry.pending_heartbeat_timeout = false;
        entry.stale_detected_at_ms = None;
    }

    pub(in crate::desktop_webview_recovery) fn evaluate_heartbeat_timeout(
        &self,
        window_label: &str,
        now_ms: u64,
    ) -> HeartbeatTimeoutEvaluation {
        let Ok(mut guard) = self.inner.lock() else {
            return HeartbeatTimeoutEvaluation::Skip;
        };
        let Some(entry) = guard.windows.get_mut(window_label) else {
            return HeartbeatTimeoutEvaluation::Skip;
        };
        if !entry.exists || entry.recovery_in_progress || entry.pending_heartbeat_timeout {
            return HeartbeatTimeoutEvaluation::Skip;
        }
        if let Some(last_suppressed_at_ms) = entry.last_suppressed_at_ms {
            if now_ms.saturating_sub(last_suppressed_at_ms) < HEARTBEAT_TIMEOUT_MS {
                return HeartbeatTimeoutEvaluation::Skip;
            }
            entry.last_suppressed_at_ms = None;
        }
        if entry.startup_completed_at_ms.is_none()
            && now_ms.saturating_sub(entry.created_at_ms) < STARTUP_GRACE_MS
        {
            return HeartbeatTimeoutEvaluation::Skip;
        }
        let last_heartbeat_at_ms = entry.last_heartbeat_at_ms.unwrap_or(entry.created_at_ms);
        if now_ms.saturating_sub(last_heartbeat_at_ms) < HEARTBEAT_TIMEOUT_MS {
            entry.pending_heartbeat_timeout = false;
            entry.stale_detected_at_ms = None;
            return HeartbeatTimeoutEvaluation::Skip;
        }
        let stale_detected_at_ms = match entry.stale_detected_at_ms {
            Some(value) => value,
            None => {
                entry.stale_detected_at_ms = Some(now_ms);
                return HeartbeatTimeoutEvaluation::AwaitConfirmation;
            }
        };
        if now_ms.saturating_sub(stale_detected_at_ms) < HEARTBEAT_CONFIRMATION_MS {
            return HeartbeatTimeoutEvaluation::AwaitConfirmation;
        }
        entry.pending_heartbeat_timeout = true;
        HeartbeatTimeoutEvaluation::Ready
    }
}
