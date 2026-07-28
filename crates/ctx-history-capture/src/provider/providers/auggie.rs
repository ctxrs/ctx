use std::path::Path;

use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, EventRole, EventType};
use serde_json::{json, Value};

use crate::provider::normalization::{
    provider_capped_json, provider_policy_body, provider_policy_event_text,
    provider_result_identifier_evidence, provider_result_outcome_evidence, provider_string_field,
    provider_timestamp_from_fields,
};
use crate::{
    CaptureError, ProviderAdapterContext, Result, AUGGIE_SESSION_JSON_SOURCE_FORMAT,
    PROVIDER_MAX_PREVIEW_CHARS,
};

pub(crate) mod native_path;

pub(crate) use native_path::import_auggie_sessions_nativepath;

pub(super) struct AuggieSessionData<'a> {
    pub(super) provider_session_id: String,
    pub(super) parent_provider_session_id: Option<String>,
    pub(super) root_provider_session_id: Option<String>,
    pub(super) external_agent_id: Option<String>,
    pub(super) chat_history: &'a [Value],
    pub(super) started_at: DateTime<Utc>,
    pub(super) ended_at: Option<DateTime<Utc>>,
    pub(super) cwd: Option<String>,
    pub(super) raw_source_path: String,
    pub(super) source_metadata: Value,
    pub(super) session_metadata: Value,
}

impl<'a> AuggieSessionData<'a> {
    pub(super) fn parse(
        session: &'a Value,
        path: &Path,
        context: &ProviderAdapterContext,
    ) -> Result<Self> {
        let provider_session_id = provider_string_field(session, &["sessionId", "session_id"])
            .ok_or_else(|| {
                CaptureError::InvalidPayload("Auggie session JSON is missing sessionId".to_owned())
            })?;
        let chat_history = session
            .get("chatHistory")
            .or_else(|| session.get("chat_history"))
            .and_then(Value::as_array)
            .ok_or_else(|| {
                CaptureError::InvalidPayload(
                    "Auggie session JSON is missing chatHistory array".to_owned(),
                )
            })?;
        let started_at = provider_timestamp_from_fields(
            session,
            &[
                "created",
                "createdAt",
                "created_at",
                "startedAt",
                "started_at",
            ],
        )
        .or_else(|| {
            chat_history
                .iter()
                .find_map(|entry| auggie_entry_time(entry, None))
        })
        .unwrap_or(context.imported_at);
        let ended_at = provider_timestamp_from_fields(
            session,
            &[
                "modified",
                "modifiedAt",
                "updatedAt",
                "updated_at",
                "endedAt",
                "ended_at",
            ],
        )
        .or_else(|| {
            chat_history
                .iter()
                .rev()
                .find_map(|entry| auggie_entry_time(entry, None))
        });
        let cwd = provider_string_field(
            session,
            &[
                "workspaceRoot",
                "workspace_root",
                "workspacePath",
                "workspace_path",
                "cwd",
            ],
        );
        let raw_source_path = path.display().to_string();
        let source_metadata = json!({
            "adapter": AUGGIE_SESSION_JSON_SOURCE_FORMAT,
            "source_path": raw_source_path,
            "upstream_schema_anchor": {
                "package": "@augmentcode/auggie@0.32.0",
                "docs": "https://docs.augmentcode.com/cli/reference",
                "package_storage": "SessionStore writes ~/.augment/sessions/<session_id>.json",
            },
        });
        let session_metadata = json!({
            "source_format": AUGGIE_SESSION_JSON_SOURCE_FORMAT,
            "provider": CaptureProvider::Auggie.as_str(),
            "display_name": "Auggie",
            "session_id": provider_session_id,
            "workspace_id": provider_string_field(session, &["workspaceId", "workspace_id"]),
            "name": provider_string_field(session, &["name", "title", "sessionName"]),
            "chat_history_count": chat_history.len(),
            "agent_state": session
                .get("agentState")
                .or_else(|| session.get("agent_state"))
                .map(|value| provider_capped_json(value, PROVIDER_MAX_PREVIEW_CHARS)),
            "limitations": [
                "ctx imports request_message and response_text fields plus recognized request_nodes/response_nodes text",
                "recognized tool-result bodies are transient output-Pro observations and are never retained in Core"
            ],
        });
        Ok(Self {
            provider_session_id,
            parent_provider_session_id: provider_string_field(
                session,
                &[
                    "parentConversationId",
                    "parentSessionId",
                    "parent_session_id",
                ],
            ),
            root_provider_session_id: provider_string_field(
                session,
                &["rootConversationId", "rootSessionId", "root_session_id"],
            ),
            external_agent_id: provider_string_field(
                session,
                &["poseidonAgentId", "agentId", "agent_id"],
            ),
            started_at,
            ended_at,
            cwd,
            chat_history,
            raw_source_path,
            source_metadata,
            session_metadata,
        })
    }
}

pub(crate) struct AuggieEventInput<'a> {
    pub(crate) provider_session_id: &'a str,
    pub(crate) provider_event_index: u64,
    pub(crate) chat_index: usize,
    pub(crate) role: EventRole,
    pub(crate) label: &'static str,
    pub(crate) occurred_at: DateTime<Utc>,
    pub(crate) text: String,
    pub(crate) entry: &'a Value,
    pub(crate) exchange: &'a Value,
    pub(crate) raw_source_path: &'a str,
}

#[derive(Debug)]
pub(crate) struct AuggieEvent {
    pub(crate) provider_session_id: String,
    pub(crate) provider_event_index: u64,
    pub(crate) provider_event_hash: String,
    pub(crate) cursor: String,
    pub(crate) event_type: EventType,
    pub(crate) role: EventRole,
    pub(crate) occurred_at: DateTime<Utc>,
    pub(crate) payload: Value,
    pub(crate) metadata: Value,
}

pub(crate) fn auggie_event(input: AuggieEventInput<'_>) -> AuggieEvent {
    let request_id = input
        .exchange
        .get("request_id")
        .or_else(|| input.exchange.get("requestId"))
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty());
    let event_hash = request_id
        .map(|id| format!("{id}:{}", input.label))
        .unwrap_or_else(|| format!("chat-{}:{}", input.chat_index, input.label));
    let body = auggie_event_body(&input, request_id);
    let event_type = EventType::Message;
    let retained_text = provider_policy_event_text(event_type, &input.text, &body);
    let retained_body = provider_policy_body(event_type, &body);
    AuggieEvent {
        provider_session_id: input.provider_session_id.to_owned(),
        provider_event_index: input.provider_event_index,
        provider_event_hash: event_hash.clone(),
        cursor: format!("{}:{event_hash}", input.raw_source_path),
        event_type,
        role: input.role,
        occurred_at: input.occurred_at,
        payload: json!({
            "text": retained_text.text,
            "text_retention": retained_text.retention.as_json(),
            "result_evidence": provider_result_identifier_evidence(
                event_type,
                &input.text,
                &body,
            ),
            "result_outcome": provider_result_outcome_evidence(event_type, &body),
            "source_format": AUGGIE_SESSION_JSON_SOURCE_FORMAT,
            "body": provider_capped_json(&retained_body, PROVIDER_MAX_PREVIEW_CHARS),
        }),
        metadata: json!({
            "source": "auggie_chat_history",
            "source_format": AUGGIE_SESSION_JSON_SOURCE_FORMAT,
            "chat_history_index": input.chat_index,
            "message_kind": input.label,
            "request_id": request_id,
            "sequence_id": input
                .entry
                .get("sequenceId")
                .or_else(|| input.entry.get("sequence_id"))
                .and_then(Value::as_u64),
            "completed": input.entry.get("completed").and_then(Value::as_bool),
            "source_kind": input.entry.get("source").and_then(Value::as_str),
        }),
    }
}

pub(crate) fn auggie_event_body(input: &AuggieEventInput<'_>, request_id: Option<&str>) -> Value {
    json!({
        "message_kind": input.label,
        "request_id": request_id,
        "raw_exchange_retention": "metadata_only",
        "sequence_id": input
            .entry
            .get("sequenceId")
            .or_else(|| input.entry.get("sequence_id"))
            .and_then(Value::as_u64),
        "completed": input.entry.get("completed").and_then(Value::as_bool),
        "source_kind": input.entry.get("source").and_then(Value::as_str),
        "request_node_count": auggie_node_count(
            input
                .exchange
                .get("request_nodes")
                .or_else(|| input.exchange.get("requestNodes")),
        ),
        "response_node_count": auggie_node_count(
            input
                .exchange
                .get("response_nodes")
                .or_else(|| input.exchange.get("responseNodes")),
        ),
        "tool_node_count": auggie_tool_node_count(input.exchange),
    })
}

fn auggie_node_count(value: Option<&Value>) -> Option<usize> {
    value.and_then(Value::as_array).map(Vec::len)
}

fn auggie_tool_node_count(exchange: &Value) -> usize {
    [
        "request_nodes",
        "requestNodes",
        "response_nodes",
        "responseNodes",
    ]
    .iter()
    .filter_map(|key| exchange.get(*key).and_then(Value::as_array))
    .flatten()
    .filter(|node| auggie_node_is_tool_metadata(node))
    .count()
}

pub(crate) fn auggie_entry_time(entry: &Value, exchange: Option<&Value>) -> Option<DateTime<Utc>> {
    provider_timestamp_from_fields(
        entry,
        &[
            "finishedAt",
            "finished_at",
            "createdAt",
            "created_at",
            "timestamp",
            "time",
        ],
    )
    .or_else(|| {
        exchange.and_then(|exchange| {
            provider_timestamp_from_fields(
                exchange,
                &[
                    "createdAt",
                    "created_at",
                    "updatedAt",
                    "updated_at",
                    "timestamp",
                    "time",
                ],
            )
        })
    })
}

pub(crate) fn auggie_request_text(exchange: &Value) -> Option<String> {
    provider_string_field(exchange, &["request_message", "requestMessage"]).or_else(|| {
        auggie_nodes_text(
            exchange
                .get("request_nodes")
                .or_else(|| exchange.get("requestNodes")),
        )
    })
}

pub(crate) fn auggie_response_text(exchange: &Value) -> Option<String> {
    provider_string_field(exchange, &["response_text", "responseText"]).or_else(|| {
        auggie_nodes_text(
            exchange
                .get("response_nodes")
                .or_else(|| exchange.get("responseNodes")),
        )
    })
}

pub(crate) fn auggie_nodes_text(value: Option<&Value>) -> Option<String> {
    let nodes = value?.as_array()?;
    let rendered = nodes
        .iter()
        .filter_map(auggie_node_text)
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>();
    (!rendered.is_empty()).then(|| rendered.join("\n"))
}

pub(crate) fn auggie_node_text(node: &Value) -> Option<String> {
    let object = node.as_object()?;
    match object.get("type") {
        None if object.len() == 1 => {}
        Some(kind) if object.len() == 2 && kind.as_u64() == Some(0) => {}
        _ => return None,
    }
    let text_node = match (object.get("text_node"), object.get("textNode")) {
        (Some(text_node), None) | (None, Some(text_node)) => text_node.as_object()?,
        _ => return None,
    };
    if text_node.len() != 1 {
        return None;
    }
    text_node
        .get("content")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|text| !text.trim().is_empty())
}

pub(crate) fn auggie_node_is_tool_metadata(node: &Value) -> bool {
    let tool_kind = node
        .get("type")
        .or_else(|| node.get("kind"))
        .and_then(Value::as_str)
        .is_some_and(|kind| {
            matches!(
                kind,
                "tool"
                    | "tool_call"
                    | "tool-call"
                    | "tool_use"
                    | "tool-use"
                    | "tool_result"
                    | "tool-result"
                    | "tool_use_result"
                    | "tool-use-result"
                    | "tool_output"
                    | "tool-output"
                    | "function_call"
                    | "function_result"
                    | "function_output"
            )
        });
    tool_kind
        || node.get("tool_call").is_some()
        || node.get("toolCall").is_some()
        || node.get("tool_result").is_some()
        || node.get("toolResult").is_some()
}

#[cfg(test)]
mod tests;
