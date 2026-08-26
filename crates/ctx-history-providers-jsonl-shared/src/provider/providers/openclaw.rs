use std::{
    fs::Metadata,
    path::{Path, PathBuf},
    time::SystemTime,
};

use ctx_history_capture_model::normalization::provider_capped_json_value;
use serde_json::Value;

use crate::{fnv1a64, Result, PROVIDER_MAX_PREVIEW_CHARS};

pub(crate) mod native_path;
mod normalization;

pub(crate) use normalization::event_fact;

pub(crate) use native_path::{
    openclaw_source_backed_adapter_v0, openclaw_source_backed_adapter_v0_with_source_root_lineage,
};

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
