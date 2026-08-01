use std::fmt;

use ctx_history_core::{EventRole, EventType};
use serde::de::{
    self, Deserialize, DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor,
};
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
    },
    ToolUse {
        role: EventRole,
        call_id: Option<String>,
        tool_name: Option<String>,
        command: Option<String>,
        declared_workdir: Option<String>,
        input_paths: Vec<String>,
        ambiguous_native_fields: bool,
    },
    ToolResult {
        role: EventRole,
        call_id: Option<String>,
        ambiguous_linkage: bool,
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
    let has_retained_blocks = classification.block_kinds.iter().any(|kind| {
        matches!(
            kind,
            CursorBlockKind::Text | CursorBlockKind::ToolUse | CursorBlockKind::ToolResult
        )
    });
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
            CursorBlockKind::ToolResult => {
                deserializer.deserialize_map(CursorToolResultBlockVisitor {
                    classification: self.classification,
                })
            }
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
                retained = Some(map.next_value_seed(CursorMessageTextSeed)?);
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        let text = retained.unwrap_or_default();
        Ok(Some(CursorSafePart::Text {
            event_type,
            role: cursor_role(self.classification.role.as_deref()),
            text,
        }))
    }
}

struct CursorMessageTextSeed;

impl<'de> DeserializeSeed<'de> for CursorMessageTextSeed {
    type Value = String;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_string(CursorMessageTextVisitor)
    }
}

struct CursorMessageTextVisitor;

impl Visitor<'_> for CursorMessageTextVisitor {
    type Value = String;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Cursor message string")
    }

    fn visit_borrowed_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(value.to_owned())
    }

    fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(value.to_owned())
    }

    fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(value)
    }
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
        let mut call_id_seen = false;
        let mut tool_name_seen = false;
        let mut ambiguous_native_fields = false;
        let mut input = CursorToolInput::default();
        while let Some(field) = map.next_key::<String>()? {
            match field.as_str() {
                "id" => {
                    let value = map.next_value_seed(ExactBoundedStringSeed {
                        max_chars: MAX_CURSOR_ATOM_CHARS,
                    })?;
                    ambiguous_native_fields |= call_id_seen || value.is_none();
                    call_id_seen = true;
                    call_id = value;
                }
                "name" => {
                    let value = map.next_value_seed(ExactBoundedStringSeed {
                        max_chars: MAX_CURSOR_ATOM_CHARS,
                    })?;
                    ambiguous_native_fields |= tool_name_seen || value.is_none();
                    tool_name_seen = true;
                    tool_name = value;
                }
                "input" => {
                    let decoded = map.next_value_seed(CursorToolInputSeed)?;
                    ambiguous_native_fields |= input.merge(decoded);
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
            command: input.command,
            declared_workdir: input.declared_workdir,
            input_paths: input.paths,
            ambiguous_native_fields,
        }))
    }
}

struct CursorToolResultBlockVisitor<'a> {
    classification: &'a CursorLineClassification,
}

impl<'de> Visitor<'de> for CursorToolResultBlockVisitor<'_> {
    type Value = Option<CursorSafePart>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Cursor tool_result content block")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut call_id = None;
        let mut call_id_seen = false;
        let mut ambiguous_linkage = false;
        while let Some(field) = map.next_key::<String>()? {
            if field == "tool_use_id" {
                let value = map.next_value_seed(ExactBoundedStringSeed {
                    max_chars: MAX_CURSOR_ATOM_CHARS,
                })?;
                ambiguous_linkage |= call_id_seen || value.is_none();
                call_id_seen = true;
                call_id = value;
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(Some(CursorSafePart::ToolResult {
            role: match cursor_role(self.classification.role.as_deref()) {
                EventRole::Unknown | EventRole::User => EventRole::Tool,
                role => role,
            },
            call_id,
            ambiguous_linkage,
        }))
    }
}

#[derive(Debug, Default)]
struct CursorToolInput {
    command: Option<String>,
    declared_workdir: Option<String>,
    paths: Vec<String>,
    command_seen: bool,
    workdir_seen: bool,
    invalid_field: bool,
}

impl CursorToolInput {
    fn merge(&mut self, incoming: Self) -> bool {
        let mut ambiguous = self.invalid_field || incoming.invalid_field;
        if incoming.command_seen {
            ambiguous |= self.command_seen || incoming.command.is_none();
            self.command_seen = true;
            self.command = incoming.command;
        }
        if incoming.workdir_seen {
            ambiguous |= self.workdir_seen || incoming.declared_workdir.is_none();
            self.workdir_seen = true;
            self.declared_workdir = incoming.declared_workdir;
        }
        self.paths.extend(incoming.paths);
        self.invalid_field |= incoming.invalid_field;
        ambiguous
    }
}

struct CursorToolInputSeed;

impl<'de> DeserializeSeed<'de> for CursorToolInputSeed {
    type Value = CursorToolInput;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(CursorToolInputVisitor)
    }
}

struct CursorToolInputVisitor;

impl<'de> Visitor<'de> for CursorToolInputVisitor {
    type Value = CursorToolInput;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Cursor tool input object")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut input = CursorToolInput::default();
        while let Some(field) = map.next_key::<String>()? {
            match field.as_str() {
                "command" => {
                    let value = map.next_value_seed(ExactBoundedStringSeed {
                        max_chars: MAX_CURSOR_PATH_CHARS,
                    })?;
                    input.invalid_field |= input.command_seen || value.is_none();
                    input.command_seen = true;
                    input.command = value;
                }
                "workdir" => {
                    let value = map.next_value_seed(ExactBoundedStringSeed {
                        max_chars: MAX_CURSOR_PATH_CHARS,
                    })?;
                    input.invalid_field |= input.workdir_seen || value.is_none();
                    input.workdir_seen = true;
                    input.declared_workdir = value;
                }
                "path" | "file_path" | "filePath" if input.paths.len() < MAX_CURSOR_INPUT_PATHS => {
                    match map.next_value_seed(ExactBoundedStringSeed {
                        max_chars: MAX_CURSOR_PATH_CHARS,
                    })? {
                        Some(path) => input.paths.push(path),
                        None => input.invalid_field = true,
                    }
                }
                "paths" if input.paths.len() < MAX_CURSOR_INPUT_PATHS => {
                    let mut decoded = map.next_value_seed(CursorPathsSeed)?;
                    let remaining = MAX_CURSOR_INPUT_PATHS.saturating_sub(input.paths.len());
                    input
                        .paths
                        .extend(decoded.drain(..decoded.len().min(remaining)));
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(input)
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(CursorToolInput::default())
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(CursorToolInput::default())
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
            let Some(path) = sequence.next_element_seed(ExactBoundedStringSeed {
                max_chars: MAX_CURSOR_PATH_CHARS,
            })?
            else {
                return Ok(paths);
            };
            if let Some(path) = path {
                paths.push(path);
            }
        }
        while sequence.next_element::<IgnoredAny>()?.is_some() {}
        Ok(paths)
    }
}

struct BoundedStringSeed {
    max_chars: usize,
}

struct ExactBoundedStringSeed {
    max_chars: usize,
}

impl<'de> DeserializeSeed<'de> for ExactBoundedStringSeed {
    type Value = Option<String>;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok((value.chars().count() <= self.max_chars).then_some(value))
    }
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
