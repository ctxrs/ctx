use std::io::Write as _;
#[cfg(unix)]
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};

use super::{
    current_executable, remove_durable, run, uninstall_install_path_for_helper,
    HostedTransactionAction, HostedTransactionArgs,
};

pub const HOSTED_UNINSTALL_POST_EXIT_READY: &[u8] = b"ctx-hosted-uninstall-ready-v1\n";

/// Arms the prepared hosted uninstall, waits for its Pro parent to exit, and
/// commits through the ordinary hosted transaction. This adapter owns no
/// persistent state; the hosted journal remains the sole lifecycle authority.
pub fn run_hosted_uninstall_after_parent_exit(parent_pid: u32) -> Result<()> {
    let helper_path = current_executable()?;
    let install_path = uninstall_install_path_for_helper(&helper_path)
        .ok_or_else(|| anyhow!("hosted uninstall continuation is not its fixed helper"))?;

    #[cfg(windows)]
    let parent = super::super::transaction::open_managed_pair_parent(parent_pid)?;
    #[cfg(unix)]
    let parent = validate_unix_parent(parent_pid)?;
    #[cfg(not(any(unix, windows)))]
    let _ = parent_pid;
    #[cfg(not(any(unix, windows)))]
    bail!("hosted uninstall post-exit continuation is unsupported on this platform");

    run(uninstall_args(
        HostedTransactionAction::UninstallArm,
        install_path.clone(),
    ))?;
    let mut stderr = std::io::stderr().lock();
    stderr.write_all(HOSTED_UNINSTALL_POST_EXIT_READY)?;
    stderr.flush()?;

    #[cfg(windows)]
    parent.wait()?;
    #[cfg(unix)]
    wait_for_unix_parent_exit(parent)?;

    run(uninstall_args(
        HostedTransactionAction::UninstallCommit,
        install_path,
    ))?;
    #[cfg(unix)]
    remove_durable(&helper_path)?;
    Ok(())
}

fn uninstall_args(
    action: HostedTransactionAction,
    install_path: std::path::PathBuf,
) -> HostedTransactionArgs {
    HostedTransactionArgs {
        action,
        install_path,
        attempt_id: None,
        marker_source: None,
        ownership_source: None,
        binary_sha256: None,
    }
}

#[cfg(unix)]
fn validate_unix_parent(parent_pid: u32) -> Result<libc::pid_t> {
    let parent = libc::pid_t::try_from(parent_pid)
        .ok()
        .filter(|value| *value > 1)
        .ok_or_else(|| anyhow!("hosted uninstall parent PID is invalid"))?;
    if unsafe { libc::getppid() } != parent {
        bail!("hosted uninstall helper was not launched by the expected Pro process");
    }
    Ok(parent)
}

#[cfg(unix)]
fn wait_for_unix_parent_exit(parent: libc::pid_t) -> Result<()> {
    let started = Instant::now();
    loop {
        let status = unsafe { libc::kill(parent, 0) };
        if status == -1 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                return Ok(());
            }
            if error.raw_os_error() != Some(libc::EPERM) {
                return Err(error).context("wait for hosted uninstall Pro parent exit");
            }
        }
        if started.elapsed() >= Duration::from_secs(5 * 60) {
            bail!("timed out waiting for the hosted uninstall Pro parent to exit");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}
