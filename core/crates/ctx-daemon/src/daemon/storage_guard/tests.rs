use std::collections::HashMap;
use std::sync::Arc;

use tempfile::tempdir;
use tokio::sync::mpsc;

use ctx_core::ids::SessionId;
use ctx_storage_admission::{
    StorageGuardLevel, StorageGuardPathStatus, StorageGuardStatus, STORAGE_GUARD_RESERVE_FILE_NAME,
};
use ctx_store::StoreManager;

use super::*;
use crate::daemon::scheduler::SchedulerCommand;
use crate::daemon::DaemonState;

async fn app_state_for_test() -> Arc<DaemonState> {
    let data_root = tempdir().expect("data root");
    let stores = StoreManager::open(data_root.path()).await.expect("stores");
    Arc::new(DaemonState::new(
        data_root.path().to_path_buf(),
        stores,
        HashMap::new(),
        "http://127.0.0.1:4399".to_string(),
        None,
    ))
}

#[tokio::test]
async fn preflight_blocks_turn_start_during_emergency() {
    let state = app_state_for_test().await;
    state.core.storage_guard.publish(StorageGuardStatus {
        level: StorageGuardLevel::Emergency,
        reserve_file_active: false,
        active: Some(StorageGuardPathStatus {
            label: "CTX data root".to_string(),
            path: state.core.data_root.to_string_lossy().to_string(),
            mount_point: "/".to_string(),
            free_bytes: 900 * MIB,
            total_bytes: 10 * GIB,
        }),
        ..StorageGuardStatus::default()
    });

    let err = preflight_turn_start(&state, &state.core.data_root)
        .await
        .expect_err("preflight should fail");
    assert!(err.to_string().contains("Storage is critically low"));
}

#[tokio::test]
async fn preflight_samples_storage_without_allocating_reserve_file() {
    let data_root = tempdir().expect("data root");
    let stores = StoreManager::open(data_root.path()).await.expect("stores");
    let state = Arc::new(DaemonState::new(
        data_root.path().to_path_buf(),
        stores,
        HashMap::new(),
        "http://127.0.0.1:4399".to_string(),
        None,
    ));
    let _ = state.core.shutdown_tx.send(());

    preflight_turn_start(&state, &state.core.data_root)
        .await
        .expect("preflight should succeed");

    assert!(!data_root
        .path()
        .join(STORAGE_GUARD_RESERVE_FILE_NAME)
        .exists());
    assert!(!state.storage_guard_snapshot().reserve_file_active);
}

#[tokio::test]
async fn dispatches_storage_emergency_interrupts_to_running_sessions() {
    let state = app_state_for_test().await;
    let session_id = SessionId(uuid::Uuid::new_v4());
    let (tx, mut rx) = mpsc::channel(1);

    {
        let mut schedulers = state.sessions.schedulers.lock().await;
        schedulers.insert(
            session_id,
            ctx_session_runtime::runtime::TimedEntry::new(tx),
        );
    }
    state
        .task_session_cleanup
        .set_running(session_id, true)
        .await;

    let interrupted = dispatch_storage_emergency_interrupt(&state, session_id).await;
    assert!(interrupted);
    let received = rx.recv().await.expect("storage emergency command");
    assert!(matches!(received, SchedulerCommand::StorageEmergency));
}
