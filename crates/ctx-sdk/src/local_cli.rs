use std::{
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use ctx_protocol::AgentHistoryErrorCode;
use serde_json::Value;

use crate::{AgentHistoryError, LocalBackendConfig};

pub(crate) fn run_ctx_json(
    config: &LocalBackendConfig,
    args: &[String],
) -> Result<Value, AgentHistoryError> {
    let mut command = Command::new(&config.ctx_binary);
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(data_root) = &config.data_root {
        command.env("CTX_DATA_ROOT", data_root);
    }
    let mut child = command.spawn().map_err(|err| {
        AgentHistoryError::new(
            AgentHistoryErrorCode::BackendUnavailable,
            "failed to start ctx CLI",
            true,
        )
        .with_cause(err.to_string())
    })?;
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().map_err(|err| {
            AgentHistoryError::new(
                AgentHistoryErrorCode::AdapterError,
                "failed to wait for ctx CLI",
                true,
            )
            .with_cause(err.to_string())
        })? {
            let output = child.wait_with_output().map_err(|err| {
                AgentHistoryError::new(
                    AgentHistoryErrorCode::AdapterError,
                    "failed to collect ctx CLI output",
                    true,
                )
                .with_cause(err.to_string())
            })?;
            if !status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(AgentHistoryError::new(
                    classify_stderr(&stderr),
                    stderr.trim().to_owned(),
                    false,
                ));
            }
            return serde_json::from_slice(&output.stdout).map_err(|err| {
                AgentHistoryError::new(
                    AgentHistoryErrorCode::DecodeError,
                    "failed to decode ctx JSON",
                    false,
                )
                .with_cause(err.to_string())
            });
        }
        if started.elapsed() > config.timeout {
            let _ = child.kill();
            return Err(AgentHistoryError::new(
                AgentHistoryErrorCode::Timeout,
                "ctx CLI command timed out",
                true,
            ));
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn classify_stderr(stderr: &str) -> AgentHistoryErrorCode {
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("not found") || lower.contains("no such") {
        AgentHistoryErrorCode::NotFound
    } else if lower.contains("not initialized") || lower.contains("setup") {
        AgentHistoryErrorCode::NotInitialized
    } else {
        AgentHistoryErrorCode::AdapterError
    }
}
