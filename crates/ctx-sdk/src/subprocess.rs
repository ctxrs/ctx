use std::{
    io::{self, Read, Write},
    process::{Child, Command, ExitStatus},
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::process::CommandExt as _;
#[cfg(windows)]
use std::os::windows::process::CommandExt as _;

use serde_json::Value;

use super::{classify_stderr, AgentHistoryError, AgentHistoryErrorCode};

#[cfg(windows)]
mod windows;

#[cfg(windows)]
use self::windows::ProcessTree;

pub(super) const MAX_RETAINED_SUBPROCESS_STDERR_BYTES: usize = 64 * 1024;
// Matches the public complete-content IPC frame payload ceiling. This leaves
// the existing wire allowance for encoded content and its response envelope
// without permitting an unbounded SDK-side subprocess buffer.
pub(super) const MAX_RETAINED_MCP_STDOUT_BYTES: usize = 80 * 1024 * 1024;

pub(super) fn spawn_configured(command: &mut Command) -> io::Result<Child> {
    configure_command(command);
    command.spawn()
}

fn configure_command(command: &mut Command) {
    #[cfg(unix)]
    command.process_group(0);
    #[cfg(windows)]
    command.creation_flags(windows_sys::Win32::System::Threading::CREATE_SUSPENDED);
    #[cfg(not(any(unix, windows)))]
    let _ = command;
}

#[derive(Debug)]
pub(super) struct McpOutput {
    pub(super) status: ExitStatus,
    pub(super) tool_response: Option<Value>,
    pub(super) stderr: Vec<u8>,
}

pub(super) fn collect_ctx_json(
    mut child: Child,
    timeout: Duration,
) -> Result<Value, AgentHistoryError> {
    let started = Instant::now();
    let process_tree = ProcessTree::start(&child).map_err(|err| {
        stop_direct_child_and_reap(&mut child);
        AgentHistoryError::new(
            AgentHistoryErrorCode::AdapterError,
            "failed to establish ctx CLI process-tree ownership",
            true,
        )
        .with_cause(err.to_string())
    })?;
    if started.elapsed() >= timeout {
        stop_and_reap(&mut child, &process_tree);
        return Err(timeout_error());
    }
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            stop_and_reap(&mut child, &process_tree);
            return Err(AgentHistoryError::new(
                AgentHistoryErrorCode::AdapterError,
                "ctx CLI stdout was unavailable",
                true,
            ));
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            stop_and_reap(&mut child, &process_tree);
            return Err(AgentHistoryError::new(
                AgentHistoryErrorCode::AdapterError,
                "ctx CLI stderr was unavailable",
                true,
            ));
        }
    };
    let stdout_reader = match thread::Builder::new()
        .name("ctx-sdk-cli-stdout".to_owned())
        .spawn(move || read_json_pipe(stdout))
    {
        Ok(reader) => reader,
        Err(err) => {
            stop_and_reap(&mut child, &process_tree);
            return Err(AgentHistoryError::new(
                AgentHistoryErrorCode::AdapterError,
                "failed to start ctx CLI stdout reader",
                true,
            )
            .with_cause(err.to_string()));
        }
    };
    let stderr_reader = match thread::Builder::new()
        .name("ctx-sdk-cli-stderr".to_owned())
        .spawn(move || read_bounded_pipe(stderr, MAX_RETAINED_SUBPROCESS_STDERR_BYTES))
    {
        Ok(reader) => reader,
        Err(err) => {
            stop_and_reap(&mut child, &process_tree);
            return Err(AgentHistoryError::new(
                AgentHistoryErrorCode::AdapterError,
                "failed to start ctx CLI stderr reader",
                true,
            )
            .with_cause(err.to_string()));
        }
    };

    let mut status = None;
    loop {
        if status.is_none() {
            match child.try_wait() {
                Ok(observed) => status = observed,
                Err(err) => {
                    stop_and_reap(&mut child, &process_tree);
                    return Err(AgentHistoryError::new(
                        AgentHistoryErrorCode::AdapterError,
                        "failed to wait for ctx CLI",
                        true,
                    )
                    .with_cause(err.to_string()));
                }
            }
        }
        if status.is_some() && stdout_reader.is_finished() && stderr_reader.is_finished() {
            break;
        }
        if started.elapsed() >= timeout {
            stop_and_reap(&mut child, &process_tree);
            return Err(timeout_error());
        }
        thread::sleep(Duration::from_millis(20).min(timeout.saturating_sub(started.elapsed())));
    }

    let stdout = stdout_reader.join();
    let stderr = stderr_reader.join();
    let stderr = stderr
        .map_err(|_| {
            AgentHistoryError::new(
                AgentHistoryErrorCode::AdapterError,
                "ctx CLI stderr reader panicked",
                true,
            )
        })?
        .map_err(|err| {
            AgentHistoryError::new(
                AgentHistoryErrorCode::AdapterError,
                "failed to read ctx CLI stderr",
                true,
            )
            .with_cause(err.to_string())
        })?;
    let Some(status) = status else {
        stop_and_reap(&mut child, &process_tree);
        return Err(AgentHistoryError::new(
            AgentHistoryErrorCode::AdapterError,
            "ctx CLI completed without a process status",
            true,
        ));
    };
    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr);
        return Err(AgentHistoryError::new(
            classify_stderr(&stderr),
            stderr.trim().to_owned(),
            false,
        ));
    }

    match stdout.map_err(|_| {
        AgentHistoryError::new(
            AgentHistoryErrorCode::AdapterError,
            "ctx CLI stdout reader panicked",
            true,
        )
    })? {
        Ok(value) => Ok(value),
        Err(JsonPipeError::Decode(err)) => Err(AgentHistoryError::new(
            AgentHistoryErrorCode::DecodeError,
            "failed to decode ctx JSON",
            false,
        )
        .with_cause(err.to_string())),
        Err(JsonPipeError::Read(err)) => Err(AgentHistoryError::new(
            AgentHistoryErrorCode::AdapterError,
            "failed to read ctx CLI stdout",
            true,
        )
        .with_cause(err.to_string())),
    }
}

pub(super) fn collect_ctx_mcp_output(
    mut child: Child,
    stdin_bytes: Vec<u8>,
    timeout: Duration,
) -> Result<McpOutput, AgentHistoryError> {
    let started = Instant::now();
    let process_tree = ProcessTree::start(&child).map_err(|err| {
        stop_direct_child_and_reap(&mut child);
        AgentHistoryError::new(
            AgentHistoryErrorCode::AdapterError,
            "failed to establish ctx MCP process-tree ownership",
            true,
        )
        .with_cause(err.to_string())
    })?;
    if started.elapsed() >= timeout {
        stop_and_reap(&mut child, &process_tree);
        return Err(mcp_timeout_error());
    }
    let stdin = match child.stdin.take() {
        Some(stdin) => stdin,
        None => {
            stop_and_reap(&mut child, &process_tree);
            return Err(AgentHistoryError::new(
                AgentHistoryErrorCode::AdapterError,
                "ctx MCP stdin was unavailable",
                true,
            ));
        }
    };
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            stop_and_reap(&mut child, &process_tree);
            return Err(AgentHistoryError::new(
                AgentHistoryErrorCode::AdapterError,
                "ctx MCP stdout was unavailable",
                true,
            ));
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            stop_and_reap(&mut child, &process_tree);
            return Err(AgentHistoryError::new(
                AgentHistoryErrorCode::AdapterError,
                "ctx MCP stderr was unavailable",
                true,
            ));
        }
    };
    let stdout_reader = match thread::Builder::new()
        .name("ctx-sdk-mcp-stdout".to_owned())
        .spawn(move || read_mcp_stdout(stdout, MAX_RETAINED_MCP_STDOUT_BYTES))
    {
        Ok(reader) => reader,
        Err(err) => {
            stop_and_reap(&mut child, &process_tree);
            return Err(thread_start_error("ctx MCP stdout reader", err));
        }
    };
    let stderr_reader = match thread::Builder::new()
        .name("ctx-sdk-mcp-stderr".to_owned())
        .spawn(move || read_bounded_pipe(stderr, MAX_RETAINED_SUBPROCESS_STDERR_BYTES))
    {
        Ok(reader) => reader,
        Err(err) => {
            stop_and_reap(&mut child, &process_tree);
            return Err(thread_start_error("ctx MCP stderr reader", err));
        }
    };
    let stdin_writer = match thread::Builder::new()
        .name("ctx-sdk-mcp-stdin".to_owned())
        .spawn(move || {
            let mut stdin = stdin;
            stdin.write_all(&stdin_bytes)
        }) {
        Ok(writer) => writer,
        Err(err) => {
            stop_and_reap(&mut child, &process_tree);
            return Err(thread_start_error("ctx MCP stdin writer", err));
        }
    };

    let mut status = None;
    loop {
        if status.is_none() {
            match child.try_wait() {
                Ok(observed) => status = observed,
                Err(err) => {
                    stop_and_reap(&mut child, &process_tree);
                    return Err(AgentHistoryError::new(
                        AgentHistoryErrorCode::AdapterError,
                        "failed to wait for ctx MCP server",
                        true,
                    )
                    .with_cause(err.to_string()));
                }
            }
        }
        if status.is_some()
            && stdin_writer.is_finished()
            && stdout_reader.is_finished()
            && stderr_reader.is_finished()
        {
            break;
        }
        if started.elapsed() >= timeout {
            stop_and_reap(&mut child, &process_tree);
            return Err(mcp_timeout_error());
        }
        thread::sleep(Duration::from_millis(20).min(timeout.saturating_sub(started.elapsed())));
    }

    join_mcp_io(
        stdin_writer,
        "ctx MCP stdin writer panicked",
        "failed to write ctx MCP request",
    )?;
    let stdout = join_mcp_stdout_reader(stdout_reader)?;
    let stderr = join_mcp_io(
        stderr_reader,
        "ctx MCP stderr reader panicked",
        "failed to read ctx MCP stderr",
    )?;
    let Some(status) = status else {
        stop_and_reap(&mut child, &process_tree);
        return Err(AgentHistoryError::new(
            AgentHistoryErrorCode::AdapterError,
            "ctx MCP completed without a process status",
            true,
        ));
    };
    let tool_response = resolve_mcp_stdout(stdout, status.success())?;
    Ok(McpOutput {
        status,
        tool_response,
        stderr,
    })
}

enum JsonPipeError {
    Decode(serde_json::Error),
    Read(io::Error),
}

fn read_json_pipe(mut pipe: impl Read) -> Result<Value, JsonPipeError> {
    match serde_json::from_reader(&mut pipe) {
        Ok(value) => Ok(value),
        Err(err) if err.is_io() => Err(JsonPipeError::Read(io::Error::new(
            err.io_error_kind().unwrap_or(io::ErrorKind::Other),
            err.to_string(),
        ))),
        Err(err) => {
            io::copy(&mut pipe, &mut io::sink()).map_err(JsonPipeError::Read)?;
            Err(JsonPipeError::Decode(err))
        }
    }
}

pub(super) fn read_bounded_pipe(mut pipe: impl Read, maximum: usize) -> io::Result<Vec<u8>> {
    let mut retained = Vec::new();
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = pipe.read(&mut buffer)?;
        if read == 0 {
            return Ok(retained);
        }
        let remaining = maximum.saturating_sub(retained.len());
        retained.extend_from_slice(&buffer[..read.min(remaining)]);
    }
}

enum McpStdoutError {
    Decode(serde_json::Error),
    LimitExceeded { maximum: usize },
    Read(io::Error),
}

fn read_mcp_stdout(
    mut pipe: impl Read,
    maximum_response_bytes: usize,
) -> Result<Option<Value>, McpStdoutError> {
    let mut response = None;
    let mut line = Vec::new();
    let mut failure = None;
    let mut observed_bytes = 0_usize;
    let mut buffer = [0_u8; 8 * 1024];

    loop {
        let read = pipe.read(&mut buffer).map_err(McpStdoutError::Read)?;
        if read == 0 {
            if failure.is_none() && !line.is_empty() {
                finish_mcp_stdout_line(&mut line, &mut response, &mut failure);
            }
            return failure.map_or(Ok(response), Err);
        }

        observed_bytes = observed_bytes.saturating_add(read);
        if observed_bytes > maximum_response_bytes {
            failure = Some(McpStdoutError::LimitExceeded {
                maximum: maximum_response_bytes,
            });
            line.clear();
            continue;
        }

        for &byte in &buffer[..read] {
            if failure.is_some() {
                continue;
            }
            if byte == b'\n' {
                finish_mcp_stdout_line(&mut line, &mut response, &mut failure);
            } else {
                line.push(byte);
            }
        }
    }
}

fn finish_mcp_stdout_line(
    line: &mut Vec<u8>,
    response: &mut Option<Value>,
    failure: &mut Option<McpStdoutError>,
) {
    if line.is_empty() {
        return;
    }
    match serde_json::from_slice::<Value>(line) {
        Ok(value) => {
            if value.get("id") == Some(&Value::from(2)) {
                *response = Some(value);
            }
        }
        Err(err) => *failure = Some(McpStdoutError::Decode(err)),
    }
    line.clear();
}

fn join_mcp_stdout_reader(
    task: thread::JoinHandle<Result<Option<Value>, McpStdoutError>>,
) -> Result<Result<Option<Value>, McpStdoutError>, AgentHistoryError> {
    task.join().map_err(|_| {
        AgentHistoryError::new(
            AgentHistoryErrorCode::AdapterError,
            "ctx MCP stdout reader panicked",
            true,
        )
    })
}

fn resolve_mcp_stdout(
    stdout: Result<Option<Value>, McpStdoutError>,
    status_success: bool,
) -> Result<Option<Value>, AgentHistoryError> {
    match stdout {
        Ok(response) if status_success => Ok(response),
        Ok(_) => Ok(None),
        Err(McpStdoutError::Decode(_) | McpStdoutError::LimitExceeded { .. })
            if !status_success =>
        {
            Ok(None)
        }
        Err(McpStdoutError::Decode(err)) => Err(AgentHistoryError::new(
            AgentHistoryErrorCode::DecodeError,
            "failed to decode ctx MCP response",
            false,
        )
        .with_cause(err.to_string())),
        Err(McpStdoutError::LimitExceeded { maximum }) => Err(AgentHistoryError::new(
            AgentHistoryErrorCode::AdapterError,
            format!("ctx MCP stdout exceeded the {maximum}-byte response limit"),
            false,
        )),
        Err(McpStdoutError::Read(err)) => Err(AgentHistoryError::new(
            AgentHistoryErrorCode::AdapterError,
            "failed to read ctx MCP stdout",
            true,
        )
        .with_cause(err.to_string())),
    }
}

fn join_mcp_io<T>(
    task: thread::JoinHandle<io::Result<T>>,
    panic_message: &'static str,
    io_message: &'static str,
) -> Result<T, AgentHistoryError> {
    task.join()
        .map_err(|_| {
            AgentHistoryError::new(AgentHistoryErrorCode::AdapterError, panic_message, true)
        })?
        .map_err(|err| {
            AgentHistoryError::new(AgentHistoryErrorCode::AdapterError, io_message, true)
                .with_cause(err.to_string())
        })
}

fn thread_start_error(task: &str, err: io::Error) -> AgentHistoryError {
    AgentHistoryError::new(
        AgentHistoryErrorCode::AdapterError,
        format!("failed to start {task}"),
        true,
    )
    .with_cause(err.to_string())
}

fn stop_and_reap(child: &mut Child, process_tree: &ProcessTree) {
    process_tree.terminate();
    stop_direct_child_and_reap(child);
}

fn timeout_error() -> AgentHistoryError {
    AgentHistoryError::new(
        AgentHistoryErrorCode::Timeout,
        "ctx CLI command timed out",
        true,
    )
}

fn mcp_timeout_error() -> AgentHistoryError {
    AgentHistoryError::new(
        AgentHistoryErrorCode::Timeout,
        "ctx MCP request timed out",
        true,
    )
}

fn stop_direct_child_and_reap(child: &mut Child) {
    if !matches!(child.try_wait(), Ok(Some(_))) {
        let _ = child.kill();
    }
    let _ = child.wait();
}

#[cfg(unix)]
struct ProcessTree {
    process_group: u32,
}

#[cfg(unix)]
impl ProcessTree {
    fn start(child: &Child) -> io::Result<Self> {
        Ok(Self {
            process_group: child.id(),
        })
    }

    fn terminate(&self) {
        let Some(process_group) = i32::try_from(self.process_group)
            .ok()
            .and_then(i32::checked_neg)
        else {
            return;
        };
        // SAFETY: spawn_configured placed this child in a fresh process group,
        // so the negative PID targets only the subprocess tree owned by this call.
        unsafe {
            kill(process_group, SIGKILL);
        }
    }
}

#[cfg(unix)]
const SIGKILL: i32 = 9;

#[cfg(unix)]
unsafe extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
}

#[cfg(not(any(unix, windows)))]
struct ProcessTree;

#[cfg(not(any(unix, windows)))]
impl ProcessTree {
    fn start(_child: &Child) -> io::Result<Self> {
        Ok(Self)
    }

    fn terminate(&self) {}
}
