use std::collections::VecDeque;
use std::time::{SystemTime, UNIX_EPOCH};

use ctx_desktop_ipc::{
    DesktopWebviewRecoveryAction, DesktopWebviewRecoveryDaemonHealth,
    DesktopWebviewRecoverySuppressionReason,
};

pub(super) const HEARTBEAT_TIMEOUT_MS: u64 = 12_000;
pub(super) const HEARTBEAT_CONFIRMATION_MS: u64 = 4_000;
pub(super) const WATCHDOG_INTERVAL_MS: u64 = 3_000;
pub(super) const STARTUP_GRACE_MS: u64 = 20_000;
pub(super) const INCIDENT_BACKOFF_WINDOW_MS: u64 = 120_000;

pub(super) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

pub(super) fn prune_recent_incidents(
    recent_incident_timestamps_ms: &mut VecDeque<u64>,
    now_ms: u64,
) -> usize {
    while matches!(
        recent_incident_timestamps_ms.front(),
        Some(ts) if now_ms.saturating_sub(*ts) > INCIDENT_BACKOFF_WINDOW_MS
    ) {
        recent_incident_timestamps_ms.pop_front();
    }
    recent_incident_timestamps_ms.len()
}

pub(super) fn decide_recovery_action(
    recent_incident_count: usize,
    daemon_health: DesktopWebviewRecoveryDaemonHealth,
    suppression_reason: Option<DesktopWebviewRecoverySuppressionReason>,
) -> DesktopWebviewRecoveryAction {
    if suppression_reason.is_some() {
        return DesktopWebviewRecoveryAction::Noop;
    }
    if daemon_health != DesktopWebviewRecoveryDaemonHealth::Ok {
        return DesktopWebviewRecoveryAction::Noop;
    }
    match recent_incident_count {
        0 => DesktopWebviewRecoveryAction::Reload,
        1 => DesktopWebviewRecoveryAction::Recreate,
        _ => DesktopWebviewRecoveryAction::PromptRestart,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decide_recovery_action_respects_suppression_and_daemon_health() {
        assert_eq!(
            decide_recovery_action(
                0,
                DesktopWebviewRecoveryDaemonHealth::Ok,
                Some(DesktopWebviewRecoverySuppressionReason::WindowNotFocused)
            ),
            DesktopWebviewRecoveryAction::Noop
        );
        assert_eq!(
            decide_recovery_action(0, DesktopWebviewRecoveryDaemonHealth::Down, None),
            DesktopWebviewRecoveryAction::Noop
        );
        assert_eq!(
            decide_recovery_action(0, DesktopWebviewRecoveryDaemonHealth::Mismatch, None),
            DesktopWebviewRecoveryAction::Noop
        );
    }

    #[test]
    fn decide_recovery_action_escalates_across_recent_incidents() {
        assert_eq!(
            decide_recovery_action(0, DesktopWebviewRecoveryDaemonHealth::Ok, None),
            DesktopWebviewRecoveryAction::Reload
        );
        assert_eq!(
            decide_recovery_action(1, DesktopWebviewRecoveryDaemonHealth::Ok, None),
            DesktopWebviewRecoveryAction::Recreate
        );
        assert_eq!(
            decide_recovery_action(2, DesktopWebviewRecoveryDaemonHealth::Ok, None),
            DesktopWebviewRecoveryAction::PromptRestart
        );
    }

    #[test]
    fn prune_recent_incidents_discards_old_entries() {
        let mut timestamps = VecDeque::from([
            1,
            INCIDENT_BACKOFF_WINDOW_MS,
            INCIDENT_BACKOFF_WINDOW_MS + 1,
        ]);
        let remaining = prune_recent_incidents(&mut timestamps, INCIDENT_BACKOFF_WINDOW_MS + 2);

        assert_eq!(remaining, 2);
        assert_eq!(
            timestamps,
            VecDeque::from([INCIDENT_BACKOFF_WINDOW_MS, INCIDENT_BACKOFF_WINDOW_MS + 1])
        );
    }
}
