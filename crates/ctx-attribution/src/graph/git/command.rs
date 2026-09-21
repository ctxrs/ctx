//! Bounded, timeout-enforced Git command execution.

use std::io::Read;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::unix::process::CommandExt as _;

use crate::git_executable::GitExecutable;
use crate::query::QueryError;

pub(super) const MAX_GIT_OUTPUT_BYTES: u64 = 4 * 1024 * 1024;
pub(super) const MAX_GIT_IDENTITY_BYTES: u64 = 8 * 1024;
pub(super) const GIT_COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
const GIT_WAIT_POLL: Duration = Duration::from_millis(5);

pub(super) fn require_git_success<'a>(
    git_executable: &GitExecutable,
    arguments: impl IntoIterator<Item = &'a str>,
    maximum_bytes: u64,
) -> Result<(), QueryError> {
    let (status, _) = run_git(git_executable, arguments, maximum_bytes)?;
    status
        .success()
        .then_some(())
        .ok_or(QueryError::RepositoryUnavailable)
}

pub(super) fn git_output<'a>(
    git_executable: &GitExecutable,
    arguments: impl IntoIterator<Item = &'a str>,
    maximum_bytes: u64,
) -> Result<Vec<u8>, QueryError> {
    let (status, output) = run_git(git_executable, arguments, maximum_bytes)?;
    status
        .success()
        .then_some(output)
        .ok_or(QueryError::RepositoryUnavailable)
}

pub(super) fn git_text<'a>(
    git_executable: &GitExecutable,
    arguments: impl IntoIterator<Item = &'a str>,
    maximum_bytes: u64,
) -> Result<String, QueryError> {
    git_text_with_timeout(
        git_executable,
        arguments,
        maximum_bytes,
        GIT_COMMAND_TIMEOUT,
    )
}

pub(super) fn git_text_with_timeout<'a>(
    git_executable: &GitExecutable,
    arguments: impl IntoIterator<Item = &'a str>,
    maximum_bytes: u64,
    timeout: Duration,
) -> Result<String, QueryError> {
    let (status, output) = run_git_with_timeout(git_executable, arguments, maximum_bytes, timeout)?;
    if !status.success() {
        return Err(QueryError::RepositoryUnavailable);
    }
    String::from_utf8(output).map_err(|_| QueryError::RepositoryUnavailable)
}

pub(super) fn run_git<'a>(
    git_executable: &GitExecutable,
    arguments: impl IntoIterator<Item = &'a str>,
    maximum_bytes: u64,
) -> Result<(ExitStatus, Vec<u8>), QueryError> {
    run_git_with_timeout(
        git_executable,
        arguments,
        maximum_bytes,
        GIT_COMMAND_TIMEOUT,
    )
}

pub(super) fn run_git_with_timeout<'a>(
    git_executable: &GitExecutable,
    arguments: impl IntoIterator<Item = &'a str>,
    maximum_bytes: u64,
    timeout: Duration,
) -> Result<(ExitStatus, Vec<u8>), QueryError> {
    git_executable
        .verify()
        .map_err(|_| QueryError::GitUnavailable)?;
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or(QueryError::RepositoryUnavailable)?;
    let null_device = if cfg!(target_os = "windows") {
        "NUL"
    } else {
        "/dev/null"
    };
    let mut command = Command::new(git_executable.path());
    command
        .env_clear()
        .arg("-c")
        .arg(format!("core.hooksPath={null_device}"))
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "maintenance.auto=false",
            "-c",
            "gc.auto=0",
            "-c",
            "credential.helper=",
            "-c",
            "credential.interactive=never",
            "-c",
            "core.askPass=",
            "-c",
            "protocol.allow=never",
            "-c",
            "protocol.file.allow=never",
            "-c",
            "submodule.recurse=false",
            "-c",
            "fetch.recurseSubmodules=false",
        ])
        .args(arguments)
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_SYSTEM", null_device)
        .env("GIT_CONFIG_GLOBAL", null_device)
        .env("GIT_CONFIG_COUNT", "0")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_PROTOCOL_FROM_USER", "0")
        .env("GIT_DISCOVERY_ACROSS_FILESYSTEM", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_PAGER", "")
        .env("GIT_EDITOR", "false")
        .env("GIT_SEQUENCE_EDITOR", "false")
        .env("GIT_MERGE_AUTOEDIT", "no")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    command.process_group(0);
    let mut child = spawn_git(&mut command)?;
    let Some(stdout) = child.stdout.take() else {
        terminate_git(&mut child);
        return Err(QueryError::Backend(
            "Git stdout pipe was unavailable".to_owned(),
        ));
    };
    let (overflow_sender, overflow_receiver) = mpsc::channel();
    let (output_sender, output_receiver) = mpsc::sync_channel(1);
    let reader = thread::spawn(move || {
        let _ = output_sender.send(drain_bounded_stdout(stdout, maximum_bytes, overflow_sender));
    });
    let mut status = None;
    let mut output = None;
    loop {
        if overflow_receiver.try_recv().is_ok() {
            terminate_git(&mut child);
            let _ = reader.join();
            return Err(QueryError::Backend(
                "Git output exceeded the attribution query bound".to_owned(),
            ));
        }
        if status.is_none() {
            match child.try_wait() {
                Ok(Some(completed)) => status = Some(completed),
                Ok(None) => {}
                Err(_) => {
                    terminate_git(&mut child);
                    let _ = reader.join();
                    return Err(QueryError::GitUnavailable);
                }
            }
        }
        if output.is_none() {
            match output_receiver.try_recv() {
                Ok(drained) => output = Some(drained),
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    terminate_git(&mut child);
                    let _ = reader.join();
                    return Err(QueryError::Backend("Git stdout reader failed".to_owned()));
                }
            }
        }
        if status.is_some() && output.is_some() {
            break;
        }
        if Instant::now() >= deadline {
            terminate_git(&mut child);
            let _ = reader.join();
            return Err(QueryError::GitUnavailable);
        }
        thread::sleep(GIT_WAIT_POLL);
    }
    reader
        .join()
        .map_err(|_| QueryError::Backend("Git stdout reader failed".to_owned()))?;
    let output = output
        .ok_or_else(|| QueryError::Backend("Git stdout reader lost output".to_owned()))?
        .map_err(|_| QueryError::Backend("Git stdout read failed".to_owned()))?;
    if output.overflowed {
        return Err(QueryError::Backend(
            "Git output exceeded the attribution query bound".to_owned(),
        ));
    }
    git_executable
        .verify()
        .map_err(|_| QueryError::GitUnavailable)?;
    Ok((
        status.ok_or(QueryError::RepositoryUnavailable)?,
        output.bytes,
    ))
}

fn spawn_git(command: &mut Command) -> Result<std::process::Child, QueryError> {
    command.spawn().map_err(|_| QueryError::GitUnavailable)
}

struct BoundedGitOutput {
    bytes: Vec<u8>,
    overflowed: bool,
}

fn drain_bounded_stdout(
    mut stdout: impl Read,
    maximum_bytes: u64,
    overflow_sender: mpsc::Sender<()>,
) -> std::io::Result<BoundedGitOutput> {
    let retained_limit = usize::try_from(maximum_bytes.saturating_add(1)).unwrap_or(usize::MAX);
    let mut bytes = Vec::with_capacity(retained_limit.min(64 * 1024));
    let mut buffer = [0_u8; 16 * 1024];
    let mut overflowed = false;
    loop {
        let read = stdout.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let remaining = retained_limit.saturating_sub(bytes.len());
        bytes.extend_from_slice(&buffer[..read.min(remaining)]);
        if !overflowed && u64::try_from(bytes.len()).map_or(true, |length| length > maximum_bytes) {
            overflowed = true;
            let _ = overflow_sender.send(());
        }
    }
    Ok(BoundedGitOutput { bytes, overflowed })
}

fn terminate_git(child: &mut std::process::Child) {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if let Ok(raw_pid) = i32::try_from(child.id())
        && let Some(process_group) = rustix::process::Pid::from_raw(raw_pid)
    {
        let _ = rustix::process::kill_process_group(process_group, rustix::process::Signal::KILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
#[path = "command_tests.rs"]
mod tests;
