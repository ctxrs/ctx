use std::{
    collections::BTreeSet,
    fs::Metadata,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{AgentType, EventType};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    provider::{
        file_touches::visit_all_file_touch_drafts,
        normalization::{provider_output_event_is_failure, provider_result_outcome_evidence},
        providers::native_jsonl::native_jsonl_timestamp,
        tool_input,
    },
    CaptureError, OutputObservationKind, OutputOutcome, Result, MISTRAL_VIBE_SOURCE_FORMAT,
};

use super::{
    schema::{
        mistral_vibe_bounded_metadata_from_bytes, mistral_vibe_event_text, mistral_vibe_event_type,
        mistral_vibe_metadata_pointer_string, mistral_vibe_metadata_string,
        mistral_vibe_metadata_timestamp,
    },
    source::{visit_mistral_vibe_session_sources, MistralVibeSessionSource},
    MISTRAL_VIBE_CAPTURE_REVISION, MISTRAL_VIBE_POLICY_REVISION,
};

pub(crate) mod source_backed;

const MAX_TOUCHES_PER_RECORD: usize = 60;

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionFact {
    provider_session_id: String,
    parent_provider_session_id: Option<String>,
    started_at: DateTime<Utc>,
    cwd: Option<String>,
    metadata: Value,
}

impl SessionFact {
    fn from_admitted(
        source: &MistralVibeSessionSource,
        imported_at: DateTime<Utc>,
        metadata_bytes: &[u8],
    ) -> Result<(Self, Option<String>)> {
        let (metadata, failure) =
            mistral_vibe_bounded_metadata_from_bytes(source, imported_at, metadata_bytes)?;
        let provider_session_id = mistral_vibe_metadata_string(&metadata, "session_id").ok_or(
            CaptureError::SystemInvariant("Mistral Vibe bounded metadata lost its session id"),
        )?;
        Ok((
            Self {
                provider_session_id,
                parent_provider_session_id: mistral_vibe_metadata_string(
                    &metadata,
                    "parent_session_id",
                ),
                started_at: mistral_vibe_metadata_timestamp(&metadata, "start_time")
                    .unwrap_or(imported_at),
                cwd: mistral_vibe_metadata_pointer_string(
                    &metadata,
                    &["/environment/working_directory"],
                ),
                metadata,
            },
            failure,
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ObservedTime {
    before_epoch: bool,
    seconds: u64,
    nanos: u32,
}

impl ObservedTime {
    fn from_system_time(value: SystemTime) -> Self {
        match value.duration_since(UNIX_EPOCH) {
            Ok(duration) => Self {
                before_epoch: false,
                seconds: duration.as_secs(),
                nanos: duration.subsec_nanos(),
            },
            Err(error) => {
                let duration = error.duration();
                Self {
                    before_epoch: true,
                    seconds: duration.as_secs(),
                    nanos: duration.subsec_nanos(),
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileStamp {
    length: u64,
    modified: ObservedTime,
    readonly: bool,
    device: Option<u64>,
    inode: Option<u64>,
}

impl FileStamp {
    fn from_metadata(metadata: &Metadata) -> Result<Self> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        #[cfg(unix)]
        let (device, inode) = (Some(metadata.dev()), Some(metadata.ino()));
        #[cfg(not(unix))]
        let (device, inode) = (None, None);

        Ok(Self {
            length: metadata.len(),
            modified: ObservedTime::from_system_time(metadata.modified()?),
            readonly: metadata.permissions().readonly(),
            device,
            inode,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SourceObservation {
    canonical_metadata_path: PathBuf,
    canonical_messages_path: PathBuf,
    metadata: FileStamp,
    messages: FileStamp,
    metadata_sha256: [u8; 32],
    exact_content_revision: String,
}

fn valid_mistral_vibe_record_role(value: &Value) -> std::result::Result<&str, &'static str> {
    if !value.is_object() {
        return Err("expected a JSON object");
    }
    let role = value
        .get("role")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|role| !role.is_empty())
        .ok_or("missing a non-empty string role")?;
    let carries_message_content = ["content", "reasoning_content", "images"]
        .iter()
        .any(|field| {
            value
                .get(*field)
                .and_then(crate::provider::normalization::provider_value_text)
                .is_some()
        });
    let carries_tool_call = value
        .get("tool_calls")
        .and_then(Value::as_array)
        .is_some_and(|calls| !calls.is_empty());
    if !carries_message_content && !carries_tool_call {
        return Err("does not contain message content, a tool call, or a tool result");
    }
    Ok(role)
}

fn collect_touched_paths(value: &Value) -> Result<Vec<String>> {
    let mut seen = BTreeSet::new();
    let mut paths = Vec::new();
    let _ = visit_all_file_touch_drafts(value, |draft| {
        let key = (
            draft.path.clone(),
            draft.old_path.clone(),
            draft.change_kind.map(|kind| format!("{kind:?}")),
        );
        if !seen.insert(key) {
            return Ok(());
        }
        if paths.len() >= MAX_TOUCHES_PER_RECORD {
            return Err(());
        }
        paths.push(draft.path);
        Ok(())
    });
    Ok(paths)
}

#[derive(Debug, Clone, Copy)]
struct OutputClassification {
    kind: OutputObservationKind,
    outcome: OutputOutcome,
}

fn output_classification(value: &Value) -> OutputClassification {
    let tool_name = value
        .get("name")
        .or_else(|| value.get("tool_name"))
        .or_else(|| value.get("tool"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("tool");
    let kind = if tool_input::is_command_tool(&tool_name.to_ascii_lowercase()) {
        OutputObservationKind::Command
    } else {
        OutputObservationKind::Tool
    };
    let outcome = if value_timed_out(value) {
        OutputOutcome::Timeout
    } else if provider_output_event_is_failure(value) {
        OutputOutcome::Failure
    } else if provider_result_outcome_evidence(EventType::ToolOutput, value).as_str()
        == Some("success")
    {
        OutputOutcome::Success
    } else {
        OutputOutcome::Unknown
    };
    OutputClassification { kind, outcome }
}

fn value_timed_out(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(value_timed_out),
        Value::Object(values) => {
            values.iter().any(|(key, value)| {
                matches!(key.as_str(), "timed_out" | "timedOut" | "timeout")
                    && value.as_bool().unwrap_or(false)
                    || matches!(key.as_str(), "status" | "state" | "outcome")
                        && value.as_str().is_some_and(|value| {
                            matches!(
                                value.trim().to_ascii_lowercase().as_str(),
                                "timeout" | "timed_out" | "timedout"
                            )
                        })
            }) || values.values().any(value_timed_out)
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}
