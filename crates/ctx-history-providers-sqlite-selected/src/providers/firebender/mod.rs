use std::{
    fs,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_capture_model::normalization::{provider_role, provider_timestamp_value};
use ctx_history_core::{EventRole, EventType};
use serde_json::Value;

use crate::{CaptureError, Result};

mod message_text;
pub(crate) mod native_path;

pub(crate) use message_text::{firebender_message_text, firebender_result_content};

pub(crate) use native_path::source_backed_driver_scoped;

pub fn firebender_source_backed_driver<B: crate::SelectedSqliteCaptureBinding>(
    source_path: &std::path::Path,
    data_root: &std::path::Path,
) -> ctx_history_capture_runtime::SourceBackedRouteDriver<B::Lifecycle, B::RouteControl> {
    firebender_source_backed_driver_scoped::<B>(
        source_path,
        data_root,
        ctx_history_core::SourceAnchorScope::Unqualified,
    )
}

pub fn firebender_source_backed_driver_scoped<B: crate::SelectedSqliteCaptureBinding>(
    source_path: &std::path::Path,
    data_root: &std::path::Path,
    source_scope: ctx_history_core::SourceAnchorScope,
) -> ctx_history_capture_runtime::SourceBackedRouteDriver<B::Lifecycle, B::RouteControl> {
    source_backed_driver_scoped::<B>(
        ctx_history_core::CaptureProvider::Firebender.as_str(),
        crate::FIREBENDER_SQLITE_SOURCE_FORMAT,
        source_path,
        data_root,
        source_scope,
    )
}

pub(crate) fn firebender_chat_history_db_path(path: &Path) -> Result<PathBuf> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            let file_type = metadata.file_type();
            if file_type.is_symlink() {
                return Err(CaptureError::InvalidProviderTranscriptPath {
                    path: path.to_path_buf(),
                    reason: "symlinked provider transcript roots are rejected",
                });
            }
            if file_type.is_file() {
                return Ok(path.to_path_buf());
            }
            if file_type.is_dir() {
                return Ok(path
                    .join(".idea")
                    .join("firebender")
                    .join("chat_history.db"));
            }
            Err(CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "Firebender import path must be chat_history.db or a project root",
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if path.file_name().and_then(|name| name.to_str()) == Some("chat_history.db") {
                Ok(path.to_path_buf())
            } else {
                Ok(path
                    .join(".idea")
                    .join("firebender")
                    .join("chat_history.db"))
            }
        }
        Err(error) => Err(CaptureError::Io(error)),
    }
}

pub(crate) fn firebender_message_time(message: &Value, fallback: DateTime<Utc>) -> DateTime<Utc> {
    provider_timestamp_value(
        message
            .get("timestamp")
            .or_else(|| message.get("created_at"))
            .or_else(|| message.get("updated_at")),
        fallback,
    )
}

pub(super) struct FirebenderEventParts {
    event_type: EventType,
    role: Option<EventRole>,
    occurred_at: DateTime<Utc>,
    text: String,
}

pub(super) fn firebender_event_parts(
    message: &Value,
    occurred_at: DateTime<Utc>,
) -> FirebenderEventParts {
    let role = message.get("role").and_then(Value::as_str);
    let tool_calls = message
        .get("tool_calls")
        .or_else(|| message.get("toolCalls"));
    let event_type = if role == Some("tool") {
        EventType::ToolOutput
    } else if tool_calls.is_some_and(|value| {
        value
            .as_array()
            .map(|items| !items.is_empty())
            .unwrap_or(true)
    }) {
        EventType::ToolCall
    } else {
        EventType::Message
    };
    FirebenderEventParts {
        event_type,
        role: Some(provider_role(role)),
        occurred_at,
        text: firebender_message_text(message)
            .unwrap_or_else(|| format!("Firebender {}", role.unwrap_or("message"))),
    }
}
