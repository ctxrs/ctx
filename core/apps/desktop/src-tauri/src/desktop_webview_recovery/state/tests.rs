use ctx_desktop_ipc::{
    DesktopWebviewRecoveryAction, DesktopWebviewRecoveryDaemonHealth,
    DesktopWebviewRecoveryHeartbeatReq, DesktopWebviewRecoverySuppressionReason,
    DesktopWebviewRecoveryTriggerKind, DesktopWebviewSurface,
};

use super::super::policy::{HEARTBEAT_CONFIRMATION_MS, STARTUP_GRACE_MS};
use super::model::HeartbeatTimeoutEvaluation;
use super::*;

#[test]
fn heartbeat_timeout_requires_confirmation_after_startup_grace() {
    let controller = DesktopWebviewRecoveryController::default();
    controller.register_window("main", DesktopWebviewSurface::Main, "/", 0);

    assert_eq!(
        controller.evaluate_heartbeat_timeout("main", STARTUP_GRACE_MS - 1),
        HeartbeatTimeoutEvaluation::Skip
    );
    assert_eq!(
        controller.evaluate_heartbeat_timeout("main", STARTUP_GRACE_MS),
        HeartbeatTimeoutEvaluation::AwaitConfirmation
    );
    assert_eq!(
        controller
            .evaluate_heartbeat_timeout("main", STARTUP_GRACE_MS + HEARTBEAT_CONFIRMATION_MS - 1),
        HeartbeatTimeoutEvaluation::AwaitConfirmation
    );
    assert_eq!(
        controller.evaluate_heartbeat_timeout("main", STARTUP_GRACE_MS + HEARTBEAT_CONFIRMATION_MS),
        HeartbeatTimeoutEvaluation::Ready
    );
}

#[test]
fn successful_recovery_resets_heartbeat_tracking() {
    let controller = DesktopWebviewRecoveryController::default();
    controller.register_window("main", DesktopWebviewSurface::Main, "/", 0);
    controller.note_heartbeat(
        "main",
        &DesktopWebviewRecoveryHeartbeatReq {
            route: "/".to_string(),
            document_visible: true,
            window_focused: true,
            startup_ready: true,
        },
    );

    controller.finish_recovery_action(
        "main",
        DesktopWebviewRecoveryTriggerKind::HeartbeatTimeout,
        DesktopWebviewRecoveryAction::Reload,
        30_000,
    );

    let snapshot = controller.automation_snapshot();
    let window = snapshot
        .windows
        .iter()
        .find(|window| window.window_label == "main")
        .expect("main window snapshot");
    assert_eq!(window.last_heartbeat_at_ms, None);
    assert_eq!(window.startup_completed_at_ms, None);

    assert_eq!(
        controller.evaluate_heartbeat_timeout("main", 30_000 + STARTUP_GRACE_MS - 1),
        HeartbeatTimeoutEvaluation::Skip
    );
    assert_eq!(
        controller.evaluate_heartbeat_timeout("main", 30_000 + STARTUP_GRACE_MS),
        HeartbeatTimeoutEvaluation::AwaitConfirmation
    );
}

#[test]
fn focus_rearms_suppressed_heartbeat_detection() {
    let controller = DesktopWebviewRecoveryController::default();
    controller.register_window("main", DesktopWebviewSurface::Main, "/", 0);

    let prepared = controller
        .prepare_incident(
            "main",
            DesktopWebviewRecoveryTriggerKind::HeartbeatTimeout,
            DesktopWebviewRecoveryDaemonHealth::Down,
            Some(DesktopWebviewRecoverySuppressionReason::DaemonDown),
            30_000,
            true,
        )
        .expect("incident");
    assert_eq!(prepared.action, DesktopWebviewRecoveryAction::Noop);

    controller.finish_recovery_action(
        "main",
        DesktopWebviewRecoveryTriggerKind::HeartbeatTimeout,
        DesktopWebviewRecoveryAction::Noop,
        30_000,
    );
    assert_eq!(
        controller.evaluate_heartbeat_timeout("main", 30_001),
        HeartbeatTimeoutEvaluation::Skip
    );

    controller.rearm_heartbeat_detection("main");
    assert_eq!(
        controller.evaluate_heartbeat_timeout("main", 30_001),
        HeartbeatTimeoutEvaluation::AwaitConfirmation
    );
}

#[test]
fn late_heartbeat_cancels_reserved_timeout_before_recovery() {
    let controller = DesktopWebviewRecoveryController::default();
    controller.register_window("main", DesktopWebviewSurface::Main, "/", 0);

    assert_eq!(
        controller.evaluate_heartbeat_timeout("main", STARTUP_GRACE_MS),
        HeartbeatTimeoutEvaluation::AwaitConfirmation
    );
    assert_eq!(
        controller.evaluate_heartbeat_timeout("main", STARTUP_GRACE_MS + HEARTBEAT_CONFIRMATION_MS),
        HeartbeatTimeoutEvaluation::Ready
    );

    let snapshot = controller.automation_snapshot();
    let window = snapshot
        .windows
        .iter()
        .find(|window| window.window_label == "main")
        .expect("main window snapshot");
    assert!(window.pending_heartbeat_timeout);

    controller.note_heartbeat(
        "main",
        &DesktopWebviewRecoveryHeartbeatReq {
            route: "/".to_string(),
            document_visible: true,
            window_focused: true,
            startup_ready: true,
        },
    );

    let prepared = controller.prepare_incident(
        "main",
        DesktopWebviewRecoveryTriggerKind::HeartbeatTimeout,
        DesktopWebviewRecoveryDaemonHealth::Ok,
        None,
        STARTUP_GRACE_MS + HEARTBEAT_CONFIRMATION_MS + 1,
        false,
    );
    assert!(prepared.is_none());

    let snapshot = controller.automation_snapshot();
    let window = snapshot
        .windows
        .iter()
        .find(|window| window.window_label == "main")
        .expect("main window snapshot");
    assert!(!window.pending_heartbeat_timeout);
}

#[test]
fn failed_recovery_preserves_heartbeat_tracking() {
    let controller = DesktopWebviewRecoveryController::default();
    controller.register_window("main", DesktopWebviewSurface::Main, "/", 0);
    controller.note_heartbeat(
        "main",
        &DesktopWebviewRecoveryHeartbeatReq {
            route: "/".to_string(),
            document_visible: true,
            window_focused: true,
            startup_ready: true,
        },
    );

    let prepared = controller
        .prepare_incident(
            "main",
            DesktopWebviewRecoveryTriggerKind::HeartbeatTimeout,
            DesktopWebviewRecoveryDaemonHealth::Ok,
            None,
            now_ms(),
            true,
        )
        .expect("incident");
    assert_eq!(prepared.action, DesktopWebviewRecoveryAction::Reload);

    controller.fail_recovery_action("main");

    let snapshot = controller.automation_snapshot();
    let window = snapshot
        .windows
        .iter()
        .find(|window| window.window_label == "main")
        .expect("main window snapshot");
    assert!(window.last_heartbeat_at_ms.is_some());
    assert!(window.startup_completed_at_ms.is_some());
    assert!(!window.recovery_in_progress);
    assert!(!window.pending_heartbeat_timeout);
}
