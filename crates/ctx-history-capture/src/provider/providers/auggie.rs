use std::path::Path;

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::provider::normalization::{provider_string_field, provider_timestamp_from_fields};
use crate::{CaptureError, ProviderAdapterContext, Result};

pub(crate) mod native_path;

pub(super) struct AuggieSessionData<'a> {
    pub(super) provider_session_id: String,
    pub(super) parent_provider_session_id: Option<String>,
    pub(super) root_provider_session_id: Option<String>,
    pub(super) chat_history: &'a [Value],
    pub(super) started_at: DateTime<Utc>,
    pub(super) cwd: Option<String>,
    pub(super) raw_source_path: String,
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
            started_at,
            cwd,
            chat_history,
            raw_source_path,
        })
    }
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

#[cfg(test)]
mod tests;
