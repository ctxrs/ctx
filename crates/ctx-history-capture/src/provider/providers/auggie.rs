use std::{
    fs::{self},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Duration, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, EventRole, EventType, Fidelity, ProviderEventEnvelope,
    ProviderSourceTrust,
};
use serde_json::{json, Value};

use crate::common::io::{
    ensure_provider_path_parents_are_not_symlinks, ensure_regular_provider_transcript_file,
};
use crate::provider::custom_history_jsonl::push_provider_import_failure;
use crate::provider::native::{
    native_event, native_provider_capture, provider_block_text, provider_capped_json,
    provider_string_field, provider_timestamp_from_fields, read_provider_json_file,
    NativeEventDraft, NativeSessionDraft,
};
use crate::{
    CaptureError, ProviderAdapterContext, ProviderNormalizationResult, Result,
    AUGGIE_SESSION_JSON_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS,
};

pub(crate) fn normalize_auggie_sessions(
    path: &Path,
    context: &ProviderAdapterContext,
) -> Result<ProviderNormalizationResult> {
    let mut session_paths = Vec::new();
    collect_auggie_session_paths(path, &mut session_paths)?;
    session_paths.sort();
    session_paths.dedup();
    if session_paths.is_empty() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "no Auggie session JSON files were found",
        });
    }

    let mut merged = ProviderNormalizationResult::default();
    for (session_ordinal, session_path) in session_paths.iter().enumerate() {
        match normalize_auggie_session_file(session_path, context, session_ordinal + 1) {
            Ok(mut result) => {
                merged.summary.merge(result.summary);
                merged.captures.append(&mut result.captures);
                merged.files_touched.append(&mut result.files_touched);
            }
            Err(err) => {
                push_provider_import_failure(
                    &mut merged.summary,
                    session_ordinal + 1,
                    format!("{}: {err}", session_path.display()),
                );
            }
        }
    }

    if merged.captures.is_empty() && merged.summary.failed == 0 {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "no Auggie sessions with chatHistory entries were found",
        });
    }
    Ok(merged)
}

pub(crate) fn collect_auggie_session_paths(root: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let metadata = fs::symlink_metadata(root)?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: root.to_path_buf(),
            reason: "symlinked provider transcript roots are rejected",
        });
    }
    ensure_provider_path_parents_are_not_symlinks(root)?;
    if file_type.is_file() {
        ensure_regular_provider_transcript_file(root)?;
        if root.extension().and_then(|ext| ext.to_str()) == Some("json") {
            out.push(root.to_path_buf());
        }
        return Ok(());
    }
    if !file_type.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_auggie_session_paths(&path, out)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
            ensure_regular_provider_transcript_file(&path)?;
            out.push(path);
        }
    }
    Ok(())
}

pub(crate) fn normalize_auggie_session_file(
    path: &Path,
    context: &ProviderAdapterContext,
    session_ordinal: usize,
) -> Result<ProviderNormalizationResult> {
    let session = read_provider_json_file(path, "Auggie session JSON")?;
    let provider_session_id = provider_string_field(&session, &["sessionId", "session_id"])
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
        &session,
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
        &session,
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
        &session,
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
        "workspace_id": provider_string_field(&session, &["workspaceId", "workspace_id"]),
        "name": provider_string_field(&session, &["name", "title", "sessionName"]),
        "chat_history_count": chat_history.len(),
        "agent_state": session
            .get("agentState")
            .or_else(|| session.get("agent_state"))
            .map(|value| provider_capped_json(value, PROVIDER_MAX_PREVIEW_CHARS)),
        "limitations": [
            "ctx imports request_message and response_text fields plus recognized request_nodes/response_nodes text",
            "tool calls and tool outputs in richer Auggie node schemas are retained only as capped native JSON until a public node contract is available"
        ],
    });
    let base_draft = NativeSessionDraft {
        provider: CaptureProvider::Auggie,
        source_format: AUGGIE_SESSION_JSON_SOURCE_FORMAT,
        provider_session_id: provider_session_id.clone(),
        parent_provider_session_id: provider_string_field(
            &session,
            &[
                "parentConversationId",
                "parentSessionId",
                "parent_session_id",
            ],
        ),
        root_provider_session_id: provider_string_field(
            &session,
            &["rootConversationId", "rootSessionId", "root_session_id"],
        ),
        external_agent_id: provider_string_field(
            &session,
            &["poseidonAgentId", "agentId", "agent_id"],
        ),
        agent_type: AgentType::Primary,
        role_hint: Some("primary".to_owned()),
        is_primary: true,
        started_at,
        ended_at,
        cwd,
        fidelity: Fidelity::Imported,
        raw_source_path: raw_source_path.clone(),
        trust: ProviderSourceTrust::ProviderNative,
        source_metadata,
        session_metadata,
    };

    let mut result = ProviderNormalizationResult::default();
    let mut provider_event_index = 0u64;
    for (chat_index, entry) in chat_history.iter().enumerate() {
        let exchange = entry.get("exchange").unwrap_or(entry);
        let base_time = auggie_entry_time(entry, Some(exchange))
            .unwrap_or_else(|| started_at + Duration::milliseconds(chat_index as i64 * 2));
        if let Some(text) = auggie_request_text(exchange) {
            let event = auggie_event(AuggieEventInput {
                provider_session_id: &provider_session_id,
                provider_event_index,
                chat_index,
                role: EventRole::User,
                label: "request",
                occurred_at: base_time,
                text,
                entry,
                exchange,
                raw_source_path: &raw_source_path,
            });
            let line = session_ordinal
                .saturating_mul(10_000)
                .saturating_add(chat_index.saturating_mul(2))
                .saturating_add(1);
            result.captures.push((
                line,
                native_provider_capture(base_draft.clone(), context, Some(event)),
            ));
            provider_event_index = provider_event_index.saturating_add(1);
        }
        if let Some(text) = auggie_response_text(exchange) {
            let event = auggie_event(AuggieEventInput {
                provider_session_id: &provider_session_id,
                provider_event_index,
                chat_index,
                role: EventRole::Assistant,
                label: "response",
                occurred_at: base_time + Duration::milliseconds(1),
                text,
                entry,
                exchange,
                raw_source_path: &raw_source_path,
            });
            let line = session_ordinal
                .saturating_mul(10_000)
                .saturating_add(chat_index.saturating_mul(2))
                .saturating_add(2);
            result.captures.push((
                line,
                native_provider_capture(base_draft.clone(), context, Some(event)),
            ));
            provider_event_index = provider_event_index.saturating_add(1);
        }
    }

    if result.captures.is_empty() {
        result.captures.push((
            session_ordinal,
            native_provider_capture(base_draft, context, None),
        ));
    }

    Ok(result)
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

pub(crate) fn auggie_event(input: AuggieEventInput<'_>) -> ProviderEventEnvelope {
    let request_id = input
        .exchange
        .get("request_id")
        .or_else(|| input.exchange.get("requestId"))
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty());
    let event_hash = request_id
        .map(|id| format!("{id}:{}", input.label))
        .unwrap_or_else(|| format!("chat-{}:{}", input.chat_index, input.label));
    native_event(NativeEventDraft {
        provider: CaptureProvider::Auggie,
        source_format: AUGGIE_SESSION_JSON_SOURCE_FORMAT,
        provider_session_id: input.provider_session_id.to_owned(),
        provider_event_index: input.provider_event_index,
        provider_event_hash: Some(event_hash.clone()),
        cursor: format!("{}:{event_hash}", input.raw_source_path),
        event_type: EventType::Message,
        role: Some(input.role),
        occurred_at: input.occurred_at,
        text: input.text,
        body: json!({
            "chat_history_entry": input.entry,
            "exchange": input.exchange,
            "message_kind": input.label,
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
    })
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
    provider_string_field(exchange, &["request_message", "requestMessage", "message"]).or_else(
        || {
            auggie_nodes_text(
                exchange
                    .get("request_nodes")
                    .or_else(|| exchange.get("requestNodes")),
            )
        },
    )
}

pub(crate) fn auggie_response_text(exchange: &Value) -> Option<String> {
    provider_string_field(exchange, &["response_text", "responseText", "response"]).or_else(|| {
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
    node.pointer("/text_node/content")
        .or_else(|| node.pointer("/textNode/content"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| provider_block_text(node))
        .or_else(|| {
            node.get("tool_name")
                .or_else(|| node.get("toolName"))
                .and_then(Value::as_str)
                .map(|name| format!("tool: {name}"))
        })
}
