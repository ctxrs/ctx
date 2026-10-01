//! Standalone outbox delivery, never a history service. A shared launch claim
//! is acquired before spawning, so high-frequency hooks cannot start a child
//! per invocation. The parent never waits or performs telemetry HTTP.
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::{Child, Command, ExitCode, Stdio},
    time::Duration,
};

const INVOCATION: &str = "--ctx-analytics-drain-v1";
const LAUNCH_INTERVAL: i64 = 60;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LaunchState {
    schema_version: u16,
    next_allowed_at: i64,
}

fn claim(path: &Path, now: i64) -> anyhow::Result<bool> {
    let Some(mut file) = crate::analytics_state::StateFile::try_open(path)? else {
        return Ok(false);
    };
    let state = match file.read::<LaunchState>() {
        Ok(state) => state,
        Err(error) if error.is::<serde_json::Error>() => {
            file.write(&LaunchState {
                schema_version: 1,
                next_allowed_at: now.saturating_add(LAUNCH_INTERVAL),
            })?;
            return Ok(false);
        }
        Err(error) => return Err(error),
    };
    if let Some(state) = state {
        if state.schema_version != 1 {
            return Ok(false);
        }
        // A corrected wall clock must not suppress standalone delivery forever.
        // Repair once under the claim lock, without an immediate retry storm.
        if state.next_allowed_at > now.saturating_add(LAUNCH_INTERVAL) {
            file.write(&LaunchState {
                schema_version: 1,
                next_allowed_at: now.saturating_add(LAUNCH_INTERVAL),
            })?;
            return Ok(false);
        }
        if state.next_allowed_at > now {
            return Ok(false);
        }
    }
    file.write(&LaunchState {
        schema_version: 1,
        next_allowed_at: now.saturating_add(LAUNCH_INTERVAL),
    })?;
    Ok(true)
}

pub(crate) fn schedule(data_root: &Path) -> Option<Child> {
    if !crate::observability_composition::optional_analytics_enabled(data_root) {
        return None;
    }
    let owner = crate::identity::try_existing_installation_id(data_root)
        .ok()
        .flatten()?;
    let claim_path =
        crate::identity::device_state_path("analytics-launch-v1.json", data_root).ok()?;
    if !claim(&claim_path, ctx_history_core::utc_now().timestamp()).ok()? {
        return None;
    }
    let executable = std::env::current_exe().ok()?;
    let mut command = Command::new(executable);
    command
        .args([
            OsString::from(INVOCATION),
            data_root.as_os_str().to_owned(),
            OsString::from(owner),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    }
    command.spawn().ok()
}

pub(crate) fn intercept(arguments: &[OsString]) -> Option<ExitCode> {
    if arguments.get(1).is_none_or(|value| value != INVOCATION) {
        return None;
    }
    if arguments.len() != 4 {
        return Some(ExitCode::FAILURE);
    }
    let root = PathBuf::from(&arguments[2]);
    let Some(owner) = arguments[3].to_str() else {
        return Some(ExitCode::FAILURE);
    };
    if crate::identity::try_existing_installation_id(&root)
        .ok()
        .flatten()
        .as_deref()
        != Some(owner)
    {
        return Some(ExitCode::FAILURE);
    }
    crate::analytics_summary::flush(&root, owner);
    // The owner-bound drain also performs explicit opt-out purge. Do not bypass
    // that policy when consent changes between scheduling and child execution.
    let _ = crate::observability_composition::drain_analytics_outbox_for_owner(
        &root,
        owner,
        Duration::from_secs(2),
    );
    Some(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests;
