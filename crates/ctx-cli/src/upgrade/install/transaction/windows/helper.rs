use std::{
    fs,
    io::{Read as _, Write as _},
    os::windows::{io::AsRawHandle as _, process::CommandExt as _},
    path::Path,
    process::{Child, ChildStdout, Command, Stdio},
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT},
    System::{
        Pipes::PeekNamedPipe,
        Threading::{OpenProcess, WaitForSingleObject, INFINITE, PROCESS_SYNCHRONIZE},
    },
};

use super::{
    protocol::{self, JournalIdentity},
    InstallTransactionJournal,
};

const READY_TIMEOUT: Duration = Duration::from_secs(10);
const READY_POLL: Duration = Duration::from_millis(10);
const MAX_READY_BYTES: usize = 512;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub(super) struct ParentProcess {
    handle: HANDLE,
}

impl ParentProcess {
    pub(super) fn open(parent_pid: u32) -> Result<Self> {
        if parent_pid == 0 || parent_pid == std::process::id() {
            return Err(anyhow!(
                "Windows replacement helper has an invalid parent PID"
            ));
        }
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, parent_pid) };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error())
                .context("open Windows replacement parent process");
        }
        match unsafe { WaitForSingleObject(handle, 0) } {
            WAIT_TIMEOUT => Ok(Self { handle }),
            WAIT_OBJECT_0 => {
                unsafe { CloseHandle(handle) };
                Err(anyhow!(
                    "Windows replacement parent exited before helper handoff"
                ))
            }
            WAIT_FAILED => {
                let error = std::io::Error::last_os_error();
                unsafe { CloseHandle(handle) };
                Err(error).context("inspect Windows replacement parent process")
            }
            status => {
                unsafe { CloseHandle(handle) };
                Err(anyhow!(
                    "unexpected Windows parent process wait status {status}"
                ))
            }
        }
    }

    pub(super) fn wait(self) -> Result<()> {
        let status = unsafe { WaitForSingleObject(self.handle, INFINITE) };
        match status {
            WAIT_OBJECT_0 => Ok(()),
            WAIT_FAILED => {
                Err(std::io::Error::last_os_error()).context("wait for Windows replacement parent")
            }
            value => Err(anyhow!(
                "unexpected Windows replacement parent wait status {value}"
            )),
        }
    }
}

impl Drop for ParentProcess {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

pub(super) fn spawn(transaction: &mut InstallTransactionJournal, parent_pid: u32) -> Result<u32> {
    let expected = protocol::prepare_launch(transaction, parent_pid)?;
    copy_helper(&transaction.install_path, expected.helper_path())?;
    let mut command = Command::new(expected.helper_path());
    command
        .arg("--data-root")
        .arg(&transaction.data_root)
        .arg("upgrade")
        .arg("--replacement-helper")
        .arg("--install-path")
        .arg(&transaction.install_path)
        .arg("--attempt-id")
        .arg(expected.attempt_id())
        .arg("--parent-pid")
        .arg(parent_pid.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW);
    crate::process_environment::sanitize_release_authority_env(&mut command);
    let mut child = command
        .spawn()
        .context("spawn Windows ctx replacement helper")?;
    let helper_pid = child.id();
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("Windows replacement helper has no readiness pipe"))?;
    let result = protocol::record_helper_pid(&transaction.install_path, &expected, helper_pid)
        .and_then(|()| wait_for_ready(&mut child, stdout, &expected, helper_pid));
    if let Err(error) = result {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    Ok(helper_pid)
}

pub(super) fn write_ready(attempt_id: &str, helper_pid: u32) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(protocol::ready_receipt(attempt_id, helper_pid).as_bytes())?;
    stdout
        .flush()
        .context("flush Windows helper readiness receipt")
}

fn wait_for_ready(
    child: &mut Child,
    mut stdout: ChildStdout,
    expected: &JournalIdentity,
    helper_pid: u32,
) -> Result<()> {
    let deadline = Instant::now() + READY_TIMEOUT;
    let mut receipt = Vec::new();
    loop {
        if let Some(status) = child.try_wait()? {
            return Err(anyhow!(
                "Windows replacement helper exited before readiness: {status}"
            ));
        }
        let mut available = 0u32;
        if unsafe {
            PeekNamedPipe(
                stdout.as_raw_handle(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut available,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error())
                .context("read Windows helper readiness pipe");
        }
        if available > 0 {
            let remaining = MAX_READY_BYTES.saturating_sub(receipt.len());
            if remaining == 0 {
                return Err(anyhow!("Windows helper readiness receipt is too large"));
            }
            let mut chunk = vec![0u8; remaining.min(available as usize)];
            let read = stdout.read(&mut chunk)?;
            if read == 0 {
                return Err(anyhow!(
                    "Windows helper readiness pipe closed before a receipt"
                ));
            }
            receipt.extend_from_slice(&chunk[..read]);
            if receipt.contains(&b'\n') {
                let receipt = std::str::from_utf8(&receipt)
                    .context("Windows helper readiness receipt is not UTF-8")?;
                return protocol::validate_ready_receipt(
                    receipt,
                    expected.attempt_id(),
                    helper_pid,
                );
            }
        }
        if Instant::now() >= deadline {
            return Err(anyhow!(
                "timed out waiting for Windows helper contract validation"
            ));
        }
        std::thread::sleep(READY_POLL);
    }
}

fn copy_helper(source: &Path, helper: &Path) -> Result<()> {
    remove_file_if_present(helper)?;
    let mut from =
        fs::File::open(source).with_context(|| format!("open running ctx {}", source.display()))?;
    let mut to = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(helper)
        .with_context(|| format!("create Windows replacement helper {}", helper.display()))?;
    std::io::copy(&mut from, &mut to)?;
    to.sync_all()?;
    drop(to);
    ctx_history_core::platform_security::restrict_private_executable(helper)?;
    ctx_history_core::platform_security::verify_private_executable(helper)?;
    Ok(())
}

pub(super) fn cleanup_stale_copies(install_path: &Path) -> Result<()> {
    let parent = install_path
        .parent()
        .ok_or_else(|| anyhow!("ctx install path has no parent"))?;
    let name = install_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("ctx install path has no file name"))?;
    let prefix = format!(".{name}.ctx-upgrade-");
    for entry in fs::read_dir(parent).with_context(|| format!("read {}", parent.display()))? {
        let entry = entry?;
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        if file_name.starts_with(&prefix) && file_name.ends_with(".helper.exe") {
            remove_file_if_present(&entry.path())?;
        }
    }
    Ok(())
}

fn remove_file_if_present(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}
