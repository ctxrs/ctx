use std::sync::Arc;

use ctx_core::ids::SessionId;
use ctx_observability::ops_events::OpsEvent;
use ctx_storage_admission::{
    StorageGuardLevel, StorageGuardReserveAction, StorageGuardReserveWarning, StorageGuardStatus,
};
use serde_json::json;

use crate::daemon::scheduler::SchedulerCommand;
use crate::daemon::DaemonState;

pub(super) async fn publish_storage_guard_snapshot(
    state: &Arc<DaemonState>,
    previous: &StorageGuardStatus,
    snapshot: &StorageGuardStatus,
) {
    let should_interrupt = previous.level != StorageGuardLevel::Emergency
        && snapshot.level == StorageGuardLevel::Emergency;
    if !snapshot.same_meaningful_state(previous) {
        emit_storage_guard_transition(state, snapshot);
    }
    state.core.storage_guard.publish(snapshot.clone());
    if should_interrupt {
        dispatch_storage_emergency_interrupts(state, snapshot).await;
    }
}

fn emit_storage_guard_transition(state: &DaemonState, snapshot: &StorageGuardStatus) {
    let mut event = OpsEvent::new(
        match snapshot.level {
            StorageGuardLevel::Emergency => "error",
            StorageGuardLevel::Warning => "warning",
            StorageGuardLevel::Normal => "info",
        },
        "storage_guard_state_changed",
    );
    event.meta = Some(json!({
        "level": snapshot.level,
        "reserve_file_active": snapshot.reserve_file_active,
        "active": snapshot.active,
    }));
    state.telemetry.ops_events.emit(event);
}

pub(super) fn emit_reserve_warnings(warnings: Vec<StorageGuardReserveWarning>) {
    for warning in warnings {
        match warning.action {
            StorageGuardReserveAction::Allocate => {
                tracing::warn!(
                    reserve_file = %warning.reserve_file_path.to_string_lossy(),
                    "failed to allocate storage reserve file: {:#}",
                    warning.message
                );
            }
            StorageGuardReserveAction::Release => {
                tracing::warn!(
                    reserve_file = %warning.reserve_file_path.to_string_lossy(),
                    "failed to release storage reserve file: {:#}",
                    warning.message
                );
            }
        }
    }
}

async fn dispatch_storage_emergency_interrupts(
    state: &Arc<DaemonState>,
    snapshot: &StorageGuardStatus,
) {
    let running_sessions = state.sessions.list_running_sessions().await;
    let mut interrupted = 0usize;
    for session_id in running_sessions {
        if dispatch_storage_emergency_interrupt(state, session_id).await {
            interrupted += 1;
        }
    }

    tracing::warn!(
        interrupted_sessions = interrupted,
        level = ?snapshot.level,
        active_path = snapshot.active.as_ref().map(|path| path.path.as_str()),
        "storage emergency interrupted active sessions"
    );
}

pub(super) async fn dispatch_storage_emergency_interrupt(
    state: &Arc<DaemonState>,
    session_id: SessionId,
) -> bool {
    let Some(tx) = state.sessions.scheduler_sender(session_id).await else {
        return false;
    };
    tx.send(SchedulerCommand::StorageEmergency).await.is_ok()
}
