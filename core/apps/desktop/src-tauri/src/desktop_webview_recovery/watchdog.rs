use std::time::Duration;

use tauri::Manager;

use super::policy::{now_ms, WATCHDOG_INTERVAL_MS};
use super::state::{DesktopWebviewRecoveryController, HeartbeatTimeoutEvaluation};

pub(super) fn start_watchdog(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(WATCHDOG_INTERVAL_MS));
        loop {
            interval.tick().await;
            let labels = {
                let controller = app.state::<DesktopWebviewRecoveryController>();
                controller.current_window_labels()
            };
            for label in labels {
                let evaluation = {
                    let controller = app.state::<DesktopWebviewRecoveryController>();
                    controller.evaluate_heartbeat_timeout(&label, now_ms())
                };
                if evaluation == HeartbeatTimeoutEvaluation::Ready {
                    let app_handle = app.clone();
                    let label_clone = label.clone();
                    tauri::async_runtime::spawn(async move {
                        let _ = super::runtime::handle_heartbeat_timeout(
                            &app_handle,
                            &label_clone,
                            false,
                        )
                        .await;
                    });
                }
            }
        }
    });
}
