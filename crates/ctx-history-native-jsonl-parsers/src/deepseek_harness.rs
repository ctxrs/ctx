use ctx_history_capture_model::{
    file_references::{
        visit_provider_file_reference_drafts_with_limit, MAX_PROVIDER_FILE_REFERENCES_PER_EVENT,
    },
    raw_object_keys_are_unique,
};
use ctx_history_core::{
    derive_native_session_id, AgentScope, CaptureProvider, EventRole, EventType,
    ProviderDeclaredFact, SourceAnchorScope, SourceKey, StableEntityId, TypedKey,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::Path;

pub const LOGICAL_FORMAT_VERSION: u64 = 0;
pub const SOURCE_SCHEMA_VARIANT: &str = "deepseek-harness-session-v0";
const SOURCE_ANCHOR_NAMESPACE: &str = "deepseek-harness-session";
const NATIVE_SESSION_NAMESPACE: &str = "deepseek-harness-session";
const LOGICAL_SESSION_KIND: &str = "deepseek-harness-session";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionHeader {
    pub id: String,
    pub created_at_ms: i64,
    pub cwd: Option<String>,
    pub parent_session: Option<String>,
    pub seed_length: Option<u64>,
    pub origin: Option<String>,
    pub delegation_depth: u64,
    pub agent_preset: Option<String>,
    pub value: Value,
}

#[derive(Debug, Clone)]
pub struct SemanticEvent {
    pub seq: u64,
    pub time_ms: i64,
    pub event_type: EventType,
    pub role: EventRole,
    pub native_kind: &'static str,
    pub native_message_id: Option<String>,
    pub call_id: Option<String>,
    pub tool_name: Option<String>,
    pub model_provider: Option<String>,
    pub model: Option<String>,
    pub text: String,
    pub content_omission_reason: Option<&'static str>,
    pub structured: Value,
    pub value: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SequenceSpan {
    pub first: u64,
    pub len: u64,
}

#[derive(Debug, Clone)]
pub enum ParsedRow {
    Header(SessionHeader),
    Semantic(SemanticEvent),
    Ignored(SequenceSpan),
}

#[derive(Debug)]
pub enum StorageRowsError<E> {
    Invalid(String),
    Visitor(E),
}

pub fn source_key(
    source_format: &'static str,
    native_session_id: &str,
) -> Result<SourceKey, String> {
    source_key_scoped(
        source_format,
        native_session_id,
        SourceAnchorScope::Unqualified,
    )
}

pub fn source_key_scoped(
    source_format: &'static str,
    native_session_id: &str,
    source_anchor_scope: SourceAnchorScope,
) -> Result<SourceKey, String> {
    SourceKey::derive_provider_native_scoped(
        CaptureProvider::DeepSeekHarness.as_str(),
        source_format,
        SOURCE_SCHEMA_VARIANT,
        1,
        SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(native_session_id).map_err(|error| error.to_string())?,
        source_anchor_scope,
    )
    .map_err(|error| error.to_string())
}

pub fn session_identity(
    source: &SourceKey,
    native_session_id: &str,
) -> Result<StableEntityId, String> {
    derive_native_session_id(
        source,
        LOGICAL_SESSION_KIND,
        NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(native_session_id).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}

pub fn agent_scope(header: &SessionHeader) -> AgentScope {
    if header.origin.as_deref() == Some("subagent") || header.delegation_depth > 0 {
        AgentScope::Subagent
    } else {
        AgentScope::Primary
    }
}

pub fn is_session_leaf(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some("session.jsonl" | "session.jsonl.zstd")
    )
}

pub fn is_zstd_session_leaf(path: &Path) -> bool {
    path.file_name().and_then(|name| name.to_str()) == Some("session.jsonl.zstd")
}

pub fn visit_storage_rows<E>(
    bytes: &[u8],
    framed: bool,
    maximum_row_bytes: usize,
    mut visit: impl FnMut(usize, &[u8]) -> Result<(), E>,
) -> Result<(), StorageRowsError<E>> {
    if !framed {
        if bytes.len() > maximum_row_bytes {
            return Err(StorageRowsError::Invalid(
                "DeepSeek Harness JSONL row exceeds the bounded row limit".to_owned(),
            ));
        }
        return visit(0, bytes.strip_suffix(b"\r").unwrap_or(bytes))
            .map_err(StorageRowsError::Visitor);
    }
    let mut rows = 0_usize;
    for terminated in bytes.split_inclusive(|byte| *byte == b'\n') {
        if terminated.last() != Some(&b'\n') {
            return Err(StorageRowsError::Invalid(
                "DeepSeek Harness Zstandard frame ends in a partial JSONL row".to_owned(),
            ));
        }
        let row = terminated[..terminated.len() - 1]
            .strip_suffix(b"\r")
            .unwrap_or(&terminated[..terminated.len() - 1]);
        if row.is_empty() || row.len() > maximum_row_bytes {
            return Err(StorageRowsError::Invalid(
                "DeepSeek Harness Zstandard frame contains an empty or oversized JSONL row"
                    .to_owned(),
            ));
        }
        visit(rows, row).map_err(StorageRowsError::Visitor)?;
        rows = rows.saturating_add(1);
    }
    if rows == 0 {
        return Err(StorageRowsError::Invalid(
            "DeepSeek Harness Zstandard frame contains no JSONL rows".to_owned(),
        ));
    }
    Ok(())
}

pub fn parse_row(bytes: &[u8]) -> Result<ParsedRow, String> {
    if bytes.is_empty() {
        return Err("empty DeepSeek Harness JSONL row".to_owned());
    }
    if !raw_object_keys_are_unique(bytes) {
        return Err(
            "DeepSeek Harness JSONL row is invalid or has duplicate object keys".to_owned(),
        );
    }
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("invalid DeepSeek Harness JSONL: {error}"))?;
    let object = value
        .as_object()
        .ok_or_else(|| "DeepSeek Harness JSONL row is not an object".to_owned())?;
    let kind = required_string(object, "type")?.to_owned();
    if kind == "session" {
        return parse_header(value);
    }
    if matches!(
        kind.as_str(),
        "text-chunks" | "reasoning-chunks" | "tool-call-chunks"
    ) {
        return parse_chunk_row(object, &kind).map(ParsedRow::Ignored);
    }
    if matches!(
        kind.as_str(),
        "user/message" | "assistant/message" | "tool/call" | "tool/result"
    ) {
        return parse_semantic(value, &kind);
    }
    if KNOWN_IGNORED_EVENTS.contains(&kind.as_str())
        || object.get("ignorable") == Some(&Value::Bool(true))
    {
        required_safe_i64(object, "time")?;
        return Ok(ParsedRow::Ignored(SequenceSpan {
            first: required_u64(object, "seq")?,
            len: 1,
        }));
    }
    Err(format!(
        "unsupported required DeepSeek Harness semantic event type {kind:?}"
    ))
}

/// Recover sequence placement independently from semantic payload validation.
/// Malformed JSON and malformed packed rows deliberately return no hint.
pub fn sequence_span(bytes: &[u8]) -> Option<SequenceSpan> {
    if !raw_object_keys_are_unique(bytes) {
        return None;
    }
    let value: Value = serde_json::from_slice(bytes).ok()?;
    let object = value.as_object()?;
    let kind = object.get("type")?.as_str()?;
    match kind {
        "session" => None,
        "text-chunks" | "reasoning-chunks" | "tool-call-chunks" => {
            parse_chunk_row(object, kind).ok()
        }
        _ => Some(SequenceSpan {
            first: required_u64(object, "seq").ok()?,
            len: 1,
        }),
    }
}

pub fn exact_file_references(value: &Value) -> Result<Vec<ProviderDeclaredFact>, String> {
    let mut facts = Vec::new();
    let arguments = value
        .pointer("/data/arguments")
        .and_then(Value::as_str)
        .and_then(|arguments| serde_json::from_str::<Value>(arguments).ok());
    for candidate in std::iter::once(value).chain(arguments.as_ref()) {
        let outcome = visit_provider_file_reference_drafts_with_limit(
            candidate,
            MAX_PROVIDER_FILE_REFERENCES_PER_EVENT.saturating_sub(facts.len()),
            |(_, draft)| -> Result<(), String> {
                facts.push(ProviderDeclaredFact {
                    kind: draft.kind,
                    value: draft.value,
                });
                Ok(())
            },
        )
        .map_err(|error| error.to_string())?;
        if outcome.limit_exceeded() {
            return Err(
                "DeepSeek Harness event exceeds the literal file-reference bound".to_owned(),
            );
        }
    }
    Ok(facts)
}

fn parse_chunk_row(object: &Map<String, Value>, kind: &str) -> Result<SequenceSpan, String> {
    if !has_exact_keys(object, &["type", "seq0", "time0", "data"]) {
        return Err(malformed_chunk(
            kind,
            "envelope must be exactly {type, seq0, time0, data}",
        ));
    }
    let seq0 = required_u64(object, "seq0")?;
    let mut time = required_safe_i64(object, "time0")?;
    let data = required_object(object, "data")?;
    let payload_key = if kind == "tool-call-chunks" {
        let unnamed = ["turn", "step", "index", "id", "dt", "args"];
        let named = ["turn", "step", "index", "id", "name", "dt", "args"];
        if !has_exact_keys(data, &unnamed) && !has_exact_keys(data, &named) {
            return Err(malformed_chunk(
                kind,
                "data must be exactly {turn, step, index, id, name?, dt, args}",
            ));
        }
        required_string(data, "id")?;
        if data.contains_key("name") {
            required_string(data, "name")?;
        }
        "args"
    } else {
        if !has_exact_keys(data, &["turn", "step", "index", "dt", "texts"]) {
            return Err(malformed_chunk(
                kind,
                "data must be exactly {turn, step, index, dt, texts}",
            ));
        }
        "texts"
    };
    for field in ["turn", "step", "index"] {
        required_u64(data, field)
            .map_err(|error| malformed_chunk(kind, &format!("{field} is invalid: {error}")))?;
    }
    let payload = required_array(data, payload_key)?;
    if payload.is_empty() || payload.iter().any(|entry| !entry.is_string()) {
        return Err(malformed_chunk(
            kind,
            &format!("{payload_key} must be a non-empty string array"),
        ));
    }
    let gaps = required_array(data, "dt")?;
    if gaps.len() != payload.len() - 1 {
        return Err(malformed_chunk(
            kind,
            &format!(
                "dt length {} does not match {} members",
                gaps.len(),
                payload.len()
            ),
        ));
    }
    for gap in gaps {
        let gap = safe_i64(gap)
            .ok_or_else(|| malformed_chunk(kind, "dt must be an array of safe integers"))?;
        time = time
            .checked_add(gap)
            .filter(|value| value.unsigned_abs() <= MAX_SAFE_INTEGER)
            .ok_or_else(|| malformed_chunk(kind, "member times must stay safe integers"))?;
    }
    let len = u64::try_from(payload.len())
        .map_err(|_| malformed_chunk(kind, "member count overflowed"))?;
    seq0.checked_add(len - 1)
        .filter(|last| *last <= MAX_SAFE_INTEGER)
        .ok_or_else(|| malformed_chunk(kind, "member seqs must stay safe integers"))?;
    Ok(SequenceSpan { first: seq0, len })
}

fn malformed_chunk(kind: &str, why: &str) -> String {
    format!("malformed {kind} storage row: {why}")
}

fn has_exact_keys(object: &Map<String, Value>, keys: &[&str]) -> bool {
    object.len() == keys.len() && keys.iter().all(|key| object.contains_key(*key))
}

fn parse_header(value: Value) -> Result<ParsedRow, String> {
    let object = value.as_object().expect("header object checked");
    let version = required_u64(object, "version")?;
    if version != LOGICAL_FORMAT_VERSION {
        return Err(format!(
            "unsupported DeepSeek Harness session format version {version}; expected {LOGICAL_FORMAT_VERSION}"
        ));
    }
    for key in object.keys() {
        if !matches!(
            key.as_str(),
            "type"
                | "version"
                | "id"
                | "createdAt"
                | "cwd"
                | "parentSession"
                | "seedLength"
                | "origin"
                | "delegationDepth"
                | "agentPreset"
        ) {
            return Err(format!(
                "unknown DeepSeek Harness session header field {key:?}"
            ));
        }
    }
    let id = nonempty(required_string(object, "id")?, "session id")?.to_owned();
    let created_at_ms = required_nonnegative_i64(object, "createdAt")?;
    let delegation_depth = required_u64(object, "delegationDepth")?;
    let origin = optional_string(object, "origin")?;
    if origin.is_some_and(|origin| origin != "subagent") {
        return Err("DeepSeek Harness session origin is not 'subagent'".to_owned());
    }
    Ok(ParsedRow::Header(SessionHeader {
        id,
        created_at_ms,
        cwd: optional_string(object, "cwd")?.map(str::to_owned),
        parent_session: optional_string(object, "parentSession")?.map(str::to_owned),
        seed_length: optional_u64(object, "seedLength")?,
        origin: origin.map(str::to_owned),
        delegation_depth,
        agent_preset: optional_string(object, "agentPreset")?.map(str::to_owned),
        value,
    }))
}

fn parse_semantic(value: Value, kind: &str) -> Result<ParsedRow, String> {
    let object = value.as_object().expect("event object checked");
    let seq = required_u64(object, "seq")?;
    let time_ms = required_safe_i64(object, "time")?;
    let data = required_object(object, "data")?;
    let event = match kind {
        "user/message" => {
            require_role(data, "user")?;
            let id = nonempty(required_string(data, "id")?, "user message id")?.to_owned();
            let content = content_projection(required_array(data, "content")?, true)?;
            let text = content.text();
            SemanticEvent {
                seq,
                time_ms,
                event_type: EventType::Message,
                role: EventRole::User,
                native_kind: "user/message",
                native_message_id: Some(id),
                call_id: None,
                tool_name: None,
                model_provider: None,
                model: None,
                text,
                content_omission_reason: content.omission_reason(),
                structured: admitted_event_data(&value),
                value,
            }
        }
        "assistant/message" => {
            let message = required_object(data, "message")?;
            require_role(message, "assistant")?;
            let id = nonempty(required_string(message, "id")?, "assistant message id")?.to_owned();
            let source = required_object(message, "source")?;
            if required_string(source, "kind")? != "model" {
                return Err("DeepSeek Harness assistant message source is not model".to_owned());
            }
            let content = content_projection(required_array(message, "content")?, false)?;
            let text = content.text();
            SemanticEvent {
                seq,
                time_ms,
                event_type: EventType::Message,
                role: EventRole::Assistant,
                native_kind: "assistant/message",
                native_message_id: Some(id),
                call_id: None,
                tool_name: None,
                model_provider: Some(
                    nonempty(required_string(source, "provider")?, "model provider")?.to_owned(),
                ),
                model: Some(nonempty(required_string(source, "model")?, "model")?.to_owned()),
                text,
                content_omission_reason: content.omission_reason(),
                structured: admitted_event_data(&value),
                value,
            }
        }
        "tool/call" => {
            let call_id = nonempty(required_string(data, "callId")?, "tool call id")?.to_owned();
            let tool_name = nonempty(required_string(data, "name")?, "tool name")?.to_owned();
            let arguments = required_string(data, "arguments")?;
            SemanticEvent {
                seq,
                time_ms,
                event_type: EventType::ToolCall,
                role: EventRole::Assistant,
                native_kind: "tool/call",
                native_message_id: None,
                call_id: Some(call_id),
                tool_name: Some(tool_name.clone()),
                model_provider: None,
                model: None,
                text: format!("{tool_name}\n{arguments}"),
                content_omission_reason: None,
                structured: admitted_event_data(&value),
                value,
            }
        }
        "tool/result" => {
            let message = required_object(data, "message")?;
            let message_id =
                nonempty(required_string(message, "id")?, "tool result message id")?.to_owned();
            require_role(message, "user")?;
            let source = required_object(message, "source")?;
            if required_string(source, "kind")? != "tool" {
                return Err("DeepSeek Harness tool result source is not tool".to_owned());
            }
            let call_id =
                nonempty(required_string(source, "callId")?, "tool result call id")?.to_owned();
            let blocks = required_array(message, "content")?;
            if blocks.len() != 1
                || blocks[0].get("type").and_then(Value::as_str) != Some("tool-result")
                || blocks[0].get("toolCallId").and_then(Value::as_str) != Some(call_id.as_str())
            {
                return Err(
                    "DeepSeek Harness tool result message has invalid correlation".to_owned(),
                );
            }
            let result_content = blocks[0]
                .get("content")
                .and_then(Value::as_array)
                .ok_or_else(|| "DeepSeek Harness tool result content is not an array".to_owned())?;
            let content = content_projection(result_content, true)?;
            let text = content.text();
            SemanticEvent {
                seq,
                time_ms,
                event_type: EventType::ToolOutput,
                role: EventRole::Tool,
                native_kind: "tool/result",
                native_message_id: Some(message_id),
                call_id: Some(call_id),
                tool_name: None,
                model_provider: None,
                model: None,
                text,
                content_omission_reason: content.omission_reason(),
                structured: admitted_event_data(&value),
                value,
            }
        }
        _ => unreachable!("selected kind checked"),
    };
    Ok(ParsedRow::Semantic(event))
}

#[derive(Default)]
struct ContentProjection {
    parts: Vec<String>,
    saw_reasoning: bool,
    saw_image: bool,
}

impl ContentProjection {
    fn text(&self) -> String {
        self.parts
            .iter()
            .filter(|part| !part.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn omission_reason(&self) -> Option<&'static str> {
        self.text().is_empty().then_some(if self.saw_image {
            "DeepSeek Harness image content is not admitted by native import policy"
        } else if self.saw_reasoning {
            "DeepSeek Harness private reasoning is not admitted by native import policy"
        } else {
            "DeepSeek Harness message has no admitted content"
        })
    }

    fn merge(&mut self, nested: Self) {
        self.parts.extend(nested.parts);
        self.saw_reasoning |= nested.saw_reasoning;
        self.saw_image |= nested.saw_image;
    }
}

fn content_projection(
    blocks: &[Value],
    include_tool_blocks: bool,
) -> Result<ContentProjection, String> {
    let mut projection = ContentProjection::default();
    for block in blocks {
        let object = block
            .as_object()
            .ok_or_else(|| "DeepSeek Harness message content block is not an object".to_owned())?;
        match required_string(object, "type")? {
            "text" => projection
                .parts
                .push(required_string(object, "text")?.to_owned()),
            "reasoning" => {
                required_string(object, "text")?;
                projection.saw_reasoning = true;
            }
            "image" => {
                validate_image_block(object)?;
                projection.saw_image = true;
            }
            "tool-call" if include_tool_blocks => {
                projection
                    .parts
                    .push(required_string(object, "name")?.to_owned());
                projection
                    .parts
                    .push(required_string(object, "arguments")?.to_owned());
            }
            "tool-result" if include_tool_blocks => {
                projection.merge(content_projection(
                    required_array(object, "content")?,
                    true,
                )?);
            }
            "tool-call" | "tool-result" => {}
            unknown => {
                return Err(format!(
                    "unsupported DeepSeek Harness semantic content block {unknown:?}"
                ));
            }
        }
    }
    Ok(projection)
}

fn admitted_event_data(value: &Value) -> Value {
    let mut data = value.get("data").cloned().unwrap_or(Value::Null);
    let content = match value.get("type").and_then(Value::as_str) {
        Some("user/message") => data.get_mut("content"),
        Some("assistant/message" | "tool/result") => data.pointer_mut("/message/content"),
        _ => None,
    };
    if let Some(Value::Array(blocks)) = content {
        sanitize_content_blocks(blocks);
    }
    data
}

fn sanitize_content_blocks(blocks: &mut Vec<Value>) {
    blocks.retain_mut(|block| match block.get("type").and_then(Value::as_str) {
        Some("reasoning" | "image") => false,
        Some("tool-result") => {
            if let Some(content) = block.get_mut("content").and_then(Value::as_array_mut) {
                sanitize_content_blocks(content);
            }
            true
        }
        _ => true,
    });
}

fn validate_image_block(object: &Map<String, Value>) -> Result<(), String> {
    let attachment = required_object(object, "attachment")?;
    for field in ["attachmentId", "mediaType"] {
        nonempty(required_string(attachment, field)?, field)?;
    }
    for field in ["bytes", "width", "height"] {
        required_u64(attachment, field)?;
    }
    optional_string(attachment, "name")?;
    Ok(())
}

fn require_role(object: &Map<String, Value>, role: &str) -> Result<(), String> {
    if required_string(object, "role")? == role {
        Ok(())
    } else {
        Err(format!("DeepSeek Harness message role is not {role:?}"))
    }
}

fn required_object<'a>(
    object: &'a Map<String, Value>,
    key: &str,
) -> Result<&'a Map<String, Value>, String> {
    object
        .get(key)
        .and_then(Value::as_object)
        .ok_or_else(|| format!("DeepSeek Harness field {key:?} is not an object"))
}

fn required_array<'a>(object: &'a Map<String, Value>, key: &str) -> Result<&'a Vec<Value>, String> {
    object
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("DeepSeek Harness field {key:?} is not an array"))
}

fn required_string<'a>(object: &'a Map<String, Value>, key: &str) -> Result<&'a str, String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("DeepSeek Harness field {key:?} is not a string"))
}

fn optional_string<'a>(
    object: &'a Map<String, Value>,
    key: &str,
) -> Result<Option<&'a str>, String> {
    object
        .get(key)
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| format!("DeepSeek Harness field {key:?} is not a string"))
        })
        .transpose()
}

fn required_u64(object: &Map<String, Value>, key: &str) -> Result<u64, String> {
    object
        .get(key)
        .and_then(Value::as_u64)
        .filter(|value| *value <= 9_007_199_254_740_991)
        .ok_or_else(|| format!("DeepSeek Harness field {key:?} is not a nonnegative safe integer"))
}

fn optional_u64(object: &Map<String, Value>, key: &str) -> Result<Option<u64>, String> {
    object
        .get(key)
        .map(|_| required_u64(object, key))
        .transpose()
}

fn required_nonnegative_i64(object: &Map<String, Value>, key: &str) -> Result<i64, String> {
    required_u64(object, key).and_then(|value| {
        i64::try_from(value).map_err(|_| format!("DeepSeek Harness field {key:?} exceeds i64"))
    })
}

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

fn safe_i64(value: &Value) -> Option<i64> {
    let value = value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))?;
    (value.unsigned_abs() <= MAX_SAFE_INTEGER).then_some(value)
}

fn required_safe_i64(object: &Map<String, Value>, key: &str) -> Result<i64, String> {
    object
        .get(key)
        .and_then(safe_i64)
        .ok_or_else(|| format!("DeepSeek Harness field {key:?} is not a safe integer"))
}

fn nonempty<'a>(value: &'a str, label: &str) -> Result<&'a str, String> {
    (!value.is_empty())
        .then_some(value)
        .ok_or_else(|| format!("DeepSeek Harness {label} is empty"))
}

const KNOWN_IGNORED_EVENTS: &[&str] = &[
    "agent-preset/selected",
    "agent/inbox/spliced",
    "approval/asked",
    "approval/decided",
    "approval/policy",
    "assistant/chunk",
    "command/done",
    "command/run",
    "compaction/end",
    "compaction/prune",
    "compaction/start",
    "compaction/summary",
    "feedback/record",
    "goal/change",
    "hook/invoked",
    "hook/result",
    "llm/retry",
    "llm/retry-started",
    "permission/preset",
    "plan/mode",
    "request/context",
    "request/header",
    "sandbox/mode",
    "schedule/change",
    "session/end-seed",
    "session/title",
    "session/title-llm-request",
    "step/end",
    "step/start",
    "subagent/descriptor",
    "todo/write",
    "tool-workflow/agent-end",
    "tool-workflow/agent-start",
    "tool-workflow/run-end",
    "tool-workflow/run-start",
    "tool/code-dispatch",
    "tool/code-dispatch-start",
    "turn/end",
    "turn/start",
    "web/deepseek-search-llm-request",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_and_session_identities_are_root_scoped() {
        let released = source_key("deepseek_harness_session_jsonl", "same-session").unwrap();
        let compatibility = source_key_scoped(
            "deepseek_harness_session_jsonl",
            "same-session",
            SourceAnchorScope::Unqualified,
        )
        .unwrap();
        let first = source_key_scoped(
            "deepseek_harness_session_jsonl",
            "same-session",
            SourceAnchorScope::Lineage([1; 32]),
        )
        .unwrap();
        let second = source_key_scoped(
            "deepseek_harness_session_jsonl",
            "same-session",
            SourceAnchorScope::Lineage([2; 32]),
        )
        .unwrap();

        assert!(released.exact_descriptor_eq(&compatibility));
        assert_ne!(first.identity(), second.identity());
        assert_ne!(
            session_identity(&first, "same-session").unwrap(),
            session_identity(&second, "same-session").unwrap()
        );
    }

    #[test]
    fn parses_completed_semantics_and_preserves_model_and_tool_payloads() {
        let assistant = br#"{"type":"assistant/message","seq":2,"time":12,"data":{"turn":1,"step":1,"message":{"id":"m2","role":"assistant","content":[{"type":"text","text":"done"}],"source":{"kind":"model","provider":"deepseek","model":"deepseek-chat"}}}}"#;
        let ParsedRow::Semantic(event) = parse_row(assistant).unwrap() else {
            panic!()
        };
        assert_eq!(event.text, "done");
        assert_eq!(event.model_provider.as_deref(), Some("deepseek"));
        assert_eq!(event.model.as_deref(), Some("deepseek-chat"));

        let call = br#"{"type":"tool/call","seq":3,"time":13,"data":{"turn":1,"step":1,"callId":"c1","name":"read","arguments":"{\"path\":\"src/lib.rs\"}"}}"#;
        let ParsedRow::Semantic(event) = parse_row(call).unwrap() else {
            panic!()
        };
        assert_eq!(event.call_id.as_deref(), Some("c1"));
        assert!(event.text.contains("src/lib.rs"));
    }

    #[test]
    fn validates_reasoning_and_durable_image_blocks_without_flattening_them() {
        let reasoning = br#"{"type":"assistant/message","seq":2,"time":12,"data":{"message":{"id":"m2","role":"assistant","content":[{"type":"reasoning","text":"private"}],"source":{"kind":"model","provider":"deepseek","model":"deepseek-chat"}}}}"#;
        let ParsedRow::Semantic(reasoning) = parse_row(reasoning).unwrap() else {
            panic!()
        };
        assert!(reasoning.text.is_empty());

        let image = br#"{"type":"user/message","seq":3,"time":13,"data":{"id":"m3","role":"user","content":[{"type":"image","attachment":{"attachmentId":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","mediaType":"image/png","bytes":68,"width":1,"height":1,"name":"pixel.png"}}]}}"#;
        let ParsedRow::Semantic(image) = parse_row(image).unwrap() else {
            panic!()
        };
        assert!(image.text.is_empty());
        assert!(parse_row(br#"{"type":"user/message","seq":3,"time":13,"data":{"id":"m3","role":"user","content":[{"type":"image","attachment":{"attachmentId":"sha256:a","mediaType":"image/png"}}]}}"#).is_err());
    }

    #[test]
    fn validates_chunks_and_only_allows_explicitly_ignorable_unknowns() {
        assert!(matches!(
            parse_row(br#"{"type":"assistant/chunk","seq":0,"time":1}"#).unwrap(),
            ParsedRow::Ignored(SequenceSpan { first: 0, len: 1 })
        ));
        assert!(matches!(
            parse_row(br#"{"type":"text-chunks","seq0":1,"time0":2,"data":{"turn":0,"step":0,"index":0,"dt":[1,1],"texts":["a","b","c"]}}"#).unwrap(),
            ParsedRow::Ignored(SequenceSpan { first: 1, len: 3 })
        ));
        assert!(matches!(
            parse_row(br#"{"type":"future/event","seq":4,"time":5,"ignorable":true}"#).unwrap(),
            ParsedRow::Ignored(SequenceSpan { first: 4, len: 1 })
        ));
        assert!(parse_row(br#"{"type":"text-chunks","seq0":1}"#).is_err());
        assert!(parse_row(br#"{"type":"text-chunks","seq0":1,"time0":2,"data":{"turn":0,"step":0,"index":0,"dt":[],"texts":["a","b"]}}"#).is_err());
        assert!(parse_row(
            br#"{"type":"text-chunks","seq0":1,"time0":2,"data":{"turn":-1,"step":0,"index":0,"dt":[],"texts":["a"]}}"#
        )
        .is_err());
        assert!(parse_row(
            br#"{"type":"text-chunks","seq0":1,"time0":2,"data":{"turn":0.5,"step":0,"index":0,"dt":[],"texts":["a"]}}"#
        )
        .is_err());
        assert!(parse_row(
            br#"{"type":"text-chunks","seq0":1,"time0":2,"data":{"turn":9007199254740992,"step":0,"index":0,"dt":[],"texts":["a"]}}"#
        )
        .is_err());
        assert!(parse_row(br#"{"type":"future/event"}"#).is_err());
        assert!(parse_row(
            br#"{"type":"session","version":1,"id":"s","createdAt":1,"delegationDepth":0}"#
        )
        .is_err());
    }
}
