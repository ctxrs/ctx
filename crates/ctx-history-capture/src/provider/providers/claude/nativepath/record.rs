use std::fmt;

use ctx_history_core::RepositoryFileObservationKind;
use serde::{
    de::{IgnoredAny, MapAccess, SeqAccess, Visitor},
    Deserialize, Deserializer,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{
    privacy::{preflight_record, RawRecordPreflight, RawResultClassification},
    rows::{
        ClaudeEventIdentity, ClaudeEventKind, ClaudeFileTouch, ClaudeNativeOrder,
        ClaudeOutputOutcome, ClaudePhysicalLocator, ClaudeRetainedRow, ClaudeToolResult,
        ToolCallRequest, CLAUDE_MAX_FILE_TOUCHES_PER_RECORD,
    },
};
use crate::{OutputOutcome, OutputOutcomeMetadata};

mod value_decoding;

use value_decoding::complete_output_rows;

const CLAUDE_BODY_HASH_DOMAIN: &[u8] = b"ctx-claude-nativepath-body-v1\0";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct ResultClassification {
    pub(super) tagged_command_output: bool,
    pub(super) result_block: bool,
    pub(super) result_like_shape: bool,
    pub(super) top_level_result: bool,
}

impl ResultClassification {
    pub(super) fn is_result(self) -> bool {
        self.tagged_command_output || self.result_block || self.result_like_shape
    }
}

impl From<RawResultClassification> for ResultClassification {
    fn from(value: RawResultClassification) -> Self {
        Self {
            tagged_command_output: value.tagged_command_output,
            result_block: value.result_block,
            result_like_shape: value.result_like_shape,
            top_level_result: value.top_level_result,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct SafeRecord {
    #[serde(rename = "type", default)]
    entry_type: Option<String>,
    #[serde(default)]
    uuid: Option<String>,
    #[serde(rename = "sessionId", default)]
    session_id: Option<String>,
    #[serde(rename = "parentUuid", default)]
    parent_uuid: Option<String>,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(rename = "gitBranch", default)]
    git_branch: Option<String>,
    #[serde(default)]
    message: Option<SafeMessage>,
    #[serde(default)]
    content: SafeContent,
    #[serde(default)]
    summary: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct SafeMessage {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    content: SafeContent,
}

#[derive(Debug, Default)]
struct SafeContent {
    direct_text: Option<String>,
    blocks: Vec<SafeBlock>,
}

impl<'de> Deserialize<'de> for SafeContent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(SafeContentVisitor)
    }
}

struct SafeContentVisitor;

impl<'de> Visitor<'de> for SafeContentVisitor {
    type Value = SafeContent;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("safe Claude message content")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut blocks = Vec::new();
        while let Some(block) = sequence.next_element::<SafeBlock>()? {
            if blocks.len() > super::rows::CLAUDE_MAX_RECORD_ROWS {
                return Err(serde::de::Error::custom(
                    "Claude content exceeds the bounded block count",
                ));
            }
            blocks.push(block);
        }
        Ok(SafeContent {
            direct_text: None,
            blocks,
        })
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(SafeContent {
            direct_text: Some(value.to_owned()),
            blocks: Vec::new(),
        })
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(SafeContent {
            direct_text: Some(value),
            blocks: Vec::new(),
        })
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(SafeContent::default())
    }

    fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E> {
        Ok(SafeContent::default())
    }

    fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E> {
        Ok(SafeContent::default())
    }

    fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E> {
        Ok(SafeContent::default())
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E> {
        Ok(SafeContent::default())
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(SafeContent::default())
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(SafeContent::default())
    }
}

#[derive(Debug, Default, Deserialize)]
struct SafeBlock {
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    input: Value,
}

fn retain_safe_record(
    mut record: SafeRecord,
    raw_ordinal: u64,
    locator: &ClaudePhysicalLocator,
    output_policy: bool,
    include_source_displays: bool,
) -> (Vec<ClaudeRetainedRow>, Option<Vec<Option<String>>>) {
    let mut source_displays = include_source_displays.then(Vec::new);
    if output_policy {
        // Result-shaped records are output units, even when they contain
        // message-like siblings. Their only Core projection is the sparse
        // failure/timeout diagnostic built separately from shared preflight.
        return (Vec::new(), source_displays);
    }
    let entry_type = record
        .entry_type
        .take()
        .unwrap_or_else(|| "unknown".to_owned());
    let message = record.message.take();
    let native_record_id = record
        .uuid
        .take()
        .or_else(|| message.as_ref().and_then(|value| value.id.clone()));
    let role = message
        .as_ref()
        .and_then(|value| value.role.clone())
        .or(record.role.take());
    let content = message
        .map(|value| value.content)
        .unwrap_or_else(|| std::mem::take(&mut record.content));
    let (body, calls) = split_safe_content(content);
    let mut rows = Vec::new();

    let kind = match entry_type.as_str() {
        "user" | "assistant" => ClaudeEventKind::Message,
        "summary" | "compact_boundary" => ClaudeEventKind::Summary,
        _ => ClaudeEventKind::Notice,
    };
    let body = body
        .or(record.summary)
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            (calls.is_empty() && kind == ClaudeEventKind::Notice)
                .then(|| format!("Claude event: {entry_type}"))
        });
    if let Some(body) = body {
        if let Some(displays) = &mut source_displays {
            displays.push(Some(body.clone()));
        }
        push_body_row(
            &mut rows,
            raw_ordinal,
            locator,
            native_record_id.clone(),
            record.parent_uuid.clone(),
            kind,
            role.clone(),
            record.timestamp.clone(),
            body,
        );
    }

    for call in calls {
        let subrecord_index = rows.len() as u64;
        let identity = identity(raw_ordinal, subrecord_index);
        rows.push(ClaudeRetainedRow {
            identity,
            native_order: order(identity),
            native_record_id: native_record_id.clone(),
            parent_native_record_id: record.parent_uuid.clone(),
            kind: ClaudeEventKind::ToolCall,
            role: role.clone(),
            occurred_at: record.timestamp.clone(),
            body: None,
            body_sha256: None,
            body_text_retention: None,
            tool_call: Some(call),
            tool_result: None,
            locator: locator.clone(),
        });
        if let Some(displays) = &mut source_displays {
            displays.push(None);
        }
    }

    (rows, source_displays)
}

#[allow(clippy::too_many_arguments)]
fn push_body_row(
    rows: &mut Vec<ClaudeRetainedRow>,
    raw_ordinal: u64,
    locator: &ClaudePhysicalLocator,
    native_record_id: Option<String>,
    parent_native_record_id: Option<String>,
    kind: ClaudeEventKind,
    role: Option<String>,
    occurred_at: Option<String>,
    body: String,
) {
    let subrecord_index = rows.len() as u64;
    let identity = identity(raw_ordinal, subrecord_index);
    let body_sha256 = retained_body_hash(kind, role.as_deref(), &body);
    rows.push(ClaudeRetainedRow {
        identity,
        native_order: order(identity),
        native_record_id,
        parent_native_record_id,
        kind,
        role,
        occurred_at,
        body: Some(body),
        body_sha256: Some(body_sha256),
        body_text_retention: None,
        tool_call: None,
        tool_result: None,
        locator: locator.clone(),
    });
}

fn split_safe_content(content: SafeContent) -> (Option<String>, Vec<ToolCallRequest>) {
    let mut text_parts = Vec::new();
    if let Some(text) = content.direct_text.filter(|value| !value.trim().is_empty()) {
        text_parts.push(text);
    }
    let mut calls = Vec::new();
    for block in content.blocks {
        match block.kind.as_deref() {
            Some("tool_use") | Some("server_tool_use") => calls.push(ToolCallRequest {
                call_id: block.id,
                tool_name: block.name.clone(),
                command: bounded_input_string(&block.input, &["command"], 1024 * 1024),
                declared_workdir: bounded_input_string(
                    &block.input,
                    &["workdir", "cwd"],
                    16 * 1024,
                ),
                file_touches: safe_file_touches(&block.input, block.name.as_deref()),
                input: block.input,
            }),
            Some("text") => {
                if let Some(text) = block.text.filter(|value| !value.trim().is_empty()) {
                    text_parts.push(text);
                }
            }
            _ => {}
        }
    }
    let body = match text_parts.len() {
        0 => None,
        1 => text_parts.pop(),
        _ => Some(text_parts.join("\n")),
    };
    (body, calls)
}

fn bounded_input_string(input: &Value, fields: &[&str], limit: usize) -> Option<String> {
    fields
        .iter()
        .find_map(|field| input.get(*field).and_then(Value::as_str))
        .filter(|value| !value.trim().is_empty() && value.len() <= limit)
        .map(str::to_owned)
}

fn safe_file_touches(input: &Value, tool_name: Option<&str>) -> Vec<ClaudeFileTouch> {
    let mut touches = Vec::new();
    let old_path = bounded_input_string(input, &["old_path", "oldPath"], 16 * 1024);
    if let Some(path) = bounded_input_string(input, &["new_path", "newPath"], 16 * 1024) {
        push_touch(
            &mut touches,
            &path,
            old_path.as_deref(),
            RepositoryFileObservationKind::Renamed,
        );
    }
    for path in ["path", "file_path", "filePath"] {
        if let Some(path) = input.get(path).and_then(Value::as_str) {
            let kind = match tool_name.unwrap_or_default().to_ascii_lowercase().as_str() {
                "read" | "glob" | "grep" => RepositoryFileObservationKind::Read,
                "edit" => RepositoryFileObservationKind::Modified,
                "write" => RepositoryFileObservationKind::Unknown,
                _ => RepositoryFileObservationKind::Unknown,
            };
            push_touch(&mut touches, path, None, kind);
        }
    }
    if let Some(patch) = bounded_input_string(input, &["patch"], 64 * 1024) {
        extract_patch_touches(&patch, &mut touches);
    }
    touches
}

fn push_touch(
    touches: &mut Vec<ClaudeFileTouch>,
    path: &str,
    previous_path: Option<&str>,
    kind: RepositoryFileObservationKind,
) {
    if touches.len() >= CLAUDE_MAX_FILE_TOUCHES_PER_RECORD
        || path.trim().is_empty()
        || touches.iter().any(|touch| {
            touch.path == path
                && touch.previous_path.as_deref() == previous_path
                && touch.kind == kind
        })
    {
        return;
    }
    touches.push(ClaudeFileTouch {
        path: path.to_owned(),
        previous_path: previous_path.map(str::to_owned),
        kind,
    });
}

fn extract_patch_touches(patch: &str, touches: &mut Vec<ClaudeFileTouch>) {
    let mut pending_old = None;
    for line in patch.lines() {
        if let Some(path) = line.strip_prefix("*** Move from: ") {
            pending_old = Some(path.trim());
        } else if let Some(path) = line.strip_prefix("*** Move to: ") {
            push_touch(
                touches,
                path.trim(),
                pending_old.take(),
                RepositoryFileObservationKind::Renamed,
            );
        } else if let Some(path) = line.strip_prefix("*** Update File: ") {
            push_touch(
                touches,
                path.trim(),
                None,
                RepositoryFileObservationKind::Modified,
            );
        } else if let Some(path) = line.strip_prefix("*** Add File: ") {
            push_touch(
                touches,
                path.trim(),
                None,
                RepositoryFileObservationKind::Created,
            );
        } else if let Some(path) = line.strip_prefix("*** Delete File: ") {
            push_touch(
                touches,
                path.trim(),
                None,
                RepositoryFileObservationKind::Deleted,
            );
        }
    }
}

fn retained_body_hash(kind: ClaudeEventKind, role: Option<&str>, body: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(CLAUDE_BODY_HASH_DOMAIN);
    hasher.update([kind as u8]);
    update_length_prefixed(&mut hasher, role.unwrap_or_default().as_bytes());
    update_length_prefixed(&mut hasher, body.as_bytes());
    hasher.finalize().into()
}

fn update_length_prefixed(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(bytes);
}

fn identity(raw_ordinal: u64, subrecord_index: u64) -> ClaudeEventIdentity {
    ClaudeEventIdentity {
        source_record_ordinal: raw_ordinal,
        source_subrecord_index: subrecord_index,
    }
}

fn order(identity: ClaudeEventIdentity) -> ClaudeNativeOrder {
    ClaudeNativeOrder {
        source_record_ordinal: identity.source_record_ordinal,
        source_subrecord_index: identity.source_subrecord_index,
    }
}

#[derive(Debug)]
pub(super) struct ParsedClaudeRecord {
    pub(super) session_id: Option<String>,
    pub(super) timestamp: Option<String>,
    pub(super) cwd: Option<String>,
    pub(super) version: Option<String>,
    pub(super) git_branch: Option<String>,
    pub(super) rows: Vec<ClaudeRetainedRow>,
    pub(super) source_displays: Option<Vec<Option<String>>>,
}

#[derive(Debug)]
pub(super) struct ClaudeOutputDescriptor {
    pub(super) subrecord_index: u32,
    pub(super) call_id: Option<String>,
    pub(super) outcome: OutputOutcomeMetadata,
}

#[derive(Debug, Default, Deserialize)]
struct MetadataOnlyRecord {
    #[serde(default)]
    uuid: Option<String>,
    #[serde(rename = "sessionId", default)]
    session_id: Option<String>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(rename = "gitBranch", default)]
    git_branch: Option<String>,
}

/// The allocation-free raw inspection first selects and bounds the semantic
/// shape. Result records are then retained completely for direct Core output
/// and exact native call/result linkage.
pub(super) fn parse_native_record(
    bytes: &[u8],
    raw_ordinal: u64,
    locator: &ClaudePhysicalLocator,
) -> Result<ParsedClaudeRecord, serde_json::Error> {
    parse_native_record_inner(bytes, raw_ordinal, locator, false)
}

pub(super) fn parse_native_record_for_decoding(
    bytes: &[u8],
    raw_ordinal: u64,
    locator: &ClaudePhysicalLocator,
) -> Result<ParsedClaudeRecord, serde_json::Error> {
    parse_native_record_inner(bytes, raw_ordinal, locator, true)
}

fn parse_native_record_inner(
    bytes: &[u8],
    raw_ordinal: u64,
    locator: &ClaudePhysicalLocator,
    include_source_displays: bool,
) -> Result<ParsedClaudeRecord, serde_json::Error> {
    let preflight = preflight_record(bytes)?;
    let result = ResultClassification::from(preflight.result);
    let record_outcome = preflight.outcome.clone();

    if result.is_result() {
        let metadata: MetadataOnlyRecord = serde_json::from_slice(bytes)?;
        let outputs = output_descriptors(&preflight, bytes, &record_outcome);
        let value: Value = serde_json::from_slice(bytes)?;
        let rows = complete_output_rows(
            raw_ordinal,
            locator,
            metadata.uuid.clone(),
            metadata.timestamp.clone(),
            &outputs,
            &value,
        );
        let source_displays = include_source_displays.then(|| vec![None; rows.len()]);
        return Ok(ParsedClaudeRecord {
            session_id: metadata.session_id,
            timestamp: metadata.timestamp,
            cwd: metadata.cwd,
            version: metadata.version,
            git_branch: metadata.git_branch,
            rows,
            source_displays,
        });
    }

    let record: SafeRecord = serde_json::from_slice(bytes)?;
    let session_id = record.session_id.clone();
    let timestamp = record.timestamp.clone();
    let cwd = record.cwd.clone();
    let version = record.version.clone();
    let git_branch = record.git_branch.clone();
    let (rows, source_displays) = retain_safe_record(
        record,
        raw_ordinal,
        locator,
        result.is_result(),
        include_source_displays,
    );
    Ok(ParsedClaudeRecord {
        session_id,
        timestamp,
        cwd,
        version,
        git_branch,
        rows,
        source_displays,
    })
}

fn output_descriptors(
    preflight: &RawRecordPreflight,
    bytes: &[u8],
    record_outcome: &OutputOutcomeMetadata,
) -> Vec<ClaudeOutputDescriptor> {
    let mut outputs = preflight
        .output_descriptors()
        .iter()
        .enumerate()
        .map(|(index, descriptor)| ClaudeOutputDescriptor {
            subrecord_index: u32::try_from(index).unwrap_or(u32::MAX),
            call_id: descriptor.decode_call_id(bytes),
            outcome: record_outcome.clone(),
        })
        .collect::<Vec<_>>();
    if outputs.is_empty() {
        outputs.push(ClaudeOutputDescriptor {
            subrecord_index: 0,
            call_id: None,
            outcome: record_outcome.clone(),
        });
    }
    outputs
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const FIXTURE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/repository_attribution/claude-native.jsonl"
    ));

    fn locator() -> ClaudePhysicalLocator {
        ClaudePhysicalLocator {
            path: PathBuf::from("claude-native.jsonl"),
            byte_start: 0,
            byte_end_exclusive: 1,
            line_number: 1,
            record_sha256: [7; 32],
        }
    }

    #[test]
    fn native_tool_use_and_linked_result_are_retained_exactly() {
        let lines = FIXTURE.lines().collect::<Vec<_>>();
        let call = parse_native_record(lines[0].as_bytes(), 0, &locator()).unwrap();
        let call = call
            .rows
            .iter()
            .find_map(|row| row.tool_call.as_ref())
            .unwrap();
        assert_eq!(call.call_id.as_deref(), Some("toolu-1"));
        assert_eq!(call.tool_name.as_deref(), Some("Bash"));
        assert_eq!(call.declared_workdir.as_deref(), Some("/tmp/repository"));
        assert!(call
            .command
            .as_deref()
            .unwrap()
            .contains("git -C /tmp/repository commit"));
        assert_eq!(
            call.input.get("workdir").and_then(Value::as_str),
            Some("/tmp/repository")
        );

        let result = parse_native_record(lines[1].as_bytes(), 1, &locator()).unwrap();
        let result = result
            .rows
            .iter()
            .find_map(|row| row.tool_result.as_ref())
            .unwrap();
        assert_eq!(result.call_id.as_deref(), Some("toolu-1"));
        assert_eq!(result.outcome, ClaudeOutputOutcome::Success);
        assert_eq!(
            result
                .tool_use_result
                .as_ref()
                .and_then(|value| value.pointer("/gitOperation/commit/sha"))
                .and_then(Value::as_str),
            Some("abcdef1")
        );
    }
}
