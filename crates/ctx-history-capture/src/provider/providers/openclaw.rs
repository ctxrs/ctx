use std::{
    fs::Metadata,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use ctx_history_core::EventType;
use serde_json::Value;

use crate::provider::normalization::{
    provider_capped_json_value, provider_output_event_is_failure, provider_result_outcome_evidence,
};
use crate::provider::tool_input;
use crate::{
    fnv1a64, OutputObservationKind, OutputOutcome, OutputOutcomeMetadata, Result,
    PROVIDER_MAX_PREVIEW_CHARS,
};

mod complete_content;
pub(crate) mod native_path;
mod normalization;

pub(crate) use complete_content::{
    message_record as openclaw_complete_content_record,
    source_from_admitted as openclaw_complete_content_source_from_admitted,
};
// The central source-backed registry consumes this provider-local hook after
// provider fan-in; keep the complete typed surface available in the interim.
#[allow(unused_imports)]
pub(crate) use native_path::{
    openclaw_source_backed_adapter_v0, OpenClawHydratedRecordV0, OpenClawSourceBackedAdapterV0,
    OpenClawSourceBackedDispositionV0, OpenClawSourceBackedErrorV0, OpenClawSourceBackedPageV0,
    OpenClawSourceBackedReaderV0, OpenClawSourceBackedResultV0, OpenClawSourceBackedScanV0,
    OpenClawSourceBackedSourceV0, OpenClawSourceBackedVerifiedPrefixV0,
};
pub(crate) use normalization::event as openclaw_event;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OpenClawFrozenFileMetadata {
    pub(super) length: u64,
    pub(super) modified: SystemTime,
    pub(super) readonly: bool,
    pub(super) device: Option<u64>,
    pub(super) inode: Option<u64>,
}

impl OpenClawFrozenFileMetadata {
    pub(super) fn from_metadata(metadata: &Metadata) -> Result<Self> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        #[cfg(unix)]
        let (device, inode) = (Some(metadata.dev()), Some(metadata.ino()));
        #[cfg(not(unix))]
        let (device, inode) = (None, None);

        Ok(Self {
            length: metadata.len(),
            modified: metadata.modified()?,
            readonly: metadata.permissions().readonly(),
            device,
            inode,
        })
    }

    fn revision_component(&self) -> String {
        let (side, seconds, nanos) = match self.modified.duration_since(UNIX_EPOCH) {
            Ok(duration) => ('+', duration.as_secs(), duration.subsec_nanos()),
            Err(error) => {
                let duration = error.duration();
                ('-', duration.as_secs(), duration.subsec_nanos())
            }
        };
        format!(
            "length={};modified={side}{seconds}.{nanos:09};readonly={};device={};inode={}",
            self.length,
            self.readonly,
            self.device
                .map_or_else(|| "none".to_owned(), |value| value.to_string()),
            self.inode
                .map_or_else(|| "none".to_owned(), |value| value.to_string()),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OpenClawSessionObservation {
    pub(super) canonical_path: PathBuf,
    pub(super) transcript: OpenClawFrozenFileMetadata,
    pub(super) index_file: Option<OpenClawFrozenFileMetadata>,
    pub(super) index: Value,
    pub(super) index_revision: u64,
}

impl OpenClawSessionObservation {
    pub(super) fn from_admitted(
        canonical_path: PathBuf,
        transcript_metadata: &Metadata,
        index: Option<(&Metadata, &[u8])>,
    ) -> Result<Self> {
        let transcript = OpenClawFrozenFileMetadata::from_metadata(transcript_metadata)?;
        let (index_file, index) = match index {
            Some((metadata, bytes)) => {
                let parsed = std::str::from_utf8(bytes)
                    .ok()
                    .and_then(|text| serde_json::from_str::<Value>(text).ok())
                    .map(|value| openclaw_session_index_for_file(&canonical_path, &value))
                    .unwrap_or(Value::Null);
                (
                    Some(OpenClawFrozenFileMetadata::from_metadata(metadata)?),
                    provider_capped_json_value(&parsed, PROVIDER_MAX_PREVIEW_CHARS),
                )
            }
            None => (None, Value::Null),
        };
        let index_revision = openclaw_index_revision(&index)?;
        Ok(Self {
            canonical_path,
            transcript,
            index_file,
            index,
            index_revision,
        })
    }

    pub(super) fn source_revision(&self) -> String {
        let index_file = self
            .index_file
            .as_ref()
            .map(OpenClawFrozenFileMetadata::revision_component)
            .unwrap_or_else(|| "absent".to_owned());
        format!(
            "openclaw-jsonl-metadata-v1:transcript={};index={index_file};index-entry={:016x}",
            self.transcript.revision_component(),
            self.index_revision,
        )
    }
}

pub(crate) fn openclaw_agent_id(path: &Path) -> Option<String> {
    let components = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    components.windows(2).find_map(|window| {
        (window[0] == "agents" && !window[1].is_empty()).then(|| window[1].clone())
    })
}

pub(super) fn openclaw_session_index_for_file(path: &Path, value: &Value) -> Value {
    let fallback_id = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("openclaw-session");
    let agent_id = openclaw_agent_id(path);
    let qualified_id = agent_id
        .as_deref()
        .map(|agent_id| format!("{agent_id}/{fallback_id}"));
    openclaw_find_session_index(value, fallback_id, qualified_id.as_deref())
        .cloned()
        .unwrap_or(Value::Null)
}

fn openclaw_find_session_index<'a>(
    value: &'a Value,
    fallback_id: &str,
    qualified_id: Option<&str>,
) -> Option<&'a Value> {
    match value {
        Value::Array(items) => items
            .iter()
            .find(|item| openclaw_index_value_matches(item, fallback_id, qualified_id)),
        Value::Object(map) => {
            if let Some(Value::Array(items)) = map.get("sessions") {
                return items
                    .iter()
                    .find(|item| openclaw_index_value_matches(item, fallback_id, qualified_id));
            }
            qualified_id
                .and_then(|qualified_id| map.get(qualified_id))
                .or_else(|| map.get(fallback_id))
                .or_else(|| {
                    map.values()
                        .find(|item| openclaw_index_value_matches(item, fallback_id, qualified_id))
                })
        }
        _ => None,
    }
}

fn openclaw_index_value_matches(
    value: &Value,
    fallback_id: &str,
    qualified_id: Option<&str>,
) -> bool {
    value
        .get("sessionId")
        .or_else(|| value.get("id"))
        .and_then(Value::as_str)
        .is_some_and(|session_id| session_id == fallback_id || qualified_id == Some(session_id))
}

pub(super) fn openclaw_index_revision(value: &Value) -> Result<u64> {
    Ok(fnv1a64(&serde_json::to_vec(value)?))
}

pub(super) struct OpenClawOutputMetadata {
    pub(super) kind: OutputObservationKind,
    pub(super) outcome: OutputOutcomeMetadata,
}

pub(super) fn openclaw_output_metadata(value: &Value) -> Option<OpenClawOutputMetadata> {
    if value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("message")
        != "message"
    {
        return None;
    }
    let message = value.get("message").unwrap_or(value);
    let role = message
        .get("role")
        .or_else(|| value.get("role"))
        .and_then(Value::as_str)?;
    if role != "tool" {
        return None;
    }
    let tool_name = message
        .get("name")
        .or_else(|| message.get("tool_name"))
        .or_else(|| message.get("tool"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("tool")
        .to_owned();
    let kind = if tool_input::is_command_tool(&tool_name.to_ascii_lowercase()) {
        OutputObservationKind::Command
    } else {
        OutputObservationKind::Tool
    };
    let timed_out = openclaw_value_timed_out(message);
    let exit_code = openclaw_i64_field(message, &["exit_code", "exitCode"])
        .and_then(|value| i32::try_from(value).ok());
    let duration_ms = openclaw_i64_field(message, &["duration_ms", "durationMs"])
        .and_then(|value| u64::try_from(value).ok());
    let outcome = if timed_out {
        OutputOutcome::Timeout
    } else if provider_output_event_is_failure(message) {
        OutputOutcome::Failure
    } else if provider_result_outcome_evidence(EventType::ToolOutput, message).as_str()
        == Some("success")
    {
        OutputOutcome::Success
    } else {
        OutputOutcome::Unknown
    };
    Some(OpenClawOutputMetadata {
        kind,
        outcome: OutputOutcomeMetadata {
            outcome,
            exit_code,
            duration_ms,
        },
    })
}

fn openclaw_value_timed_out(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(openclaw_value_timed_out),
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
            }) || values.values().any(openclaw_value_timed_out)
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

fn openclaw_i64_field(value: &Value, fields: &[&str]) -> Option<i64> {
    match value {
        Value::Array(values) => values
            .iter()
            .find_map(|value| openclaw_i64_field(value, fields)),
        Value::Object(values) => fields
            .iter()
            .find_map(|field| values.get(*field).and_then(Value::as_i64))
            .or_else(|| {
                values
                    .values()
                    .find_map(|value| openclaw_i64_field(value, fields))
            }),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => None,
    }
}
