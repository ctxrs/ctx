use std::fmt;

use ctx_history_core::MAX_CORE_CONTENT_BYTES;
use serde::{
    de::{IgnoredAny, MapAccess, SeqAccess, Visitor},
    Deserialize, Deserializer,
};
use serde_json::value::RawValue;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{
    privacy::{preflight_record, RawRecordPreflight},
    rows::{
        ClaudeEventIdentity, ClaudeEventKind, ClaudeNativeOrder, ClaudePhysicalLocator,
        ClaudeRetainedRow, ClaudeToolResult, ToolCallRequest,
    },
};
use crate::raw_json::{audit_json, SelectorGroup};

mod ownership;
mod value_decoding;

use ownership::ClaudeRecordOwnership;

use value_decoding::complete_output_rows;

const CLAUDE_BODY_HASH_DOMAIN: &[u8] = b"ctx-claude-nativepath-body-v1\0";

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
    body: Option<String>,
    calls: Vec<ToolCallRequest>,
    saw_private_thinking: bool,
    saw_other_block: bool,
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
        let mut content = SafeContent::default();
        while let Some(raw) = sequence.next_element::<Box<RawValue>>()? {
            let block = decode_safe_block(&raw).map_err(serde::de::Error::custom)?;
            content
                .push_block(block)
                .map_err(serde::de::Error::custom)?;
        }
        Ok(content)
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        if value.len() > MAX_CORE_CONTENT_BYTES {
            return Err(serde::de::Error::custom(
                "Claude message body exceeds the Core content limit",
            ));
        }
        Ok(SafeContent {
            body: (!value.trim().is_empty()).then(|| value.to_owned()),
            calls: Vec::new(),
            ..SafeContent::default()
        })
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        if value.len() > MAX_CORE_CONTENT_BYTES {
            return Err(serde::de::Error::custom(
                "Claude message body exceeds the Core content limit",
            ));
        }
        Ok(SafeContent {
            body: (!value.trim().is_empty()).then_some(value),
            calls: Vec::new(),
            ..SafeContent::default()
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

#[derive(Debug, Default)]
struct SafeBlock {
    kind: Option<String>,
    text: Option<String>,
    id: Option<String>,
    name: Option<String>,
    input: Option<Value>,
    native_content: Value,
    protocol: Option<String>,
    server: Option<String>,
    explicit_tool: Option<String>,
    call_id_unavailable: bool,
    tool_name_unavailable: bool,
    input_unavailable: bool,
    mcp_identity_unavailable: bool,
    native_content_unavailable: bool,
    literal_facts: Vec<ctx_history_core::ProviderDeclaredFact>,
}

fn decode_safe_block(raw: &RawValue) -> Result<SafeBlock, String> {
    let audit = audit_json(
        raw.get().as_bytes(),
        claude_selector_group,
        claude_literal_kind_for_key,
    )
    .map_err(|error| format!("invalid Claude content block: {error}"))?;
    let native_content: Value = serde_json::from_str(raw.get())
        .map_err(|error| format!("invalid Claude content block: {error}"))?;
    let object = native_content
        .as_object()
        .ok_or_else(|| "Claude content block must be an object".to_owned())?;
    let (kind, kind_invalid) = bounded_block_string(object.get("type"));
    let (text, text_invalid) = bounded_block_string(object.get("text"));
    let (id, id_invalid) = bounded_block_string(object.get("id"));
    let (name, name_invalid) = bounded_block_string(object.get("name"));
    let (protocol, protocol_invalid) = bounded_block_string(object.get("protocol"));
    let (server, server_invalid) = bounded_block_string(object.get("server"));
    let (explicit_tool, explicit_tool_invalid) = bounded_block_string(object.get("tool"));
    let call_id_unavailable = audit.selector_ambiguous(SelectorGroup::CallId) || id_invalid;
    let tool_name_unavailable = audit.selector_ambiguous(SelectorGroup::ToolName) || name_invalid;
    let input_unavailable = audit.selector_ambiguous(SelectorGroup::Arguments);
    let mcp_identity_unavailable = audit.selector_ambiguous(SelectorGroup::Protocol)
        || audit.selector_ambiguous(SelectorGroup::Server)
        || audit.selector_ambiguous(SelectorGroup::McpTool)
        || protocol_invalid
        || server_invalid
        || explicit_tool_invalid;
    let input = (!input_unavailable)
        .then(|| object.get("input").cloned())
        .flatten();
    Ok(SafeBlock {
        kind,
        text,
        id: (!call_id_unavailable).then_some(id).flatten(),
        name: (!tool_name_unavailable).then_some(name).flatten(),
        input,
        native_content,
        protocol: (!mcp_identity_unavailable).then_some(protocol).flatten(),
        server: (!mcp_identity_unavailable).then_some(server).flatten(),
        explicit_tool: (!mcp_identity_unavailable)
            .then_some(explicit_tool)
            .flatten(),
        call_id_unavailable,
        tool_name_unavailable,
        input_unavailable,
        mcp_identity_unavailable,
        native_content_unavailable: audit.any_selector_ambiguous()
            || kind_invalid
            || text_invalid
            || id_invalid
            || name_invalid
            || protocol_invalid
            || server_invalid
            || explicit_tool_invalid,
        literal_facts: audit.facts().to_vec(),
    })
}

fn bounded_block_string(value: Option<&Value>) -> (Option<String>, bool) {
    match value {
        None | Some(Value::Null) => (None, false),
        Some(Value::String(value)) if !value.is_empty() && value.len() <= 64 * 1024 => {
            (Some(value.clone()), false)
        }
        Some(_) => (None, true),
    }
}

impl SafeContent {
    fn push_block(&mut self, block: SafeBlock) -> Result<(), &'static str> {
        if block.kind.as_deref() == Some("thinking") {
            self.saw_private_thinking = true;
        } else {
            self.saw_other_block = true;
        }
        match block.kind.as_deref() {
            Some("tool_use") | Some("server_tool_use") => {
                let represented_rows = self.calls.len() + usize::from(self.body.is_some());
                if represented_rows >= super::rows::CLAUDE_MAX_RECORD_ROWS {
                    return Err("Claude content exceeds the representable row limit");
                }
                self.calls.push(ToolCallRequest {
                    call_id: block.id,
                    tool_name: block.name.clone(),
                    input: block.input,
                    native_content: block.native_content,
                    protocol: block.protocol,
                    server: block.server,
                    explicit_tool: block.explicit_tool,
                    call_id_unavailable: block.call_id_unavailable,
                    tool_name_unavailable: block.tool_name_unavailable,
                    input_unavailable: block.input_unavailable,
                    mcp_identity_unavailable: block.mcp_identity_unavailable,
                    native_content_unavailable: block.native_content_unavailable,
                    literal_facts: block.literal_facts,
                });
            }
            Some("text") => {
                if let Some(text) = block.text.filter(|value| !value.trim().is_empty()) {
                    self.push_text(text)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn push_text(&mut self, text: String) -> Result<(), &'static str> {
        let additional = text.len() + usize::from(self.body.is_some());
        let normalized_len = self
            .body
            .as_ref()
            .map_or(0, String::len)
            .checked_add(additional)
            .ok_or("Claude normalized message body length overflowed")?;
        if normalized_len > MAX_CORE_CONTENT_BYTES {
            return Err("Claude normalized message body exceeds the Core content limit");
        }
        if let Some(body) = self.body.as_mut() {
            body.push('\n');
            body.push_str(&text);
        } else {
            if self.calls.len() >= super::rows::CLAUDE_MAX_RECORD_ROWS {
                return Err("Claude content exceeds the representable row limit");
            }
            self.body = Some(text);
        }
        Ok(())
    }

    fn into_parts(self) -> (Option<String>, Vec<ToolCallRequest>, bool) {
        (
            self.body,
            self.calls,
            self.saw_private_thinking && !self.saw_other_block,
        )
    }
}

pub(super) fn claude_selector_group(key: &str) -> Option<SelectorGroup> {
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

pub(super) fn claude_literal_kind_for_key(key: &str) -> Option<ctx_history_core::LiteralFactKind> {
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
        "branch" | "branch_name" | "gitBranch" => Some(LiteralFactKind::Branch),
        "workspace" | "workspace_id" => Some(LiteralFactKind::Workspace),
        _ => None,
    }
}

struct SafeRecordProjection {
    rows: Vec<ClaudeRetainedRow>,
    ignored_private_thinking: bool,
}

fn retain_safe_record(
    mut record: SafeRecord,
    raw_ordinal: u64,
    locator: &ClaudePhysicalLocator,
) -> SafeRecordProjection {
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
    let (body, calls, private_thinking_only) = content.into_parts();
    let private_thinking_only =
        private_thinking_only && entry_type == "assistant" && role.as_deref() == Some("assistant");
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
            body_text_retention: None,
            tool_call: Some(call),
            tool_result: None,
            locator: locator.clone(),
        });
    }

    SafeRecordProjection {
        ignored_private_thinking: rows.is_empty() && private_thinking_only,
        rows,
    }
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
    pub(super) ownership: ClaudeRecordOwnership,
    pub(super) session_id: Option<String>,
    pub(super) timestamp: Option<String>,
    pub(super) cwd: Option<String>,
    pub(super) version: Option<String>,
    pub(super) git_branch: Option<String>,
    pub(super) ignored_private_thinking: bool,
    pub(super) rows: Vec<ClaudeRetainedRow>,
}

#[derive(Debug)]
pub(super) struct ClaudeOutputDescriptor {
    pub(super) subrecord_index: u32,
    pub(super) call_id: Option<String>,
    pub(super) content: Option<Value>,
    pub(super) call_id_unavailable: bool,
    pub(super) content_unavailable: bool,
    pub(super) native_content_unavailable: bool,
    pub(super) literal_facts: Vec<ctx_history_core::ProviderDeclaredFact>,
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

/// The body-allocation-free raw inspection bounds the native shape. Records
/// with an explicit provider result declaration are retained completely for
/// direct Core output and exact native call/result linkage.
pub(super) fn parse_native_record(
    bytes: &[u8],
    raw_ordinal: u64,
    locator: &ClaudePhysicalLocator,
) -> Result<ParsedClaudeRecord, serde_json::Error> {
    parse_native_record_inner(bytes, raw_ordinal, locator)
}

fn parse_native_record_inner(
    bytes: &[u8],
    raw_ordinal: u64,
    locator: &ClaudePhysicalLocator,
) -> Result<ParsedClaudeRecord, serde_json::Error> {
    let preflight = preflight_record(bytes)?;
    let ownership = serde_json::from_slice(bytes)?;
    if preflight.explicit_result {
        let value: Value = serde_json::from_slice(bytes)?;
        let metadata = metadata_from_value(&value);
        let outputs = output_descriptors(&preflight, bytes)?;
        let rows = complete_output_rows(
            raw_ordinal,
            locator,
            metadata.uuid.clone(),
            metadata.timestamp.clone(),
            &outputs,
            &value,
        );
        validate_row_count(&rows)?;
        return Ok(ParsedClaudeRecord {
            ownership,
            session_id: metadata.session_id,
            timestamp: metadata.timestamp,
            cwd: metadata.cwd,
            version: metadata.version,
            git_branch: metadata.git_branch,
            ignored_private_thinking: false,
            rows,
        });
    }

    let record: SafeRecord = serde_json::from_slice(bytes)?;
    let session_id = record.session_id.clone();
    let timestamp = record.timestamp.clone();
    let cwd = record.cwd.clone();
    let version = record.version.clone();
    let git_branch = record.git_branch.clone();
    let projection = retain_safe_record(record, raw_ordinal, locator);
    let rows = projection.rows;
    validate_row_count(&rows)?;
    Ok(ParsedClaudeRecord {
        ownership,
        session_id,
        timestamp,
        cwd,
        version,
        git_branch,
        ignored_private_thinking: projection.ignored_private_thinking,
        rows,
    })
}

fn metadata_from_value(value: &Value) -> MetadataOnlyRecord {
    MetadataOnlyRecord {
        uuid: exact_metadata_string(value, "uuid"),
        session_id: exact_metadata_string(value, "sessionId"),
        timestamp: exact_metadata_string(value, "timestamp"),
        cwd: exact_metadata_string(value, "cwd"),
        version: exact_metadata_string(value, "version"),
        git_branch: exact_metadata_string(value, "gitBranch"),
    }
}

fn exact_metadata_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 64 * 1024)
        .map(str::to_owned)
}

fn validate_row_count(rows: &[ClaudeRetainedRow]) -> Result<(), serde_json::Error> {
    if rows.len() > super::rows::CLAUDE_MAX_RECORD_ROWS {
        return Err(serde_json::Error::io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Claude record exceeds the representable row limit",
        )));
    }
    Ok(())
}

fn output_descriptors(
    preflight: &RawRecordPreflight,
    bytes: &[u8],
) -> Result<Vec<ClaudeOutputDescriptor>, serde_json::Error> {
    let mut outputs = preflight
        .output_descriptors()
        .iter()
        .enumerate()
        .map(|(index, descriptor)| {
            let native_bytes = descriptor.value_bytes(bytes).unwrap_or(bytes);
            let audit = audit_json(
                native_bytes,
                claude_selector_group,
                claude_literal_kind_for_key,
            )?;
            let call_id_unavailable = audit.selector_ambiguous(SelectorGroup::CallId);
            let content_unavailable = audit.selector_ambiguous(SelectorGroup::Content);
            Ok(ClaudeOutputDescriptor {
                subrecord_index: u32::try_from(index).unwrap_or(u32::MAX),
                call_id: (!call_id_unavailable)
                    .then(|| descriptor.decode_call_id(bytes))
                    .flatten(),
                content: descriptor.decode_value(bytes)?,
                call_id_unavailable,
                content_unavailable,
                native_content_unavailable: preflight.duplicate_critical
                    || audit.any_selector_ambiguous(),
                literal_facts: audit.facts().to_vec(),
            })
        })
        .collect::<Result<Vec<_>, serde_json::Error>>()?;
    if outputs.is_empty() {
        outputs.push(ClaudeOutputDescriptor {
            subrecord_index: 0,
            call_id: None,
            content: None,
            call_id_unavailable: preflight.duplicate_critical,
            content_unavailable: preflight.duplicate_critical,
            native_content_unavailable: preflight.duplicate_critical,
            literal_facts: audit_json(bytes, claude_selector_group, claude_literal_kind_for_key)?
                .facts()
                .to_vec(),
        });
    }
    Ok(outputs)
}

#[cfg(test)]
mod ordinary_message_tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn string_message_content_is_retained_as_literal_core_body() {
        let bytes = br#"{"type":"user","uuid":"literal-first","sessionId":"neutral-claude-session","message":{"role":"user","content":"literal first"}}"#;
        let locator = ClaudePhysicalLocator {
            path: PathBuf::from("neutral-claude-session.jsonl"),
            byte_start: 0,
            byte_end_exclusive: bytes.len() as u64,
            line_number: 1,
            record_sha256: Sha256::digest(bytes).into(),
        };
        let parsed = parse_native_record(bytes, 0, &locator).unwrap();
        assert_eq!(parsed.session_id.as_deref(), Some("neutral-claude-session"));
        assert_eq!(parsed.rows.len(), 1);
        assert_eq!(parsed.rows[0].body.as_deref(), Some("literal first"));
    }
}
