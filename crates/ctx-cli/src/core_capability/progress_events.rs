use std::io::Write;

use anyhow::Result;
use ctx_history_refresh::{
    RefreshLogicalPhase, RefreshRequestState, RefreshStatus, RefreshStatusKind,
    RefreshTerminalOutcome,
};
use serde_json::{json, Value};

use super::{canonical, write_response_frame, Operation, CORE_PRO_PROTOCOL_VERSION};

const MAX_EVENT_FRAME_BYTES: usize = 48 * 1024;
const MAX_EVENT_STREAM_BYTES: usize = 32 * 1024 * 1024;
const MAX_EVENT_FRAMES: u64 = 20_000;

pub(super) trait CapabilityEventSink {
    fn refresh(&mut self, status: &RefreshStatus) -> Result<()>;
}

pub(super) struct ProtocolEventWriter<'a> {
    operation: Operation,
    sequence: u64,
    stream_bytes: usize,
    writer: &'a mut dyn Write,
}

impl<'a> ProtocolEventWriter<'a> {
    pub(super) fn new(operation: Operation, writer: &'a mut dyn Write) -> Self {
        Self {
            operation,
            sequence: 0,
            stream_bytes: 0,
            writer,
        }
    }

    #[cfg(test)]
    pub(super) fn exhaust_byte_budget_for_test(&mut self) {
        self.stream_bytes = MAX_EVENT_STREAM_BYTES;
    }

    #[cfg(test)]
    pub(super) fn exhaust_frame_budget_for_test(&mut self) {
        self.sequence = MAX_EVENT_FRAMES;
    }
}

impl CapabilityEventSink for ProtocolEventWriter<'_> {
    fn refresh(&mut self, status: &RefreshStatus) -> Result<()> {
        if self.sequence >= MAX_EVENT_FRAMES {
            return Err(CapabilityEventWriterError::StreamTooLarge.into());
        }
        let frame = refresh_event_frame(self.operation, self.sequence, status)?;
        let bytes = canonical(&frame)?;
        if bytes.len() > MAX_EVENT_FRAME_BYTES {
            return Err(CapabilityEventWriterError::FrameTooLarge.into());
        }
        // Canonical JSON escapes C0 values. Reject raw terminal control bytes
        // as a second boundary check before the frame reaches stdout.
        if bytes
            .iter()
            .any(|byte| matches!(byte, b'\0' | b'\r' | 0x1b))
        {
            return Err(CapabilityEventWriterError::ControlByte.into());
        }
        let framed_bytes = bytes
            .len()
            .checked_add(1)
            .ok_or(CapabilityEventWriterError::StreamTooLarge)?;
        self.stream_bytes = self
            .stream_bytes
            .checked_add(framed_bytes)
            .filter(|total| *total <= MAX_EVENT_STREAM_BYTES)
            .ok_or(CapabilityEventWriterError::StreamTooLarge)?;
        write_response_frame(&mut self.writer, &bytes)
            .map_err(|_| CapabilityEventWriterError::Write)?;
        self.sequence += 1;
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum CapabilityEventWriterError {
    #[error("Core capability event frame exceeds its bound")]
    FrameTooLarge,
    #[error("Core capability event stream exceeds its bound")]
    StreamTooLarge,
    #[error("Core capability event frame contains a control byte")]
    ControlByte,
    #[error("Core capability event stream write failed")]
    Write,
}

fn refresh_event_frame(
    operation: Operation,
    sequence: u64,
    status: &RefreshStatus,
) -> Result<Value> {
    let kind = status.kind()?;
    let progress = status.progress()?;
    let current_source_progress = progress.current_source_progress.map(|current| {
        let mut value = json!({
            "logical_certified_bytes": current.logical_certified_bytes,
            "logical_rows_scanned": current.logical_rows_scanned,
            "snapshot_bytes_completed": current.snapshot_bytes_completed,
            "snapshot_bytes_total": current.snapshot_bytes_total,
            "snapshot_pages_completed": current.snapshot_pages_completed,
            "snapshot_pages_total": current.snapshot_pages_total,
            "stage": current.stage.as_str(),
        });
        remove_null_fields(&mut value);
        value
    });
    let mut refresh = json!({
        "completed_bytes": progress.completed_bytes,
        "completed_records": progress.completed_records,
        "completed_sources": u64::try_from(progress.completed_sources)?,
        "current_source": progress.current_source.as_deref().map(neutral_dynamic_text),
        "current_source_progress": current_source_progress,
        "elapsed_millis": progress.elapsed_millis,
        "estimated_remaining_millis": status.estimated_remaining_millis()?,
        "phase": neutral_dynamic_text(&progress.phase),
        "processed_bytes": progress.processed_bytes,
        "processed_messages": progress.processed_messages,
        "processed_sessions": progress.processed_sessions,
        "processed_tool_calls": progress.processed_tool_calls,
        "providers": progress.providers.iter().map(|provider| neutral_dynamic_text(provider)).collect::<Vec<_>>(),
        "request_state": request_state_name(kind.request_state()),
        "total_sources": u64::try_from(progress.total_sources)?,
        "total_sources_known": status.total_sources_known()?,
        "whole_run_stage": status.whole_run_stage()?.as_str(),
    });
    if let Some(request_id) = status.request_id() {
        refresh["request_id"] = json!(neutral_dynamic_text(request_id));
    }
    append_typed_status(&mut refresh, &kind)?;
    Ok(json!({
        "event": "refresh",
        "operation": operation.name(),
        "protocol_version": CORE_PRO_PROTOCOL_VERSION.get(),
        "refresh": refresh,
        "schema_version": 1,
        "sequence": sequence,
        "type": "ctx_core_capability_event",
    }))
}

fn append_typed_status(refresh: &mut Value, kind: &RefreshStatusKind) -> Result<()> {
    match kind {
        RefreshStatusKind::Legacy { .. } => {}
        RefreshStatusKind::BackgroundMaintenanceWake(_) => {
            refresh["logical_phase"] = json!("waiting");
            refresh["maintenance_wake"] = json!(true);
        }
        RefreshStatusKind::Logical(logical) => {
            refresh["logical_phase"] = json!(logical_phase_name(logical.logical_phase));
            refresh["physical_attempt_id"] =
                json!(neutral_dynamic_text(&logical.physical_attempt_id));
            refresh["physical_attempt_state"] =
                json!(request_state_name(logical.physical_attempt_state));
            refresh["progress_owner_request_id"] =
                json!(neutral_dynamic_text(&logical.progress_owner_request_id));
            refresh["progress_owner_attempt_state"] =
                json!(request_state_name(logical.progress_owner_attempt_state));
            if let Some(outcome) = logical.structured_outcome.as_ref() {
                refresh["terminal_state"] = terminal_state(outcome)?;
            }
        }
    }
    Ok(())
}

fn terminal_state(outcome: &RefreshTerminalOutcome) -> Result<Value> {
    outcome.validate()?;
    let mut details = json!({
        "affected_routes": outcome.affected_routes.iter().map(|route| route.as_str()).collect::<Vec<_>>(),
        "blocked_routes": outcome.blocked_routes.iter().map(|route| route.as_str()).collect::<Vec<_>>(),
        "class": outcome.class.as_str(),
        "physical_attempt_id": neutral_dynamic_text(&outcome.physical_attempt_id),
        "published_generation": outcome.published_generation.as_deref().map(neutral_dynamic_text),
        "retained_generation": outcome.retained_generation.as_deref().map(neutral_dynamic_text),
        "retry_advice": outcome.retry_advice.map(|advice| advice.as_str()),
        "retryable_routes": outcome.retryable_routes.iter().map(|route| route.as_str()).collect::<Vec<_>>(),
    });
    remove_null_fields(&mut details);
    Ok(json!({
        "details": details,
        "error_code": outcome.code.as_str(),
        "retryable": outcome.retryable,
    }))
}

fn remove_null_fields(value: &mut Value) {
    if let Value::Object(fields) = value {
        fields.retain(|_, value| !value.is_null());
    }
}

fn request_state_name(state: RefreshRequestState) -> &'static str {
    match state {
        RefreshRequestState::AdmissionPending => "admission_pending",
        RefreshRequestState::Queued => "queued",
        RefreshRequestState::Running => "running",
        RefreshRequestState::Published => "published",
        RefreshRequestState::Failed => "failed",
    }
}

fn logical_phase_name(phase: RefreshLogicalPhase) -> &'static str {
    match phase {
        RefreshLogicalPhase::Waiting => "waiting",
        RefreshLogicalPhase::Attached => "attached",
        RefreshLogicalPhase::CoverageCheck => "coverage_check",
        RefreshLogicalPhase::ExactSuccessor => "exact_successor",
        RefreshLogicalPhase::Direct => "direct",
        RefreshLogicalPhase::Terminal => "terminal",
    }
}

pub(super) fn neutral_dynamic_text(value: &str) -> String {
    value
        .chars()
        .flat_map(|character| {
            if character.is_control() {
                format!("\\u{{{:04X}}}", u32::from(character))
                    .chars()
                    .collect()
            } else {
                vec![character]
            }
        })
        .collect()
}

pub(super) struct IgnoreEvents;

impl CapabilityEventSink for IgnoreEvents {
    fn refresh(&mut self, _status: &RefreshStatus) -> Result<()> {
        Ok(())
    }
}

pub(super) fn event_writer_error(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.downcast_ref::<CapabilityEventWriterError>().is_some())
}
