use super::*;
use std::{
    os::windows::{
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
        process::CommandExt,
    },
    process::Command,
    thread,
};
use windows_sys::Win32::{
    Foundation::WAIT_OBJECT_0,
    System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE},
};

#[test]
fn windows_task_launcher_has_gui_subsystem() {
    // PE header offset in the DOS header, then signature + COFF header.
    let pe = u32::from_le_bytes(WINDOWS_TASK_LAUNCHER[0x3c..0x40].try_into().unwrap()) as usize;
    let optional_header = pe + 4 + 20;
    let subsystem = &WINDOWS_TASK_LAUNCHER[optional_header + 68..optional_header + 70];
    assert_eq!(
        subsystem,
        2u16.to_le_bytes(),
        "expected IMAGE_SUBSYSTEM_WINDOWS_GUI"
    );
}

#[test]
fn ending_windows_task_launcher_releases_its_powershell_process() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let launcher = temp.path().join(WINDOWS_TASK_LAUNCHER_NAME);
    fs::write(&launcher, WINDOWS_TASK_LAUNCHER)?;
    let marker = temp.path().join("supervisor.pid");
    let script = format!(
        "[IO.File]::WriteAllText('{}', [string]$PID); Start-Sleep -Seconds 30",
        powershell_single_quote(marker.to_str().unwrap())
    );
    let root = PathBuf::from(std::env::var_os("SystemRoot").unwrap());
    let (_, arguments) =
        windows_task_action(&temp.path().join("windows-task.xml"), &root, &script)?;
    let mut action = Command::new(launcher).raw_arg(arguments).spawn()?;
    let deadline = Instant::now() + Duration::from_secs(15);
    let pid = loop {
        if let Some(pid) = fs::read_to_string(&marker)
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
        {
            break pid;
        }
        if action.try_wait()?.is_some() || Instant::now() >= deadline {
            return Err(anyhow!("PowerShell did not publish its PID"));
        }
        thread::sleep(Duration::from_millis(25));
    };
    let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    if handle.is_null() {
        return Err(std::io::Error::last_os_error().into());
    }
    let child = unsafe { OwnedHandle::from_raw_handle(handle) };
    action.kill()?;
    action.wait()?;
    assert_eq!(
        unsafe { WaitForSingleObject(child.as_raw_handle(), 5000) },
        WAIT_OBJECT_0
    );
    Ok(())
}
