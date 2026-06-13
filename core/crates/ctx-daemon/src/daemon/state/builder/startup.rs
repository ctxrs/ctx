use std::path::PathBuf;
use std::sync::Arc;

use ctx_execution_runtime::ExecutionSetupCoordinator;
use ctx_store::Store;

pub(super) fn spawn_startup_prewarm_loader(
    data_root: PathBuf,
    execution_setup: Arc<ExecutionSetupCoordinator>,
) {
    tokio::spawn(async move {
        let db_path = data_root.join("db").join("db.sqlite");
        match Store::open_sqlite(&db_path, None).await {
            Ok(store) => {
                let loaded = ctx_settings_service::load_settings(&store).await;
                store.close().await;
                match loaded {
                    Ok(settings) => {
                        execution_setup
                            .spawn_startup_prewarm(settings.execution.unwrap_or_default());
                    }
                    Err(err) => {
                        execution_setup
                            .record_startup_prewarm_error(format!(
                                "failed to load execution settings: {err:#}"
                            ))
                            .await;
                    }
                }
            }
            Err(err) => {
                execution_setup
                    .record_startup_prewarm_error(format!(
                        "failed to open global settings store: {err:#}"
                    ))
                    .await;
            }
        }
    });
}
