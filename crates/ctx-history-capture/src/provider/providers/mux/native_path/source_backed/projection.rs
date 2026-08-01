use std::sync::Arc;

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, CoreContentPolicyStatus, CoreRecord, EventIdentityInput, NativeItemKey,
    SourceKey, TypedKey,
};
use serde_json::Value;

use crate::{
    common::io::ProviderSourceRoot,
    provider::{
        file_touches::{
            event_type_supports_structured_file_touches,
            visit_provider_file_touch_drafts_with_limit,
        },
        normalization::provider_value_text,
        providers::mux::normalization::{
            apply_mux_core_output_diagnostic, mux_core_event, mux_event_id, mux_event_text,
            mux_event_type, mux_message_timestamp_opt, mux_output_projection,
            mux_partial_event_index, mux_result_content, MuxMessageRow, MuxOutputProjection,
        },
        source_backed::family::jsonl::{
            JsonlFamilyProjector, JsonlReader, JsonlRecordRef, JsonlSourceIdentity,
        },
    },
    CaptureError, Result,
};

use super::{
    open_verified, MuxBinding, MuxStreamKind, LOGICAL_EVENT_KIND, MAX_EVENT_SEQUENCE_ORDINAL,
    PARSER_REVISION, PARTIAL_EVENT_SEQUENCE_BASE,
};

const NATIVE_ITEM_NAMESPACE: &str = "mux.record";
const MAX_FILE_TOUCHES: usize = 448;

pub(super) struct MuxProjector {
    source: SourceKey,
    authority: Arc<ProviderSourceRoot>,
    binding: MuxBinding,
}

impl MuxProjector {
    pub(super) fn new(
        source: SourceKey,
        authority: Arc<ProviderSourceRoot>,
        binding: MuxBinding,
    ) -> Self {
        Self {
            source,
            authority,
            binding,
        }
    }

    fn project_record(
        &self,
        stream: MuxStreamKind,
        record: JsonlRecordRef<'_>,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        let bytes = record.bytes();
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(());
        }
        let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
            return Ok(());
        };
        if !value.is_object() {
            return Ok(());
        }
        if value
            .get("workspaceId")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .is_some_and(|owner| owner != self.binding.metadata.provider_session_id)
        {
            return Err(CaptureError::InvalidPayload(
                "Mux record changed its native session owner".to_owned(),
            ));
        }
        let output = mux_output_projection(&value);
        let content_omission = mux_output_content_omission(&value, output.as_ref());
        let evidence = record.evidence();
        let ordinal = evidence.physical_ordinal();
        if !stream.is_partial() && ordinal > MAX_EVENT_SEQUENCE_ORDINAL {
            return Err(CaptureError::InvalidPayload(
                "Mux source ordinal exceeds event identity capacity".to_owned(),
            ));
        }
        let event_sequence = if stream.is_partial() {
            PARTIAL_EVENT_SEQUENCE_BASE
                | (mux_partial_event_index(bytes) & MAX_EVENT_SEQUENCE_ORDINAL)
        } else {
            ordinal
        };
        let line_number = usize::try_from(ordinal)
            .ok()
            .and_then(|value| value.checked_add(1))
            .ok_or(CaptureError::SystemInvariant(
                "Mux source ordinal exceeds platform limits",
            ))?;
        let role = value
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let native_record_id = mux_event_id(&value, line_number, role, stream.is_partial());
        let native_item_key = NativeItemKey::native_id(
            NATIVE_ITEM_NAMESPACE,
            TypedKey::utf8(&native_record_id).map_err(contract)?,
        )
        .map_err(contract)?;
        let event_id = derive_event_id(EventIdentityInput {
            source: &self.source,
            session_id: self.binding.session_id,
            logical_item_kind: LOGICAL_EVENT_KIND,
            native_item_key: &native_item_key,
            subrecord_selector: None,
        })
        .map_err(contract)?;
        let row = MuxMessageRow { value };
        let occurred_at = mux_message_timestamp_opt(&row.value).unwrap_or_else(|| {
            self.binding
                .metadata
                .started_at
                .parse::<DateTime<Utc>>()
                .unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
        });
        let mut event = mux_core_event(&row, occurred_at);
        if let Some(output) = output.as_ref() {
            apply_mux_core_output_diagnostic(&mut event, &row.value, output);
        }
        let body = match mux_exact_logical_content(&row.value) {
            Ok(body) => body,
            Err(_) if content_omission.is_some() => "Mux output content omitted".to_owned(),
            Err(error) => return Err(error),
        };
        if body.is_empty() {
            return Err(CaptureError::InvalidPayload(
                "Mux source-backed event has no exact lexical body".to_owned(),
            ));
        }
        let mut touched_files = Vec::new();
        if event_type_supports_structured_file_touches(event.event_type) {
            let _ = visit_provider_file_touch_drafts_with_limit(
                &row.value,
                true,
                MAX_FILE_TOUCHES,
                |(_, touch)| {
                    touched_files.push(touch.path);
                    Ok::<(), std::convert::Infallible>(())
                },
            );
        }
        let tools = row
            .value
            .get("parts")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|part| part.get("type").and_then(Value::as_str) == Some("dynamic-tool"))
            .map(|part| {
                serde_json::json!({
                    "name": part.get("toolName").or_else(|| part.get("name")),
                    "call_id": part.get("toolCallId").or_else(|| part.get("id")),
                    "state": part.get("state"),
                    "input": part.get("input"),
                    "output": part.get("output"),
                })
            })
            .collect::<Vec<_>>();
        let structured_content = (!touched_files.is_empty() || !tools.is_empty())
            .then(|| serde_json::json!({"tools": tools, "file_touches": touched_files}));
        let agent_type = if self.binding.parent_session_id.is_some() {
            "subagent"
        } else {
            "primary"
        };
        let mut record = CoreRecord::new_selected(
            event_id,
            self.binding.session_id,
            self.binding.root_session_id,
            self.source.clone(),
            event_sequence,
            event.event_type.as_str(),
            agent_type,
            self.binding.parent_session_id.is_none(),
            PARSER_REVISION,
            body,
        )
        .map_err(contract)?;
        record.parent_session_id = self.binding.parent_session_id;
        record.provider_session_id = Some(self.binding.metadata.provider_session_id.clone());
        record.native_event_id = Some(TypedKey::utf8(native_record_id).map_err(contract)?);
        record.occurred_at_unix_ms = Some(event.occurred_at.timestamp_millis());
        record.role = event.role.map(|role| role.as_str().to_owned());
        record.cwd = self.binding.metadata.cwd.clone();
        record.content.structured_content = structured_content;
        if let Some((kind, reason)) = content_omission {
            record.content.policy_status = CoreContentPolicyStatus::Omitted {
                reason: reason.to_owned(),
            };
            record.content.normalized_body = None;
            record.content.structured_content = None;
            record.metadata.insert(
                "content_omission".to_owned(),
                serde_json::json!({"kind": kind, "reason": reason}),
            );
        }
        record.validate_contract().map_err(contract)?;
        emit(record)
    }
}

fn mux_output_content_omission(
    value: &Value,
    output: Option<&MuxOutputProjection>,
) -> Option<(&'static str, &'static str)> {
    output.filter(|output| !output.body_available)?;
    let explicitly_redacted = value
        .get("parts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|part| part.get("state").and_then(Value::as_str) == Some("output-redacted"));
    if explicitly_redacted {
        Some((
            "explicit_redaction",
            "Mux provider marked the tool output as redacted",
        ))
    } else {
        Some((
            "provider_private_framing",
            "Mux output framing contains no admitted textual or structured result",
        ))
    }
}

impl JsonlFamilyProjector for MuxProjector {
    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        self.project_record(self.binding.primary_stream, record, emit)
    }

    fn finish_projecting(&mut self, emit: &mut dyn FnMut(CoreRecord) -> Result<()>) -> Result<()> {
        if self.binding.primary_stream.is_partial() {
            return Ok(());
        }
        let Some(partial) = self.binding.partial.as_ref() else {
            return Ok(());
        };
        let source_file = open_verified(&self.authority, partial)?;
        let path = self.authority.named_path().join(&partial.relative_path);
        let mut reader = JsonlReader::open_whole_record(
            JsonlSourceIdentity::new(
                "mux",
                PARSER_REVISION,
                "mux-bounded-partial-snapshot-v1",
                self.source.exact_descriptor_digest(),
                path,
            ),
            source_file,
            None,
        )?;
        while reader
            .visit_page(&mut |record| self.project_record(MuxStreamKind::Partial, record, emit))?
            .is_some()
        {}
        if reader.outcome().is_none() {
            return Err(CaptureError::SystemInvariant(
                "Mux partial snapshot scan has no terminal evidence",
            ));
        }
        Ok(())
    }
}

fn mux_exact_logical_content(value: &Value) -> Result<String> {
    let event_type = mux_event_type(value);
    if matches!(
        event_type,
        ctx_history_core::EventType::ToolOutput | ctx_history_core::EventType::CommandOutput
    ) {
        return mux_result_content(value).ok_or_else(|| {
            CaptureError::InvalidPayload("Mux exact output body is unavailable".to_owned())
        });
    }
    let mut rendered = Vec::new();
    if let Some(parts) = value.get("parts").and_then(Value::as_array) {
        for part in parts {
            match part.get("type").and_then(Value::as_str) {
                Some("text" | "reasoning") => {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        rendered.push(text.to_owned());
                    }
                }
                Some("dynamic-tool") => rendered.push(exact_tool_part_text(part)),
                Some("file") => {
                    if let Some(label) = exact_file_part_text(part) {
                        rendered.push(label);
                    }
                }
                _ => {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        rendered.push(text.to_owned());
                    }
                }
            }
        }
    }
    if !rendered.is_empty() {
        return Ok(rendered.join("\n"));
    }
    if let Some(text) = value
        .get("content")
        .or_else(|| value.get("message"))
        .and_then(provider_value_text)
    {
        return Ok(text);
    }
    Ok(mux_event_text(value, event_type))
}

fn exact_tool_part_text(part: &Value) -> String {
    let name = part
        .get("toolName")
        .or_else(|| part.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("tool");
    let state = part.get("state").and_then(Value::as_str);
    let prefix = if matches!(state, Some("output-available" | "output-redacted"))
        || part.get("output").is_some()
    {
        "tool output"
    } else {
        "tool call"
    };
    let mut text = format!("{prefix}: {name}");
    if let Some(input) = part.get("input") {
        text.push_str("\ninput: ");
        text.push_str(&exact_value_text(input));
    }
    if let Some(output) = part.get("output") {
        text.push_str("\noutput: ");
        text.push_str(&exact_value_text(output));
    }
    if let Some(nested) = part.get("nestedCalls").and_then(Value::as_array) {
        let names = nested
            .iter()
            .filter_map(|call| {
                call.get("toolName")
                    .or_else(|| call.get("name"))
                    .and_then(Value::as_str)
            })
            .collect::<Vec<_>>();
        if !names.is_empty() {
            text.push_str("\nnested tools: ");
            text.push_str(&names.join(", "));
        }
    }
    text
}

fn exact_value_text(value: &Value) -> String {
    provider_value_text(value)
        .or_else(|| serde_json::to_string(value).ok())
        .unwrap_or_else(|| value.to_string())
}

fn exact_file_part_text(part: &Value) -> Option<String> {
    let label = part
        .get("filename")
        .or_else(|| part.get("name"))
        .or_else(|| part.get("mediaType"))
        .or_else(|| part.get("mimeType"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| {
            part.get("url")
                .and_then(Value::as_str)
                .filter(|url| !url.starts_with("data:") && url.len() < 256)
                .map(str::to_owned)
        })?;
    Some(format!("file: {label}"))
}
fn contract(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn successful_textual_result_over_16k_is_complete() {
        let tail = "mux_success_result_tail_complete";
        let output = format!("{} {tail}", "successful mux output ".repeat(800));
        assert!(output.len() > 16_000);
        let value = serde_json::json!({
            "role": "assistant",
            "parts": [{
                "type": "dynamic-tool",
                "toolName": "shell",
                "toolCallId": "complete-success",
                "state": "output-available",
                "output": output,
            }]
        });

        assert_eq!(mux_exact_logical_content(&value).unwrap(), output);
        assert!(
            mux_output_content_omission(&value, mux_output_projection(&value).as_ref()).is_none()
        );
    }

    #[test]
    fn explicit_redaction_has_truthful_omission_reason() {
        let value = serde_json::json!({
            "role": "assistant",
            "parts": [{
                "type": "dynamic-tool",
                "toolName": "shell",
                "toolCallId": "redacted",
                "state": "output-redacted",
            }]
        });
        assert_eq!(
            mux_output_content_omission(&value, mux_output_projection(&value).as_ref()),
            Some((
                "explicit_redaction",
                "Mux provider marked the tool output as redacted"
            ))
        );
    }
}
