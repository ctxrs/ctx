use std::{
    io::{self, Read},
    os::unix::{io::AsRawFd as _, process::CommandExt as _},
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command},
};

use crate::BridgeError;

pub(super) struct ForegroundTerminal {
    terminal: Option<(libc::c_int, libc::pid_t)>,
}

impl ForegroundTerminal {
    pub(super) fn handoff(enabled: bool, process_group: u32) -> Result<Self, BridgeError> {
        if !enabled || unsafe { libc::isatty(libc::STDIN_FILENO) } != 1 {
            return Ok(Self { terminal: None });
        }
        let previous_group = unsafe { libc::tcgetpgrp(libc::STDIN_FILENO) };
        if previous_group == -1 {
            return Err(BridgeError::Transport(io::Error::last_os_error()));
        }
        // Do not steal a terminal when ctx itself was launched as a background job.
        if previous_group != unsafe { libc::getpgrp() } {
            return Ok(Self { terminal: None });
        }
        let process_group = libc::pid_t::try_from(process_group)
            .map_err(|_| BridgeError::Transport(io::Error::other("invalid process group")))?;
        set_foreground_process_group(libc::STDIN_FILENO, process_group)
            .map_err(BridgeError::Transport)?;
        Ok(Self {
            terminal: Some((libc::STDIN_FILENO, previous_group)),
        })
    }

    pub(super) fn restore(&mut self) -> Result<(), BridgeError> {
        let Some((terminal, process_group)) = self.terminal.take() else {
            return Ok(());
        };
        set_foreground_process_group(terminal, process_group).map_err(BridgeError::Transport)
    }
}

impl Drop for ForegroundTerminal {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

fn set_foreground_process_group(
    terminal: libc::c_int,
    process_group: libc::pid_t,
) -> io::Result<()> {
    // Restoring the parent's foreground group is itself a background-terminal
    // operation. Blocking SIGTTOU for this thread makes tcsetpgrp atomic without
    // changing the process-wide signal disposition.
    let mut blocked = unsafe { std::mem::zeroed::<libc::sigset_t>() };
    let mut previous = unsafe { std::mem::zeroed::<libc::sigset_t>() };
    if unsafe { libc::sigemptyset(&raw mut blocked) } == -1
        || unsafe { libc::sigaddset(&raw mut blocked, libc::SIGTTOU) } == -1
    {
        return Err(io::Error::last_os_error());
    }
    let mask_result =
        unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &raw const blocked, &raw mut previous) };
    if mask_result != 0 {
        return Err(io::Error::from_raw_os_error(mask_result));
    }
    let result = if unsafe { libc::tcsetpgrp(terminal, process_group) } == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    };
    let restore_result = unsafe {
        libc::pthread_sigmask(libc::SIG_SETMASK, &raw const previous, std::ptr::null_mut())
    };
    if restore_result != 0 && result.is_ok() {
        return Err(io::Error::from_raw_os_error(restore_result));
    }
    result
}

pub(super) struct ProcessTree {
    process_group: u32,
}

impl ProcessTree {
    pub(super) fn terminate(&self) {
        if let Some(process_group) = i32::try_from(self.process_group)
            .ok()
            .and_then(i32::checked_neg)
        {
            unsafe {
                libc::kill(process_group, libc::SIGKILL);
            }
        }
    }
}

pub(super) fn configure_required_environment(_command: &mut Command) -> Result<(), BridgeError> {
    Ok(())
}

pub(super) fn spawn(command: &mut Command) -> Result<(Child, ProcessTree), BridgeError> {
    #[cfg(target_os = "linux")]
    let expected_parent = unsafe { libc::getpid() };
    command.process_group(0);
    unsafe {
        command.pre_exec(move || {
            #[cfg(target_os = "linux")]
            {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) == -1 {
                    return Err(io::Error::last_os_error());
                }
                if libc::getppid() != expected_parent {
                    libc::kill(libc::getpid(), libc::SIGKILL);
                    libc::_exit(127);
                }
            }
            Ok(())
        });
    }
    let child = command.spawn().map_err(BridgeError::Spawn)?;
    let tree = ProcessTree {
        process_group: child.id(),
    };
    Ok((child, tree))
}

pub(super) enum PipeRead {
    Data(usize),
    Pending,
    Closed,
}

pub(super) fn prepare_pipes(
    stdin: &ChildStdin,
    stdout: &ChildStdout,
    stderr: &ChildStderr,
) -> io::Result<()> {
    for fd in [stdin.as_raw_fd(), stdout.as_raw_fd(), stderr.as_raw_fd()] {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags == -1 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1
        {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

pub(super) fn read_pipe(pipe: &mut impl Read, buffer: &mut [u8]) -> io::Result<PipeRead> {
    match pipe.read(buffer) {
        Ok(0) => Ok(PipeRead::Closed),
        Ok(read) => Ok(PipeRead::Data(read)),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(PipeRead::Pending),
        Err(error) => Err(error),
    }
}
