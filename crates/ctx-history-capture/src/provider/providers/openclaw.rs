#[cfg(test)]
use std::cell::Cell;
use std::{
    fs::{self, Metadata},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use ctx_history_core::EventType;
use serde_json::Value;

use crate::common::io::{ensure_regular_provider_transcript_file, read_text_file_limited};
use crate::provider::normalization::{
    provider_capped_json, provider_output_event_is_failure, provider_result_outcome_evidence,
};
use crate::provider::tool_input;
use crate::{
    fnv1a64, CaptureError, OutputCommandContext, OutputObservationKind, OutputOutcome,
    OutputOutcomeMetadata, Result, MAX_OPENCLAW_SESSION_INDEX_BYTES, PROVIDER_MAX_PREVIEW_CHARS,
};

const OPENCLAW_RELEASED_CAPTURE_REVISION: u32 = 3;
const OPENCLAW_RELEASED_POLICY_REVISION: u32 = 6;
#[cfg(test)]
thread_local! {
    static OMIT_FILE_IDS: Cell<bool> = const { Cell::new(false) };
}

mod complete_content;
pub(crate) mod native_path;
mod normalization;

pub(crate) use complete_content::{
    message_record as openclaw_complete_content_record,
    source_from_admitted as openclaw_complete_content_source_from_admitted,
};
pub(crate) use native_path::import_openclaw_nativepath_tree;
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
    pub(super) fn read(path: &Path) -> Result<Self> {
        ensure_regular_provider_transcript_file(path)?;
        Self::from_metadata(&fs::symlink_metadata(path)?)
    }

    pub(super) fn read_optional(path: &Path) -> Result<Option<Self>> {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_file() => {
                Self::from_metadata(&metadata).map(Some)
            }
            Ok(_) => Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub(super) fn from_metadata(metadata: &Metadata) -> Result<Self> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        #[cfg(unix)]
        let (device, inode) = {
            #[cfg(test)]
            if OMIT_FILE_IDS.with(Cell::get) {
                (None, None)
            } else {
                (Some(metadata.dev()), Some(metadata.ino()))
            }
            #[cfg(not(test))]
            {
                (Some(metadata.dev()), Some(metadata.ino()))
            }
        };
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

#[cfg(test)]
pub(super) fn without_file_ids<T>(operation: impl FnOnce() -> T) -> T {
    struct Restore(bool);

    impl Drop for Restore {
        fn drop(&mut self) {
            OMIT_FILE_IDS.with(|omit| omit.set(self.0));
        }
    }

    let previous = OMIT_FILE_IDS.with(|omit| omit.replace(true));
    let restore = Restore(previous);
    let result = operation();
    drop(restore);
    result
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
    pub(super) fn read(path: &Path) -> Result<Self> {
        let transcript = OpenClawFrozenFileMetadata::read(path)?;
        let canonical_path = fs::canonicalize(path)?;
        let index_path = path
            .parent()
            .map(|parent| parent.join("sessions.json"))
            .unwrap_or_else(|| PathBuf::from("sessions.json"));
        let index_file = OpenClawFrozenFileMetadata::read_optional(&index_path)?;
        let index = if index_file.is_some() {
            read_text_file_limited(
                &index_path,
                MAX_OPENCLAW_SESSION_INDEX_BYTES,
                "OpenClaw sessions.json",
            )
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .map(|value| openclaw_session_index_for_file(path, &value))
            .unwrap_or(Value::Null)
        } else {
            Value::Null
        };
        let index = provider_capped_json(&index, PROVIDER_MAX_PREVIEW_CHARS);
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

    pub(super) fn revalidate(&self, path: &Path) -> Result<bool> {
        match Self::read(path) {
            Ok(current) => Ok(current == *self),
            Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(false)
            }
            Err(CaptureError::InvalidProviderTranscriptPath { .. }) => Ok(false),
            Err(error) => Err(error),
        }
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
    pub(super) native_record_id: String,
    pub(super) call_id: Option<String>,
    pub(super) command: Option<OutputCommandContext>,
    pub(super) outcome: OutputOutcomeMetadata,
}

pub(super) fn openclaw_output_metadata(
    value: &Value,
    line_number: usize,
    session_cwd: Option<&str>,
) -> Option<OpenClawOutputMetadata> {
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
    let call_id = [
        "tool_call_id",
        "toolCallId",
        "call_id",
        "callId",
        "tool_use_id",
        "toolUseId",
    ]
    .iter()
    .find_map(|field| {
        message
            .get(*field)
            .or_else(|| value.get(*field))
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
    });
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
    let command = (kind == OutputObservationKind::Command).then(|| OutputCommandContext {
        tool_name: tool_name.clone(),
        command: message
            .get("input")
            .or_else(|| message.get("arguments"))
            .or_else(|| message.get("args"))
            .and_then(tool_input::command)
            .unwrap_or_default(),
        working_directory: message
            .get("input")
            .or_else(|| message.get("arguments"))
            .or_else(|| message.get("args"))
            .and_then(tool_input::working_directory)
            .or_else(|| session_cwd.map(str::to_owned)),
    });
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
        native_record_id: value
            .get("id")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("line-{line_number}")),
        call_id,
        command,
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

#[cfg(test)]
#[path = "openclaw/tests.rs"]
mod tests;
