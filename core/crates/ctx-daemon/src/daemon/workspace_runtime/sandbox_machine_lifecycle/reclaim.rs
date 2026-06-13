#[cfg(any(target_os = "macos", target_os = "windows"))]
use super::*;

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub(super) async fn maybe_reclaim_sandbox_machine(
    manager: &HarnessRuntimeManager,
    settings: &ContainerExecutionSettings,
    system: &SystemSnapshot,
    observer: Option<&dyn HarnessSetupObserver>,
    stores: &StoreManager,
    running_sessions: &Arc<Mutex<HashSet<SessionId>>>,
    terminals: &TerminalManager,
) -> Result<bool> {
    if !sandbox_machine_required() {
        return Ok(false);
    }
    if manager.runtime_operation_count() > 0 || manager.prewarm_artifact_operation_count() > 0 {
        return Ok(false);
    }
    if should_defer_reclaim_for_active_container_runtime(stores, running_sessions, terminals).await
    {
        manager.note_runtime_activity();
        return Ok(false);
    }
    let idle_for = manager.runtime_idle_for();
    let idle_timeout = Duration::from_secs(normalize_container_machine_idle_shutdown_seconds(
        settings.machine.idle_shutdown_seconds,
    ));
    let swap_threshold_bytes =
        u64::from(settings.machine.host_pressure_swap_threshold_mb) * 1024 * 1024;
    let host_pressure = swap_threshold_bytes > 0 && system.swap_used_bytes >= swap_threshold_bytes;
    let pressure_idle_grace = if cfg!(test) {
        Duration::from_millis(100)
    } else {
        Duration::from_secs(60)
    };
    let should_stop =
        idle_for >= idle_timeout || (host_pressure && idle_for >= pressure_idle_grace);
    if !should_stop {
        return Ok(false);
    }

    let machine_name = sandbox_machine_name(manager.data_root());
    let machine_lock = sandbox_machine_singleflight_lock(&machine_name);
    let _machine_guard = machine_lock.lock().await;
    if manager.runtime_operation_count() > 0 || manager.prewarm_artifact_operation_count() > 0 {
        return Ok(false);
    }
    if should_defer_reclaim_for_active_container_runtime(stores, running_sessions, terminals).await
    {
        manager.note_runtime_activity();
        return Ok(false);
    }
    if !sandbox_machine_present(manager.data_root(), &machine_name).await? {
        return Ok(false);
    }
    let stopped = manager
        .stop_sandbox_machine_locked(&machine_name, observer)
        .await?;
    if stopped {
        manager.note_runtime_activity();
    }
    Ok(stopped)
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub(super) async fn should_defer_reclaim_for_active_container_runtime(
    stores: &StoreManager,
    running_sessions: &Arc<Mutex<HashSet<SessionId>>>,
    terminals: &TerminalManager,
) -> bool {
    if terminals.has_running_container_backed().await {
        return true;
    }

    let session_ids = {
        let running = running_sessions.lock().await;
        running.iter().copied().collect::<Vec<_>>()
    };

    for session_id in session_ids {
        let store = match stores.store_for_session(session_id).await {
            Ok(store) => store,
            Err(err) => {
                tracing::warn!(
                    session_id = ?session_id,
                    "deferring local sandbox reclaim because running session store lookup failed: {err:#}"
                );
                return true;
            }
        };
        let session = match store.get_session(session_id).await {
            Ok(Some(session)) => session,
            Ok(None) => continue,
            Err(err) => {
                tracing::warn!(
                    session_id = ?session_id,
                    "deferring local sandbox reclaim because running session lookup failed: {err:#}"
                );
                return true;
            }
        };
        if matches!(session.execution_environment, ExecutionEnvironment::Sandbox) {
            return true;
        }
    }

    false
}
