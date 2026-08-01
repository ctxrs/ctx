use std::path::Path;

use chrono::{DateTime, Utc};
use ctx_history_core::{AgentType, CaptureProvider, EventRole, EventType, SessionStatus};
use serde_json::{json, Value};

use crate::common::time::parse_rfc3339_utc;
use crate::provider::normalization::{provider_capped_json, provider_role, provider_value_text};
use crate::PROVIDER_MAX_PREVIEW_CHARS;

use super::native_path::{
    factory_droid_event_text, factory_droid_event_type, factory_droid_header_cwd,
    factory_droid_header_session_id, factory_droid_model, factory_droid_role,
    factory_droid_session_relationships, windsurf_event_role, windsurf_event_text,
    windsurf_event_type,
};
pub(crate) fn antigravity_tool_call_text(value: &Value) -> Option<String> {
    value.as_array().and_then(|calls| {
        let names: Vec<&str> = calls
            .iter()
            .filter_map(|call| call.get("name").and_then(Value::as_str))
            .collect();
        if names.is_empty() {
            None
        } else {
            Some(format!("tool calls: {}", names.join(", ")))
        }
    })
}
pub(crate) fn native_jsonl_header_session_id(
    provider: CaptureProvider,
    value: &Value,
) -> Option<String> {
    if provider == CaptureProvider::FactoryAiDroid {
        return factory_droid_header_session_id(value);
    }
    match provider {
        CaptureProvider::Gemini | CaptureProvider::Tabnine => {
            value.get("sessionId").and_then(Value::as_str)
        }
        CaptureProvider::CopilotCli => (value.get("type").and_then(Value::as_str)
            == Some("session.start"))
        .then(|| value.pointer("/data/sessionId").and_then(Value::as_str))
        .flatten(),
        CaptureProvider::QwenCode => value.get("sessionId").and_then(Value::as_str),
        _ => None,
    }
    .filter(|id| !id.trim().is_empty())
    .map(str::to_owned)
}

pub(crate) fn native_jsonl_header_start_time(
    provider: CaptureProvider,
    value: &Value,
) -> Option<DateTime<Utc>> {
    match provider {
        CaptureProvider::Antigravity => value.get("created_at").and_then(Value::as_str),
        CaptureProvider::Gemini | CaptureProvider::Tabnine => {
            value.get("startTime").and_then(Value::as_str)
        }
        CaptureProvider::CopilotCli => value.pointer("/data/startTime").and_then(Value::as_str),
        _ => None,
    }
    .and_then(parse_rfc3339_utc)
}

pub(crate) fn native_jsonl_header_cwd(provider: CaptureProvider, value: &Value) -> Option<String> {
    if provider == CaptureProvider::FactoryAiDroid {
        return factory_droid_header_cwd(value);
    }
    match provider {
        CaptureProvider::Gemini | CaptureProvider::Tabnine => value
            .get("directories")
            .and_then(Value::as_array)
            .and_then(|dirs| dirs.first())
            .and_then(Value::as_str),
        CaptureProvider::CopilotCli => value.pointer("/data/context/cwd").and_then(Value::as_str),
        CaptureProvider::QwenCode => value.get("cwd").and_then(Value::as_str),
        _ => None,
    }
    .filter(|cwd| !cwd.trim().is_empty())
    .map(str::to_owned)
}

pub(crate) fn native_jsonl_path_session(
    provider: CaptureProvider,
    path: &Path,
    header: &Value,
    native_session_id: &str,
) -> (String, Option<String>, Option<String>, AgentType) {
    match provider {
        CaptureProvider::Gemini | CaptureProvider::Tabnine => {
            let parent = path
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str());
            if parent.is_some_and(|name| name != "chats") {
                return (
                    native_session_id.to_owned(),
                    parent.map(str::to_owned),
                    None,
                    AgentType::Subagent,
                );
            }
            (native_session_id.to_owned(), None, None, AgentType::Primary)
        }
        CaptureProvider::FactoryAiDroid => {
            factory_droid_session_relationships(header, native_session_id)
        }
        _ => (native_session_id.to_owned(), None, None, AgentType::Primary),
    }
}

pub(crate) fn antigravity_session_id_from_path(path: &Path) -> Option<String> {
    let components: Vec<String> = path
        .components()
        .filter_map(|component| component.as_os_str().to_str().map(str::to_owned))
        .collect();
    components
        .windows(2)
        .find_map(|window| {
            (window[0] == "brain" && !window[1].trim().is_empty()).then(|| window[1].clone())
        })
        .or_else(|| {
            components.windows(2).find_map(|window| {
                (window[1] == ".system_generated" && !window[0].trim().is_empty())
                    .then(|| window[0].clone())
            })
        })
        .or_else(|| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .filter(|stem| !stem.trim().is_empty())
                .map(str::to_owned)
        })
}

pub(crate) fn native_jsonl_timestamp(value: &Value) -> Option<DateTime<Utc>> {
    value
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_rfc3339_utc)
        .or_else(|| {
            value
                .get("created_at")
                .and_then(Value::as_str)
                .and_then(parse_rfc3339_utc)
        })
        .or_else(|| {
            value
                .pointer("/time/created")
                .and_then(Value::as_i64)
                .and_then(DateTime::<Utc>::from_timestamp_millis)
        })
}

pub(crate) fn native_jsonl_session_status(
    provider: CaptureProvider,
    header: &Value,
) -> SessionStatus {
    if provider == CaptureProvider::CopilotCli
        && header.get("type").and_then(Value::as_str) == Some("abort")
    {
        SessionStatus::Interrupted
    } else {
        SessionStatus::Imported
    }
}

pub(super) fn native_jsonl_normalized_header_metadata(header: &Value) -> Value {
    let header_preview = provider_capped_json(header, PROVIDER_MAX_PREVIEW_CHARS);
    provider_capped_json(&header_preview, PROVIDER_MAX_PREVIEW_CHARS)
}

pub(super) fn native_jsonl_session_metadata_from_normalized_header(
    provider: CaptureProvider,
    source_format: &str,
    normalized_header_metadata: &Value,
    path: &Path,
) -> Value {
    json!({
        "source_format": source_format,
        "provider": provider.as_str(),
        "source_path": path.display().to_string(),
        "header": normalized_header_metadata,
    })
}

pub(crate) fn native_jsonl_entry_type(provider: CaptureProvider, value: &Value) -> String {
    match provider {
        CaptureProvider::Antigravity => value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown"),
        CaptureProvider::Gemini | CaptureProvider::Tabnine => {
            if value.get("$set").is_some() {
                "$set"
            } else if value.get("$rewindTo").is_some() {
                "$rewindTo"
            } else {
                value
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
            }
        }
        _ => value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown"),
    }
    .to_owned()
}

pub(crate) fn native_jsonl_event_type(provider: CaptureProvider, value: &Value) -> EventType {
    match provider {
        CaptureProvider::Antigravity => match value.get("type").and_then(Value::as_str) {
            Some("USER_INPUT" | "CONVERSATION_HISTORY") => EventType::Message,
            Some("PLANNER_RESPONSE") => {
                if value.get("tool_calls").is_some() {
                    EventType::ToolCall
                } else {
                    EventType::Message
                }
            }
            Some("CODE_ACTION") => EventType::ToolCall,
            Some("CHECKPOINT") => EventType::Summary,
            Some("SYSTEM_MESSAGE") => EventType::Notice,
            _ => EventType::Notice,
        },
        CaptureProvider::Gemini | CaptureProvider::Tabnine => {
            if value.get("$set").is_some() || value.get("$rewindTo").is_some() {
                EventType::Notice
            } else if value.get("toolCalls").is_some() {
                if gemini_tool_calls_have_result(value) {
                    EventType::ToolOutput
                } else {
                    EventType::ToolCall
                }
            } else {
                match value.get("type").and_then(Value::as_str) {
                    Some("user" | "gemini" | "tabnine") => EventType::Message,
                    _ => EventType::Notice,
                }
            }
        }
        CaptureProvider::FactoryAiDroid => factory_droid_event_type(value),
        CaptureProvider::CopilotCli => match value.get("type").and_then(Value::as_str) {
            Some("user.message" | "assistant.message") => EventType::Message,
            Some("tool.execution_start") => EventType::ToolCall,
            Some("tool.execution_complete") => EventType::ToolOutput,
            Some("session.truncation") => EventType::Summary,
            Some("abort") => EventType::Notice,
            _ => EventType::Notice,
        },
        CaptureProvider::Windsurf => windsurf_event_type(value),
        CaptureProvider::QwenCode => match value.get("type").and_then(Value::as_str) {
            Some("user" | "assistant") if native_jsonl_content_has(value, "tool_use") => {
                EventType::ToolCall
            }
            Some("tool_result") => EventType::ToolOutput,
            Some("user" | "assistant") => EventType::Message,
            Some("system") => EventType::Notice,
            _ if value.get("toolCallResult").is_some() => EventType::ToolOutput,
            _ => EventType::Notice,
        },
        _ => EventType::Notice,
    }
}

pub(crate) fn native_jsonl_role(provider: CaptureProvider, value: &Value) -> EventRole {
    match provider {
        CaptureProvider::Antigravity => match value.get("source").and_then(Value::as_str) {
            Some("user") => EventRole::User,
            Some("planner" | "agent" | "assistant") => EventRole::Assistant,
            Some("tool" | "executor") => EventRole::Tool,
            Some("system") => EventRole::System,
            _ => match value.get("type").and_then(Value::as_str) {
                Some("USER_INPUT") => EventRole::User,
                Some("SYSTEM_MESSAGE" | "CHECKPOINT") => EventRole::System,
                _ => EventRole::Assistant,
            },
        },
        CaptureProvider::Gemini | CaptureProvider::Tabnine => {
            match value.get("type").and_then(Value::as_str) {
                Some("user") => EventRole::User,
                Some("gemini" | "tabnine") => EventRole::Assistant,
                _ => EventRole::System,
            }
        }
        CaptureProvider::FactoryAiDroid => factory_droid_role(value),
        CaptureProvider::CopilotCli => match value.get("type").and_then(Value::as_str) {
            Some("user.message") => EventRole::User,
            Some("assistant.message") => EventRole::Assistant,
            Some("tool.execution_start" | "tool.execution_complete") => EventRole::Tool,
            _ => EventRole::System,
        },
        CaptureProvider::Windsurf => windsurf_event_role(value),
        CaptureProvider::QwenCode => provider_role(
            value
                .pointer("/message/role")
                .or_else(|| value.get("type"))
                .and_then(Value::as_str),
        ),
        _ => EventRole::Unknown,
    }
}

pub(crate) fn native_jsonl_event_text(
    provider: CaptureProvider,
    value: &Value,
    _event_type: EventType,
    entry_type: &str,
) -> String {
    match provider {
        CaptureProvider::Antigravity => value
            .get("content")
            .and_then(provider_value_text)
            .map(|content| {
                value
                    .get("tool_calls")
                    .and_then(antigravity_tool_call_text)
                    .map(|tools| format!("{content}\n{tools}"))
                    .unwrap_or(content)
            })
            .or_else(|| value.get("thinking").and_then(provider_value_text))
            .or_else(|| value.get("tool_calls").and_then(antigravity_tool_call_text))
            .unwrap_or_default(),
        CaptureProvider::Gemini | CaptureProvider::Tabnine => value
            .get("content")
            .and_then(provider_value_text)
            .or_else(|| value.get("toolCalls").and_then(provider_value_text))
            .or_else(|| value.get("$set").and_then(provider_value_text))
            .or_else(|| {
                value
                    .get("$rewindTo")
                    .and_then(Value::as_str)
                    .map(|id| format!("rewind to {id}"))
            })
            .unwrap_or_default(),
        CaptureProvider::FactoryAiDroid => factory_droid_event_text(value),
        CaptureProvider::CopilotCli => value
            .pointer("/data/content")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| {
                value
                    .pointer("/data/result/content")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .or_else(|| {
                value
                    .pointer("/data/error/message")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .or_else(|| {
                value
                    .pointer("/data/toolName")
                    .and_then(Value::as_str)
                    .map(|tool| format!("tool {tool}"))
            })
            .unwrap_or_default(),
        CaptureProvider::Windsurf => windsurf_event_text(value, entry_type),
        CaptureProvider::QwenCode => value
            .pointer("/message/content")
            .or_else(|| value.get("message"))
            .and_then(provider_value_text)
            .or_else(|| value.get("toolCallResult").and_then(provider_value_text))
            .or_else(|| value.get("content").and_then(provider_value_text))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

pub(crate) fn native_jsonl_model(provider: CaptureProvider, value: &Value) -> Option<Value> {
    match provider {
        CaptureProvider::Antigravity => value.get("model").cloned(),
        CaptureProvider::Gemini | CaptureProvider::Tabnine => value.get("model").cloned(),
        CaptureProvider::FactoryAiDroid => factory_droid_model(value),
        CaptureProvider::CopilotCli => value.pointer("/data/selectedModel").cloned(),
        CaptureProvider::QwenCode => value
            .get("model")
            .cloned()
            .or_else(|| value.pointer("/message/model").cloned()),
        _ => None,
    }
}

pub(crate) fn native_jsonl_tokens(_provider: CaptureProvider, value: &Value) -> Option<Value> {
    value
        .get("tokens")
        .or_else(|| value.get("usageMetadata"))
        .cloned()
}

pub(crate) fn gemini_tool_calls_have_result(value: &Value) -> bool {
    value
        .get("toolCalls")
        .and_then(Value::as_array)
        .map(|calls| calls.iter().any(|call| call.get("result").is_some()))
        .unwrap_or(false)
}

pub(crate) fn native_jsonl_content_has(value: &Value, expected: &str) -> bool {
    value
        .pointer("/message/content")
        .or_else(|| value.get("content"))
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .any(|block| block.get("type").and_then(Value::as_str) == Some(expected))
        })
        .unwrap_or(false)
}
