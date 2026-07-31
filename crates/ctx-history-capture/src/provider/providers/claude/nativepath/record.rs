use std::fmt;

use serde::{
    de::{IgnoredAny, MapAccess, SeqAccess, Visitor},
    Deserialize, Deserializer,
};
use sha2::{Digest, Sha256};

use super::{
    privacy::{preflight_record, RawRecordPreflight, RawResultClassification},
    rows::{
        ClaudeEventIdentity, ClaudeEventKind, ClaudeFileTouch, ClaudeNativeOrder,
        ClaudeOutputOutcome, ClaudePhysicalLocator, ClaudeRetainedRow,
        ClaudeSparseOutputDiagnostic, ToolCallRequest, CLAUDE_MAX_FILE_TOUCHES_PER_RECORD,
    },
};
use crate::{OutputOutcome, OutputOutcomeMetadata};

mod value_hydration;

use value_hydration::sparse_output_rows;

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
    input: SafeToolInput,
}

#[derive(Debug, Default, Deserialize)]
struct SafeToolInput {
    #[serde(default)]
    path: Option<BoundedPath>,
    #[serde(rename = "file_path", alias = "filePath", default)]
    file_path: Option<BoundedPath>,
    #[serde(rename = "old_path", alias = "oldPath", default)]
    old_path: Option<BoundedPath>,
    #[serde(rename = "new_path", alias = "newPath", default)]
    new_path: Option<BoundedPath>,
    #[serde(default)]
    command: Option<BoundedPatch>,
    #[serde(default)]
    patch: Option<BoundedPatch>,
}

#[derive(Debug)]
struct BoundedPath(Option<String>);

impl BoundedPath {
    fn as_deref(&self) -> Option<&str> {
        self.0.as_deref()
    }
}

impl<'de> Deserialize<'de> for BoundedPath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(BoundedPathVisitor)
    }
}

struct BoundedPathVisitor;

impl<'de> Visitor<'de> for BoundedPathVisitor {
    type Value = BoundedPath;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a bounded Claude tool path")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(BoundedPath(
            (value.len() <= 4 * 1024).then(|| value.to_owned()),
        ))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(BoundedPath((value.len() <= 4 * 1024).then_some(value)))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(BoundedPath(None))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(BoundedPath(None))
    }

    fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E> {
        Ok(BoundedPath(None))
    }

    fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E> {
        Ok(BoundedPath(None))
    }

    fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E> {
        Ok(BoundedPath(None))
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E> {
        Ok(BoundedPath(None))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(BoundedPath(None))
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<IgnoredAny>()?.is_some() {}
        Ok(BoundedPath(None))
    }
}

#[derive(Debug)]
struct BoundedPatch(Option<String>);

impl<'de> Deserialize<'de> for BoundedPatch {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(BoundedPatchVisitor)
    }
}

struct BoundedPatchVisitor;

impl<'de> Visitor<'de> for BoundedPatchVisitor {
    type Value = BoundedPatch;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a bounded Claude patch command")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(BoundedPatch(
            (value.len() <= 64 * 1024).then(|| value.to_owned()),
        ))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(BoundedPatch((value.len() <= 64 * 1024).then_some(value)))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(BoundedPatch(None))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(BoundedPatch(None))
    }

    fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E> {
        Ok(BoundedPatch(None))
    }

    fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E> {
        Ok(BoundedPatch(None))
    }

    fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E> {
        Ok(BoundedPatch(None))
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E> {
        Ok(BoundedPatch(None))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(BoundedPatch(None))
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<IgnoredAny>()?.is_some() {}
        Ok(BoundedPatch(None))
    }
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
            complete_body_ref: None,
            tool_call: Some(call),
            sparse_output: None,
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
        complete_body_ref: None,
        tool_call: None,
        sparse_output: None,
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
                tool_name: block.name,
                file_touches: safe_file_touches(&block.input),
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

fn safe_file_touches(input: &SafeToolInput) -> Vec<ClaudeFileTouch> {
    let mut touches = Vec::new();
    let old_path = input.old_path.as_ref().and_then(BoundedPath::as_deref);
    if let Some(path) = input.new_path.as_ref().and_then(BoundedPath::as_deref) {
        push_touch(&mut touches, path, old_path);
    }
    for path in [&input.path, &input.file_path]
        .into_iter()
        .flatten()
        .filter_map(BoundedPath::as_deref)
    {
        push_touch(&mut touches, path, None);
    }
    for patch in [&input.command, &input.patch]
        .into_iter()
        .flatten()
        .filter_map(|value| value.0.as_deref())
    {
        extract_patch_touches(patch, &mut touches);
    }
    touches
}

fn push_touch(touches: &mut Vec<ClaudeFileTouch>, path: &str, previous_path: Option<&str>) {
    if touches.len() >= CLAUDE_MAX_FILE_TOUCHES_PER_RECORD
        || path.trim().is_empty()
        || touches
            .iter()
            .any(|touch| touch.path == path && touch.previous_path.as_deref() == previous_path)
    {
        return;
    }
    touches.push(ClaudeFileTouch {
        path: path.to_owned(),
        previous_path: previous_path.map(str::to_owned),
    });
}

fn extract_patch_touches(patch: &str, touches: &mut Vec<ClaudeFileTouch>) {
    let mut pending_old = None;
    for line in patch.lines() {
        if let Some(path) = line.strip_prefix("*** Move from: ") {
            pending_old = Some(path.trim());
        } else if let Some(path) = line.strip_prefix("*** Move to: ") {
            push_touch(touches, path.trim(), pending_old.take());
        } else if let Some(path) = line
            .strip_prefix("*** Update File: ")
            .or_else(|| line.strip_prefix("*** Add File: "))
            .or_else(|| line.strip_prefix("*** Delete File: "))
        {
            push_touch(touches, path.trim(), None);
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

/// Performs exactly one semantic JSON deserialization for a complete record.
///
/// The allocation-free raw inspection only selects the safe semantic shape.
/// Core-only result content is always visited as ignored JSON and therefore is
/// never decoded, hashed, previewed, touched, or retained.
pub(super) fn parse_native_record(
    bytes: &[u8],
    raw_ordinal: u64,
    locator: &ClaudePhysicalLocator,
) -> Result<ParsedClaudeRecord, serde_json::Error> {
    parse_native_record_inner(bytes, raw_ordinal, locator, false)
}

pub(super) fn parse_native_record_for_hydration(
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
        let rows = sparse_output_rows(
            raw_ordinal,
            locator,
            metadata.uuid.clone(),
            metadata.timestamp.clone(),
            0,
            &outputs,
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
