use super::fixtures::test_state;
use ctx_execution_runtime::StartupPrewarmState;

#[tokio::test]
async fn test_state_does_not_auto_spawn_startup_prewarm_in_unit_tests() {
    let temp = tempfile::tempdir().expect("tempdir");
    let state = test_state(&temp).await;

    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_millis(25)).await;

    let startup = state.execution.setup.startup_status().await;
    assert_eq!(startup.state, StartupPrewarmState::Idle);
    assert!(startup.last_attempt_at.is_none(), "{startup:?}");
}
