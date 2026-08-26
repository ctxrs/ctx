use std::{fmt, path::Path};

use chrono::{DateTime, Utc};
use ctx_history_core::EventType;
use serde::{
    de::{IgnoredAny, MapAccess, Visitor},
    Deserializer as _,
};
use serde_json::{json, Value};

use ctx_history_capture_model::normalization::provider_value_text;
use ctx_history_capture_model::time::parse_rfc3339_utc;
use ctx_history_provider_runtime::{CaptureError, Result};

use super::source::MistralVibeSessionSource;
use super::{MISTRAL_VIBE_MAX_ID_BYTES, PROVIDER_MAX_PREVIEW_CHARS};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MistralVibeBoundedMetadata {
    pub(super) value: Value,
    pub(super) lineage_ambiguous: bool,
}

pub(super) fn mistral_vibe_bounded_metadata_from_bytes(
    source: &MistralVibeSessionSource,
    imported_at: DateTime<Utc>,
    bytes: &[u8],
) -> Result<(MistralVibeBoundedMetadata, Option<String>)> {
    let raw_parent = mistral_vibe_raw_parent_authority(bytes).unwrap_or_else(|_| {
        MistralVibeRawParentAuthority {
            ambiguous: true,
            ..MistralVibeRawParentAuthority::default()
        }
    });
    let (metadata, failure) = match serde_json::from_slice::<Value>(bytes) {
        Ok(value) if value.is_object() => (value, None),
        Ok(_) => (
            Value::Null,
            Some("Mistral Vibe meta.json must contain a JSON object".to_owned()),
        ),
        Err(error) => (
            Value::Null,
            Some(bounded_metadata_text(format!(
                "invalid Mistral Vibe meta.json: {error}"
            ))),
        ),
    };
    mistral_vibe_bounded_metadata_from_value(source, imported_at, metadata, raw_parent, failure)
}

fn mistral_vibe_bounded_metadata_from_value(
    source: &MistralVibeSessionSource,
    imported_at: DateTime<Utc>,
    metadata: Value,
    raw_parent: MistralVibeRawParentAuthority,
    failure: Option<String>,
) -> Result<(MistralVibeBoundedMetadata, Option<String>)> {
    let provider_session_id = bounded_mistral_vibe_identity(
        mistral_vibe_metadata_string(&metadata, "session_id").or_else(|| {
            source
                .session_dir
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| !name.trim().is_empty())
                .map(str::to_owned)
        }),
        &source.session_dir,
        "session id",
    )?
    .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
        path: source.session_dir.clone(),
        reason: "Mistral Vibe session directory is missing a session id",
    })?;
    let parent_provider_session_id = raw_parent.value;
    let lineage_ambiguous = raw_parent.ambiguous
        || parent_provider_session_id
            .as_deref()
            .is_some_and(|parent| parent == provider_session_id);
    let started_at = mistral_vibe_metadata_timestamp(&metadata, "start_time")
        .unwrap_or(imported_at)
        .to_rfc3339();
    Ok((
        MistralVibeBoundedMetadata {
            value: json!({
                "session_id": provider_session_id,
                "parent_session_id": parent_provider_session_id,
                "start_time": started_at,
                "git_branch": mistral_vibe_metadata_string(&metadata, "git_branch")
                    .map(bounded_metadata_text),
                "environment": {
                    "working_directory": mistral_vibe_metadata_pointer_string(
                        &metadata,
                        &["/environment/working_directory"],
                    )
                    .map(bounded_metadata_text),
                },
            }),
            lineage_ambiguous,
        },
        failure,
    ))
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct MistralVibeRawParentAuthority {
    value: Option<String>,
    saw_absent: bool,
    ambiguous: bool,
}

fn mistral_vibe_raw_parent_authority(
    bytes: &[u8],
) -> serde_json::Result<MistralVibeRawParentAuthority> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let authority = deserializer.deserialize_map(MistralVibeRawParentVisitor)?;
    deserializer.end()?;
    Ok(authority)
}

struct MistralVibeRawParentVisitor;

impl<'de> Visitor<'de> for MistralVibeRawParentVisitor {
    type Value = MistralVibeRawParentAuthority;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Mistral Vibe metadata JSON object")
    }

    fn visit_map<M>(self, mut map: M) -> std::result::Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut authority = MistralVibeRawParentAuthority::default();
        while let Some(key) = map.next_key::<String>()? {
            if !matches!(key.as_str(), "parent_session_id" | "parentSessionId") {
                map.next_value::<IgnoredAny>()?;
                continue;
            }
            authority.admit_occurrence(map.next_value::<Value>()?);
        }
        Ok(authority)
    }
}

impl MistralVibeRawParentAuthority {
    fn admit_occurrence(&mut self, occurrence: Value) {
        match occurrence {
            Value::Null => {
                if self.value.is_some() {
                    self.ambiguous = true;
                }
                self.saw_absent = true;
            }
            Value::String(value)
                if !value.trim().is_empty() && value.len() <= MISTRAL_VIBE_MAX_ID_BYTES =>
            {
                if self.saw_absent
                    || self
                        .value
                        .as_deref()
                        .is_some_and(|selected| selected != value)
                {
                    self.ambiguous = true;
                }
                if self.value.is_none() {
                    self.value = Some(value);
                }
            }
            _ => self.ambiguous = true,
        }
    }
}

fn bounded_metadata_text(text: impl Into<String>) -> String {
    text.into()
        .chars()
        .take(PROVIDER_MAX_PREVIEW_CHARS)
        .collect()
}

fn bounded_mistral_vibe_identity(
    value: Option<String>,
    path: &Path,
    label: &'static str,
) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.len() > MISTRAL_VIBE_MAX_ID_BYTES {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: match label {
                "session id" => "Mistral Vibe session id exceeds the supported size",
                _ => "Mistral Vibe parent session id exceeds the supported size",
            },
        });
    }
    Ok(Some(value))
}

pub(super) fn mistral_vibe_event_type(role: &str, value: &Value) -> EventType {
    if role == "tool" || value.get("tool_call_id").is_some() {
        EventType::ToolOutput
    } else if value
        .get("tool_calls")
        .and_then(Value::as_array)
        .is_some_and(|calls| !calls.is_empty())
    {
        EventType::ToolCall
    } else if role == "system" {
        EventType::Notice
    } else {
        EventType::Message
    }
}

pub(super) fn mistral_vibe_event_text(role: &str, value: &Value, event_type: EventType) -> String {
    let mut parts = Vec::new();
    if let Some(content) = value.get("content").and_then(provider_value_text) {
        parts.push(content);
    }
    if let Some(reasoning) = value.get("reasoning_content").and_then(provider_value_text) {
        parts.push(reasoning);
    }
    if let Some(tool_calls) = value
        .get("tool_calls")
        .and_then(mistral_vibe_tool_calls_text)
    {
        parts.push(tool_calls);
    }
    if let Some(images) = value.get("images").and_then(provider_value_text) {
        parts.push(images);
    }
    if !parts.is_empty() {
        return parts.join("\n");
    }
    match event_type {
        EventType::ToolOutput => format!("Mistral Vibe {role} output"),
        EventType::ToolCall => format!("Mistral Vibe {role} tool call"),
        _ => format!("Mistral Vibe {role} message"),
    }
}

pub(super) fn mistral_vibe_tool_calls_text(value: &Value) -> Option<String> {
    value.as_array()?;
    serde_json::to_string(value).ok()
}

pub(super) fn mistral_vibe_metadata_string(value: &Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|raw| !raw.trim().is_empty())
        .map(str::to_owned)
}

pub(super) fn mistral_vibe_metadata_pointer_string(
    value: &Value,
    pointers: &[&str],
) -> Option<String> {
    pointers.iter().find_map(|pointer| {
        value
            .pointer(pointer)
            .and_then(Value::as_str)
            .filter(|raw| !raw.trim().is_empty())
            .map(str::to_owned)
    })
}

pub(super) fn mistral_vibe_metadata_timestamp(value: &Value, field: &str) -> Option<DateTime<Utc>> {
    value
        .get(field)
        .and_then(Value::as_str)
        .and_then(parse_rfc3339_utc)
}

#[cfg(test)]
mod lineage_tests {
    use super::*;

    fn bounded(bytes: &[u8]) -> MistralVibeBoundedMetadata {
        let temp = tempfile::tempdir().unwrap();
        let session_dir = temp.path().join("mistral-child");
        let source = MistralVibeSessionSource {
            metadata_path: session_dir.join("meta.json"),
            messages_path: session_dir.join("messages.jsonl"),
            session_dir,
        };
        mistral_vibe_bounded_metadata_from_bytes(&source, DateTime::<Utc>::UNIX_EPOCH, bytes)
            .unwrap()
            .0
    }

    fn admitted_parent(metadata: &MistralVibeBoundedMetadata) -> Option<&str> {
        metadata
            .value
            .get("parent_session_id")
            .and_then(Value::as_str)
    }

    #[test]
    fn raw_parent_admission_retains_one_exact_direct_claim() {
        for bytes in [
            br#"{"session_id":"mistral-child","parent_session_id":"mistral-parent"}"#.as_slice(),
            br#"{"session_id":"mistral-child","parentSessionId":"mistral-parent"}"#.as_slice(),
            br#"{
                "session_id":"mistral-child",
                "parent_session_id":"mistral-parent",
                "parent_session_id":"mistral-parent",
                "parentSessionId":"mistral-parent"
            }"#
            .as_slice(),
        ] {
            let metadata = bounded(bytes);
            assert!(!metadata.lineage_ambiguous);
            assert_eq!(admitted_parent(&metadata), Some("mistral-parent"));
        }
    }

    #[test]
    fn raw_duplicate_and_conflicting_aliases_preserve_a_claim_but_abstain() {
        for bytes in [
            br#"{
                "session_id":"mistral-child",
                "parent_session_id":"mistral-parent",
                "parent_session_id":"conflicting-parent"
            }"#
            .as_slice(),
            br#"{
                "session_id":"mistral-child",
                "parent_session_id":"mistral-parent",
                "parentSessionId":"conflicting-parent"
            }"#
            .as_slice(),
        ] {
            let metadata = bounded(bytes);
            assert!(metadata.lineage_ambiguous);
            assert_eq!(admitted_parent(&metadata), Some("mistral-parent"));
        }
    }

    #[test]
    fn raw_null_and_positive_parent_claims_are_ambiguous_in_both_orders() {
        for bytes in [
            br#"{
                "session_id":"mistral-child",
                "parent_session_id":null,
                "parent_session_id":"mistral-parent"
            }"#
            .as_slice(),
            br#"{
                "session_id":"mistral-child",
                "parent_session_id":"mistral-parent",
                "parent_session_id":null
            }"#
            .as_slice(),
        ] {
            let metadata = bounded(bytes);
            assert!(metadata.lineage_ambiguous);
            assert_eq!(admitted_parent(&metadata), Some("mistral-parent"));
        }
    }

    #[test]
    fn raw_malformed_and_self_parent_claims_abstain_explicitly() {
        for (bytes, admitted) in [
            (
                br#"{"session_id":"mistral-child","parent_session_id":7}"#.as_slice(),
                None,
            ),
            (
                br#"{"session_id":"mistral-child","parent_session_id":""}"#.as_slice(),
                None,
            ),
            (
                br#"{"session_id":"mistral-child","parent_session_id":"mistral-child"}"#.as_slice(),
                Some("mistral-child"),
            ),
        ] {
            let metadata = bounded(bytes);
            assert!(metadata.lineage_ambiguous);
            assert_eq!(admitted_parent(&metadata), admitted);
        }
    }

    #[test]
    fn raw_null_parent_and_unrelated_duplicates_remain_primary_evidence() {
        for bytes in [
            br#"{"session_id":"mistral-child","parent_session_id":null}"#.as_slice(),
            br#"{
                "session_id":"mistral-child",
                "parent_session_id":null,
                "title":"first",
                "title":"second"
            }"#
            .as_slice(),
        ] {
            let metadata = bounded(bytes);
            assert!(!metadata.lineage_ambiguous);
            assert_eq!(admitted_parent(&metadata), None);
        }
    }
}
