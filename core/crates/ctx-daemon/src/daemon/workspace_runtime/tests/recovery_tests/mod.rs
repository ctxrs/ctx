use super::fixtures::*;
use super::*;
use ctx_sandbox_contract::CTX_CONTAINER_WORKSPACE_ROOT;
use ctx_settings_model::ContainerNetworkMode;
use ctx_workspace_container::workspace_container_name;

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
async fn save_test_execution_settings(data_root: &Path, execution: ExecutionSettings) {
    let db_dir = data_root.join("db");
    std::fs::create_dir_all(&db_dir).expect("create db dir");
    let db_path = db_dir.join("db.sqlite");
    let store = ctx_store::Store::open_sqlite(&db_path, None)
        .await
        .expect("open settings db");
    ctx_settings_service::save_settings(
        &store,
        &ctx_settings_model::Settings {
            execution: Some(execution),
            ..ctx_settings_model::Settings::default()
        },
    )
    .await
    .expect("save settings");
    store.close().await;
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
async fn write_invalid_test_execution_settings(data_root: &Path, settings_json: &str) {
    let db_dir = data_root.join("db");
    std::fs::create_dir_all(&db_dir).expect("create db dir");
    let db_path = db_dir.join("db.sqlite");
    let store = ctx_store::Store::open_sqlite(&db_path, None)
        .await
        .expect("open settings db");
    store
        .upsert_runtime_settings_document(1, settings_json)
        .await
        .expect("write invalid runtime settings");
    store.close().await;
}

mod container_reuse;
mod machine_start_recovery;
mod missing_cleanup;
mod recovery_settings;
