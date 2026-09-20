//! Turning one Devin `chat_message` into the events it contains.
//!
//! A single node is not a single event. One assistant node can carry visible
//! content, a reasoning block, and several tool calls at once, so
//! normalization fans a node out into an ordered list and the caller assigns
//! subrecord positions from that order.
//!
//! Two rules shape what is admitted. Text is only taken from fields whose
//! meaning has been observed, never synthesized: a tool call with no readable
//! arguments contributes a serialized argument object, not a rendered
//! `tool call: name` placeholder. And `thinking.signature` is never copied —
//! it is a provider attestation over the reasoning text, not content, and
//! retaining it would republish a signature ctx cannot verify.

use ctx_history_core::{EventRole, EventType};
use serde_json::{json, Map, Value};

use ctx_history_capture_model::{
    acp::{acp_terminal_status, acp_visible_text},
    file_references::visit_literal_file_reference_drafts,
    normalization::provider_role,
    raw_object_keys_are_unique, tool_input,
};
use ctx_history_core::LiteralFactKind;

use crate::{CaptureError, Result};

/// Why a node produced no events.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DevinNodeDisposition {
    /// The payload is not a shape ctx can read.
    Unsupported,
    /// The payload is readable but carries nothing to import.
    Empty,
}

/// One event derived from a node.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct DevinNativeEvent {
    pub(super) event_type: EventType,
    pub(super) role: EventRole,
    pub(super) text: String,
    pub(super) structured_content: Option<Value>,
    pub(super) provider_call_id: Option<String>,
    pub(super) tool_name: Option<String>,
    pub(super) command: Option<String>,
    pub(super) file_paths: Vec<String>,
    pub(super) workdir: Option<String>,
    pub(super) status: Option<String>,
    pub(super) duration_ms: Option<i64>,
    pub(super) completed_at: Option<String>,
}

impl DevinNativeEvent {
    fn new(event_type: EventType, role: EventRole, text: String) -> Self {
        Self {
            event_type,
            role,
            text,
            structured_content: None,
            provider_call_id: None,
            tool_name: None,
            command: None,
            file_paths: Vec::new(),
            workdir: None,
            status: None,
            duration_ms: None,
            completed_at: None,
        }
    }
}

/// The parsed node, plus the events it fans out to.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct DevinNormalizedNode {
    pub(super) message_id: Option<String>,
    pub(super) created_at_rfc3339: Option<String>,
    pub(super) events: Vec<DevinNativeEvent>,
    pub(super) disposition: Option<DevinNodeDisposition>,
}

/// Normalizes one node's `chat_message`.
///
/// `summarized_from` comes from the node row rather than the payload, because
/// it is the row that records a compaction.
pub(super) fn normalize_node(
    chat_message: &str,
    summarized_from: Option<i64>,
) -> Result<DevinNormalizedNode> {
    // A duplicate key means two different readers could disagree about this
    // payload, so it is refused rather than resolved by last-writer-wins.
    if !raw_object_keys_are_unique(chat_message.as_bytes()) {
        return Ok(unsupported());
    }
    let Ok(Value::Object(message)) = serde_json::from_str::<Value>(chat_message) else {
        return Ok(unsupported());
    };

    let message_id = string_field(&message, "message_id");
    let created_at_rfc3339 = message
        .get("metadata")
        .and_then(Value::as_object)
        .and_then(|metadata| metadata.get("created_at"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let role_text = message.get("role").and_then(Value::as_str);
    let content = message
        .get("content")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty());

    // The Core role comes from the shared mapper, which agrees with Devin's
    // four roles exactly. The match below decides only which events a role's
    // structure produces, and refuses a role the mapper cannot place.
    let role = provider_role(role_text);
    let mut events = Vec::new();
    match role_text {
        Some("user") => {
            if let Some(content) = content {
                events.push(DevinNativeEvent::new(
                    EventType::Message,
                    role,
                    content.to_owned(),
                ));
            }
        }
        Some("assistant") => {
            // Reasoning precedes the visible answer it produced.
            if let Some(thinking) = thinking_text(&message) {
                events.push(DevinNativeEvent::new(EventType::Summary, role, thinking));
            }
            if let Some(content) = content {
                let mut event = DevinNativeEvent::new(EventType::Message, role, content.to_owned());
                event.structured_content = Some(structured_assistant_content(&message));
                events.push(event);
            }
            events.extend(tool_call_events(&message, role)?);
        }
        Some("tool") => {
            if let Some(event) = tool_output_event(&message, content, role) {
                events.push(event);
            }
        }
        Some("system") => {
            // A compaction's continuation node is a summary of history, not a
            // notice about the environment, so the row's own record of the
            // compaction decides which it is.
            let (event_type, _) = if summarized_from.is_some() {
                (EventType::Summary, ())
            } else {
                (EventType::Notice, ())
            };
            if let Some(content) = content {
                events.push(DevinNativeEvent::new(event_type, role, content.to_owned()));
            }
        }
        // An unknown role is a shape ctx has not audited. Emitting it with a
        // guessed role would put unreviewed text into searchable history.
        Some(_) | None => return Ok(unsupported()),
    }

    let disposition = events.is_empty().then_some(DevinNodeDisposition::Empty);
    Ok(DevinNormalizedNode {
        message_id,
        created_at_rfc3339,
        events,
        disposition,
    })
}

fn unsupported() -> DevinNormalizedNode {
    DevinNormalizedNode {
        message_id: None,
        created_at_rfc3339: None,
        events: Vec::new(),
        disposition: Some(DevinNodeDisposition::Unsupported),
    }
}

fn string_field(message: &Map<String, Value>, field: &str) -> Option<String> {
    message
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// The reasoning text, if the node carries one.
///
/// `thinking.signature` and `signature_type` are deliberately not read.
fn thinking_text(message: &Map<String, Value>) -> Option<String> {
    message
        .get("thinking")
        .and_then(Value::as_object)?
        .get("thinking")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// The assistant node as structured content, with the reasoning attestation
/// stripped.
fn structured_assistant_content(message: &Map<String, Value>) -> Value {
    let mut copy = message.clone();
    if let Some(Value::Object(thinking)) = copy.get_mut("thinking") {
        thinking.remove("signature");
        thinking.remove("signature_type");
    }
    Value::Object(copy)
}

fn tool_call_events(
    message: &Map<String, Value>,
    role: EventRole,
) -> Result<Vec<DevinNativeEvent>> {
    let Some(calls) = message.get("tool_calls").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut events = Vec::with_capacity(calls.len());
    for call in calls {
        let Some(call) = call.as_object() else {
            continue;
        };
        let name = call.get("name").and_then(Value::as_str);
        let arguments = call.get("arguments");
        let command = name
            .filter(|name| tool_input::is_command_tool(name))
            .and(arguments)
            .and_then(tool_input::command);
        // Prefer the command a command tool was given; otherwise retain the
        // arguments verbatim rather than describing the call in prose.
        let text = match (&command, arguments) {
            (Some(command), _) => command.clone(),
            (None, Some(arguments)) if !arguments.is_null() => serde_json::to_string(arguments)
                .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?,
            (None, _) => String::new(),
        };
        let mut event = DevinNativeEvent::new(EventType::ToolCall, role, text);
        event.provider_call_id = call
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_owned);
        event.tool_name = name.map(str::to_owned);
        event.command = command;
        if let Some(arguments) = arguments {
            event.file_paths = literal_file_paths(arguments);
            event.structured_content = Some(arguments.clone());
        }
        events.push(event);
    }
    Ok(events)
}

fn tool_output_event(
    message: &Map<String, Value>,
    content: Option<&str>,
    role: EventRole,
) -> Option<DevinNativeEvent> {
    let extensions = message
        .get("metadata")
        .and_then(Value::as_object)
        .and_then(|metadata| metadata.get("extensions"))
        .and_then(Value::as_object);
    let terminal = extensions.and_then(|ext| ext.get("chisel/terminal_output"));

    // The node's own content is the tool's rendered result. When it is absent,
    // the recorded terminal output is the same text the harness showed.
    let text = content.map(str::to_owned).or_else(|| {
        terminal
            .and_then(Value::as_object)
            .and_then(|output| output.get("text"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    })?;

    let mut event = DevinNativeEvent::new(EventType::ToolOutput, role, text);
    event.provider_call_id = message
        .get("tool_call_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_owned);
    event.workdir = terminal
        .and_then(Value::as_object)
        .and_then(|output| output.get("cwd"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    if let Some(timing) = extensions
        .and_then(|ext| ext.get("chisel/tool_call_timing"))
        .and_then(Value::as_object)
    {
        event.duration_ms = timing.get("duration_ms").and_then(Value::as_i64);
        event.completed_at = timing
            .get("finished_at")
            .and_then(Value::as_str)
            .map(str::to_owned);
    }
    Some(event)
}

/// Merges the ACP `tool_call_state` record for a tool output, when one exists.
///
/// Enrichment is additive and optional: many tool nodes have no row, and a row
/// may hold only the initial call. Nothing already read from the node is
/// overwritten, so a present row can add a status or a path but cannot change
/// the text the harness displayed.
pub(super) fn enrich_tool_output(
    event: &mut DevinNativeEvent,
    tool_call_json: Option<&Value>,
    tool_call_update_json: Option<&Value>,
) {
    for blob in [tool_call_update_json, tool_call_json]
        .into_iter()
        .flatten()
    {
        if event.status.is_none() {
            event.status = acp_terminal_status(blob).map(str::to_owned);
        }
        if event.workdir.is_none() {
            event.workdir = blob
                .pointer("/_meta/cognition.ai~1cwd")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
        if event.tool_name.is_none() {
            event.tool_name = blob
                .pointer("/_meta/cognition.ai~1inferenceToolName")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
        if event.text.trim().is_empty() {
            if let Some(text) = blob.get("content").and_then(acp_visible_text) {
                if !text.trim().is_empty() {
                    event.text = text;
                }
            }
        }
        for path in acp_location_paths(blob) {
            if !event.file_paths.contains(&path) {
                event.file_paths.push(path);
            }
        }
    }
    let structured = [
        ("tool_call", tool_call_json),
        ("tool_call_update", tool_call_update_json),
    ]
    .into_iter()
    .filter_map(|(key, blob)| blob.map(|blob| (key.to_owned(), blob.clone())))
    .collect::<Map<String, Value>>();
    if !structured.is_empty() {
        event.structured_content = Some(json!({ "acp": Value::Object(structured) }));
    }
}

/// File paths an ACP record names, from `locations[]` and its raw input.
fn acp_location_paths(blob: &Value) -> Vec<String> {
    let mut paths = blob
        .get("locations")
        .and_then(Value::as_array)
        .map(|locations| {
            locations
                .iter()
                .filter_map(|location| location.get("path").and_then(Value::as_str))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if let Some(raw_input) = blob.get("rawInput") {
        for path in literal_file_paths(raw_input) {
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
    }
    paths
}

/// File paths named literally in a provider-supplied value.
///
/// Only `File` drafts are taken; the shared visitor also reports URLs and
/// other literal kinds, which are not file touches.
fn literal_file_paths(value: &Value) -> Vec<String> {
    let mut paths = Vec::new();
    let _ = visit_literal_file_reference_drafts::<std::convert::Infallible>(value, |draft| {
        if draft.kind == LiteralFactKind::File && !paths.contains(&draft.value) {
            paths.push(draft.value);
        }
        Ok(())
    });
    paths
}
