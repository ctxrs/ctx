#[cfg(test)]
use std::path::PathBuf;
use std::{mem::MaybeUninit, time::Duration};
#[cfg(test)]
use windows_sys::Win32::System::Threading::QueryFullProcessImageNameW;

use anyhow::{anyhow, Context, Result};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, ERROR_INVALID_PARAMETER, FILETIME, HANDLE, WAIT_FAILED, WAIT_OBJECT_0,
        WAIT_TIMEOUT,
    },
    System::Threading::{
        GetProcessTimes, OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    },
};

const WINDOWS_TO_UNIX_EPOCH_100NS: u64 = 116_444_736_000_000_000;

pub(super) struct WindowsProcess {
    pub(super) handle: HANDLE,
    pub(super) pid: u32,
    pub(super) creation_time: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum WindowsProcessAccess {
    Observe,
    ModernTerminate,
}

impl WindowsProcess {
    pub(super) fn open(pid: u32, access: WindowsProcessAccess) -> Result<Option<Self>> {
        let handle = unsafe { OpenProcess(process_access_rights(access), 0, pid) };
        if handle.is_null() {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
                return Ok(None);
            }
            return Err(error).context("open residual ctx daemon process");
        }
        let creation_time = match process_creation_time(handle) {
            Ok(creation_time) => creation_time,
            Err(error) => {
                unsafe { CloseHandle(handle) };
                return Err(error);
            }
        };
        let process = Self {
            handle,
            pid,
            creation_time,
        };
        if process.is_running()? {
            Ok(Some(process))
        } else {
            Ok(None)
        }
    }

    #[cfg(test)]
    pub(super) fn executable_path(&self) -> Option<PathBuf> {
        let mut buffer = vec![0_u16; 32_768];
        let mut length = u32::try_from(buffer.len()).ok()?;
        if unsafe {
            QueryFullProcessImageNameW(self.handle, 0, buffer.as_mut_ptr(), &raw mut length)
        } == 0
        {
            return None;
        }
        Some(PathBuf::from(String::from_utf16_lossy(
            &buffer[..usize::try_from(length).ok()?],
        )))
    }

    pub(super) fn is_running(&self) -> Result<bool> {
        match unsafe { WaitForSingleObject(self.handle, 0) } {
            WAIT_TIMEOUT => Ok(true),
            WAIT_OBJECT_0 => Ok(false),
            WAIT_FAILED => Err(std::io::Error::last_os_error())
                .context("inspect residual ctx daemon process state"),
            status => Err(anyhow!(
                "unexpected residual ctx daemon process wait status {status}"
            )),
        }
    }

    pub(super) fn wait_for_exit(&self, timeout: Duration) -> Result<()> {
        let timeout_ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
        match unsafe { WaitForSingleObject(self.handle, timeout_ms) } {
            WAIT_OBJECT_0 => Ok(()),
            WAIT_TIMEOUT => Err(anyhow!(
                "released ctx daemon owner process {} did not exit within {:?}",
                self.pid,
                timeout
            )),
            WAIT_FAILED => {
                Err(std::io::Error::last_os_error()).context("wait for residual ctx daemon process")
            }
            status => Err(anyhow!(
                "unexpected residual ctx daemon process wait status {status}"
            )),
        }
    }
}

impl Drop for WindowsProcess {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.handle) };
    }
}

pub(super) fn process_access_rights(access: WindowsProcessAccess) -> u32 {
    let base = PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE;
    match access {
        WindowsProcessAccess::Observe => base,
        WindowsProcessAccess::ModernTerminate => base | PROCESS_TERMINATE,
    }
}

fn process_creation_time(handle: HANDLE) -> Result<u64> {
    let mut creation = MaybeUninit::<FILETIME>::zeroed();
    let mut exit = MaybeUninit::<FILETIME>::zeroed();
    let mut kernel = MaybeUninit::<FILETIME>::zeroed();
    let mut user = MaybeUninit::<FILETIME>::zeroed();
    if unsafe {
        GetProcessTimes(
            handle,
            creation.as_mut_ptr(),
            exit.as_mut_ptr(),
            kernel.as_mut_ptr(),
            user.as_mut_ptr(),
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error())
            .context("read residual ctx daemon process creation identity");
    }
    Ok(filetime_value(unsafe { creation.assume_init() }))
}

pub(super) fn filetime_value(value: FILETIME) -> u64 {
    (u64::from(value.dwHighDateTime) << 32) | u64::from(value.dwLowDateTime)
}

pub(super) fn filetime_unix_ms(value: u64) -> Option<i64> {
    let ticks = value.checked_sub(WINDOWS_TO_UNIX_EPOCH_100NS)?;
    i64::try_from(ticks / 10_000).ok()
}

#[cfg(test)]
mod tests {
    use std::{env, fs, path::Path};

    use super::super::tests::{
        fixture_test_guard, spawn_fixture_child, wait_for_path, DaemonFixture,
    };
    use super::super::{image_identity::same_windows_path, wait_for_released_residual_daemon};
    use super::*;
    use crate::{daemon_lock_path, observe_pid_advisory_lock, PidAdvisoryLockObservation};

    #[test]
    fn modern_termination_rights_do_not_request_vm_read() {
        use windows_sys::Win32::System::Threading::{
            PROCESS_QUERY_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
            PROCESS_TERMINATE, PROCESS_VM_READ,
        };
        let modern = process_access_rights(WindowsProcessAccess::ModernTerminate);
        assert_eq!(
            modern,
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE | PROCESS_TERMINATE
        );
        assert_eq!(modern & (PROCESS_QUERY_INFORMATION | PROCESS_VM_READ), 0);
    }

    #[test]
    fn renamed_digest_bearing_owner_terminates_with_modern_rights() {
        let _serial = fixture_test_guard();
        let mut fixture = DaemonFixture::start();
        let target = WindowsProcess::open(fixture.owner.id(), WindowsProcessAccess::Observe)
            .expect("open renamed modern owner signal handle")
            .expect("live renamed modern owner");
        let moved = replace_running_image(&fixture);
        assert_renamed_process_path(&target, &moved);

        let mut takeover = spawn_fixture_child(&fixture.active, &fixture.root, "takeover");
        assert!(
            takeover.wait().expect("join modern takeover").success(),
            "digest-bearing renamed-image takeover failed"
        );
        assert!(
            !target.is_running().expect("inspect modern owner signal"),
            "modern residual termination returned before process exit"
        );
        assert!(fixture
            .owner
            .try_wait()
            .expect("inspect renamed modern owner")
            .is_some());
    }

    #[test]
    fn released_renamed_digest_bearing_owner_waits_for_exit() {
        let _serial = fixture_test_guard();
        let mut fixture = DaemonFixture::start();
        let target = WindowsProcess::open(fixture.owner.id(), WindowsProcessAccess::Observe)
            .expect("open released modern owner signal handle")
            .expect("live released modern owner");
        let moved = replace_running_image(&fixture);
        assert_renamed_process_path(&target, &moved);
        fs::write(fixture.root.join("release-trigger"), b"release")
            .expect("trigger modern guard release");
        wait_for_path(&fixture.root.join("release-published"));
        assert_eq!(
            observe_pid_advisory_lock(&daemon_lock_path(&fixture.root)),
            Some(PidAdvisoryLockObservation {
                held: true,
                released: true,
            }),
            "modern fixture did not retain its guard after publishing release"
        );

        wait_for_released_residual_daemon(&fixture.root, &fixture.active)
            .expect("wait for renamed digest-bearing owner");
        assert!(
            !target.is_running().expect("inspect released modern owner"),
            "released modern owner wait returned before process exit"
        );
        let status = fixture
            .owner
            .try_wait()
            .expect("inspect clean modern owner")
            .expect("released modern owner did not exit before return");
        assert!(status.success(), "{status}");
        assert!(fixture.root.join("clean-exit").exists());
    }

    fn replace_running_image(fixture: &DaemonFixture) -> PathBuf {
        let moved = fixture.active.with_file_name("ctx.modern-running.exe");
        fs::rename(&fixture.active, &moved).expect("rename running modern image");
        fs::copy(
            env::current_exe().expect("current test image"),
            &fixture.active,
        )
        .expect("publish same-path modern takeover candidate");
        moved
    }

    fn assert_renamed_process_path(target: &WindowsProcess, moved: &Path) {
        let observed = target
            .executable_path()
            .expect("query renamed modern owner path");
        assert!(
            same_windows_path(&observed, moved),
            "QueryFullProcessImageNameW did not report the renamed modern image: {observed:?}"
        );
    }
}
