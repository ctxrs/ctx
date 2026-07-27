use std::fmt;

use serde::{
    de::{self, DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor},
    Deserialize,
};

use super::{BoundedStringVisitor, CursorRejectionKind, MAX_CURSOR_ATOM_CHARS};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CursorContentLocation {
    Message,
    TopLevel,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CursorRecordAdmission {
    UserMessage,
    AssistantMessage,
    TurnEndedSummary,
    Excluded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CursorBlockKind {
    Text,
    ToolUse,
    Excluded,
}

#[derive(Debug)]
pub(super) struct CursorLineClassification {
    pub(super) timestamp: Option<String>,
    pub(super) role: Option<String>,
    pub(super) event: Option<String>,
    pub(super) record_type: Option<String>,
    pub(super) status: Option<String>,
    pub(super) location: CursorContentLocation,
    pub(super) admission: CursorRecordAdmission,
    pub(super) block_kinds: Vec<CursorBlockKind>,
    pub(super) result_blocks: u32,
}

pub(super) fn classify_cursor_line(
    bytes: &[u8],
) -> std::result::Result<CursorLineClassification, CursorRejectionKind> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let classified = CursorClassificationSeed
        .deserialize(&mut deserializer)
        .map_err(|_| CursorRejectionKind::MalformedJson)?;
    deserializer
        .end()
        .map_err(|_| CursorRejectionKind::MalformedJson)?;
    classified.ok_or(CursorRejectionKind::UnsupportedShape)
}

struct CursorClassificationSeed;

impl<'de> DeserializeSeed<'de> for CursorClassificationSeed {
    type Value = Option<CursorLineClassification>;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(CursorClassificationVisitor)
    }
}

struct CursorClassificationVisitor;

impl<'de> Visitor<'de> for CursorClassificationVisitor {
    type Value = Option<CursorLineClassification>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Cursor transcript object")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut timestamp = None;
        let mut role = None;
        let mut event = None;
        let mut record_type = None;
        let mut status = None;
        let mut message = None;
        let mut top_level = None;
        let mut role_seen = false;
        let mut event_seen = false;
        let mut type_seen = false;
        let mut status_seen = false;
        let mut message_seen = false;
        let mut top_level_seen = false;
        let mut shape_safe = true;
        while let Some(field) = map.next_key::<String>()? {
            match field.as_str() {
                "timestamp" => timestamp = Some(map.next_value::<BoundedAtom>()?.0),
                "role" => {
                    shape_safe &= !role_seen;
                    role_seen = true;
                    let value = map.next_value::<BoundedAtom>()?.0;
                    shape_safe &= matches!(value.as_str(), "user" | "assistant");
                    role = Some(value);
                }
                "event" => {
                    shape_safe &= !event_seen;
                    event_seen = true;
                    let value = map.next_value::<BoundedAtom>()?.0;
                    shape_safe &= matches!(value.as_str(), "turn_ended" | "summary");
                    event = Some(value);
                }
                "type" => {
                    shape_safe &= !type_seen;
                    type_seen = true;
                    let value = map.next_value::<BoundedAtom>()?.0;
                    shape_safe &= matches!(value.as_str(), "message" | "turn_ended" | "summary");
                    record_type = Some(value);
                }
                "status" => {
                    shape_safe &= !status_seen;
                    status_seen = true;
                    status = Some(map.next_value::<BoundedAtom>()?.0);
                }
                "message" => {
                    shape_safe &= !message_seen;
                    message_seen = true;
                    message = Some(map.next_value::<ClassifiedMessage>()?);
                }
                "content" => {
                    shape_safe &= !top_level_seen;
                    top_level_seen = true;
                    top_level = Some(map.next_value::<ClassifiedContent>()?);
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        shape_safe &= !(message_seen && top_level_seen);
        let (location, content, message_role) = if let Some(message) = message {
            (
                CursorContentLocation::Message,
                message.content,
                message.role,
            )
        } else if let Some(content) = top_level {
            (CursorContentLocation::TopLevel, content, None)
        } else {
            (
                CursorContentLocation::None,
                ClassifiedContent::default(),
                None,
            )
        };
        shape_safe &= content.valid;
        let admission = cursor_record_admission(
            role.as_deref(),
            message_role.as_deref(),
            event.as_deref(),
            record_type.as_deref(),
            status.as_deref(),
            location,
            shape_safe,
        );
        let role = role.or(message_role);
        let mut block_kinds = content.kinds;
        match admission {
            CursorRecordAdmission::AssistantMessage => {}
            CursorRecordAdmission::UserMessage | CursorRecordAdmission::TurnEndedSummary => {
                for kind in &mut block_kinds {
                    if *kind == CursorBlockKind::ToolUse {
                        *kind = CursorBlockKind::Excluded;
                    }
                }
            }
            CursorRecordAdmission::Excluded => block_kinds.fill(CursorBlockKind::Excluded),
        }
        let mut result_blocks = block_kinds
            .iter()
            .filter(|kind| **kind == CursorBlockKind::Excluded)
            .count() as u32;
        if admission == CursorRecordAdmission::Excluded && result_blocks == 0 {
            result_blocks = 1;
        }
        Ok(Some(CursorLineClassification {
            timestamp,
            role,
            event,
            record_type,
            status,
            location,
            admission,
            block_kinds,
            result_blocks,
        }))
    }

    fn visit_bool<E>(self, _value: bool) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(None)
    }

    fn visit_i64<E>(self, _value: i64) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(None)
    }

    fn visit_u64<E>(self, _value: u64) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(None)
    }

    fn visit_f64<E>(self, _value: f64) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(None)
    }

    fn visit_str<E>(self, _value: &str) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(None)
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(None)
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(None)
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<IgnoredAny>()?.is_some() {}
        Ok(None)
    }
}

#[allow(clippy::too_many_arguments)]
fn cursor_record_admission(
    role: Option<&str>,
    message_role: Option<&str>,
    event: Option<&str>,
    record_type: Option<&str>,
    status: Option<&str>,
    location: CursorContentLocation,
    shape_safe: bool,
) -> CursorRecordAdmission {
    if !shape_safe || location != CursorContentLocation::Message {
        return CursorRecordAdmission::Excluded;
    }
    if event.is_none()
        && matches!(record_type, None | Some("message"))
        && status.is_none()
        && role == Some("user")
        && message_role == Some("user")
    {
        return CursorRecordAdmission::UserMessage;
    }
    if event.is_none()
        && matches!(record_type, None | Some("message"))
        && status.is_none()
        && role == Some("assistant")
        && message_role == Some("assistant")
    {
        return CursorRecordAdmission::AssistantMessage;
    }
    let summary_kind = event.or(record_type);
    let summary_discriminators_match = summary_kind.is_some_and(|kind| {
        matches!(kind, "turn_ended" | "summary")
            && event.is_none_or(|value| value == kind)
            && record_type.is_none_or(|value| value == kind)
    });
    if summary_discriminators_match
        && role.is_none()
        && message_role.is_none()
        && matches!(status, None | Some("completed"))
    {
        return CursorRecordAdmission::TurnEndedSummary;
    }
    CursorRecordAdmission::Excluded
}

#[derive(Deserialize)]
struct BoundedAtom(#[serde(deserialize_with = "deserialize_cursor_atom")] String);

fn deserialize_cursor_atom<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    deserializer.deserialize_string(BoundedStringVisitor {
        max_chars: MAX_CURSOR_ATOM_CHARS,
    })
}

#[derive(Deserialize)]
struct ClassifiedMessage {
    #[serde(default, deserialize_with = "deserialize_optional_atom")]
    role: Option<String>,
    #[serde(default)]
    content: ClassifiedContent,
}

fn deserialize_optional_atom<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<BoundedAtom>::deserialize(deserializer).map(|value| value.map(|atom| atom.0))
}

#[derive(Debug, Default)]
struct ClassifiedContent {
    kinds: Vec<CursorBlockKind>,
    valid: bool,
}

impl<'de> Deserialize<'de> for ClassifiedContent {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(ClassifiedContentVisitor)
    }
}

struct ClassifiedContentVisitor;

impl<'de> Visitor<'de> for ClassifiedContentVisitor {
    type Value = ClassifiedContent;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Cursor content array")
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut kinds = Vec::new();
        while let Some(kind) = sequence.next_element::<ClassifiedBlock>()? {
            kinds.push(kind.0);
        }
        Ok(ClassifiedContent { kinds, valid: true })
    }

    fn visit_str<E>(self, _value: &str) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(ClassifiedContent::default())
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(ClassifiedContent::default())
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(ClassifiedContent::default())
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(ClassifiedContent::default())
    }
}

struct ClassifiedBlock(CursorBlockKind);

impl<'de> Deserialize<'de> for ClassifiedBlock {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(ClassifiedBlockVisitor)
    }
}

struct ClassifiedBlockVisitor;

impl<'de> Visitor<'de> for ClassifiedBlockVisitor {
    type Value = ClassifiedBlock;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Cursor content block")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut kind = None;
        let mut type_seen = false;
        while let Some(field) = map.next_key::<String>()? {
            if field == "type" {
                if type_seen {
                    map.next_value::<IgnoredAny>()?;
                    kind = Some(CursorBlockKind::Excluded);
                    continue;
                }
                type_seen = true;
                kind = Some(match map.next_value::<BoundedAtom>()?.0.as_str() {
                    "text" => CursorBlockKind::Text,
                    "tool_use" => CursorBlockKind::ToolUse,
                    _ => CursorBlockKind::Excluded,
                });
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(ClassifiedBlock(kind.unwrap_or(CursorBlockKind::Excluded)))
    }

    fn visit_str<E>(self, _value: &str) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(ClassifiedBlock(CursorBlockKind::Excluded))
    }
}
