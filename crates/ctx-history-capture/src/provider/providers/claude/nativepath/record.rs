use std::fmt;

use serde::{
    de::{IgnoredAny, MapAccess, SeqAccess, Visitor},
    Deserialize, Deserializer,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{
    privacy::{
        is_result_label, is_result_shape_label, preclassify_result, preflight_record,
        RawRecordPreflight, RawResultClassification,
    },
    rows::{
        ClaudeEventIdentity, ClaudeEventKind, ClaudeFileTouch, ClaudeNativeOrder,
        ClaudeOutputOutcome, ClaudePhysicalLocator, ClaudeRetainedRow,
        ClaudeSparseOutputDiagnostic, ToolCallRequest, CLAUDE_MAX_FILE_TOUCHES_PER_RECORD,
    },
};
use crate::{
    provider::normalization::provider_explicit_result_value_text, OutputOutcome,
    OutputOutcomeMetadata,
};

const CLAUDE_BODY_HASH_DOMAIN: &[u8] = b"ctx-claude-nativepath-body-v1\0";
const MAX_CLASSIFICATION_METADATA_BYTES: usize = 4 * 1024;

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

    fn merge(&mut self, other: Self) {
        self.tagged_command_output |= other.tagged_command_output;
        self.result_block |= other.result_block;
        self.result_like_shape |= other.result_like_shape;
        self.top_level_result |= other.top_level_result;
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

#[derive(Debug)]
pub(super) struct RecordClassification {
    pub(super) result: ResultClassification,
    pub(super) preallocation_exclusion: bool,
    pub(super) native_record_id: Option<String>,
    pub(super) session_id: Option<String>,
    pub(super) timestamp: Option<String>,
    pub(super) cwd: Option<String>,
    pub(super) version: Option<String>,
    pub(super) git_branch: Option<String>,
}

#[derive(Debug, Default)]
struct ClassificationRecord {
    result: ResultClassification,
    native_record_id: Option<String>,
    session_id: Option<String>,
    timestamp: Option<String>,
    cwd: Option<String>,
    version: Option<String>,
    git_branch: Option<String>,
}

impl<'de> Deserialize<'de> for ClassificationRecord {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(ClassificationRecordVisitor)
    }
}

struct ClassificationRecordVisitor;

impl<'de> Visitor<'de> for ClassificationRecordVisitor {
    type Value = ClassificationRecord;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Claude JSONL record")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut record = ClassificationRecord::default();
        while let Some(field) = map.next_key::<RecordField>()? {
            match field {
                RecordField::EntryType => {
                    let label = map.next_value::<ClassifiedLabel>()?;
                    record.result.merge(label.result);
                }
                RecordField::SessionId => {
                    record.session_id = map.next_value::<BoundedString>()?.0;
                }
                RecordField::Uuid => {
                    record.native_record_id = map.next_value::<BoundedString>()?.0;
                }
                RecordField::Timestamp => {
                    record.timestamp = map.next_value::<BoundedString>()?.0;
                }
                RecordField::Cwd => {
                    record.cwd = map.next_value::<BoundedString>()?.0;
                }
                RecordField::Version => {
                    record.version = map.next_value::<BoundedString>()?.0;
                }
                RecordField::GitBranch => {
                    record.git_branch = map.next_value::<BoundedString>()?.0;
                }
                RecordField::Message => {
                    map.next_value::<IgnoredAny>()?;
                }
                RecordField::Content | RecordField::Summary => {
                    map.next_value::<IgnoredAny>()?;
                }
                RecordField::ResultLike => {
                    record.result.result_like_shape = true;
                    map.next_value::<IgnoredAny>()?;
                }
                RecordField::Other => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(record)
    }
}

enum RecordField {
    EntryType,
    SessionId,
    Uuid,
    Timestamp,
    Cwd,
    Version,
    GitBranch,
    Message,
    Content,
    Summary,
    ResultLike,
    Other,
}

impl<'de> Deserialize<'de> for RecordField {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_identifier(RecordFieldVisitor)
    }
}

struct RecordFieldVisitor;

impl Visitor<'_> for RecordFieldVisitor {
    type Value = RecordField;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Claude record field")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(match value {
            "type" => RecordField::EntryType,
            "sessionId" => RecordField::SessionId,
            "uuid" => RecordField::Uuid,
            "timestamp" => RecordField::Timestamp,
            "cwd" => RecordField::Cwd,
            "version" => RecordField::Version,
            "gitBranch" => RecordField::GitBranch,
            "message" => RecordField::Message,
            "content" => RecordField::Content,
            "summary" => RecordField::Summary,
            _ if is_result_label(value) => RecordField::ResultLike,
            _ => RecordField::Other,
        })
    }
}

#[derive(Debug, Default)]
struct BoundedString(Option<String>);

impl<'de> Deserialize<'de> for BoundedString {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(BoundedStringVisitor)
    }
}

struct BoundedStringVisitor;

impl Visitor<'_> for BoundedStringVisitor {
    type Value = BoundedString;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a bounded Claude metadata string")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(BoundedString(
            (value.len() <= MAX_CLASSIFICATION_METADATA_BYTES).then(|| value.to_owned()),
        ))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(BoundedString(
            (value.len() <= MAX_CLASSIFICATION_METADATA_BYTES).then_some(value),
        ))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(BoundedString(None))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(BoundedString(None))
    }
}

#[derive(Debug, Default)]
struct ClassifiedLabel {
    result: ResultClassification,
}

impl<'de> Deserialize<'de> for ClassifiedLabel {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(ClassifiedLabelVisitor)
    }
}

struct ClassifiedLabelVisitor;

impl Visitor<'_> for ClassifiedLabelVisitor {
    type Value = ClassifiedLabel;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Claude type label")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(ClassifiedLabel {
            result: ResultClassification {
                result_block: is_result_label(value),
                ..ResultClassification::default()
            },
        })
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(ClassifiedLabel::default())
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(ClassifiedLabel::default())
    }
}

pub(super) fn classify_record(bytes: &[u8]) -> Result<RecordClassification, serde_json::Error> {
    if let Some(result) = preclassify_result(bytes)? {
        return Ok(RecordClassification {
            result: result.into(),
            preallocation_exclusion: true,
            native_record_id: None,
            session_id: None,
            timestamp: None,
            cwd: None,
            version: None,
            git_branch: None,
        });
    }
    let record: ClassificationRecord = serde_json::from_slice(bytes)?;
    Ok(RecordClassification {
        result: record.result,
        preallocation_exclusion: false,
        native_record_id: record.native_record_id,
        session_id: record.session_id,
        timestamp: record.timestamp,
        cwd: record.cwd,
        version: record.version,
        git_branch: record.git_branch,
    })
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

pub(super) fn retain_record(
    bytes: &[u8],
    raw_ordinal: u64,
    locator: &ClaudePhysicalLocator,
) -> Result<Vec<ClaudeRetainedRow>, serde_json::Error> {
    let record: SafeRecord = serde_json::from_slice(bytes)?;
    Ok(retain_safe_record(record, raw_ordinal, locator, false))
}

fn retain_safe_record(
    mut record: SafeRecord,
    raw_ordinal: u64,
    locator: &ClaudePhysicalLocator,
    output_policy: bool,
) -> Vec<ClaudeRetainedRow> {
    if output_policy {
        // Result-shaped records are output units, even when they contain
        // message-like siblings. Their only Core projection is the sparse
        // failure/timeout diagnostic built separately from shared preflight.
        return Vec::new();
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
            tool_call: Some(call),
            sparse_output: None,
            locator: locator.clone(),
        });
    }

    rows
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
    pub(super) result: ResultClassification,
    /// True only when Core classified the record before the body-bearing
    /// retention DTO could be deserialized.
    pub(super) preallocation_exclusion: bool,
    pub(super) native_record_id: Option<String>,
    pub(super) session_id: Option<String>,
    pub(super) timestamp: Option<String>,
    pub(super) cwd: Option<String>,
    pub(super) version: Option<String>,
    pub(super) git_branch: Option<String>,
    pub(super) rows: Vec<ClaudeRetainedRow>,
    pub(super) outputs: Vec<ParsedClaudeOutput>,
}

#[derive(Debug)]
pub(super) struct ParsedClaudeOutput {
    pub(super) subrecord_index: u32,
    pub(super) call_id: Option<String>,
    pub(super) outcome: OutputOutcomeMetadata,
    /// Present only for the Pro profile. Empty output is represented by an
    /// owned empty vector, while Core-only never owns this field.
    pub(super) content: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ClaudeRecordMode {
    CoreOnly,
    CoreAndPro,
    ProReplayOnly,
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
    #[serde(default)]
    message: Option<MetadataOnlyMessage>,
}

#[derive(Debug, Default, Deserialize)]
struct MetadataOnlyMessage {
    #[serde(default)]
    id: Option<String>,
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
    mode: ClaudeRecordMode,
) -> Result<ParsedClaudeRecord, serde_json::Error> {
    let preflight = preflight_record(bytes)?;
    let result = ResultClassification::from(preflight.result);
    let record_outcome = preflight.outcome.clone();
    if mode == ClaudeRecordMode::ProReplayOnly && !result.is_result() {
        let metadata: MetadataOnlyRecord = serde_json::from_slice(bytes)?;
        return Ok(ParsedClaudeRecord {
            result,
            preallocation_exclusion: false,
            native_record_id: metadata
                .uuid
                .or_else(|| metadata.message.and_then(|message| message.id)),
            session_id: metadata.session_id,
            timestamp: metadata.timestamp,
            cwd: metadata.cwd,
            version: metadata.version,
            git_branch: metadata.git_branch,
            rows: Vec::new(),
            outputs: Vec::new(),
        });
    }
    if matches!(
        mode,
        ClaudeRecordMode::CoreAndPro | ClaudeRecordMode::ProReplayOnly
    ) && result.is_result()
    {
        let core_outputs = if mode == ClaudeRecordMode::CoreAndPro {
            preflight_output_descriptors(&preflight, bytes, &record_outcome)
        } else {
            Vec::new()
        };
        let value: Value = serde_json::from_slice(bytes)?;
        let outputs = if preflight.output_descriptors().is_empty() {
            value_output_descriptors(&value, result, &record_outcome)
        } else {
            hydrate_preflight_output_descriptors(&preflight, bytes, &record_outcome)?
        };
        let mut parsed =
            parsed_from_value(&value, raw_ordinal, locator, result, &core_outputs, outputs);
        if mode == ClaudeRecordMode::ProReplayOnly {
            parsed.rows.clear();
        }
        return Ok(parsed);
    }

    if result.is_result() {
        let metadata: MetadataOnlyRecord = serde_json::from_slice(bytes)?;
        let mut outputs = preflight_output_descriptors(&preflight, bytes, &record_outcome);
        let rows = sparse_output_rows(
            raw_ordinal,
            locator,
            metadata.uuid.clone(),
            metadata.timestamp.clone(),
            0,
            &outputs,
        );
        debug_assert!(outputs.iter().all(|output| output.content.is_none()));
        return Ok(ParsedClaudeRecord {
            result,
            preallocation_exclusion: true,
            native_record_id: metadata
                .uuid
                .or_else(|| metadata.message.and_then(|message| message.id)),
            session_id: metadata.session_id,
            timestamp: metadata.timestamp,
            cwd: metadata.cwd,
            version: metadata.version,
            git_branch: metadata.git_branch,
            rows,
            outputs: std::mem::take(&mut outputs),
        });
    }

    let record: SafeRecord = serde_json::from_slice(bytes)?;
    let native_record_id = record.uuid.clone().or_else(|| {
        record
            .message
            .as_ref()
            .and_then(|message| message.id.clone())
    });
    let session_id = record.session_id.clone();
    let timestamp = record.timestamp.clone();
    let cwd = record.cwd.clone();
    let version = record.version.clone();
    let git_branch = record.git_branch.clone();
    let rows = retain_safe_record(record, raw_ordinal, locator, result.is_result());
    Ok(ParsedClaudeRecord {
        result,
        preallocation_exclusion: false,
        native_record_id,
        session_id,
        timestamp,
        cwd,
        version,
        git_branch,
        rows,
        outputs: Vec::new(),
    })
}

fn preflight_output_descriptors(
    preflight: &RawRecordPreflight,
    bytes: &[u8],
    record_outcome: &OutputOutcomeMetadata,
) -> Vec<ParsedClaudeOutput> {
    let mut outputs = preflight
        .output_descriptors()
        .iter()
        .enumerate()
        .map(|(index, descriptor)| ParsedClaudeOutput {
            subrecord_index: u32::try_from(index).unwrap_or(u32::MAX),
            call_id: descriptor.decode_call_id(bytes),
            outcome: record_outcome.clone(),
            content: None,
        })
        .collect::<Vec<_>>();
    if outputs.is_empty() {
        outputs.push(ParsedClaudeOutput {
            subrecord_index: 0,
            call_id: None,
            outcome: record_outcome.clone(),
            content: None,
        });
    }
    outputs
}

fn hydrate_preflight_output_descriptors(
    preflight: &RawRecordPreflight,
    bytes: &[u8],
    record_outcome: &OutputOutcomeMetadata,
) -> Result<Vec<ParsedClaudeOutput>, serde_json::Error> {
    preflight
        .output_descriptors()
        .iter()
        .enumerate()
        .map(|(index, descriptor)| {
            let content = match descriptor.value(bytes) {
                Some(raw) => {
                    provider_explicit_result_value_text(&serde_json::from_slice::<Value>(raw)?)
                        .unwrap_or_default()
                        .into_bytes()
                }
                None => Vec::new(),
            };
            Ok(ParsedClaudeOutput {
                subrecord_index: u32::try_from(index).unwrap_or(u32::MAX),
                call_id: descriptor.decode_call_id(bytes),
                outcome: record_outcome.clone(),
                content: Some(content),
            })
        })
        .collect()
}

fn parsed_from_value(
    value: &Value,
    raw_ordinal: u64,
    locator: &ClaudePhysicalLocator,
    result: ResultClassification,
    core_outputs: &[ParsedClaudeOutput],
    outputs: Vec<ParsedClaudeOutput>,
) -> ParsedClaudeRecord {
    let safe = safe_record_from_value(value, result);
    let native_record_id = safe
        .uuid
        .clone()
        .or_else(|| safe.message.as_ref().and_then(|message| message.id.clone()));
    let session_id = safe.session_id.clone();
    let timestamp = safe.timestamp.clone();
    let cwd = safe.cwd.clone();
    let version = safe.version.clone();
    let git_branch = safe.git_branch.clone();
    let mut rows = retain_safe_record(safe, raw_ordinal, locator, result.is_result());
    let sparse_base = u64::try_from(rows.len()).unwrap_or(u64::MAX);
    rows.extend(sparse_output_rows(
        raw_ordinal,
        locator,
        native_record_id.clone(),
        timestamp.clone(),
        sparse_base,
        core_outputs,
    ));
    ParsedClaudeRecord {
        result,
        preallocation_exclusion: false,
        native_record_id,
        session_id,
        timestamp,
        cwd,
        version,
        git_branch,
        rows,
        outputs,
    }
}

fn safe_record_from_value(value: &Value, result: ResultClassification) -> SafeRecord {
    let message_value = value.get("message");
    let message = message_value
        .and_then(Value::as_object)
        .map(|message| SafeMessage {
            id: bounded_value_string(message.get("id"), MAX_CLASSIFICATION_METADATA_BYTES),
            role: bounded_value_string(message.get("role"), MAX_CLASSIFICATION_METADATA_BYTES),
            content: safe_content_from_value(
                message.get("content"),
                !result.top_level_result
                    && !result.tagged_command_output
                    && (!result.result_like_shape || result.result_block),
            ),
        });
    SafeRecord {
        entry_type: bounded_value_string(value.get("type"), MAX_CLASSIFICATION_METADATA_BYTES),
        uuid: bounded_value_string(value.get("uuid"), MAX_CLASSIFICATION_METADATA_BYTES),
        session_id: bounded_value_string(value.get("sessionId"), MAX_CLASSIFICATION_METADATA_BYTES),
        parent_uuid: bounded_value_string(
            value.get("parentUuid"),
            MAX_CLASSIFICATION_METADATA_BYTES,
        ),
        role: bounded_value_string(value.get("role"), MAX_CLASSIFICATION_METADATA_BYTES),
        timestamp: bounded_value_string(value.get("timestamp"), MAX_CLASSIFICATION_METADATA_BYTES),
        cwd: bounded_value_string(value.get("cwd"), MAX_CLASSIFICATION_METADATA_BYTES),
        version: bounded_value_string(value.get("version"), MAX_CLASSIFICATION_METADATA_BYTES),
        git_branch: bounded_value_string(value.get("gitBranch"), MAX_CLASSIFICATION_METADATA_BYTES),
        message,
        content: safe_content_from_value(
            value.get("content"),
            !result.top_level_result
                && !result.tagged_command_output
                && (!result.result_like_shape || result.result_block),
        ),
        summary: bounded_value_string(value.get("summary"), 8 * 1024 * 1024),
    }
}

fn safe_content_from_value(value: Option<&Value>, retain_direct_text: bool) -> SafeContent {
    if !retain_direct_text {
        return SafeContent::default();
    }
    match value {
        Some(Value::String(text)) if retain_direct_text => SafeContent {
            direct_text: Some(text.clone()),
            blocks: Vec::new(),
        },
        Some(Value::Array(blocks)) => SafeContent {
            direct_text: None,
            blocks: blocks
                .iter()
                .take(super::rows::CLAUDE_MAX_RECORD_ROWS + 1)
                .filter_map(Value::as_object)
                .map(|block| SafeBlock {
                    kind: bounded_value_string(
                        block.get("type"),
                        MAX_CLASSIFICATION_METADATA_BYTES,
                    ),
                    text: match block.get("type").and_then(Value::as_str) {
                        Some("text") => bounded_value_string(block.get("text"), 8 * 1024 * 1024),
                        _ => None,
                    },
                    id: bounded_value_string(block.get("id"), MAX_CLASSIFICATION_METADATA_BYTES),
                    name: bounded_value_string(
                        block.get("name"),
                        MAX_CLASSIFICATION_METADATA_BYTES,
                    ),
                    input: safe_tool_input_from_value(block.get("input")),
                })
                .collect(),
        },
        _ => SafeContent::default(),
    }
}

fn safe_tool_input_from_value(value: Option<&Value>) -> SafeToolInput {
    let Some(object) = value.and_then(Value::as_object) else {
        return SafeToolInput::default();
    };
    SafeToolInput {
        path: bounded_path_from_value(object.get("path")),
        file_path: bounded_path_from_value(
            object.get("file_path").or_else(|| object.get("filePath")),
        ),
        old_path: bounded_path_from_value(object.get("old_path").or_else(|| object.get("oldPath"))),
        new_path: bounded_path_from_value(object.get("new_path").or_else(|| object.get("newPath"))),
        command: bounded_patch_from_value(object.get("command")),
        patch: bounded_patch_from_value(object.get("patch")),
    }
}

fn bounded_path_from_value(value: Option<&Value>) -> Option<BoundedPath> {
    Some(BoundedPath(Some(bounded_value_string(value, 4 * 1024)?)))
}

fn bounded_patch_from_value(value: Option<&Value>) -> Option<BoundedPatch> {
    bounded_value_string(value, 64 * 1024).map(|value| BoundedPatch(Some(value)))
}

fn bounded_value_string(value: Option<&Value>, max_bytes: usize) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|value| value.len() <= max_bytes)
        .map(str::to_owned)
}

fn value_output_descriptors(
    value: &Value,
    result: ResultClassification,
    record_outcome: &OutputOutcomeMetadata,
) -> Vec<ParsedClaudeOutput> {
    let message = value.get("message").unwrap_or(value);
    let top_result = value
        .get("toolUseResult")
        .or_else(|| message.get("toolUseResult"));
    let mut outputs = Vec::new();
    if let Some(blocks) = message.get("content").and_then(Value::as_array) {
        for block in blocks {
            if outputs.len() > super::rows::CLAUDE_MAX_RECORD_ROWS {
                break;
            }
            let Some(object) = block.as_object() else {
                continue;
            };
            let kind = object.get("type").and_then(Value::as_str);
            let is_result = kind.is_some_and(is_result_label)
                || object.keys().any(|key| is_result_shape_label(key));
            if is_result {
                let content = object
                    .get("content")
                    .or_else(|| object.get("text"))
                    .and_then(provider_explicit_result_value_text)
                    .unwrap_or_default()
                    .into_bytes();
                outputs.push(ParsedClaudeOutput {
                    subrecord_index: u32::try_from(outputs.len()).unwrap_or(u32::MAX),
                    call_id: bounded_value_string(
                        object
                            .get("tool_use_id")
                            .or_else(|| object.get("toolUseId")),
                        256,
                    ),
                    outcome: record_outcome.clone(),
                    content: Some(content),
                });
                if outputs.len() > super::rows::CLAUDE_MAX_RECORD_ROWS {
                    break;
                }
            }
            for (key, candidate) in object {
                if matches!(
                    key.as_str(),
                    "type"
                        | "content"
                        | "text"
                        | "tool_use_id"
                        | "toolUseId"
                        | "is_error"
                        | "isError"
                ) || !is_result_label(key)
                {
                    continue;
                }
                outputs.push(ParsedClaudeOutput {
                    subrecord_index: u32::try_from(outputs.len()).unwrap_or(u32::MAX),
                    call_id: None,
                    outcome: record_outcome.clone(),
                    content: Some(
                        provider_explicit_result_value_text(candidate)
                            .unwrap_or_default()
                            .into_bytes(),
                    ),
                });
                if outputs.len() > super::rows::CLAUDE_MAX_RECORD_ROWS {
                    break;
                }
            }
        }
    }
    if outputs.is_empty()
        && value
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(is_result_label)
    {
        let content = value
            .get("content")
            .or_else(|| value.get("output"))
            .or_else(|| value.get("result"))
            .and_then(provider_explicit_result_value_text)
            .unwrap_or_default()
            .into_bytes();
        outputs.push(ParsedClaudeOutput {
            subrecord_index: 0,
            call_id: bounded_value_string(
                value.get("tool_use_id").or_else(|| value.get("toolUseId")),
                256,
            ),
            outcome: record_outcome.clone(),
            content: Some(content),
        });
    }
    if outputs.is_empty() {
        if let Some(object) = value.as_object() {
            for (key, candidate) in object {
                if matches!(
                    key.as_str(),
                    "type"
                        | "content"
                        | "output"
                        | "result"
                        | "toolUseResult"
                        | "tool_use_id"
                        | "toolUseId"
                        | "is_error"
                        | "isError"
                ) || !is_result_label(key)
                {
                    continue;
                }
                outputs.push(ParsedClaudeOutput {
                    subrecord_index: u32::try_from(outputs.len()).unwrap_or(u32::MAX),
                    call_id: None,
                    outcome: record_outcome.clone(),
                    content: Some(
                        provider_explicit_result_value_text(candidate)
                            .unwrap_or_default()
                            .into_bytes(),
                    ),
                });
                if outputs.len() > super::rows::CLAUDE_MAX_RECORD_ROWS {
                    break;
                }
            }
        }
    }
    if outputs.is_empty() {
        if let Some(top_result) = top_result {
            outputs.push(ParsedClaudeOutput {
                subrecord_index: 0,
                call_id: None,
                outcome: record_outcome.clone(),
                content: Some(
                    tool_use_result_text(top_result)
                        .unwrap_or_default()
                        .into_bytes(),
                ),
            });
        } else if result.tagged_command_output {
            let content = message
                .get("content")
                .and_then(provider_explicit_result_value_text)
                .unwrap_or_default();
            outputs.push(ParsedClaudeOutput {
                subrecord_index: 0,
                call_id: None,
                outcome: record_outcome.clone(),
                content: Some(content.into_bytes()),
            });
        } else if result.is_result() {
            outputs.push(ParsedClaudeOutput {
                subrecord_index: 0,
                call_id: None,
                outcome: record_outcome.clone(),
                content: Some(Vec::new()),
            });
        }
    }
    outputs
}

fn tool_use_result_text(value: &Value) -> Option<String> {
    let Some(object) = value.as_object() else {
        return provider_explicit_result_value_text(value);
    };
    let streams = ["stdout", "stderr"]
        .into_iter()
        .filter_map(|key| {
            object
                .get(key)
                .and_then(provider_explicit_result_value_text)
        })
        .collect::<Vec<_>>();
    if !streams.is_empty() {
        return Some(streams.join("\n"));
    }
    ["output", "content", "result"].into_iter().find_map(|key| {
        object
            .get(key)
            .and_then(provider_explicit_result_value_text)
    })
}

fn sparse_output_rows(
    raw_ordinal: u64,
    locator: &ClaudePhysicalLocator,
    native_record_id: Option<String>,
    timestamp: Option<String>,
    core_subrecord_base: u64,
    outputs: &[ParsedClaudeOutput],
) -> Vec<ClaudeRetainedRow> {
    outputs
        .iter()
        .filter_map(|output| {
            let outcome = match output.outcome.outcome {
                OutputOutcome::Failure => ClaudeOutputOutcome::Failure,
                OutputOutcome::Timeout => ClaudeOutputOutcome::Timeout,
                OutputOutcome::Success | OutputOutcome::Unknown => return None,
            };
            let subrecord_index =
                core_subrecord_base.saturating_add(u64::from(output.subrecord_index));
            let identity = identity(raw_ordinal, subrecord_index);
            Some(ClaudeRetainedRow {
                identity,
                native_order: order(identity),
                native_record_id: native_record_id.clone(),
                parent_native_record_id: None,
                kind: ClaudeEventKind::ToolOutput,
                role: Some("tool".to_owned()),
                occurred_at: timestamp.clone(),
                body: None,
                body_sha256: None,
                tool_call: None,
                sparse_output: Some(ClaudeSparseOutputDiagnostic {
                    call_id: output.call_id.clone(),
                    outcome,
                    exit_code: output.outcome.exit_code,
                    duration_ms: output.outcome.duration_ms,
                }),
                locator: locator.clone(),
            })
        })
        .collect()
}
