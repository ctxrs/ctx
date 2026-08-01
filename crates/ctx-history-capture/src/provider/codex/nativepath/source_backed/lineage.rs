use std::{
    collections::{HashMap, HashSet},
    io::BufReader,
    sync::Mutex,
};

use serde_json::Value;

use super::*;
use crate::common::io::{read_provider_jsonl_line_or_skip_oversized, ProviderJsonlLineRead};
use crate::provider::codex::nativepath::reader::open_codex_source_capability;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CodexOutcomeOriginV0 {
    UniqueToSession,
    CopiedFromAncestor,
    Unproven,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AncestorCallPresenceV0 {
    Present,
    Absent,
    Unproven,
}

#[derive(Debug)]
pub(super) struct CodexOutcomeLineageAuthorityV0 {
    sources: HashMap<String, CodexCatalogSource>,
    call_presence: Mutex<HashMap<(String, String, String), AncestorCallPresenceV0>>,
}

impl CodexOutcomeLineageAuthorityV0 {
    pub(super) fn from_sources(sources: &[(CodexCatalogSource, SourceKey, String)]) -> Self {
        Self {
            sources: sources
                .iter()
                .map(|(source, _, native_session_id)| (native_session_id.clone(), source.clone()))
                .collect(),
            call_presence: Mutex::new(HashMap::new()),
        }
    }

    #[cfg(test)]
    pub(super) fn unscoped() -> Self {
        Self {
            sources: HashMap::new(),
            call_presence: Mutex::new(HashMap::new()),
        }
    }

    pub(super) fn classify(
        &self,
        native_session_id: &str,
        origin_call_id: &str,
        result_call_id: &str,
    ) -> CodexSourceBackedResultV0<CodexOutcomeOriginV0> {
        let Some(current) = self.sources.get(native_session_id) else {
            return Ok(CodexOutcomeOriginV0::Unproven);
        };
        let mut parent = current.catalog_parent_native_session_id.as_deref();
        let mut visited = HashSet::new();

        while let Some(parent_id) = parent {
            if !visited.insert(parent_id.to_owned()) {
                return Ok(CodexOutcomeOriginV0::Unproven);
            }
            let Some(parent_source) = self.sources.get(parent_id) else {
                return Ok(CodexOutcomeOriginV0::Unproven);
            };
            match self.ancestor_call_presence(
                parent_id,
                parent_source,
                origin_call_id,
                result_call_id,
            )? {
                AncestorCallPresenceV0::Present => {
                    return Ok(CodexOutcomeOriginV0::CopiedFromAncestor)
                }
                AncestorCallPresenceV0::Absent => {
                    parent = parent_source.catalog_parent_native_session_id.as_deref();
                }
                AncestorCallPresenceV0::Unproven => return Ok(CodexOutcomeOriginV0::Unproven),
            }
        }
        Ok(CodexOutcomeOriginV0::UniqueToSession)
    }

    fn ancestor_call_presence(
        &self,
        native_session_id: &str,
        source: &CodexCatalogSource,
        origin_call_id: &str,
        result_call_id: &str,
    ) -> CodexSourceBackedResultV0<AncestorCallPresenceV0> {
        if origin_call_id.is_empty() || result_call_id.is_empty() {
            return Ok(AncestorCallPresenceV0::Unproven);
        }
        let key = (
            native_session_id.to_owned(),
            origin_call_id.to_owned(),
            result_call_id.to_owned(),
        );
        let Ok(mut cache) = self.call_presence.lock() else {
            return Ok(AncestorCallPresenceV0::Unproven);
        };
        if let Some(presence) = cache.get(&key).copied() {
            return Ok(presence);
        }
        let presence = scan_ancestor_for_execution(source, origin_call_id, result_call_id)?;
        cache.insert(key, presence);
        Ok(presence)
    }
}

fn scan_ancestor_for_execution(
    source: &CodexCatalogSource,
    origin_call_id: &str,
    result_call_id: &str,
) -> CodexSourceBackedResultV0<AncestorCallPresenceV0> {
    let opened = open_codex_source_capability(source)?;
    let before = opened_codex_file_observation(&source.source_path, opened.file())?;
    if before != source.catalog_observation {
        return Err(CaptureError::SourceChangedDuringCapture.into());
    }

    let mut reader = BufReader::new(opened.file().try_clone()?);
    let mut line = Vec::new();
    let mut uncertain = false;
    let mut found_origin = false;
    let mut found_result = false;
    let presence = loop {
        match read_provider_jsonl_line_or_skip_oversized(&mut reader, &mut line)? {
            ProviderJsonlLineRead::Eof => {
                break if uncertain || found_origin || found_result {
                    AncestorCallPresenceV0::Unproven
                } else {
                    AncestorCallPresenceV0::Absent
                };
            }
            ProviderJsonlLineRead::Oversized { .. } => uncertain = true,
            ProviderJsonlLineRead::Line { .. } => {
                if !contains_bytes(&line, br#""call_id""#) {
                    continue;
                }
                let Ok(record) = serde_json::from_slice::<Value>(&line) else {
                    uncertain = true;
                    continue;
                };
                found_origin |= is_structured_tool_call(&record, origin_call_id);
                found_result |= is_structured_tool_result(&record, result_call_id);
                if found_origin && found_result {
                    break AncestorCallPresenceV0::Present;
                }
            }
        }
    };

    opened.revalidate_same_object()?;
    let after = opened_codex_file_observation(&source.source_path, opened.file())?;
    if after != before {
        return Err(CaptureError::SourceChangedDuringCapture.into());
    }
    Ok(presence)
}

fn is_structured_tool_call(record: &Value, origin_call_id: &str) -> bool {
    if record.get("type").and_then(Value::as_str) != Some("response_item") {
        return false;
    }
    let Some(payload) = record.get("payload") else {
        return false;
    };
    matches!(
        payload.get("type").and_then(Value::as_str),
        Some("function_call" | "custom_tool_call" | "web_search_call" | "tool_search_call")
    ) && payload.get("call_id").and_then(Value::as_str) == Some(origin_call_id)
}

fn is_structured_tool_result(record: &Value, result_call_id: &str) -> bool {
    let Some(record_type) = record.get("type").and_then(Value::as_str) else {
        return false;
    };
    if !matches!(record_type, "response_item" | "event_msg") {
        return false;
    }
    let Some(payload) = record.get("payload") else {
        return false;
    };
    let Some(item_type) = payload.get("type").and_then(Value::as_str) else {
        return false;
    };
    (item_type.ends_with("_output")
        || matches!(
            item_type,
            "patch_apply_end"
                | "web_search_end"
                | "exec_command_end"
                | "command_complete"
                | "tool_complete"
        ))
        && payload.get("call_id").and_then(Value::as_str) == Some(result_call_id)
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|candidate| candidate == needle)
}
