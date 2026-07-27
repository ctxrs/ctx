use std::{fs, path::Path, process::Stdio};

use anyhow::{Context, Result};

pub(super) struct ScheduledReplacement {
    pub(super) helper_pid: u32,
}

pub(super) fn schedule_replacement(
    staged: &Path,
    script_source: &str,
) -> Result<ScheduledReplacement> {
    let script = staged.with_extension("ps1");
    fs::write(&script, script_source)?;
    let child = std::process::Command::new("powershell")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&script)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("spawn Windows ctx replacement helper")?;
    Ok(ScheduledReplacement {
        helper_pid: child.id(),
    })
}
