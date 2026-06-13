use super::*;

static SANDBOX_MACHINE_SINGLEFLIGHT_LOCKS: OnceLock<StdMutex<HashMap<String, Arc<Mutex<()>>>>> =
    OnceLock::new();

pub(in crate::daemon::workspace_runtime) fn sandbox_machine_singleflight_lock(
    machine_name: &str,
) -> Arc<Mutex<()>> {
    let registry = SANDBOX_MACHINE_SINGLEFLIGHT_LOCKS.get_or_init(|| StdMutex::new(HashMap::new()));
    let mut guard = match registry.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard
        .entry(machine_name.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

pub(in crate::daemon::workspace_runtime) async fn sandbox_machine_present(
    data_root: &Path,
    machine_name: &str,
) -> Result<bool> {
    let mut cmd = sandbox_container_command(data_root)?;
    cmd.arg("machine").arg("inspect").arg(machine_name);
    let output = command_output_with_timeout(cmd, SANDBOX_INFO_TIMEOUT).await?;
    Ok(output.status.success())
}

pub(in crate::daemon::workspace_runtime) fn looks_like_missing_machine_error(
    message_lc: &str,
) -> bool {
    message_lc.contains("no such")
        || message_lc.contains("not found")
        || message_lc.contains("does not exist")
        || message_lc.contains("no machine")
}

pub(in crate::daemon::workspace_runtime) fn looks_like_recoverable_machine_start_error(
    message_lc: &str,
) -> bool {
    message_lc.contains("already running")
        || message_lc.contains("already starting")
        || message_lc.contains("already started")
        || message_lc.contains("in progress")
        || message_lc.contains("timed out")
        || message_lc.contains("resource busy")
        || message_lc.contains("another process")
        || message_lc.contains("lock")
        || message_lc.contains("port conflict")
        || message_lc.contains("unable to connect to \"gvproxy\" socket")
        || message_lc.contains("reassigning")
        || message_lc.contains("exited unexpectedly")
        || message_lc.contains("address already in use")
}

pub(in crate::daemon::workspace_runtime) fn looks_like_running_but_unreachable_machine_start_error(
    message_lc: &str,
) -> bool {
    message_lc.contains("already running")
        || message_lc.contains("already started")
        || message_lc.contains("unable to connect to \"gvproxy\" socket")
}
