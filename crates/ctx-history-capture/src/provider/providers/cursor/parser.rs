use std::fmt;

use ctx_history_core::{ContentRef, EventRole, EventType};
use serde::de::{
    self, Deserialize, DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::Result;

const MAX_CURSOR_ATOM_CHARS: usize = 512;
const MAX_CURSOR_PATH_CHARS: usize = 4_096;
const MAX_CURSOR_INPUT_PATHS: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CursorRejectionKind {
    MalformedJson,
    UnsupportedShape,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CursorSafePart {
    BodyFree {
        event_type: EventType,
        role: EventRole,
    },
    Text {
        event_type: EventType,
        role: EventRole,
        text: String,
        complete_content_ref: Option<ContentRef>,
    },
    ToolUse {
        role: EventRole,
        call_id: Option<String>,
        tool_name: Option<String>,
        input_paths: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CursorSanitizedRecord {
    pub(super) semantic_ordinal: u64,
    pub(super) physical_ordinal: u64,
    pub(super) byte_start: u64,
    pub(super) byte_end_exclusive: u64,
    pub(super) record_sha256: [u8; 32],
    pub(super) timestamp: Option<String>,
    pub(super) parts: Vec<CursorSafePart>,
}

mod classification;

use classification::{
    classify_cursor_line, CursorBlockKind, CursorContentLocation, CursorLineClassification,
    CursorRecordAdmission,
};

pub(super) fn project_cursor_jsonl_record(
    bytes: &[u8],
    semantic_ordinal: u64,
    physical_ordinal: u64,
    byte_start: u64,
    byte_end_exclusive: u64,
) -> Result<Option<Vec<super::projection::CursorNativeEvent>>> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    let classification = match classify_cursor_line(bytes) {
        Ok(classification) => classification,
        Err(_) => return Ok(None),
    };
    let sanitized = match decode_sanitized_record(
        bytes,
        semantic_ordinal,
        physical_ordinal,
        byte_start,
        byte_end_exclusive,
        &classification,
    ) {
        Ok(record) => record,
        Err(_) => return Ok(None),
    };
    Ok(Some(super::projection::project_cursor_record(sanitized)?))
}

fn decode_sanitized_record(
    bytes: &[u8],
    semantic_ordinal: u64,
    physical_ordinal: u64,
    byte_start: u64,
    byte_end_exclusive: u64,
    classification: &CursorLineClassification,
) -> serde_json::Result<CursorSanitizedRecord> {
    let record_sha256 = Sha256::digest(bytes).into();
    if classification.admission == CursorRecordAdmission::Excluded {
        return Ok(CursorSanitizedRecord {
            semantic_ordinal,
            physical_ordinal,
            byte_start,
            byte_end_exclusive,
            record_sha256,
            timestamp: classification.timestamp.clone(),
            parts: Vec::new(),
        });
    }
    let has_retained_blocks = classification
        .block_kinds
        .iter()
        .any(|kind| matches!(kind, CursorBlockKind::Text | CursorBlockKind::ToolUse));
    let mut parts = if has_retained_blocks {
        let mut deserializer = serde_json::Deserializer::from_slice(bytes);
        let parts = CursorRetainedSeed { classification }.deserialize(&mut deserializer)?;
        deserializer.end()?;
        parts
    } else {
        Vec::new()
    };
    let role = cursor_role(classification.role.as_deref());
    if parts.is_empty() && classification.result_blocks == 0 {
        parts.push(CursorSafePart::BodyFree {
            event_type: cursor_text_event_type(classification),
            role,
        });
    }
    Ok(CursorSanitizedRecord {
        semantic_ordinal,
        physical_ordinal,
        byte_start,
        byte_end_exclusive,
        record_sha256,
        timestamp: classification.timestamp.clone(),
        parts,
    })
}

fn cursor_text_event_type(classification: &CursorLineClassification) -> EventType {
    match classification.admission {
        CursorRecordAdmission::UserMessage | CursorRecordAdmission::AssistantMessage => {
            EventType::Message
        }
        CursorRecordAdmission::TurnEndedSummary => EventType::Summary,
        CursorRecordAdmission::Excluded => EventType::Notice,
    }
}

fn cursor_role(role: Option<&str>) -> EventRole {
    match role {
        Some("user") => EventRole::User,
        Some("assistant") => EventRole::Assistant,
        Some("system") => EventRole::System,
        Some("tool") => EventRole::Tool,
        _ => EventRole::Unknown,
    }
}

struct CursorRetainedSeed<'a> {
    classification: &'a CursorLineClassification,
}

impl<'de> DeserializeSeed<'de> for CursorRetainedSeed<'_> {
    type Value = Vec<CursorSafePart>;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(CursorRetainedVisitor {
            classification: self.classification,
        })
    }
}

struct CursorRetainedVisitor<'a> {
    classification: &'a CursorLineClassification,
}

impl<'de> Visitor<'de> for CursorRetainedVisitor<'_> {
    type Value = Vec<CursorSafePart>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a classified Cursor transcript object")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut parts = Vec::new();
        while let Some(field) = map.next_key::<String>()? {
            match (field.as_str(), self.classification.location) {
                ("message", CursorContentLocation::Message) => {
                    parts = map.next_value_seed(CursorMessageRetainedSeed {
                        kinds: &self.classification.block_kinds,
                        classification: self.classification,
                    })?;
                }
                ("content", CursorContentLocation::TopLevel) => {
                    parts = map.next_value_seed(CursorContentRetainedSeed {
                        kinds: &self.classification.block_kinds,
                        classification: self.classification,
                    })?;
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(parts)
    }
}

struct CursorMessageRetainedSeed<'a> {
    kinds: &'a [CursorBlockKind],
    classification: &'a CursorLineClassification,
}

impl<'de> DeserializeSeed<'de> for CursorMessageRetainedSeed<'_> {
    type Value = Vec<CursorSafePart>;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(CursorMessageRetainedVisitor {
            kinds: self.kinds,
            classification: self.classification,
        })
    }
}

struct CursorMessageRetainedVisitor<'a> {
    kinds: &'a [CursorBlockKind],
    classification: &'a CursorLineClassification,
}

impl<'de> Visitor<'de> for CursorMessageRetainedVisitor<'_> {
    type Value = Vec<CursorSafePart>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Cursor message object")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut parts = Vec::new();
        while let Some(field) = map.next_key::<String>()? {
            if field == "content" {
                parts = map.next_value_seed(CursorContentRetainedSeed {
                    kinds: self.kinds,
                    classification: self.classification,
                })?;
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(parts)
    }
}

struct CursorContentRetainedSeed<'a> {
    kinds: &'a [CursorBlockKind],
    classification: &'a CursorLineClassification,
}

impl<'de> DeserializeSeed<'de> for CursorContentRetainedSeed<'_> {
    type Value = Vec<CursorSafePart>;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(CursorContentRetainedVisitor {
            kinds: self.kinds,
            classification: self.classification,
        })
    }
}

struct CursorContentRetainedVisitor<'a> {
    kinds: &'a [CursorBlockKind],
    classification: &'a CursorLineClassification,
}

impl<'de> Visitor<'de> for CursorContentRetainedVisitor<'_> {
    type Value = Vec<CursorSafePart>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the previously classified Cursor content array")
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut parts = Vec::new();
        for kind in self.kinds {
            let Some(part) = sequence.next_element_seed(CursorBlockRetainedSeed {
                kind: *kind,
                classification: self.classification,
            })?
            else {
                return Err(de::Error::custom(
                    "Cursor content changed between classification and decoding",
                ));
            };
            if let Some(part) = part {
                parts.push(part);
            }
        }
        while sequence.next_element::<IgnoredAny>()?.is_some() {}
        Ok(parts)
    }
}

struct CursorBlockRetainedSeed<'a> {
    kind: CursorBlockKind,
    classification: &'a CursorLineClassification,
}

impl<'de> DeserializeSeed<'de> for CursorBlockRetainedSeed<'_> {
    type Value = Option<CursorSafePart>;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        match self.kind {
            CursorBlockKind::Text => deserializer.deserialize_map(CursorTextBlockVisitor {
                classification: self.classification,
            }),
            CursorBlockKind::ToolUse => deserializer.deserialize_map(CursorToolUseBlockVisitor {
                classification: self.classification,
            }),
            CursorBlockKind::Excluded => {
                IgnoredAny::deserialize(deserializer)?;
                Ok(None)
            }
        }
    }
}

struct CursorTextBlockVisitor<'a> {
    classification: &'a CursorLineClassification,
}

impl<'de> Visitor<'de> for CursorTextBlockVisitor<'_> {
    type Value = Option<CursorSafePart>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Cursor text content block")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let event_type = cursor_text_event_type(self.classification);
        let mut retained = None;
        while let Some(field) = map.next_key::<String>()? {
            if field == "text" {
                retained = Some(map.next_value_seed(CursorMessageTextSeed { event_type })?);
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        let retained = retained.unwrap_or_else(|| retained_cursor_message_text(event_type, ""));
        Ok(Some(CursorSafePart::Text {
            event_type,
            role: cursor_role(self.classification.role.as_deref()),
            text: retained.text,
            complete_content_ref: retained.complete_content_ref,
        }))
    }
}

struct CursorMessageTextSeed {
    event_type: EventType,
}

impl<'de> DeserializeSeed<'de> for CursorMessageTextSeed {
    type Value = CursorRetainedMessageText;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_string(CursorMessageTextVisitor {
            event_type: self.event_type,
        })
    }
}

struct CursorMessageTextVisitor {
    event_type: EventType,
}

impl Visitor<'_> for CursorMessageTextVisitor {
    type Value = CursorRetainedMessageText;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Cursor message string")
    }

    fn visit_borrowed_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(retained_cursor_message_text(self.event_type, value))
    }

    fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(retained_cursor_message_text(self.event_type, value))
    }

    fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(retained_cursor_message_text(self.event_type, &value))
    }
}

struct CursorRetainedMessageText {
    text: String,
    complete_content_ref: Option<ContentRef>,
}

fn retained_cursor_message_text(event_type: EventType, value: &str) -> CursorRetainedMessageText {
    let complete_content_ref = (event_type == EventType::Message)
        .then(|| ContentRef::from_bytes(value.as_bytes()))
        .flatten();
    CursorRetainedMessageText {
        text: value.to_owned(),
        complete_content_ref,
    }
}

pub(crate) fn cursor_complete_content_message_record(
    value: &Value,
    physical_ordinal: u64,
    subrecord_index: u32,
    indexed_text: &str,
) -> Option<(String, String, String)> {
    let encoded = serde_json::to_vec(value).ok()?;
    let classification = classify_cursor_line(&encoded).ok()?;
    if !matches!(
        classification.admission,
        CursorRecordAdmission::UserMessage | CursorRecordAdmission::AssistantMessage
    ) {
        return None;
    }
    let content = match classification.location {
        CursorContentLocation::Message => value.get("message")?.get("content")?.as_array()?,
        CursorContentLocation::TopLevel => value.get("content")?.as_array()?,
        CursorContentLocation::None => return None,
    };
    let mut projected_ordinal = 0_u32;
    for (kind, block) in classification.block_kinds.iter().zip(content) {
        match kind {
            CursorBlockKind::Excluded => continue,
            CursorBlockKind::ToolUse => {
                projected_ordinal = projected_ordinal.checked_add(1)?;
            }
            CursorBlockKind::Text => {
                if projected_ordinal == subrecord_index {
                    let complete_text = block.get("text").and_then(Value::as_str)?;
                    let event_type = cursor_text_event_type(&classification);
                    let role = cursor_role(classification.role.as_deref());
                    if complete_text != indexed_text {
                        return None;
                    }
                    let body = super::projection::CursorEventBody::Text {
                        text: complete_text.to_owned(),
                    };
                    let encoded =
                        serde_json::to_vec(&("cursor-event-payload-v1", event_type, role, &body))
                            .ok()?;
                    return Some((
                        complete_text.to_owned(),
                        format!("cursor-line-v1:{physical_ordinal}:{subrecord_index}"),
                        format!("{:x}", Sha256::digest(encoded)),
                    ));
                }
                projected_ordinal = projected_ordinal.checked_add(1)?;
            }
        }
    }
    None
}

struct CursorToolUseBlockVisitor<'a> {
    classification: &'a CursorLineClassification,
}

impl<'de> Visitor<'de> for CursorToolUseBlockVisitor<'_> {
    type Value = Option<CursorSafePart>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Cursor tool_use content block")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut call_id = None;
        let mut tool_name = None;
        let mut input_paths = Vec::new();
        while let Some(field) = map.next_key::<String>()? {
            match field.as_str() {
                "id" => {
                    call_id = Some(map.next_value_seed(BoundedStringSeed {
                        max_chars: MAX_CURSOR_ATOM_CHARS,
                    })?);
                }
                "name" => {
                    tool_name = Some(map.next_value_seed(BoundedStringSeed {
                        max_chars: MAX_CURSOR_ATOM_CHARS,
                    })?);
                }
                "input" => {
                    input_paths = map.next_value_seed(CursorToolInputSeed)?;
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(Some(CursorSafePart::ToolUse {
            role: match cursor_role(self.classification.role.as_deref()) {
                EventRole::Unknown => EventRole::Assistant,
                role => role,
            },
            call_id,
            tool_name,
            input_paths,
        }))
    }
}

struct CursorToolInputSeed;

impl<'de> DeserializeSeed<'de> for CursorToolInputSeed {
    type Value = Vec<String>;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(CursorToolInputVisitor)
    }
}

struct CursorToolInputVisitor;

impl<'de> Visitor<'de> for CursorToolInputVisitor {
    type Value = Vec<String>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Cursor tool input object")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut paths = Vec::new();
        while let Some(field) = map.next_key::<String>()? {
            match field.as_str() {
                "path" | "file_path" | "filePath" if paths.len() < MAX_CURSOR_INPUT_PATHS => {
                    paths.push(map.next_value_seed(BoundedStringSeed {
                        max_chars: MAX_CURSOR_PATH_CHARS,
                    })?);
                }
                "paths" if paths.len() < MAX_CURSOR_INPUT_PATHS => {
                    let mut decoded = map.next_value_seed(CursorPathsSeed)?;
                    let remaining = MAX_CURSOR_INPUT_PATHS.saturating_sub(paths.len());
                    paths.extend(decoded.drain(..decoded.len().min(remaining)));
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(paths)
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Vec::new())
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Vec::new())
    }
}

struct CursorPathsSeed;

impl<'de> DeserializeSeed<'de> for CursorPathsSeed {
    type Value = Vec<String>;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(CursorPathsVisitor)
    }
}

struct CursorPathsVisitor;

impl<'de> Visitor<'de> for CursorPathsVisitor {
    type Value = Vec<String>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a bounded array of Cursor input paths")
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut paths = Vec::new();
        while paths.len() < MAX_CURSOR_INPUT_PATHS {
            let Some(path) = sequence.next_element_seed(BoundedStringSeed {
                max_chars: MAX_CURSOR_PATH_CHARS,
            })?
            else {
                return Ok(paths);
            };
            paths.push(path);
        }
        while sequence.next_element::<IgnoredAny>()?.is_some() {}
        Ok(paths)
    }
}

struct BoundedStringSeed {
    max_chars: usize,
}

impl<'de> DeserializeSeed<'de> for BoundedStringSeed {
    type Value = String;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_string(BoundedStringVisitor {
            max_chars: self.max_chars,
        })
    }
}

struct BoundedStringVisitor {
    max_chars: usize,
}

impl Visitor<'_> for BoundedStringVisitor {
    type Value = String;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a bounded string")
    }

    fn visit_borrowed_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(value.chars().take(self.max_chars).collect())
    }

    fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(value.chars().take(self.max_chars).collect())
    }

    fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        if value.chars().count() <= self.max_chars {
            Ok(value)
        } else {
            Ok(value.chars().take(self.max_chars).collect())
        }
    }
}
