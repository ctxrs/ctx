use super::super::*;

#[cfg_attr(test, allow(dead_code))]
fn default_sandbox_machine_memory_mb() -> u32 {
    container_machine_memory_mb(&ContainerExecutionSettings::default())
}

#[cfg_attr(test, allow(dead_code))]
pub(in crate::daemon::workspace_runtime::sandbox_machine_recovery) async fn configured_sandbox_machine_memory_mb(
    data_root: &Path,
    observer: Option<&dyn HarnessSetupObserver>,
) -> u32 {
    let default_memory_mb = default_sandbox_machine_memory_mb();
    let db_path = data_root.join("db").join("db.sqlite");
    let store = match Store::open_sqlite(&db_path, None).await {
        Ok(store) => store,
        Err(err) => {
            observe_log(
                observer,
                HarnessSetupPhase::MachineStartOrInit,
                HarnessSetupLogLevel::Warn,
                &format!(
                    "failed to open execution settings while recovering local sandbox runtime; using default machine memory: {err}"
                ),
            );
            return default_memory_mb;
        }
    };

    let loaded = ctx_settings_service::load_settings(&store).await;
    store.close().await;

    match loaded {
        Ok(settings) => {
            container_machine_memory_mb(&settings.execution.unwrap_or_default().container)
        }
        Err(err) => {
            observe_log(
                observer,
                HarnessSetupPhase::MachineStartOrInit,
                HarnessSetupLogLevel::Warn,
                &format!(
                    "failed to load execution settings while recovering local sandbox runtime; using default machine memory: {err:#}"
                ),
            );
            default_memory_mb
        }
    }
}
