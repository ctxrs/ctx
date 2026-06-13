use std::collections::HashMap;

use ctx_core::ids::WorkspaceId;
use ctx_provider_install::install_state::InstallTarget;
use ctx_settings_model::{ExecutionMode, ExecutionSettings};
use ctx_store::StoreManager;

use crate::daemon::DaemonState;

use super::effective_install_target;

#[test]
fn install_target_for_settings_matches_execution_mode() {
    let host = ExecutionSettings {
        mode: ExecutionMode::Host,
        ..ExecutionSettings::default()
    };
    let container = ExecutionSettings {
        mode: ExecutionMode::Sandbox,
        ..ExecutionSettings::default()
    };

    assert_eq!(
        ctx_settings_service::install_target_for_settings(&host),
        InstallTarget::Host
    );
    assert_eq!(
        ctx_settings_service::install_target_for_settings(&container),
        InstallTarget::Container
    );
}

#[tokio::test]
async fn effective_install_target_errors_for_missing_workspace() {
    let temp = tempfile::tempdir().expect("tempdir");
    let stores = StoreManager::open(temp.path()).await.expect("open stores");
    let state = DaemonState::new(
        temp.path().to_path_buf(),
        stores,
        HashMap::new(),
        "http://127.0.0.1:4310".to_string(),
        None,
    );

    let err = effective_install_target(&state, WorkspaceId::new())
        .await
        .expect_err("missing workspace should fail");
    let message = format!("{err:#}");
    assert!(message.contains("workspace"));
    assert!(message.contains("not found"));
}
