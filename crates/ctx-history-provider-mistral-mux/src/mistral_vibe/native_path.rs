use std::{
    fs::Metadata,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use chrono::{DateTime, Utc};
use ctx_history_capture_model::file_references::visit_literal_file_reference_drafts;
use ctx_history_core::{admit_provider_declared_fact, ProviderDeclaredFact};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use ctx_history_provider_runtime::{CaptureError, Result};

use super::{
    schema::{
        mistral_vibe_bounded_metadata_from_bytes, mistral_vibe_event_text, mistral_vibe_event_type,
        mistral_vibe_metadata_pointer_string, mistral_vibe_metadata_string,
        mistral_vibe_metadata_timestamp, MistralVibeBoundedMetadata,
    },
    source::{visit_mistral_vibe_session_sources, MistralVibeSessionSource},
    MISTRAL_VIBE_CAPTURE_REVISION, MISTRAL_VIBE_POLICY_REVISION, MISTRAL_VIBE_SOURCE_FORMAT,
};

pub(crate) mod source_backed;

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionFact {
    provider_session_id: String,
    parent_provider_session_id: Option<String>,
    started_at: DateTime<Utc>,
    cwd: Option<String>,
    metadata: Value,
    lineage_ambiguous: bool,
}

impl SessionFact {
    fn from_admitted(
        source: &MistralVibeSessionSource,
        imported_at: DateTime<Utc>,
        metadata_bytes: &[u8],
    ) -> Result<(Self, Option<String>)> {
        let (metadata, failure) =
            mistral_vibe_bounded_metadata_from_bytes(source, imported_at, metadata_bytes)?;
        let MistralVibeBoundedMetadata {
            value: metadata,
            lineage_ambiguous,
        } = metadata;
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
                lineage_ambiguous,
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
                .and_then(ctx_history_capture_model::normalization::provider_value_text)
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

fn collect_file_facts(value: &Value, facts: &mut Vec<ProviderDeclaredFact>) {
    let _ = visit_literal_file_reference_drafts(value, |draft| {
        if let Some(fact) = admit_provider_declared_fact(draft.kind, draft.value, facts.len()) {
            facts.push(fact);
        }
        Ok::<(), std::convert::Infallible>(())
    });
}
