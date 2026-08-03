use std::{
    io::{self, Read},
    process::Child,
    thread,
    time::{Duration, Instant},
};

use serde_json::Value;

use super::{classify_stderr, AgentHistoryError, AgentHistoryErrorCode};

pub(super) const MAX_RETAINED_SUBPROCESS_STDERR_BYTES: usize = 64 * 1024;

pub(super) fn collect_ctx_json(
    mut child: Child,
    timeout: Duration,
) -> Result<Value, AgentHistoryError> {
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            stop_and_reap(&mut child);
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
            stop_and_reap(&mut child);
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
            stop_and_reap(&mut child);
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
            stop_and_reap(&mut child);
            let _ = stdout_reader.join();
            return Err(AgentHistoryError::new(
                AgentHistoryErrorCode::AdapterError,
                "failed to start ctx CLI stderr reader",
                true,
            )
            .with_cause(err.to_string()));
        }
    };

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(err) => {
                stop_and_reap(&mut child);
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(AgentHistoryError::new(
                    AgentHistoryErrorCode::AdapterError,
                    "failed to wait for ctx CLI",
                    true,
                )
                .with_cause(err.to_string()));
            }
        }
        if started.elapsed() >= timeout {
            stop_and_reap(&mut child);
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err(AgentHistoryError::new(
                AgentHistoryErrorCode::Timeout,
                "ctx CLI command timed out",
                true,
            ));
        }
        thread::sleep(Duration::from_millis(20));
    };

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

fn stop_and_reap(child: &mut Child) {
    if !matches!(child.try_wait(), Ok(Some(_))) {
        let _ = child.kill();
    }
    let _ = child.wait();
}
