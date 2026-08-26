use std::fmt;

use ctx_history_core::{EventRole, EventType};
use serde::de::{self, DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::Deserialize;
use serde_json::{value::RawValue, Value};
use sha2::{Digest, Sha256};

use crate::raw_json::{audit_json, SelectorGroup};
use ctx_history_provider_runtime::Result;

const MAX_CURSOR_CONTENT_BLOCKS: usize = u16::MAX as usize + 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CursorRejectionKind {
    MalformedJson,
    UnsupportedShape,
}

impl CursorRejectionKind {
    fn from_json_error(error: &serde_json::Error) -> Self {
        match error.classify() {
            serde_json::error::Category::Syntax | serde_json::error::Category::Eof => {
                Self::MalformedJson
            }
            serde_json::error::Category::Data | serde_json::error::Category::Io => {
                Self::UnsupportedShape
            }
        }
    }
}

pub(super) enum CursorJsonlRecordOutcome {
    Events(Vec<super::projection::CursorNativeEvent>),
    Ignored,
    Rejected(CursorRejectionKind, String),
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
        native_content: Value,
        call_id: Option<String>,
        tool_name: Option<String>,
        arguments: Option<Value>,
        protocol: Option<String>,
        server: Option<String>,
        explicit_tool: Option<String>,
        call_id_unavailable: bool,
        tool_name_unavailable: bool,
        arguments_unavailable: bool,
        mcp_identity_unavailable: bool,
        native_content_unavailable: bool,
        literal_facts: Vec<ctx_history_core::ProviderDeclaredFact>,
    },
    ToolResult {
        role: EventRole,
        native_content: Value,
        call_id: Option<String>,
        call_id_unavailable: bool,
        content_unavailable: bool,
        native_content_unavailable: bool,
        literal_facts: Vec<ctx_history_core::ProviderDeclaredFact>,
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

mod bounded_strings;
mod classification;

use bounded_strings::MAX_CURSOR_ATOM_BYTES;
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
    Ok(
        match project_cursor_jsonl_record_with_rejection(
            bytes,
            semantic_ordinal,
            physical_ordinal,
            byte_start,
            byte_end_exclusive,
        )? {
            CursorJsonlRecordOutcome::Events(events) => Some(events),
            CursorJsonlRecordOutcome::Ignored | CursorJsonlRecordOutcome::Rejected(_, _) => None,
        },
    )
}

pub(super) fn project_cursor_jsonl_record_with_rejection(
    bytes: &[u8],
    semantic_ordinal: u64,
    physical_ordinal: u64,
    byte_start: u64,
    byte_end_exclusive: u64,
) -> Result<CursorJsonlRecordOutcome> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(CursorJsonlRecordOutcome::Ignored);
    }
    let classification = match classify_cursor_line(bytes) {
        Ok(classification) => classification,
        Err(CursorRejectionKind::UnsupportedShape) => {
            return Ok(CursorJsonlRecordOutcome::Rejected(
                CursorRejectionKind::UnsupportedShape,
                "Cursor record has a well-formed but unsupported shape".to_owned(),
            ))
        }
        Err(CursorRejectionKind::MalformedJson) => {
            return Ok(CursorJsonlRecordOutcome::Rejected(
                CursorRejectionKind::MalformedJson,
                "Cursor record is malformed JSON".to_owned(),
            ))
        }
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
        Err(error) => {
            return Ok(CursorJsonlRecordOutcome::Rejected(
                CursorRejectionKind::from_json_error(&error),
                format!("Cursor record could not be decoded safely: {error}"),
            ))
        }
    };
    Ok(CursorJsonlRecordOutcome::Events(
        super::projection::project_cursor_record(sanitized)?,
    ))
}

fn decode_sanitized_record(
    bytes: &[u8],
    semantic_ordinal: u64,
    physical_ordinal: u64,
    byte_start: u64,
    byte_end_exclusive: u64,
    classification: &CursorLineClassification,
) -> serde_json::Result<CursorSanitizedRecord> {
    let record_audit = audit_json(
        bytes,
        cursor_record_selector_group,
        cursor_literal_kind_for_key,
    )?;
    let critical_selectors_unavailable = record_audit.selector_ambiguous(SelectorGroup::Invocation);
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
        let parts = CursorRetainedSeed {
            classification,
            critical_selectors_unavailable,
        }
        .deserialize(&mut deserializer)?;
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
    critical_selectors_unavailable: bool,
}

impl<'de> DeserializeSeed<'de> for CursorRetainedSeed<'_> {
    type Value = Vec<CursorSafePart>;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(CursorRetainedVisitor {
            classification: self.classification,
            critical_selectors_unavailable: self.critical_selectors_unavailable,
        })
    }
}

struct CursorRetainedVisitor<'a> {
    classification: &'a CursorLineClassification,
    critical_selectors_unavailable: bool,
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
                        critical_selectors_unavailable: self.critical_selectors_unavailable,
                    })?;
                }
                ("content", CursorContentLocation::TopLevel) => {
                    parts = map.next_value_seed(CursorContentRetainedSeed {
                        kinds: &self.classification.block_kinds,
                        classification: self.classification,
                        critical_selectors_unavailable: self.critical_selectors_unavailable,
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
    critical_selectors_unavailable: bool,
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
            critical_selectors_unavailable: self.critical_selectors_unavailable,
        })
    }
}

struct CursorMessageRetainedVisitor<'a> {
    kinds: &'a [CursorBlockKind],
    classification: &'a CursorLineClassification,
    critical_selectors_unavailable: bool,
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
                    critical_selectors_unavailable: self.critical_selectors_unavailable,
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
    critical_selectors_unavailable: bool,
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
            critical_selectors_unavailable: self.critical_selectors_unavailable,
        })
    }
}

struct CursorContentRetainedVisitor<'a> {
    kinds: &'a [CursorBlockKind],
    classification: &'a CursorLineClassification,
    critical_selectors_unavailable: bool,
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
                critical_selectors_unavailable: self.critical_selectors_unavailable,
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
    critical_selectors_unavailable: bool,
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
            CursorBlockKind::ToolUse => decode_tool_use_block(
                deserializer,
                self.classification,
                self.critical_selectors_unavailable,
            ),
            CursorBlockKind::ToolResult => decode_tool_result_block(
                deserializer,
                self.classification,
                self.critical_selectors_unavailable,
            ),
            CursorBlockKind::Excluded => {
                IgnoredAny::deserialize(deserializer)?;
                Ok(None)
            }
        }
    }
}

fn decode_tool_use_block<'de, D>(
    deserializer: D,
    classification: &CursorLineClassification,
    record_selectors_unavailable: bool,
) -> std::result::Result<Option<CursorSafePart>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = Box::<RawValue>::deserialize(deserializer)?;
    let audit = audit_json(
        raw.get().as_bytes(),
        cursor_selector_group,
        cursor_literal_kind_for_key,
    )
    .map_err(de::Error::custom)?;
    let native_content: Value = serde_json::from_str(raw.get()).map_err(de::Error::custom)?;
    let object = native_content
        .as_object()
        .ok_or_else(|| de::Error::custom("Cursor tool-use block must be an object"))?;
    let (call_id, call_id_invalid) = bounded_cursor_string(object.get("id"));
    let (tool_name, tool_name_invalid) = bounded_cursor_string(object.get("name"));
    let (protocol, protocol_invalid) = bounded_cursor_string(object.get("protocol"));
    let (server, server_invalid) = bounded_cursor_string(object.get("server"));
    let (explicit_tool, explicit_tool_invalid) = bounded_cursor_string(object.get("tool"));
    let call_id_unavailable = record_selectors_unavailable
        || audit.selector_ambiguous(SelectorGroup::CallId)
        || call_id_invalid;
    let tool_name_unavailable = record_selectors_unavailable
        || audit.selector_ambiguous(SelectorGroup::ToolName)
        || tool_name_invalid;
    let arguments_unavailable =
        record_selectors_unavailable || audit.selector_ambiguous(SelectorGroup::Arguments);
    let mcp_identity_unavailable = record_selectors_unavailable
        || audit.selector_ambiguous(SelectorGroup::Protocol)
        || audit.selector_ambiguous(SelectorGroup::Server)
        || audit.selector_ambiguous(SelectorGroup::McpTool)
        || protocol_invalid
        || server_invalid
        || explicit_tool_invalid;
    let arguments = (!arguments_unavailable)
        .then(|| {
            object
                .get("input")
                .or_else(|| object.get("arguments"))
                .cloned()
        })
        .flatten();
    Ok(Some(CursorSafePart::ToolUse {
        role: match cursor_role(classification.role.as_deref()) {
            EventRole::Unknown => EventRole::Assistant,
            role => role,
        },
        native_content,
        call_id: (!call_id_unavailable).then_some(call_id).flatten(),
        tool_name: (!tool_name_unavailable).then_some(tool_name).flatten(),
        arguments,
        protocol: (!mcp_identity_unavailable).then_some(protocol).flatten(),
        server: (!mcp_identity_unavailable).then_some(server).flatten(),
        explicit_tool: (!mcp_identity_unavailable)
            .then_some(explicit_tool)
            .flatten(),
        call_id_unavailable,
        tool_name_unavailable,
        arguments_unavailable,
        mcp_identity_unavailable,
        native_content_unavailable: record_selectors_unavailable
            || audit.any_selector_ambiguous()
            || call_id_invalid
            || tool_name_invalid
            || protocol_invalid
            || server_invalid
            || explicit_tool_invalid,
        literal_facts: audit.facts().to_vec(),
    }))
}

fn decode_tool_result_block<'de, D>(
    deserializer: D,
    classification: &CursorLineClassification,
    record_selectors_unavailable: bool,
) -> std::result::Result<Option<CursorSafePart>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = Box::<RawValue>::deserialize(deserializer)?;
    let audit = audit_json(
        raw.get().as_bytes(),
        cursor_selector_group,
        cursor_literal_kind_for_key,
    )
    .map_err(de::Error::custom)?;
    let native_content: Value = serde_json::from_str(raw.get()).map_err(de::Error::custom)?;
    let object = native_content
        .as_object()
        .ok_or_else(|| de::Error::custom("Cursor tool-result block must be an object"))?;
    let (call_id, call_id_invalid) = bounded_cursor_string(
        object
            .get("tool_use_id")
            .or_else(|| object.get("toolUseId"))
            .or_else(|| object.get("toolCallId")),
    );
    let call_id_unavailable = record_selectors_unavailable
        || audit.selector_ambiguous(SelectorGroup::CallId)
        || call_id_invalid;
    let content_unavailable =
        record_selectors_unavailable || audit.selector_ambiguous(SelectorGroup::Content);
    Ok(Some(CursorSafePart::ToolResult {
        role: match cursor_role(classification.role.as_deref()) {
            EventRole::Unknown | EventRole::User => EventRole::Tool,
            role => role,
        },
        native_content,
        call_id: (!call_id_unavailable).then_some(call_id).flatten(),
        call_id_unavailable,
        content_unavailable,
        native_content_unavailable: record_selectors_unavailable
            || audit.any_selector_ambiguous()
            || call_id_invalid,
        literal_facts: audit.facts().to_vec(),
    }))
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

fn bounded_cursor_string(value: Option<&Value>) -> (Option<String>, bool) {
    match value {
        None | Some(Value::Null) => (None, false),
        Some(Value::String(value)) if !value.is_empty() && value.len() <= MAX_CURSOR_ATOM_BYTES => {
            (Some(value.clone()), false)
        }
        Some(_) => (None, true),
    }
}

fn cursor_selector_group(key: &str) -> Option<SelectorGroup> {
    match key {
        "type" => Some(SelectorGroup::Type),
        "id" | "tool_use_id" | "toolUseId" | "toolCallId" => Some(SelectorGroup::CallId),
        "name" => Some(SelectorGroup::ToolName),
        "input" | "arguments" | "args" => Some(SelectorGroup::Arguments),
        "result" | "output" => Some(SelectorGroup::Result),
        "protocol" => Some(SelectorGroup::Protocol),
        "server" => Some(SelectorGroup::Server),
        "tool" => Some(SelectorGroup::McpTool),
        "content" => Some(SelectorGroup::Content),
        "message" => Some(SelectorGroup::Invocation),
        _ => None,
    }
}

fn cursor_record_selector_group(key: &str) -> Option<SelectorGroup> {
    (key == "message").then_some(SelectorGroup::Invocation)
}

fn cursor_literal_kind_for_key(key: &str) -> Option<ctx_history_core::LiteralFactKind> {
    use ctx_history_core::LiteralFactKind;
    match key {
        "cwd" | "workdir" | "working_directory" => Some(LiteralFactKind::ToolWorkdir),
        "file" | "file_path" | "filePath" | "path" | "paths" | "old_path" | "new_path" => {
            Some(LiteralFactKind::File)
        }
        "url" | "uri" | "repository_url" | "repositoryUrl" | "remote_url" => {
            Some(LiteralFactKind::Url)
        }
        "forge" | "forge_url" => Some(LiteralFactKind::Forge),
        "project" | "project_id" | "repository" | "repo" => Some(LiteralFactKind::Project),
        "vcs" | "git" => Some(LiteralFactKind::Vcs),
        "commit" | "commit_id" | "commit_sha" | "sha" => Some(LiteralFactKind::Commit),
        "pull_request" | "pullRequest" | "pr" | "pr_id" => Some(LiteralFactKind::PullRequest),
        "command" | "cmd" => Some(LiteralFactKind::Command),
        "branch" | "branch_name" => Some(LiteralFactKind::Branch),
        "workspace" | "workspace_id" => Some(LiteralFactKind::Workspace),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        project_cursor_jsonl_record, project_cursor_jsonl_record_with_rejection,
        CursorJsonlRecordOutcome, CursorRejectionKind, MAX_CURSOR_CONTENT_BLOCKS,
    };

    fn cursor_record_outcome(record: &[u8]) -> CursorJsonlRecordOutcome {
        project_cursor_jsonl_record_with_rejection(record, 0, 0, 0, record.len() as u64)
            .expect("Cursor record handling must remain record local")
    }

    fn assert_unsupported_shape(label: &str, record: &[u8]) {
        assert!(
            matches!(
                cursor_record_outcome(record),
                CursorJsonlRecordOutcome::Rejected(CursorRejectionKind::UnsupportedShape, _)
            ),
            "Cursor record was not rejected as an unsupported shape: {label}"
        );
    }

    #[test]
    fn cursor_distinguishes_malformed_json_from_unsupported_shapes() {
        for (record, expected) in [
            (b"{".as_slice(), CursorRejectionKind::MalformedJson),
            (b"[]".as_slice(), CursorRejectionKind::UnsupportedShape),
        ] {
            let outcome = cursor_record_outcome(record);
            assert!(matches!(
                outcome,
                CursorJsonlRecordOutcome::Rejected(kind, _) if kind == expected
            ));
        }
    }

    fn assert_message_retained(label: &str, role: &str, record: &[u8]) {
        let events = project_cursor_jsonl_record(record, 0, 0, 0, record.len() as u64)
            .unwrap()
            .unwrap_or_else(|| panic!("Cursor message was rejected: {label}"));
        assert_eq!(events.len(), 1, "unexpected Cursor event count: {label}");
        assert_eq!(
            events[0].role.as_str(),
            role,
            "top-level Cursor role was not authoritative: {label}"
        );
    }

    fn assert_no_events(label: &str, record: &[u8]) {
        let event_count = project_cursor_jsonl_record(record, 0, 0, 0, record.len() as u64)
            .unwrap()
            .map_or(0, |events| events.len());
        assert_eq!(
            event_count, 0,
            "invalid Cursor record emitted events: {label}"
        );
    }

    #[test]
    fn cursor_messages_without_a_nested_message_role_are_retained() {
        // Cursor agent transcripts carry the role only at the top level; the
        // message object holds content alone.
        for role in ["user", "assistant"] {
            let record = format!(
                r#"{{"role":"{role}","message":{{"content":[{{"type":"text","text":"retained"}}]}}}}"#
            );
            assert_message_retained(role, role, record.as_bytes());
        }
    }

    #[test]
    fn cursor_messages_with_matching_legacy_nested_roles_are_retained() {
        for role in ["user", "assistant"] {
            let record = format!(
                r#"{{"role":"{role}","message":{{"role":"{role}","content":[{{"type":"text","text":"retained"}}]}}}}"#
            );
            assert_message_retained(role, role, record.as_bytes());
        }
    }

    #[test]
    fn cursor_malformed_nested_roles_are_record_local_unsupported_shapes() {
        for (label, record) in [
            (
                "disagreeing nested role",
                br#"{"role":"user","message":{"role":"assistant","content":[{"type":"text","text":"MUST_NOT_EMIT"}]}}"#.as_slice(),
            ),
            (
                "explicit null nested role",
                br#"{"role":"user","message":{"role":null,"content":[{"type":"text","text":"MUST_NOT_EMIT"}]}}"#.as_slice(),
            ),
            (
                "non-string nested role",
                br#"{"role":"assistant","message":{"role":{"value":"assistant"},"content":[{"type":"text","text":"MUST_NOT_EMIT"}]}}"#.as_slice(),
            ),
        ] {
            assert_unsupported_shape(label, record);
        }
    }

    #[test]
    fn cursor_messages_require_a_valid_top_level_role() {
        for (label, record) in [
            (
                "missing top-level role",
                br#"{"message":{"content":[{"type":"text","text":"MUST_NOT_EMIT"}]}}"#.as_slice(),
            ),
            (
                "null top-level role",
                br#"{"role":null,"message":{"content":[{"type":"text","text":"MUST_NOT_EMIT"}]}}"#.as_slice(),
            ),
            (
                "unsupported top-level role",
                br#"{"role":"system","message":{"content":[{"type":"text","text":"MUST_NOT_EMIT"}]}}"#.as_slice(),
            ),
        ] {
            assert_no_events(label, record);
        }
    }

    #[test]
    fn cursor_malformed_or_ambiguous_content_is_rejected() {
        for (label, record) in [
            (
                "null message content",
                br#"{"role":"user","message":{"content":null}}"#.as_slice(),
            ),
            (
                "malformed content block",
                br#"{"role":"assistant","message":{"content":[false]}}"#.as_slice(),
            ),
            (
                "top-level and nested content",
                br#"{"role":"user","message":{"content":[{"type":"text","text":"MUST_NOT_EMIT"}]},"content":[{"type":"text","text":"MUST_NOT_EMIT"}]}"#.as_slice(),
            ),
        ] {
            assert_no_events(label, record);
        }
    }

    fn cursor_record_with_content_blocks(block_count: usize) -> Vec<u8> {
        let mut record = br#"{"role":"assistant","message":{"content":["#.to_vec();
        for index in 0..block_count {
            if index != 0 {
                record.push(b',');
            }
            record.extend_from_slice(br#"{"type":"text","text":""}"#);
        }
        record.extend_from_slice(br#"]}}"#);
        record
    }

    #[test]
    fn cursor_content_block_bound_is_record_local() {
        let at_bound = cursor_record_with_content_blocks(MAX_CURSOR_CONTENT_BLOCKS);
        let events = match cursor_record_outcome(&at_bound) {
            CursorJsonlRecordOutcome::Events(events) => events,
            CursorJsonlRecordOutcome::Ignored | CursorJsonlRecordOutcome::Rejected(_, _) => {
                panic!("Cursor rejected {MAX_CURSOR_CONTENT_BLOCKS} content blocks")
            }
        };
        assert_eq!(events.len(), MAX_CURSOR_CONTENT_BLOCKS);
        assert_eq!(
            events.last().map(|event| event.native_order.part_ordinal),
            Some(u32::from(u16::MAX))
        );

        let over_bound = cursor_record_with_content_blocks(MAX_CURSOR_CONTENT_BLOCKS + 1);
        assert_unsupported_shape("65,537 content blocks", &over_bound);
    }

    #[test]
    fn cursor_conflicting_duplicate_critical_selectors_are_rejected_before_retention() {
        let baseline = br#"{"type":"message","role":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"baseline"}]}}"#;
        assert!(
            project_cursor_jsonl_record(baseline, 0, 0, 0, baseline.len() as u64)
                .unwrap()
                .is_some()
        );

        for (label, record) in [
            (
                "message",
                br#"{"type":"message","role":"assistant","message":{"role":"user","content":[{"type":"text","text":"first"}]},"message":{"role":"assistant","content":[{"type":"text","text":"MUST_NOT_EMIT"}]}}"#.as_slice(),
            ),
            (
                "role",
                br#"{"type":"message","role":"user","role":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"MUST_NOT_EMIT"}]}}"#.as_slice(),
            ),
            (
                "type",
                br#"{"type":"summary","type":"message","role":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"MUST_NOT_EMIT"}]}}"#.as_slice(),
            ),
            (
                "event",
                br#"{"event":"turn_ended","event":"summary","message":{"content":[{"type":"text","text":"MUST_NOT_EMIT"}]}}"#.as_slice(),
            ),
            (
                "status",
                br#"{"event":"summary","status":"running","status":"completed","message":{"content":[{"type":"text","text":"MUST_NOT_EMIT"}]}}"#.as_slice(),
            ),
            (
                "message.role",
                br#"{"type":"message","role":"assistant","message":{"role":"user","role":"assistant","content":[{"type":"text","text":"MUST_NOT_EMIT"}]}}"#.as_slice(),
            ),
            (
                "content block type",
                br#"{"type":"message","role":"assistant","message":{"role":"assistant","content":[{"type":"tool_result","type":"text","text":"MUST_NOT_EMIT"}]}}"#.as_slice(),
            ),
        ] {
            assert_unsupported_shape(label, record);
        }
    }

    #[test]
    fn cursor_identical_duplicate_critical_selectors_are_also_rejected() {
        for (label, record) in [
            (
                "message",
                br#"{"type":"message","role":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"MUST_NOT_EMIT"}]},"message":{"role":"assistant","content":[{"type":"text","text":"MUST_NOT_EMIT"}]}}"#.as_slice(),
            ),
            (
                "role",
                br#"{"type":"message","role":"assistant","role":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"MUST_NOT_EMIT"}]}}"#.as_slice(),
            ),
            (
                "type",
                br#"{"type":"message","type":"message","role":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"MUST_NOT_EMIT"}]}}"#.as_slice(),
            ),
            (
                "event",
                br#"{"event":"summary","event":"summary","message":{"content":[{"type":"text","text":"MUST_NOT_EMIT"}]}}"#.as_slice(),
            ),
            (
                "status",
                br#"{"event":"summary","status":"completed","status":"completed","message":{"content":[{"type":"text","text":"MUST_NOT_EMIT"}]}}"#.as_slice(),
            ),
            (
                "message.role",
                br#"{"type":"message","role":"assistant","message":{"role":"assistant","role":"assistant","content":[{"type":"text","text":"MUST_NOT_EMIT"}]}}"#.as_slice(),
            ),
            (
                "content block type",
                br#"{"type":"message","role":"assistant","message":{"role":"assistant","content":[{"type":"text","type":"text","text":"MUST_NOT_EMIT"}]}}"#.as_slice(),
            ),
        ] {
            assert_unsupported_shape(label, record);
        }
    }
}
