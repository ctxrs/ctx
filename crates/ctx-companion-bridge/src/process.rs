use std::{
    io::{self, Read, Write as _},
    process::{Child, ExitStatus, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

#[path = "process/mcp.rs"]
mod mcp;
#[cfg(unix)]
#[path = "process/unix.rs"]
mod platform;
#[cfg(windows)]
#[path = "process/windows.rs"]
mod platform;

pub(crate) use mcp::launch_mcp;

use crate::{
    limits::LimitConfiguration,
    protocol::InstalledCompanion,
    request::{CancellationToken, CapturedProcessRequest, ProcessRequest},
    BridgeError,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminationReason {
    Cancelled,
    WallTime,
    StdoutLimit,
    StderrLimit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExitClass {
    Success,
    Code(i32),
    #[cfg(unix)]
    Signal(i32),
    UnknownFailure,
    Terminated(TerminationReason),
}

#[derive(Debug)]
pub(crate) struct ProcessOutput {
    pub(crate) exit: ExitClass,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
    pub(crate) stdout_truncated: bool,
    pub(crate) stderr_truncated: bool,
}

#[derive(Debug)]
pub(crate) struct ProcessExit {
    pub(crate) exit: ExitClass,
}

pub(crate) fn run_captured(
    companion: &InstalledCompanion,
    request: CapturedProcessRequest,
    cancellation: &CancellationToken,
    limits: LimitConfiguration,
) -> Result<ProcessOutput, BridgeError> {
    run_captured_with_wall_time(
        companion,
        request,
        cancellation,
        limits,
        limits.captured_wall_time,
    )
}

pub(crate) fn run_maintenance(
    companion: &InstalledCompanion,
    request: CapturedProcessRequest,
    cancellation: &CancellationToken,
    limits: LimitConfiguration,
    wall_time: Duration,
) -> Result<ProcessOutput, BridgeError> {
    run_captured_with_wall_time(companion, request, cancellation, limits, wall_time)
}

fn run_captured_with_wall_time(
    companion: &InstalledCompanion,
    request: CapturedProcessRequest,
    cancellation: &CancellationToken,
    limits: LimitConfiguration,
    wall_time: Duration,
) -> Result<ProcessOutput, BridgeError> {
    let CapturedProcessRequest {
        control,
        stdin: captured_stdin,
    } = request;
    let mut command = std::process::Command::new(companion.executable());
    configure_command(&mut command, &control)?;
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let started = Instant::now();
    let (mut child, tree) = platform::spawn(&mut command)?;
    let child_stdin = child.stdin.take().ok_or_else(|| {
        tree.terminate();
        terminate_direct(&mut child);
        BridgeError::Transport(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "child stdin missing",
        ))
    })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        tree.terminate();
        terminate_direct(&mut child);
        BridgeError::Transport(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "child stdout missing",
        ))
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        tree.terminate();
        terminate_direct(&mut child);
        BridgeError::Transport(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "child stderr missing",
        ))
    })?;

    let stdout_exceeded = Arc::new(AtomicBool::new(false));
    let stderr_exceeded = Arc::new(AtomicBool::new(false));
    let stdout_reader = match spawn_reader(
        "ctx-companion-stdout",
        stdout,
        limits.stdout_bytes,
        Arc::clone(&stdout_exceeded),
    ) {
        Ok(reader) => reader,
        Err(error) => {
            tree.terminate();
            terminate_direct(&mut child);
            return Err(error);
        }
    };
    let stderr_reader = match spawn_reader(
        "ctx-companion-stderr",
        stderr,
        limits.stderr_bytes,
        Arc::clone(&stderr_exceeded),
    ) {
        Ok(reader) => reader,
        Err(error) => {
            tree.terminate();
            terminate_direct(&mut child);
            let _ = stdout_reader.join();
            return Err(error);
        }
    };
    let stdin_writer = match thread::Builder::new()
        .name("ctx-companion-stdin".to_owned())
        .spawn(move || {
            let mut child_stdin = child_stdin;
            child_stdin.write_all(&captured_stdin)
        }) {
        Ok(writer) => writer,
        Err(error) => {
            tree.terminate();
            terminate_direct(&mut child);
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err(BridgeError::Transport(error));
        }
    };

    let mut observed_status = None;
    let mut termination = None;
    loop {
        if cancellation.is_cancelled() {
            termination = Some(TerminationReason::Cancelled);
            break;
        }
        if stdout_exceeded.load(Ordering::Acquire) {
            termination = Some(TerminationReason::StdoutLimit);
            break;
        }
        if stderr_exceeded.load(Ordering::Acquire) {
            termination = Some(TerminationReason::StderrLimit);
            break;
        }
        if started.elapsed() >= wall_time {
            termination = Some(TerminationReason::WallTime);
            break;
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                observed_status = Some(status);
                break;
            }
            Ok(None) => {}
            Err(error) => {
                tree.terminate();
                terminate_direct(&mut child);
                let _ = stdin_writer.join();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(BridgeError::Transport(error));
            }
        }
        thread::sleep(Duration::from_millis(5).min(wall_time.saturating_sub(started.elapsed())));
    }

    // A companion operation never authorizes detached descendants. Closing the
    // whole containment object after direct completion also closes inherited pipes.
    tree.terminate();
    if termination.is_some() {
        terminate_direct(&mut child);
    }
    let status = match observed_status {
        Some(status) => Ok(status),
        None => child.wait().map_err(BridgeError::Transport),
    };
    let stdin_result = stdin_writer.join();
    let stdout = stdout_reader.join();
    let stderr = stderr_reader.join();
    let status = status?;
    let stdin_result = stdin_result.map_err(|_| BridgeError::WorkerFailed)?;
    let stdout = stdout
        .map_err(|_| BridgeError::WorkerFailed)?
        .map_err(BridgeError::Transport)?;
    let stderr = stderr
        .map_err(|_| BridgeError::WorkerFailed)?
        .map_err(BridgeError::Transport)?;
    if termination.is_none() {
        termination = if stdout_exceeded.load(Ordering::Acquire) {
            Some(TerminationReason::StdoutLimit)
        } else if stderr_exceeded.load(Ordering::Acquire) {
            Some(TerminationReason::StderrLimit)
        } else {
            None
        };
    }
    if termination.is_none()
        && status.success()
        && stdin_result
            .as_ref()
            .is_err_and(|error| error.kind() != io::ErrorKind::BrokenPipe)
    {
        return Err(BridgeError::Transport(stdin_result.unwrap_err()));
    }
    Ok(ProcessOutput {
        exit: termination.map_or_else(|| classify_exit(status), ExitClass::Terminated),
        stdout,
        stderr,
        stdout_truncated: stdout_exceeded.load(Ordering::Acquire),
        stderr_truncated: stderr_exceeded.load(Ordering::Acquire),
    })
}

pub(crate) fn run_streaming(
    companion: &InstalledCompanion,
    request: ProcessRequest,
    cancellation: &CancellationToken,
) -> Result<ProcessExit, BridgeError> {
    run_streaming_with_stdio_inner(
        companion,
        request,
        cancellation,
        Stdio::inherit(),
        Stdio::inherit(),
        Stdio::inherit(),
        true,
    )
}

#[cfg(test)]
pub(crate) fn run_streaming_with_stdio(
    companion: &InstalledCompanion,
    request: ProcessRequest,
    cancellation: &CancellationToken,
    stdin: Stdio,
    stdout: Stdio,
    stderr: Stdio,
) -> Result<ProcessExit, BridgeError> {
    run_streaming_with_stdio_inner(
        companion,
        request,
        cancellation,
        stdin,
        stdout,
        stderr,
        false,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_streaming_with_stdio_inner(
    companion: &InstalledCompanion,
    request: ProcessRequest,
    cancellation: &CancellationToken,
    stdin: Stdio,
    stdout: Stdio,
    stderr: Stdio,
    handoff_terminal: bool,
) -> Result<ProcessExit, BridgeError> {
    let mut command = std::process::Command::new(companion.executable());
    configure_command(&mut command, &request)?;
    command.stdin(stdin).stdout(stdout).stderr(stderr);
    let (mut child, tree) = platform::spawn(&mut command)?;
    let mut foreground = match platform::ForegroundTerminal::handoff(handoff_terminal, child.id()) {
        Ok(foreground) => foreground,
        Err(error) => {
            tree.terminate();
            terminate_direct(&mut child);
            return Err(error);
        }
    };
    let mut observed_status = None;
    let mut termination = None;
    loop {
        if cancellation.is_cancelled() {
            termination = Some(TerminationReason::Cancelled);
            break;
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                observed_status = Some(status);
                break;
            }
            Ok(None) => {}
            Err(error) => {
                tree.terminate();
                terminate_direct(&mut child);
                foreground.restore()?;
                return Err(BridgeError::Transport(error));
            }
        }
        thread::sleep(Duration::from_millis(5));
    }

    tree.terminate();
    if termination.is_some() {
        terminate_direct(&mut child);
    }
    let status = match observed_status {
        Some(status) => status,
        None => child.wait().map_err(BridgeError::Transport)?,
    };
    foreground.restore()?;
    Ok(ProcessExit {
        exit: termination.map_or_else(|| classify_exit(status), ExitClass::Terminated),
    })
}

fn configure_command(
    command: &mut std::process::Command,
    request: &ProcessRequest,
) -> Result<(), BridgeError> {
    command.args(request.arguments()).env_clear();
    for (key, value) in request.environment.iter() {
        command.env(key, value);
    }
    platform::configure_required_environment(command)
}

fn spawn_reader(
    name: &'static str,
    pipe: impl Read + Send + 'static,
    maximum: usize,
    exceeded: Arc<AtomicBool>,
) -> Result<thread::JoinHandle<io::Result<Vec<u8>>>, BridgeError> {
    thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || read_bounded_pipe(pipe, maximum, &exceeded))
        .map_err(BridgeError::Transport)
}

fn read_bounded_pipe(
    mut pipe: impl Read,
    maximum: usize,
    exceeded: &AtomicBool,
) -> io::Result<Vec<u8>> {
    let mut retained = Vec::new();
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = pipe.read(&mut buffer)?;
        if read == 0 {
            return Ok(retained);
        }
        let remaining = maximum.saturating_sub(retained.len());
        retained.extend_from_slice(&buffer[..read.min(remaining)]);
        if read > remaining {
            exceeded.store(true, Ordering::Release);
        }
    }
}

fn terminate_direct(child: &mut Child) {
    if !matches!(child.try_wait(), Ok(Some(_))) {
        let _ = child.kill();
    }
    let _ = child.wait();
}

fn classify_exit(status: ExitStatus) -> ExitClass {
    if status.success() {
        return ExitClass::Success;
    }
    if let Some(code) = status.code() {
        return ExitClass::Code(code);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        if let Some(signal) = status.signal() {
            return ExitClass::Signal(signal);
        }
    }
    ExitClass::UnknownFailure
}
