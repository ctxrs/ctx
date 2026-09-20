//! Projection of fx's manifest-v4 conversation stream. Provider replay and
//! artifact framing are normalized at import; ordinary readers need no files.
use ctx_history_core::{
    admit_provider_declared_fact, derive_event_id, ActivityInvocation, ActivityJsonCapture,
    ActivityResult, ActivityTextCapture, AgentScope, CoreActivity, CoreRecord, EventIdentityInput,
    EventRole, EventType, LiteralFactKind, NativeItemKey, ProviderNativeSessionRelationship,
    TypedKey, CORE_ACTIVITY_REVISION, MAX_CORE_CONTENT_BYTES,
};
use ctx_history_provider_runtime::CaptureError;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{ProjectionBinding, FX_PARSER_REVISION};

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ConversationManifest {
    pub schema_version: u8,
    pub id: String,
    pub workspace_root: String,
    #[serde(default)]
    pub subagent_child: bool,
}

#[derive(Deserialize)]
struct Envelope {
    schema_version: u8,
    seq: u64,
    timestamp_ms: i64,
    event: serde_json::Map<String, Value>,
}

/// Artifact bytes are obtained only through the shared capture authority.
#[derive(Clone, Copy)]
pub(crate) enum ArtifactKind {
    Tool,
    Command,
}

pub(crate) enum Artifact {
    Present(Vec<u8>),
    Unavailable,
    Omitted { reason: String, observed_bytes: u64 },
}

pub(crate) fn project(
    binding: ProjectionBinding<'_>,
    manifest: &ConversationManifest,
    parent: Option<ProjectionBinding<'_>>,
    bytes: &[u8],
    read_artifact: &mut impl FnMut(ArtifactKind, &str) -> Result<Artifact, CaptureError>,
) -> Result<CoreRecord, CaptureError> {
    let frame: Envelope = serde_json::from_slice(bytes)?;
    if !matches!(frame.schema_version, 1 | 2) {
        return Err(CaptureError::UnsupportedSchema(
            "fx conversation envelope".into(),
        ));
    }
    if frame.seq == 0 || frame.timestamp_ms < 0 || frame.event.len() != 1 {
        return Err(invalid("invalid fx conversation envelope"));
    }
    let (kind, mut payload) = frame
        .event
        .into_iter()
        .next()
        .ok_or_else(|| invalid("missing fx conversation event"))?;
    if !payload.is_object() {
        return Err(invalid("fx conversation event must be an object"));
    }
    let mut body = String::new();
    let mut invocation = None;
    let mut result = None;
    let (event_type, role) = match kind.as_str() {
        "user" | "steering" => {
            append(&mut body, required_text(&payload, "text")?)?;
            (EventType::Message, EventRole::User)
        }
        "assistant" => {
            append(&mut body, required_text(&payload, "text")?)?;
            if let Some(replay) = payload
                .as_object_mut()
                .and_then(|p| p.remove("provider_replay"))
            {
                if !replay.is_null() {
                    let mut parts: Value =
                        serde_json::from_str(required_text(&replay, "parts_json")?)?;
                    let mut texts = Vec::new();
                    replay_text(&parts, &mut texts);
                    for text in &texts {
                        append(&mut body, text)?;
                    }
                    let omitted = omit_replay_secrets(&mut parts);
                    payload["provider_replay"] =
                        json!({"source": replay.get("source"), "parts": parts});
                    if omitted {
                        payload["provider_replay_opaque_state_omitted"] = json!(true);
                    }
                }
            }
            (EventType::Message, EventRole::Assistant)
        }
        "tool_call" => {
            let arguments = required_text(&payload, "arguments_json")?;
            append(&mut body, arguments)?;
            invocation = Some(ActivityInvocation {
                protocol: None,
                server: None,
                tool: required_text(&payload, "tool_name")?.to_owned(),
                arguments: ActivityJsonCapture::Present {
                    value: serde_json::from_str(arguments)
                        .unwrap_or_else(|_| Value::String(arguments.to_owned())),
                },
                started_at_unix_ms: Some(frame.timestamp_ms),
            });
            // Raw arguments are already the body and parsed arguments belong to
            // activity; a third full copy can reject an otherwise bounded call.
            payload
                .as_object_mut()
                .expect("object payload")
                .remove("arguments_json");
            payload["arguments_capture"] = json!("activity.invocation.arguments");
            (EventType::ToolCall, EventRole::Assistant)
        }
        "tool_result" => {
            let handle = required_text(&payload, "artifact_ref")?;
            let text = artifact_text(read_artifact(ArtifactKind::Tool, handle)?, &mut body)?;
            result = Some(ActivityResult {
                status: Some(required_text(&payload, "status")?.to_owned()),
                completed_at_unix_ms: Some(frame.timestamp_ms),
                duration_ns: None,
                text,
                structured_content: ActivityJsonCapture::Absent,
            });
            // Preview remains explicitly preview metadata; never substitute it for the artifact.
            (EventType::ToolOutput, EventRole::Tool)
        }
        "interrupted" => {
            if let Some(text) = payload.get("partial_text").and_then(Value::as_str) {
                append(&mut body, text)?;
            }
            if let Some(handle) = payload.get("command_artifact_ref").and_then(Value::as_str) {
                let capture =
                    artifact_text(read_artifact(ArtifactKind::Command, handle)?, &mut body)?;
                payload["command_artifact_capture"] = serde_json::to_value(capture)?;
            }
            append_metadata_text(&payload, &mut body, &["partial_text"])?;
            (EventType::Message, EventRole::Assistant)
        }
        "context_checkpoint" => {
            append(&mut body, required_text(&payload, "summary")?)?;
            (EventType::Summary, EventRole::System)
        }
        "turn_completed" => {
            append_metadata_text(&payload, &mut body, &[])?;
            (EventType::Notice, EventRole::Assistant)
        }
        _ => {
            return Err(CaptureError::UnsupportedSchema(format!(
                "fx conversation event {kind}"
            )))
        }
    };
    let replay_body_start = body.len();
    let mut replay_observed_bytes = None;
    if let Some(handle) = payload.get("command_replay_ref").and_then(Value::as_str) {
        let capture = match read_artifact(ArtifactKind::Command, handle)? {
            Artifact::Present(bytes) => {
                replay_observed_bytes = Some(bytes.len() as u64);
                match crate::conversation_replay::capture(&bytes, &mut body) {
                    Ok(capture) => capture,
                    Err(error) => {
                        body.truncate(replay_body_start);
                        json!({"capture_status":"omitted", "reason":error.to_string(), "observed_bytes":bytes.len()})
                    }
                }
            }
            Artifact::Unavailable => json!({"capture_status":"unavailable"}),
            Artifact::Omitted {
                reason,
                observed_bytes,
            } => {
                json!({"capture_status":"omitted", "reason":reason, "observed_bytes":observed_bytes})
            }
        };
        payload["command_replay_capture"] = capture;
    }
    let result_was_normalized = result
        .as_ref()
        .is_some_and(|result| matches!(result.text, ActivityTextCapture::NormalizedBody));
    if result_was_normalized && body.len() != replay_body_start {
        result.as_mut().expect("normalized result").text = ActivityTextCapture::Present {
            value: body[..replay_body_start].to_owned(),
        };
    }
    let session_id =
        crate::projection::core_session_id(binding).map_err(super::source_backed::fx_error)?;
    let key = NativeItemKey::composite("fx.conversation.seq", vec![TypedKey::U64(frame.seq)])
        .map_err(contract)?;
    let event_id = derive_event_id(EventIdentityInput {
        source: binding.source,
        session_id,
        logical_item_kind: "fx_conversation_event",
        native_item_key: &key,
        subrecord_selector: None,
    })
    .map_err(contract)?;
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        binding.source.clone(),
        frame.seq,
        event_type.as_str(),
        FX_PARSER_REVISION,
        &kind,
    )
    .map_err(contract)?;
    record.content.normalized_body = (!body.is_empty()).then_some(body);
    record.provider_session_id = Some(manifest.id.clone());
    record.native_event_id = Some(TypedKey::U64(frame.seq));
    record.occurred_at_unix_ms = Some(frame.timestamp_ms);
    record.role = Some(role.as_str().to_owned());
    record.agent_scope = Some(if manifest.subagent_child {
        AgentScope::Subagent
    } else {
        AgentScope::Primary
    });
    if let Some(parent) = parent {
        record.parent_session_id = Some(
            crate::projection::core_session_id(ProjectionBinding {
                source: parent.source,
                native_session_id: parent.native_session_id,
            })
            .map_err(super::source_backed::fx_error)?,
        );
        record.session_relationship = Some(ProviderNativeSessionRelationship::Delegated);
    }
    let call_id = payload
        .get("call_id")
        .and_then(Value::as_str)
        .map(TypedKey::utf8)
        .transpose()
        .map_err(contract)?;
    let mut facts = Vec::new();
    if let Some(fact) = admit_provider_declared_fact(
        LiteralFactKind::SessionCwd,
        manifest.workspace_root.clone(),
        facts.len(),
    ) {
        facts.push(fact);
    }
    if let Some(files) = payload.get("files").and_then(Value::as_array) {
        for file in files {
            for field in ["path", "new_path"] {
                if let Some(path) = file.get(field).and_then(Value::as_str) {
                    if let Some(fact) = admit_provider_declared_fact(
                        LiteralFactKind::File,
                        path.to_owned(),
                        facts.len(),
                    ) {
                        facts.push(fact);
                    }
                }
            }
        }
    }
    record.content.structured_content =
        Some(json!({"event": {kind.clone(): payload}, "workspace_root": manifest.workspace_root}));
    record.content.activity = (invocation.is_some() || result.is_some() || !facts.is_empty())
        .then_some(CoreActivity {
            revision: CORE_ACTIVITY_REVISION,
            provider_call_id: call_id,
            invocation,
            result,
            facts,
        });
    if !ctx_history_jsonl::selected_content_fits(
        record
            .content
            .normalized_body
            .as_deref()
            .unwrap_or_default(),
        record.content.structured_content.as_ref(),
        record.content.activity.as_ref(),
        MAX_CORE_CONTENT_BYTES,
    ) {
        if let Some(observed_bytes) = replay_observed_bytes {
            if result_was_normalized {
                if let Some(result) = record
                    .content
                    .activity
                    .as_mut()
                    .and_then(|activity| activity.result.as_mut())
                {
                    result.text = ActivityTextCapture::NormalizedBody;
                }
            }
            if let Some(body) = &mut record.content.normalized_body {
                body.truncate(replay_body_start);
            }
            if record.content.normalized_body.as_deref() == Some("") {
                record.content.normalized_body = None;
            }
            record
                .content
                .structured_content
                .as_mut()
                .expect("conversation structured content")["event"][&kind]
                ["command_replay_capture"] = json!({"capture_status":"omitted", "reason":"fx replay exceeds aggregate Core content bound", "observed_bytes":observed_bytes});
        }
    }
    record.validate_contract().map_err(contract)?;
    Ok(record)
}

fn artifact_text(
    artifact: Artifact,
    body: &mut String,
) -> Result<ActivityTextCapture, CaptureError> {
    match artifact {
        Artifact::Present(bytes) => match String::from_utf8(bytes) {
            Ok(text) => {
                append(body, &text)?;
                Ok(if text.is_empty() {
                    ActivityTextCapture::Present { value: text }
                } else {
                    ActivityTextCapture::NormalizedBody
                })
            }
            Err(error) => Ok(ActivityTextCapture::Omitted {
                reason: "fx artifact is not UTF-8 text".into(),
                observed_bytes: Some(error.as_bytes().len() as u64),
            }),
        },
        Artifact::Unavailable => Ok(ActivityTextCapture::Unavailable),
        Artifact::Omitted {
            reason,
            observed_bytes,
        } => Ok(ActivityTextCapture::Omitted {
            reason,
            observed_bytes: Some(observed_bytes),
        }),
    }
}

fn omit_replay_secrets(value: &mut Value) -> bool {
    let mut omitted = false;
    match value {
        Value::Array(values) => {
            for value in values {
                omitted |= omit_replay_secrets(value);
            }
        }
        Value::Object(fields) => {
            for key in [
                "encrypted_content",
                "encryptedContent",
                "signature",
                "thoughtSignature",
            ] {
                omitted |= fields.remove(key).is_some();
            }
            for value in fields.values_mut() {
                omitted |= omit_replay_secrets(value);
            }
        }
        _ => {}
    }
    omitted
}

fn replay_text<'a>(value: &'a Value, texts: &mut Vec<&'a str>) {
    match value {
        Value::Array(values) => {
            for value in values {
                replay_text(value, texts);
            }
        }
        Value::Object(fields) => {
            for (key, value) in fields {
                match (key.as_str(), value) {
                    ("text" | "summary" | "thinking" | "reasoning", Value::String(text)) => {
                        texts.push(text)
                    }
                    ("content" | "summary" | "parts" | "reasoning", _) => replay_text(value, texts),
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

fn append_metadata_text(
    value: &Value,
    body: &mut String,
    skip: &[&str],
) -> Result<(), CaptureError> {
    match value {
        Value::String(text) => append(body, text)?,
        Value::Array(values) => {
            for value in values {
                append_metadata_text(value, body, skip)?;
            }
        }
        Value::Object(fields) => {
            for (key, value) in fields {
                if !skip.contains(&key.as_str()) {
                    append_metadata_text(value, body, skip)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn append(body: &mut String, text: &str) -> Result<(), CaptureError> {
    if text.is_empty() {
        return Ok(());
    }
    if body.len().saturating_add(text.len()).saturating_add(1) > MAX_CORE_CONTENT_BYTES {
        return Err(invalid("fx conversation content exceeds Core byte bound"));
    }
    if !body.is_empty() {
        body.push('\n');
    }
    body.push_str(text);
    Ok(())
}

fn required_text<'a>(value: &'a Value, field: &str) -> Result<&'a str, CaptureError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(&format!("fx conversation missing {field}")))
}

fn invalid(message: &str) -> CaptureError {
    CaptureError::InvalidPayload(message.to_owned())
}
fn contract(error: impl std::fmt::Display) -> CaptureError {
    invalid(&error.to_string())
}
