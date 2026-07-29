use std::io;

use clap::ValueEnum;
use ctx_history_capture::complete_content::{CompleteContentError, CompleteContentErrorKind};
use serde_json::{json, Value};
use uuid::Uuid;

pub(crate) const CLI_COMPLETE_CONTENT_MAX_OUTPUT_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const MCP_COMPLETE_CONTENT_MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Default)]
struct SerializedByteCounter {
    bytes: usize,
}

impl io::Write for SerializedByteCounter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes = self.bytes.saturating_add(buffer.len());
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum ContentPolicy {
    Indexed,
    Complete,
}

impl ContentPolicy {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Indexed => "indexed",
            Self::Complete => "complete",
        }
    }
}

pub(crate) fn enforce_complete_content_output_limit(
    policy: ContentPolicy,
    serialized_output_bytes: usize,
    output_limit_bytes: usize,
    event_id: Uuid,
) -> Result<(), CompleteContentError> {
    if policy == ContentPolicy::Complete && serialized_output_bytes > output_limit_bytes {
        return Err(CompleteContentError::new(
            CompleteContentErrorKind::ContentTooLarge,
            event_id,
        ));
    }
    Ok(())
}

pub(crate) fn enforce_complete_content_cli_output_limit(
    policy: ContentPolicy,
    rendered_output: &str,
    writes_stdout: bool,
    output_limit_bytes: usize,
    event_id: Uuid,
) -> Result<(), CompleteContentError> {
    let serialized_output_bytes = rendered_output.len().saturating_add(usize::from(
        writes_stdout && !rendered_output.ends_with('\n'),
    ));
    enforce_complete_content_output_limit(
        policy,
        serialized_output_bytes,
        output_limit_bytes,
        event_id,
    )
}

pub(crate) fn serialized_json_line_bytes(value: &Value) -> serde_json::Result<usize> {
    let mut counter = SerializedByteCounter::default();
    serde_json::to_writer(&mut counter, value)?;
    Ok(counter.bytes.saturating_add(1))
}

pub(crate) fn complete_content_error_json(error: &CompleteContentError) -> Value {
    json!({
        "error": error.kind.as_str(),
        "error_code": error.kind.as_str(),
        "ctx_event_id": error.event_id,
        "retryable": error.retryable,
        "remediation": format!("ctx locate event {}", error.event_id),
    })
}
