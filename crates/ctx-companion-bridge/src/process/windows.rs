use std::{
    ffi::OsString,
    io::{self, Read, Write},
    mem::size_of,
    os::windows::{
        ffi::OsStringExt as _,
        io::{AsRawHandle, FromRawHandle as _, OwnedHandle},
        process::CommandExt as _,
    },
    process::{Child, Command},
    ptr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::System::{
    Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    },
    JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    },
    SystemInformation::GetSystemWindowsDirectoryW,
    Threading::{OpenThread, ResumeThread, CREATE_SUSPENDED, THREAD_SUSPEND_RESUME},
};

use crate::BridgeError;

pub(super) struct ForegroundTerminal;

impl ForegroundTerminal {
    pub(super) fn handoff(_enabled: bool, _process_group: u32) -> Result<Self, BridgeError> {
        Ok(Self)
    }

    pub(super) fn restore(&mut self) -> Result<(), BridgeError> {
        Ok(())
    }
}

pub(super) struct ProcessTree {
    job: OwnedHandle,
}

impl ProcessTree {
    pub(super) fn terminate(&self) {
        unsafe {
            TerminateJobObject(self.job.as_raw_handle(), 1);
        }
    }
}

pub(super) fn configure_required_environment(command: &mut Command) -> Result<(), BridgeError> {
    let mut buffer = vec![0_u16; 260];
    loop {
        let returned =
            unsafe { GetSystemWindowsDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) }
                as usize;
        if returned == 0 {
            return Err(BridgeError::Transport(io::Error::last_os_error()));
        }
        if returned >= buffer.len() {
            let capacity = returned
                .checked_add(1)
                .filter(|value| *value <= 32_768)
                .ok_or(BridgeError::Limit("Windows system-root bytes"))?;
            buffer.resize(capacity, 0);
            continue;
        }
        buffer.truncate(returned);
        command.env("SystemRoot", OsString::from_wide(&buffer));
        return Ok(());
    }
}

pub(super) fn spawn(command: &mut Command) -> Result<(Child, ProcessTree), BridgeError> {
    command.creation_flags(CREATE_SUSPENDED);
    let mut child = command.spawn().map_err(BridgeError::Spawn)?;
    match start_job(&child) {
        Ok(tree) => Ok((child, tree)),
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(BridgeError::Spawn(error))
        }
    }
}

pub(super) enum PipeRead {
    Data(usize),
    Pending,
    Closed,
}

pub(super) fn prepare_pipes(
    _stdin: &std::process::ChildStdin,
    _stdout: &std::process::ChildStdout,
    _stderr: &std::process::ChildStderr,
) -> io::Result<()> {
    Ok(())
}

pub(super) fn read_pipe<T: Read + AsRawHandle>(
    pipe: &mut T,
    buffer: &mut [u8],
) -> io::Result<PipeRead> {
    use windows_sys::Win32::{
        Foundation::{GetLastError, ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_PIPE_NOT_CONNECTED},
        System::Pipes::PeekNamedPipe,
    };

    let mut available = 0u32;
    if unsafe {
        PeekNamedPipe(
            pipe.as_raw_handle(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            &mut available,
            std::ptr::null_mut(),
        )
    } == 0
    {
        let error = unsafe { GetLastError() };
        if matches!(
            error,
            ERROR_BROKEN_PIPE | ERROR_NO_DATA | ERROR_PIPE_NOT_CONNECTED
        ) {
            return Ok(PipeRead::Closed);
        }
        return Err(io::Error::from_raw_os_error(error as i32));
    }
    if available == 0 {
        return Ok(PipeRead::Pending);
    }
    let readable = buffer.len().min(available as usize);
    match pipe.read(&mut buffer[..readable])? {
        0 => Ok(PipeRead::Closed),
        read => Ok(PipeRead::Data(read)),
    }
}

/// Windows anonymous child-stdin pipes are not pollable. This is the sole
/// helper-thread mechanism for both sequential bounded writes: it is always
/// cancelled with `CancelSynchronousIo` and joined before its child is reaped.
pub(super) struct PipeWriter {
    worker: Option<std::thread::JoinHandle<(std::process::ChildStdin, io::Result<()>)>>,
    cancelled: Arc<AtomicBool>,
}

impl PipeWriter {
    pub(super) fn spawn(stdin: std::process::ChildStdin, frame: Vec<u8>) -> io::Result<Self> {
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let worker = std::thread::Builder::new()
            .name("ctx-companion-mcp-pipe-write".to_owned())
            .spawn(move || {
                let mut stdin = stdin;
                let mut offset = 0;
                let result = loop {
                    if worker_cancelled.load(Ordering::Acquire) {
                        break Err(io::Error::new(
                            io::ErrorKind::Interrupted,
                            "MCP request write cancelled",
                        ));
                    }
                    if offset == frame.len() {
                        break stdin.flush();
                    }
                    match stdin.write(&frame[offset..]) {
                        Ok(0) => {
                            break Err(io::Error::new(
                                io::ErrorKind::WriteZero,
                                "MCP request pipe closed",
                            ));
                        }
                        Ok(written) => offset += written,
                        Err(error) => break Err(error),
                    }
                };
                (stdin, result)
            })?;
        Ok(Self {
            worker: Some(worker),
            cancelled,
        })
    }

    pub(super) fn poll(&mut self) -> Option<io::Result<std::process::ChildStdin>> {
        if !self.worker.as_ref()?.is_finished() {
            return None;
        }
        let worker = self.worker.take()?;
        Some(match worker.join() {
            Ok((stdin, result)) => result.map(|()| stdin),
            Err(_) => Err(io::Error::other("MCP request writer panicked")),
        })
    }

    pub(super) fn cancel_and_join(mut self) -> io::Result<()> {
        use windows_sys::Win32::Foundation::{GetLastError, ERROR_NOT_FOUND};
        use windows_sys::Win32::System::IO::CancelSynchronousIo;

        let Some(worker) = self.worker.take() else {
            return Ok(());
        };
        // Set the flag before native cancellation. If the writer is between
        // writes (`ERROR_NOT_FOUND`), retry across that race; if it is in a
        // write, CancelSynchronousIo completes it with cancellation.
        self.cancelled.store(true, Ordering::Release);
        let cancellation_error = loop {
            if worker.is_finished() {
                break None;
            }
            if unsafe { CancelSynchronousIo(worker.as_raw_handle()) } != 0 {
                break None;
            }
            let error = unsafe { GetLastError() };
            if error != ERROR_NOT_FOUND {
                break Some(io::Error::from_raw_os_error(error as i32));
            }
            // The writer can be between its cancellation check and the next
            // blocking write. Retry until that write is either cancelled or
            // the worker observes the flag and exits.
            std::thread::yield_now();
        };
        match worker.join() {
            Ok(_) => cancellation_error.map_or(Ok(()), Err),
            Err(_) => Err(io::Error::other("MCP request writer panicked")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PipeWriter;
    use std::{
        process::Stdio,
        thread,
        time::{Duration, Instant},
    };

    #[test]
    fn cancelled_request_writer_joins_without_leaking() {
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "timeout /T 60 /NOBREAK >NUL"])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let writer =
            PipeWriter::spawn(child.stdin.take().unwrap(), vec![b'x'; 4 * 1024 * 1024]).unwrap();
        thread::sleep(Duration::from_millis(25));
        let started = Instant::now();
        writer.cancel_and_join().unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn start_job(child: &Child) -> io::Result<ProcessTree> {
    let raw_job = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
    if raw_job.is_null() {
        return Err(io::Error::last_os_error());
    }
    let job = unsafe { OwnedHandle::from_raw_handle(raw_job) };
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    if unsafe {
        SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            ptr::addr_of!(limits).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    } == 0
        || unsafe { AssignProcessToJobObject(job.as_raw_handle(), child.as_raw_handle()) } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let tree = ProcessTree { job };
    if let Err(error) = resume_process(child.id()) {
        tree.terminate();
        return Err(error);
    }
    Ok(tree)
}

fn resume_process(process_id: u32) -> io::Result<()> {
    let raw_snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if raw_snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let snapshot = unsafe { OwnedHandle::from_raw_handle(raw_snapshot) };
    let mut entry = THREADENTRY32 {
        dwSize: size_of::<THREADENTRY32>() as u32,
        ..THREADENTRY32::default()
    };
    if unsafe { Thread32First(snapshot.as_raw_handle(), &raw mut entry) } == 0 {
        return Err(io::Error::last_os_error());
    }
    loop {
        if entry.th32OwnerProcessID == process_id {
            let raw_thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if raw_thread.is_null() {
                return Err(io::Error::last_os_error());
            }
            let thread = unsafe { OwnedHandle::from_raw_handle(raw_thread) };
            if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
                return Err(io::Error::last_os_error());
            }
            return Ok(());
        }
        if unsafe { Thread32Next(snapshot.as_raw_handle(), &raw mut entry) } == 0 {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "suspended companion thread not found",
            ));
        }
    }
}
