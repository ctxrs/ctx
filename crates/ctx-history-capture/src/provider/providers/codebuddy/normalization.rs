use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, EventType, Fidelity, ProviderCaptureEnvelope,
    ProviderCursorCheckpoint, ProviderCursorRange, ProviderEventEnvelope, ProviderSessionEnvelope,
    ProviderSourceEnvelope, ProviderSourceTrust, SessionStatus,
    PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::provider::importer::{
    provider_cursor_stream, ProviderProjectionFatal, ProviderProjectionOutput,
    ProviderProjectionResult,
};
use crate::provider::normalization::{
    provider_capped_json, provider_policy_body, provider_policy_event_text,
    provider_result_identifier_evidence, provider_result_outcome_evidence, provider_role,
};
use crate::{
    CaptureError, ProviderAdapterContext, ProviderImportFailure, ProviderImportSummary, Result,
    CODEBUDDY_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS,
};

use super::{
    CODEBUDDY_MAX_CHECKPOINT_FAILURES, CODEBUDDY_MAX_CHECKPOINT_TEXT_BYTES,
    CODEBUDDY_MAX_FAILURE_BYTES,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct CodeBuddyBoundedFailure {
    pub(super) line: usize,
    pub(super) error: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CodeBuddyProjectionCounts {
    pub(super) accepted_captures: u64,
    accepted_events: u64,
    pub(super) rejected_records: u64,
    pub(super) failures: Vec<CodeBuddyBoundedFailure>,
}

impl CodeBuddyProjectionCounts {
    pub(super) fn accept(&mut self) -> ProviderProjectionResult<()> {
        self.accepted_captures = self.accepted_captures.checked_add(1).ok_or_else(|| {
            ProviderProjectionFatal::system_invariant("CodeBuddy capture count overflowed")
        })?;
        self.accepted_events = self.accepted_events.checked_add(1).ok_or_else(|| {
            ProviderProjectionFatal::system_invariant("CodeBuddy event count overflowed")
        })?;
        Ok(())
    }

    pub(super) fn reject(
        &mut self,
        output: &mut dyn ProviderProjectionOutput,
        line: usize,
        error: String,
    ) -> ProviderProjectionResult<()> {
        self.rejected_records = self.rejected_records.checked_add(1).ok_or_else(|| {
            ProviderProjectionFatal::system_invariant("CodeBuddy rejection count overflowed")
        })?;
        let error = bounded_codebuddy_failure(error);
        if self.failures.len() < CODEBUDDY_MAX_CHECKPOINT_FAILURES {
            self.failures.push(CodeBuddyBoundedFailure {
                line,
                error: error.clone(),
            });
        }
        output.reject_record(line, error);
        Ok(())
    }

    pub(super) fn replay_summary(&self) -> Result<ProviderImportSummary> {
        let skipped_sessions = usize::from(self.accepted_captures != 0);
        let skipped_events = usize::try_from(self.accepted_events).map_err(|_| {
            CaptureError::SystemInvariant("CodeBuddy replay event count exceeds platform limits")
        })?;
        let skipped =
            skipped_sessions
                .checked_add(skipped_events)
                .ok_or(CaptureError::SystemInvariant(
                    "CodeBuddy replay summary count overflowed",
                ))?;
        let failed = usize::try_from(self.rejected_records).map_err(|_| {
            CaptureError::SystemInvariant(
                "CodeBuddy replay rejection count exceeds platform limits",
            )
        })?;
        Ok(ProviderImportSummary {
            skipped,
            failed,
            skipped_sessions,
            skipped_events,
            accepted_content_records: skipped_events,
            failures: self
                .failures
                .iter()
                .map(|failure| ProviderImportFailure {
                    line: failure.line,
                    error: failure.error.clone(),
                })
                .collect(),
            ..ProviderImportSummary::default()
        })
    }
}

fn bounded_codebuddy_failure(mut error: String) -> String {
    if error.is_empty() {
        return "CodeBuddy record was deterministically rejected".to_owned();
    }
    if error.len() <= CODEBUDDY_MAX_FAILURE_BYTES {
        return error;
    }
    let mut boundary = CODEBUDDY_MAX_FAILURE_BYTES;
    while !error.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    error.truncate(boundary);
    error
}

pub(super) fn codebuddy_bounded_checkpoint_text(value: &str) -> Option<String> {
    (value.len() <= CODEBUDDY_MAX_CHECKPOINT_TEXT_BYTES).then(|| value.to_owned())
}

pub(super) fn codebuddy_mark_skipped_session(summary: &mut ProviderImportSummary) {
    summary.skipped = summary.skipped.saturating_add(1);
    summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
}

pub(super) fn codebuddy_checkpoint_time(
    value: Option<String>,
    field: &str,
) -> Result<Option<DateTime<Utc>>> {
    value
        .map(|value| {
            value.parse::<DateTime<Utc>>().map_err(|_| {
                CaptureError::InvalidPayload(format!(
                    "CodeBuddy parser checkpoint has an invalid {field}"
                ))
            })
        })
        .transpose()
}

pub(super) fn codebuddy_title_from_text(text: &str) -> Option<String> {
    let title = text.replace('\n', " ").chars().take(50).collect::<String>();
    (!title.trim().is_empty()).then_some(title)
}

pub(super) fn codebuddy_captured_batch_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

#[derive(Debug, Clone)]
pub(super) struct CodeBuddyEventInput {
    pub(super) provider_event_index: u64,
    pub(super) native_message_id: String,
    pub(super) role: Option<String>,
    pub(super) ref_type: Option<String>,
    pub(super) occurred_at: DateTime<Utc>,
    pub(super) text: String,
    pub(super) raw_message: Value,
    pub(super) decoded_message: Value,
}

pub(crate) fn codebuddy_decoded_message(raw_message: &Value) -> Value {
    match raw_message.get("message") {
        Some(Value::String(text)) => {
            serde_json::from_str(text).unwrap_or_else(|_| json!({ "content": text }))
        }
        Some(value) => value.clone(),
        None => raw_message.clone(),
    }
}

pub(crate) fn codebuddy_message_text(decoded: &Value, raw_message: &Value) -> String {
    let text = decoded
        .get("content")
        .and_then(codebuddy_content_text)
        .or_else(|| {
            decoded
                .get("text")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .or_else(|| decoded.as_str().map(str::to_owned))
        .or_else(|| raw_message.get("content").and_then(codebuddy_content_text))
        .or_else(|| {
            raw_message
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_default();
    codebuddy_clean_content(&text)
}

fn codebuddy_content_text(content: &Value) -> Option<String> {
    if let Some(text) = content.as_str() {
        return Some(text.to_owned());
    }
    let blocks = content.as_array()?;
    let parts = blocks
        .iter()
        .filter_map(|block| {
            let block_type = block.get("type").and_then(Value::as_str);
            if block_type.is_some_and(|kind| kind != "text") {
                return None;
            }
            block
                .get("text")
                .or_else(|| block.get("content"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect::<Vec<_>>();
    (!parts.is_empty()).then(|| parts.join("\n"))
}

pub(super) fn codebuddy_clean_content(content: &str) -> String {
    let mut cleaned = content.to_owned();
    for tag in [
        "user_info",
        "project_context",
        "project_layout",
        "system_reminder",
        "additional_data",
        "currently_opened_file",
    ] {
        cleaned = remove_xml_like_block(&cleaned, tag);
    }
    cleaned = cleaned.replace("<user_query>", "");
    cleaned = cleaned.replace("</user_query>", "");
    cleaned.trim().to_owned()
}

fn remove_xml_like_block(input: &str, tag: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut output = input.to_owned();
    while let Some(start) = output.find(&open) {
        let Some(relative_end) = output[start + open.len()..].find(&close) else {
            output.replace_range(start..start + open.len(), "");
            continue;
        };
        let end = start + open.len() + relative_end + close.len();
        output.replace_range(start..end, "");
    }
    output
}

#[derive(Clone, Copy)]
pub(super) enum CodeBuddyNativeShape {
    Extension,
    Cli,
}

impl CodeBuddyNativeShape {
    fn as_str(self) -> &'static str {
        match self {
            Self::Extension => "extension_json",
            Self::Cli => "cli_jsonl",
        }
    }

    fn event_source(self) -> &'static str {
        match self {
            Self::Extension => "codebuddy_messages_json",
            Self::Cli => "codebuddy_cli_jsonl",
        }
    }

    fn schema_proof(self) -> Option<&'static str> {
        match self {
            Self::Extension => Some("WayLog shayne-snap/WayLog@6939033b7a39326fbdc249e28e6aa12461db1f09 src/services/readers/codebuddy-reader.ts"),
            Self::Cli => None,
        }
    }

    fn limitations(self) -> &'static [&'static str] {
        match self {
            Self::Extension => &[
                "The original project path is represented by CodeBuddy's MD5 project directory when not available in the current IDE workspace",
                "Message file mtimes are used when native message timestamps are absent",
                "Non-text content blocks and binary attachments are preserved only in capped native JSON metadata",
            ],
            Self::Cli => &[
                "Non-message CLI JSONL rows are not imported; only their contribution to the source row count is recorded",
                "Non-text content blocks and binary attachments are preserved only in capped native JSON metadata",
            ],
        }
    }
}

pub(super) struct CodeBuddyCaptureDraft<'a> {
    pub(super) provider_session_id: &'a str,
    pub(super) native_session_id: &'a str,
    pub(super) project_hash: &'a str,
    pub(super) raw_source_path: &'a str,
    pub(super) context: &'a ProviderAdapterContext,
    pub(super) started_at: DateTime<Utc>,
    pub(super) ended_at: Option<DateTime<Utc>>,
    pub(super) title: Option<&'a str>,
    pub(super) cwd: Option<&'a str>,
    pub(super) project_index: Option<&'a Value>,
    pub(super) conversation: Option<&'a Value>,
    pub(super) session_index: &'a Value,
    pub(super) file_names: &'a [&'a str],
    pub(super) shape: CodeBuddyNativeShape,
}

pub(super) fn codebuddy_capture(
    draft: &CodeBuddyCaptureDraft<'_>,
    event: CodeBuddyEventInput,
) -> ProviderCaptureEnvelope {
    let event_envelope = codebuddy_event(
        draft.provider_session_id,
        draft.project_hash,
        draft.shape,
        &event,
    );
    let cursor = event_envelope
        .cursor
        .clone()
        .unwrap_or_else(|| draft.provider_session_id.to_owned());
    let observed_at = event_envelope.occurred_at;
    codebuddy_capture_envelope(draft, cursor, observed_at, Some(event_envelope))
}

pub(super) fn codebuddy_capture_envelope(
    draft: &CodeBuddyCaptureDraft<'_>,
    cursor: String,
    cursor_observed_at: DateTime<Utc>,
    event: Option<ProviderEventEnvelope>,
) -> ProviderCaptureEnvelope {
    ProviderCaptureEnvelope {
        schema_version: PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
        provider: CaptureProvider::CodeBuddy,
        source: ProviderSourceEnvelope {
            source_format: CODEBUDDY_SOURCE_FORMAT.to_owned(),
            machine_id: draft.context.machine_id.clone(),
            observed_at: draft.context.imported_at,
            raw_source_path: Some(draft.raw_source_path.to_owned()),
            source_root: draft
                .context
                .source_root_display()
                .or_else(|| Some(draft.raw_source_path.to_owned())),
            trust: ProviderSourceTrust::ProviderNative,
            fidelity: Fidelity::Imported,
            cursor: Some(ProviderCursorRange {
                before: None,
                after: Some(ProviderCursorCheckpoint {
                    stream: provider_cursor_stream(
                        CaptureProvider::CodeBuddy,
                        CODEBUDDY_SOURCE_FORMAT,
                    ),
                    cursor,
                    observed_at: cursor_observed_at,
                }),
            }),
            idempotency_key: Some(format!(
                "provider-source:codebuddy:{CODEBUDDY_SOURCE_FORMAT}:{}",
                draft.provider_session_id
            )),
            metadata: json!({
                "adapter": CODEBUDDY_SOURCE_FORMAT,
                "native_shape": draft.shape.as_str(),
                "native_project_hash": draft.project_hash,
                "native_session_id": draft.native_session_id,
                "files": draft.file_names,
                "schema_proof": draft.shape.schema_proof(),
            }),
        },
        session: ProviderSessionEnvelope {
            provider_session_id: draft.provider_session_id.to_owned(),
            parent_provider_session_id: None,
            root_provider_session_id: None,
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("primary".to_owned()),
            is_primary: true,
            status: SessionStatus::Imported,
            started_at: draft.started_at,
            ended_at: draft.ended_at,
            cwd: draft.cwd.map(str::to_owned),
            fidelity: Fidelity::Imported,
            idempotency_key: Some(format!(
                "provider-session:codebuddy:{}",
                draft.provider_session_id
            )),
            artifacts: Vec::new(),
            metadata: json!({
                "source_format": CODEBUDDY_SOURCE_FORMAT,
                "provider": CaptureProvider::CodeBuddy.as_str(),
                "display_name": "CodeBuddy",
                "title": draft.title,
                "native_shape": draft.shape.as_str(),
                "native_project_hash": draft.project_hash,
                "native_session_id": draft.native_session_id,
                "project_index": draft.project_index.map(|value| provider_capped_json(value, PROVIDER_MAX_PREVIEW_CHARS)),
                "conversation": draft.conversation.map(|value| provider_capped_json(value, PROVIDER_MAX_PREVIEW_CHARS)),
                "session_index": provider_capped_json(draft.session_index, PROVIDER_MAX_PREVIEW_CHARS),
                "files": draft.file_names,
                "limitations": draft.shape.limitations(),
            }),
        },
        event,
    }
}

fn codebuddy_event(
    provider_session_id: &str,
    project_hash: &str,
    shape: CodeBuddyNativeShape,
    event: &CodeBuddyEventInput,
) -> ProviderEventEnvelope {
    let event_type = EventType::Message;
    let retained_text = provider_policy_event_text(event_type, &event.text, &event.raw_message);
    let result_evidence =
        provider_result_identifier_evidence(event_type, &event.text, &event.raw_message);
    let result_outcome = provider_result_outcome_evidence(event_type, &event.raw_message);
    let event_id = format!("{provider_session_id}:{}", event.native_message_id);
    let role = provider_role(event.role.as_deref());
    ProviderEventEnvelope {
        provider_event_index: event.provider_event_index,
        provider_event_hash: Some(event_id.clone()),
        cursor: Some(event_id.clone()),
        event_type,
        role: Some(role),
        occurred_at: event.occurred_at,
        fidelity: Fidelity::Imported,
        idempotency_key: Some(format!(
            "provider-event:codebuddy:{CODEBUDDY_SOURCE_FORMAT}:{event_id}"
        )),
        artifacts: Vec::new(),
        payload: json!({
            "entry_type": event.ref_type.as_deref().unwrap_or("message"),
            "event_id": event_id,
            "native_project_hash": project_hash,
            "native_message_id": event.native_message_id,
            "text": retained_text.text,
            "text_retention": retained_text.retention.as_json(),
            "result_evidence": result_evidence,
            "result_outcome": result_outcome,
            "body": provider_capped_json(&provider_policy_body(event_type, &event.raw_message), PROVIDER_MAX_PREVIEW_CHARS),
            "decoded_body": provider_capped_json(&provider_policy_body(event_type, &event.decoded_message), PROVIDER_MAX_PREVIEW_CHARS),
        }),
        metadata: json!({
            "source": shape.event_source(),
            "source_format": CODEBUDDY_SOURCE_FORMAT,
            "native_message_id": event.native_message_id,
            "role": event.role,
            "ref_type": event.ref_type,
            "model": event.decoded_message.get("model").cloned().or_else(|| event.decoded_message.pointer("/providerData/model").cloned()),
        }),
    }
}
