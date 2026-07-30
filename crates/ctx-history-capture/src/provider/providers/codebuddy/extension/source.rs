use std::path::Path;

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::provider::providers::task_json::{task_json_string_field, task_json_time_field};
use crate::Result;

use super::super::CODEBUDDY_MAX_METADATA_TEXT_BYTES;

#[derive(Debug, Clone)]
pub(super) struct CodeBuddyExtensionMetadata {
    pub(super) native_session_id: String,
    pub(super) project_hash: String,
    pub(super) conversation: Option<Value>,
    pub(super) session_index: Value,
}

impl CodeBuddyExtensionMetadata {
    pub(super) fn messages(&self) -> &[Value] {
        self.session_index
            .get("messages")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }
}

pub(super) fn codebuddy_extension_metadata_from_admitted(
    session_dir: &Path,
    session_index_bytes: &[u8],
    project_index_bytes: Option<&[u8]>,
) -> Result<CodeBuddyExtensionMetadata> {
    let session_index: Value = serde_json::from_slice(session_index_bytes)?;
    let project_hash = session_dir
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("unknown-project")
        .to_owned();
    let native_session_id = session_dir
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("unknown-session")
        .to_owned();
    let project_index = project_index_bytes
        .map(serde_json::from_slice::<Value>)
        .transpose()?;
    let conversation = project_index
        .as_ref()
        .and_then(|value| value.get("conversations"))
        .and_then(Value::as_array)
        .and_then(|items| {
            items
                .iter()
                .find(|item| item.get("id").and_then(Value::as_str) == Some(&native_session_id))
        })
        .cloned();
    Ok(CodeBuddyExtensionMetadata {
        native_session_id,
        project_hash,
        conversation,
        session_index,
    })
}

pub(super) fn codebuddy_extension_line_number(
    session_ordinal: usize,
    message_index: usize,
) -> usize {
    session_ordinal
        .saturating_mul(10_000)
        .saturating_add(message_index)
        .saturating_add(1)
}

pub(super) fn codebuddy_extension_metadata_text(
    metadata: &CodeBuddyExtensionMetadata,
    fields: &[&str],
) -> Option<String> {
    metadata
        .conversation
        .as_ref()
        .and_then(|value| task_json_string_field(value, fields))
        .filter(|value| value.len() <= CODEBUDDY_MAX_METADATA_TEXT_BYTES)
}

pub(super) fn codebuddy_message_time(
    raw_message: &Value,
    decoded_message: &Value,
    message_modified: Option<std::time::SystemTime>,
    fallback: DateTime<Utc>,
) -> DateTime<Utc> {
    task_json_time_field(
        raw_message,
        &["createdAt", "created_at", "timestamp", "time", "date"],
    )
    .or_else(|| {
        task_json_time_field(
            decoded_message,
            &["createdAt", "created_at", "timestamp", "time", "date"],
        )
    })
    .or_else(|| message_modified.map(DateTime::<Utc>::from))
    .unwrap_or(fallback)
}
