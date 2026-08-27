use std::path::PathBuf;

use ctx_history_core::EventType;
use serde_json::{json, Value};

use crate::tool_backend::{
    ToolBackend, ToolBackendError, ToolOperation, ToolOutcome, ToolSearchContentScope,
    ToolSearchRequest, ToolUsageFacts,
};

mod arguments;
mod companion;
mod input;
mod query_events;
mod response;
mod response_bound;
mod show;
mod value_support;

use arguments::{
    optional_bool, optional_content_scope, optional_f32, optional_provider,
    optional_search_backend, optional_string, optional_strings, optional_transcript_mode,
    optional_usize, validate_argument_keys, validate_search_filter_arguments,
    SourceIdentityFilterArgs,
};
pub use companion::validated_companion_tool_request;
pub use input::{read_mcp_input_line, McpInputLine};
use query_events::query_events_operation;
pub use response::error_response;
use response::{
    invalid_request_response, invalid_tool_request, json_rpc_error, success_response,
    tool_error_result, tool_result_with_text,
};
use response_bound::{bound_query_events_mcp_response, bound_show_mcp_response};
use show::{show_event_operation, show_session_operation};
use value_support::{compact_json, provider_root_selectors};

pub const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
pub const MCP_SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &[MCP_PROTOCOL_VERSION, "2025-06-18"];
pub const MCP_MAX_LINE_BYTES: usize = 1024 * 1024;
pub const MCP_PRESENTATION_MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
/// Maximum exact JSON encoding size of an accepted string request ID.
/// Numeric IDs have a much smaller intrinsic `serde_json::Number` representation.
/// This leaves ample fixed-envelope room inside the smallest MCP tool response bound.
pub const MCP_MAX_ENCODED_REQUEST_ID_BYTES: usize = 64 * 1024;
const MCP_DEFAULT_SESSION_PAGE_LIMIT: usize = 200;
const MCP_MAX_SESSION_PAGE_LIMIT: usize = 4_096;
const MCP_MAX_SESSION_CURSOR_BYTES: usize = 4_096;
const MAX_SEARCH_LIMIT: usize = 200;
const MAX_PROVIDER_ROOT_SELECTORS: usize = 64;
const PROVIDER_ROOT_SELECTOR_PATTERN: &str = "^[A-Za-z0-9_-]{1,64}$";
const MAX_EVENT_WINDOW: usize = 50;
const MCP_MIN_EVENT_QUERY_LIMIT: u64 = 1;
const MCP_DEFAULT_EVENT_QUERY_LIMIT: u64 = 10_000;
const MCP_MAX_EVENT_QUERY_LIMIT: u64 = 10_000_000;

/// Serializes one exact newline-delimited MCP response without a second JSON pass.
pub fn encode_response_line(response: &Value) -> serde_json::Result<String> {
    let mut encoded = serde_json::to_string(response)?;
    encoded.push('\n');
    Ok(encoded)
}

fn request_id_is_accepted(id: Option<&Value>) -> bool {
    match id {
        None | Some(Value::Number(_)) => true,
        Some(Value::String(value)) => {
            encoded_json_string_bytes(value) <= MCP_MAX_ENCODED_REQUEST_ID_BYTES
        }
        Some(Value::Null | Value::Bool(_) | Value::Array(_) | Value::Object(_)) => false,
    }
}

fn encoded_json_string_bytes(value: &str) -> usize {
    value.chars().fold(2_usize, |bytes, character| {
        bytes.saturating_add(match character {
            '"' | '\\' | '\u{0008}' | '\t' | '\n' | '\u{000c}' | '\r' => 2,
            '\u{0000}'..='\u{001f}' => 6,
            character => character.len_utf8(),
        })
    })
}

#[derive(Debug, Clone, Copy)]
pub struct McpServerIdentity<'a> {
    pub name: &'a str,
    pub version: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpToolKind {
    Status,
    Sources,
    Search,
    ShowSession,
    ShowEvent,
    QueryEvents,
    Blame,
    ProStatus,
    Unknown,
    Missing,
}

const NO_ARGUMENTS: &[&str] = &[];
const SEARCH_ARGUMENTS: &[&str] = &[
    "query",
    "limit",
    "provider",
    "history_source",
    "provider_key",
    "source_id",
    "source_format",
    "source_roots",
    "source_groups",
    "workspace",
    "since",
    "primary_only",
    "content_scope",
    "event_type",
    "file",
    "session",
    "events",
    "include_current_session",
    "backend",
    "semantic_weight",
];
const SHOW_SESSION_ARGUMENTS: &[&str] = &["ctx_session_id", "mode", "limit", "cursor"];
const SHOW_EVENT_ARGUMENTS: &[&str] = &["ctx_event_id", "before", "after", "window"];
const QUERY_EVENTS_ARGUMENTS: &[&str] = &[
    "since",
    "until",
    "providers",
    "source",
    "history_source",
    "provider_key",
    "source_id",
    "source_format",
    "provider_session",
    "session",
    "parent_session",
    "root_session",
    "branch",
    "workspace",
    "event_type",
    "role",
    "agent_type",
    "scope",
    "file",
    "direction",
    "cursor",
    "limit",
    "content",
];
impl McpToolKind {
    pub fn from_tool_name(name: Option<&str>) -> Self {
        match name {
            Some("status") => Self::Status,
            Some("sources") => Self::Sources,
            Some("search") => Self::Search,
            Some("show_session") => Self::ShowSession,
            Some("show_event") => Self::ShowEvent,
            Some("query_events") => Self::QueryEvents,
            Some("blame") => Self::Blame,
            Some("pro_status") => Self::ProStatus,
            Some(_) => Self::Unknown,
            None => Self::Missing,
        }
    }

    pub const fn tool_name(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Sources => "sources",
            Self::Search => "search",
            Self::ShowSession => "show_session",
            Self::ShowEvent => "show_event",
            Self::QueryEvents => "query_events",
            Self::Blame => "blame",
            Self::ProStatus => "pro_status",
            Self::Unknown => "unknown",
            Self::Missing => "missing",
        }
    }

    pub const fn is_companion_owned(self) -> bool {
        matches!(self, Self::Blame | Self::ProStatus)
    }

    fn allowed_arguments(self) -> Option<&'static [&'static str]> {
        match self {
            Self::Status | Self::Sources => Some(NO_ARGUMENTS),
            Self::Search => Some(SEARCH_ARGUMENTS),
            Self::ShowSession => Some(SHOW_SESSION_ARGUMENTS),
            Self::ShowEvent => Some(SHOW_EVENT_ARGUMENTS),
            Self::QueryEvents => Some(QUERY_EVENTS_ARGUMENTS),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestDescriptor {
    Initialize,
    Ping,
    ToolsList,
    ToolCall { operation: McpToolKind },
    UnknownRequest,
    MissingRequest,
    InitializedNotification,
    UnknownNotification,
    InvalidJson,
    InvalidUtf8,
    LineTooLarge,
}

impl RequestDescriptor {
    pub fn from_message(message: &Value) -> Self {
        let Some(object) = message.as_object() else {
            return Self::MissingRequest;
        };
        let method = object.get("method").and_then(Value::as_str);
        if !object.contains_key("id") {
            return if method == Some("notifications/initialized") {
                Self::InitializedNotification
            } else {
                Self::UnknownNotification
            };
        }
        match method {
            Some("initialize") => Self::Initialize,
            Some("ping") => Self::Ping,
            Some("tools/list") => Self::ToolsList,
            Some("tools/call") => Self::ToolCall {
                operation: McpToolKind::from_tool_name(
                    message.pointer("/params/name").and_then(Value::as_str),
                ),
            },
            Some(_) => Self::UnknownRequest,
            None => Self::MissingRequest,
        }
    }
}

pub struct McpHandled<T> {
    pub value: T,
    pub usage: Option<McpUsage>,
}

impl<T> McpHandled<T> {
    pub fn plain(value: T) -> Self {
        Self { value, usage: None }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpUsage {
    pub operation: McpToolKind,
    pub facts: ToolUsageFacts,
}

pub(crate) struct McpOperationParseError {
    pub(crate) error: ToolBackendError,
    pub(crate) usage: ToolUsageFacts,
}

impl From<ToolBackendError> for McpOperationParseError {
    fn from(error: ToolBackendError) -> Self {
        Self {
            error,
            usage: ToolUsageFacts::default(),
        }
    }
}

pub fn handle_protocol_message<B: ToolBackend>(
    message: Value,
    descriptor: RequestDescriptor,
    initialized: &mut bool,
    server_identity: McpServerIdentity<'_>,
    backend: &B,
    render_text: impl Fn(&Value) -> String,
) -> McpHandled<Option<Value>> {
    let Some(object) = message.as_object() else {
        return McpHandled::plain(Some(error_response(
            Value::Null,
            -32600,
            "Invalid Request",
            None,
        )));
    };
    if !request_id_is_accepted(object.get("id")) {
        return McpHandled::plain(Some(invalid_request_response(None)));
    }
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return McpHandled::plain(Some(invalid_request_response(object.get("id"))));
    }
    let tool_operation = match descriptor {
        RequestDescriptor::ToolCall { operation } => Some(operation),
        _ => None,
    };
    let id = object.get("id").cloned();
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return McpHandled::plain(Some(invalid_request_response(id.as_ref())));
    };
    let Some(id) = id else {
        if method == "notifications/initialized" {
            *initialized = true;
        }
        return McpHandled::plain(None);
    };
    let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
    if !params.is_object() {
        return McpHandled::plain(Some(error_response(
            id,
            -32602,
            "Invalid params",
            Some(json!({ "error": "params must be an object" })),
        )));
    }
    if method != "initialize" && !*initialized {
        return McpHandled::plain(Some(error_response(
            id,
            -32002,
            "Server not initialized",
            Some(json!({ "error": "send initialize before calling ctx MCP tools" })),
        )));
    }

    let result: Result<McpHandled<Value>, McpHandled<Value>> = match method {
        "initialize" => {
            *initialized = true;
            Ok(McpHandled::plain(initialize_result(
                &params,
                server_identity,
            )))
        }
        "ping" => Ok(McpHandled::plain(json!({}))),
        "tools/list" => Ok(McpHandled::plain(json!({
            "tools": tool_definitions(backend.provider_names())
        }))),
        "tools/call" => handle_tools_call_with_backend(
            params,
            tool_operation.unwrap_or(McpToolKind::Missing),
            backend,
            &render_text,
        ),
        _ => Err(McpHandled::plain(json_rpc_error(
            -32601,
            "Method not found",
            None,
        ))),
    };
    let response_id = id.clone();
    let handled = match result {
        Ok(handled) => McpHandled {
            value: success_response(id, handled.value),
            usage: handled.usage,
        },
        Err(failure) => {
            let error = failure.value;
            let value = if let Some(object) = error.as_object() {
                let code = object.get("code").and_then(Value::as_i64).unwrap_or(-32603);
                let message = object
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("Internal error");
                error_response(id, code, message, object.get("data").cloned())
            } else {
                error_response(id, -32603, "Internal error", Some(error))
            };
            McpHandled {
                value,
                usage: failure.usage,
            }
        }
    };
    let response = match tool_operation {
        Some(McpToolKind::ShowSession | McpToolKind::ShowEvent) => bound_show_mcp_response(
            handled.value,
            response_id,
            MCP_PRESENTATION_MAX_OUTPUT_BYTES,
        ),
        Some(McpToolKind::QueryEvents) => bound_query_events_mcp_response(
            handled.value,
            response_id,
            MCP_PRESENTATION_MAX_OUTPUT_BYTES,
        ),
        _ => handled.value,
    };
    McpHandled {
        value: Some(response),
        usage: handled.usage,
    }
}

fn initialize_result(params: &Value, server_identity: McpServerIdentity<'_>) -> Value {
    let protocol_version = negotiate_protocol_version(params);
    json!({
        "protocolVersion": protocol_version,
        "capabilities": {
            "tools": {
                "listChanged": false
            }
        },
        "serverInfo": {
            "name": server_identity.name,
            "version": server_identity.version
        },
        "instructions": "Local access to ctx Core tools and declarative companion-owned routes. Tool output may include absolute paths, source metadata, snippets, and transcript text; MCP hosts may log or forward it. Companion-owned requests are forwarded opaquely by the executable."
    })
}

fn negotiate_protocol_version(params: &Value) -> &'static str {
    let Some(requested) = params.get("protocolVersion").and_then(Value::as_str) else {
        return MCP_PROTOCOL_VERSION;
    };
    MCP_SUPPORTED_PROTOCOL_VERSIONS
        .iter()
        .copied()
        .find(|version| *version == requested)
        .unwrap_or(MCP_PROTOCOL_VERSION)
}

#[allow(
    clippy::result_large_err,
    reason = "MCP responses and raw receipts stay inline to avoid transport-path allocations"
)]
fn handle_tools_call_with_backend<B: ToolBackend>(
    params: Value,
    operation: McpToolKind,
    backend: &B,
    render_text: &impl Fn(&Value) -> String,
) -> Result<McpHandled<Value>, McpHandled<Value>> {
    if operation.is_companion_owned() {
        return Err(McpHandled::plain(json_rpc_error(
            -32603,
            "Companion unavailable",
            Some(json!({
                "error": "companion_unavailable",
                "error_code": "companion_unavailable",
                "retryable": true,
            })),
        )));
    }
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return Err(McpHandled::plain(json_rpc_error(
            -32602,
            "Invalid params",
            Some(json!({ "error": "tools/call requires params.name" })),
        )));
    };
    let Some(allowed_arguments) = operation.allowed_arguments() else {
        return Err(McpHandled::plain(json_rpc_error(
            -32602,
            "Invalid params",
            Some(json!({ "error": format!("unknown tool {name}") })),
        )));
    };
    let mut usage = McpUsage {
        operation,
        facts: if operation == McpToolKind::Search {
            ToolUsageFacts::search_preparation()
        } else {
            ToolUsageFacts::default()
        },
    };
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !arguments.is_object() {
        return Err(McpHandled {
            value: json_rpc_error(
                -32602,
                "Invalid params",
                Some(json!({ "error": "tools/call params.arguments must be an object" })),
            ),
            usage: Some(usage),
        });
    }

    if let Err(error) = validate_argument_keys(&arguments, allowed_arguments) {
        return Ok(parse_failure_result(error.into(), usage));
    }
    let operation = match parse_operation(operation, &arguments, backend) {
        Ok(operation) => operation,
        Err(failure) => return Ok(parse_failure_result(failure, usage)),
    };
    match backend.execute(operation) {
        Ok(ToolOutcome {
            structured,
            compact,
            usage: backend_usage,
        }) => {
            usage.facts.merge(backend_usage);
            let text = render_text(compact.as_ref().unwrap_or(&structured));
            Ok(McpHandled {
                value: tool_result_with_text(structured, text),
                usage: Some(usage),
            })
        }
        Err(failure) => {
            usage.facts.merge(*failure.usage);
            Ok(McpHandled {
                value: tool_error_result(*failure.error),
                usage: Some(usage),
            })
        }
    }
}

fn parse_failure_result(failure: McpOperationParseError, mut usage: McpUsage) -> McpHandled<Value> {
    usage.facts.merge(failure.usage);
    McpHandled {
        value: tool_error_result(failure.error),
        usage: Some(usage),
    }
}

#[allow(
    clippy::result_large_err,
    reason = "raw typed receipts stay inline to avoid allocations in the MCP tool path"
)]
fn parse_operation<B: ToolBackend>(
    operation: McpToolKind,
    arguments: &Value,
    backend: &B,
) -> Result<ToolOperation, McpOperationParseError> {
    if operation.is_companion_owned() {
        return Err(invalid_tool_request(format!("unknown tool {}", operation.tool_name())).into());
    }
    let operation = match operation {
        McpToolKind::Status => Ok(ToolOperation::Status),
        McpToolKind::Sources => Ok(ToolOperation::Sources),
        McpToolKind::Search => search_request(arguments, backend).map(ToolOperation::Search),
        McpToolKind::ShowSession => show_session_operation(arguments),
        McpToolKind::ShowEvent => show_event_operation(arguments),
        McpToolKind::QueryEvents => query_events_operation(arguments),
        _ => Err(invalid_tool_request(format!(
            "unknown tool {}",
            operation.tool_name()
        ))),
    }
    .map_err(McpOperationParseError::from)?;
    Ok(operation)
}

fn search_request<B: ToolBackend>(
    arguments: &Value,
    backend: &B,
) -> Result<ToolSearchRequest, ToolBackendError> {
    let event_type = optional_string(arguments, "event_type")?;
    let content_scope = optional_content_scope(arguments, "content_scope")?;
    if content_scope.is_some() && event_type.is_some() {
        return Err(invalid_tool_request(
            "content_scope and event_type are mutually exclusive",
        ));
    }
    let query = optional_string(arguments, "query")?.unwrap_or_default();
    let limit = optional_usize(arguments, "limit")?.unwrap_or(20);
    if !(1..=MAX_SEARCH_LIMIT).contains(&limit) {
        return Err(invalid_tool_request(format!(
            "limit must be between 1 and {MAX_SEARCH_LIMIT}"
        )));
    }
    let provider_names = backend.provider_names();
    let provider = optional_provider(
        arguments,
        "provider",
        |value| backend.parse_provider(value),
        &provider_names,
    )?;
    let history_source = optional_string(arguments, "history_source")?;
    let provider_key = optional_string(arguments, "provider_key")?;
    let source_id = optional_string(arguments, "source_id")?;
    let source_format = optional_string(arguments, "source_format")?;
    let source_roots = provider_root_selectors(arguments, "source_roots")?;
    let source_groups = provider_root_selectors(arguments, "source_groups")?;
    let session = optional_string(arguments, "session")?;
    let workspace = optional_string(arguments, "workspace")?;
    let since = optional_string(arguments, "since")?;
    let primary_only = optional_bool(arguments, "primary_only")?.unwrap_or(false);
    let file = optional_string(arguments, "file")?.map(PathBuf::from);
    let events = optional_bool(arguments, "events")?.unwrap_or(false) || session.is_some();
    let include_current_session =
        optional_bool(arguments, "include_current_session")?.unwrap_or(false);
    let backend = optional_search_backend(arguments, "backend")?;
    let semantic_weight = optional_f32(arguments, "semantic_weight")?.unwrap_or(0.35);
    if !(0.0..=1.0).contains(&semantic_weight) || !semantic_weight.is_finite() {
        return Err(invalid_tool_request(
            "semantic_weight must be between 0.0 and 1.0",
        ));
    }
    let source_identity = SourceIdentityFilterArgs {
        history_source,
        provider_key,
        source_id,
        source_format,
    };
    if query.trim().is_empty() && file.is_none() {
        return Err(invalid_tool_request("search needs a query or file"));
    }
    validate_search_filter_arguments(
        provider.as_ref(),
        &source_identity,
        session.as_deref(),
        since.as_deref(),
        event_type.as_deref(),
    )?;
    Ok(ToolSearchRequest {
        query,
        limit,
        provider,
        history_source: source_identity.history_source,
        provider_key: source_identity.provider_key,
        source_id: source_identity.source_id,
        source_format: source_identity.source_format,
        source_roots,
        source_groups,
        workspace,
        since,
        primary_only,
        content_scope: content_scope.unwrap_or(ToolSearchContentScope::All),
        event_type,
        file,
        session,
        events,
        include_current_session,
        backend,
        semantic_weight,
    })
}

fn tool_definitions(provider_names: Vec<&'static str>) -> Vec<Value> {
    vec![
        json!({
            "name": McpToolKind::Status.tool_name(),
            "title": "Status",
            "description": "Return detailed local ctx readiness and service status without writing to provider history or repositories.",
            "inputSchema": object_schema(json!({}), vec![]),
            "annotations": { "readOnlyHint": true },
        }),
        json!({
            "name": McpToolKind::Sources.tool_name(),
            "title": "Sources",
            "description": "List discovered local agent history sources.",
            "inputSchema": object_schema(json!({}), vec![]),
            "annotations": { "readOnlyHint": true },
        }),
        json!({
            "name": McpToolKind::Search.tool_name(),
            "title": "Search",
            "description": "Search the existing local ctx index by query text or touched-file path. This does not refresh or import provider history.",
            "inputSchema": object_schema_with_mutually_exclusive(json!({
                "query": { "type": "string", "description": "Non-empty text query. Required unless file is provided." },
                "limit": { "type": "integer", "minimum": 1, "maximum": MAX_SEARCH_LIMIT, "default": 20 },
                "provider": { "type": "string", "enum": provider_names },
                "history_source": { "type": "string", "description": "Custom history source selector as plugin/source or provider_key/source_id." },
                "provider_key": { "type": "string", "description": "Custom history provider_key." },
                "source_id": { "type": "string", "description": "Custom history source_id." },
                "source_format": { "type": "string", "description": "Custom history source_format." },
                "source_roots": { "type": "array", "maxItems": MAX_PROVIDER_ROOT_SELECTORS, "items": { "type": "string", "minLength": 1, "maxLength": 64, "pattern": PROVIDER_ROOT_SELECTOR_PATTERN }, "description": "Case-sensitive configured provider-root names to union." },
                "source_groups": { "type": "array", "maxItems": MAX_PROVIDER_ROOT_SELECTORS, "items": { "type": "string", "minLength": 1, "maxLength": 64, "pattern": PROVIDER_ROOT_SELECTOR_PATTERN }, "description": "Case-sensitive configured provider-root groups to union." },
                "workspace": { "type": "string", "description": "Workspace path or name text." },
                "since": { "type": "string", "description": "RFC3339 timestamp or day window such as 30d." },
                "primary_only": { "type": "boolean", "default": false, "description": "Search only primary agent sessions." },
                "content_scope": { "type": "string", "enum": content_scope_names(), "default": "all", "description": "Search all indexed content, transcript text, tool/command calls, or tool/command outputs." },
                "event_type": { "type": "string", "enum": event_type_names() },
                "file": { "type": "string", "description": "Indexed touched-file path. Required unless query is provided." },
                "session": { "type": "string", "description": "ctx session id." },
                "events": { "type": "boolean", "default": false },
                "include_current_session": { "type": "boolean", "default": false, "description": "Compatibility input accepted by MCP; MCP does not infer a current session, so this has no effect." },
                "backend": { "type": "string", "enum": ["hybrid", "semantic", "lexical"], "description": "Optional backend override. Defaults to lexical unless local semantic search is enabled in ctx config, then hybrid." },
                "semantic_weight": { "type": "number", "minimum": 0.0, "maximum": 1.0, "default": 0.35 }
            }), vec![], "content_scope", "event_type"),
            "annotations": { "readOnlyHint": true },
        }),
        json!({
            "name": McpToolKind::ShowSession.tool_name(),
            "title": "Show Session",
            "description": "Return one bounded page of an indexed session transcript by ctx session id. Continue with next_cursor while has_more is true.",
            "inputSchema": object_schema(json!({
                "ctx_session_id": { "type": "string" },
                "mode": { "type": "string", "enum": ["full", "lite", "log"], "default": "lite" },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MCP_MAX_SESSION_PAGE_LIMIT,
                    "default": MCP_DEFAULT_SESSION_PAGE_LIMIT,
                    "description": "Maximum selected transcript events to return after applying mode."
                },
                "cursor": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": MCP_MAX_SESSION_CURSOR_BYTES,
                    "description": "Opaque next_cursor from the preceding page of this exact session and Core generation."
                },
            }), vec!["ctx_session_id"]),
            "annotations": { "readOnlyHint": true },
        }),
        json!({
            "name": McpToolKind::ShowEvent.tool_name(),
            "title": "Show Event",
            "description": "Return an indexed event and optional surrounding event window by ctx event id.",
            "inputSchema": object_schema(json!({
                "ctx_event_id": { "type": "string" },
                "before": { "type": "integer", "minimum": 0, "default": 0 },
                "after": { "type": "integer", "minimum": 0, "default": 0 },
                "window": { "type": "integer", "minimum": 0 }
            }), vec!["ctx_event_id"]),
            "annotations": { "readOnlyHint": true },
        }),
        json!({
            "name": McpToolKind::QueryEvents.tool_name(),
            "title": "Query Events",
            "description": "Return one bounded deterministic page from the pinned normalized Core event corpus.",
            "inputSchema": object_schema(json!({
                "since": { "type": "string", "description": "Inclusive millisecond-aligned absolute RFC3339 lower bound; requires until." },
                "until": { "type": "string", "description": "Exclusive millisecond-aligned absolute RFC3339 upper bound; requires since." },
                "providers": { "type": "array", "items": { "type": "string", "minLength": 1 } },
                "source": { "type": "string", "description": "Exact public ctx source UUID." },
                "history_source": { "type": "string" },
                "provider_key": { "type": "string" },
                "source_id": { "type": "string" },
                "source_format": { "type": "string" },
                "provider_session": { "type": "string" },
                "session": { "type": "string", "description": "Exact public ctx session UUID." },
                "parent_session": { "type": "string", "description": "Exact public parent ctx session UUID." },
                "root_session": { "type": "string", "description": "Exact public root ctx session UUID." },
                "branch": { "type": "string" },
                "workspace": { "type": "string" },
                "event_type": { "type": "string", "description": "Exact open event type string." },
                "role": { "type": "string" },
                "agent_type": { "type": "string" },
                "scope": { "type": "string", "enum": ["all", "primary", "subagent"], "default": "all" },
                "file": { "type": "string" },
                "direction": { "type": "string", "enum": ["ascending", "descending"], "default": "ascending" },
                "cursor": { "type": "string", "description": "Opaque next_cursor from the preceding page of this exact selection and generation." },
                "limit": {
                    "type": "integer",
                    "minimum": MCP_MIN_EVENT_QUERY_LIMIT,
                    "maximum": MCP_MAX_EVENT_QUERY_LIMIT,
                    "default": MCP_DEFAULT_EVENT_QUERY_LIMIT
                },
                "content": { "type": "string", "enum": ["full", "text", "none"], "default": "full" }
            }), vec![]),
            "annotations": { "readOnlyHint": true },
        }),
    ]
}

fn object_schema(properties: Value, required: Vec<&str>) -> Value {
    compact_json(json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    }))
}

fn object_schema_with_mutually_exclusive(
    properties: Value,
    required: Vec<&str>,
    left: &str,
    right: &str,
) -> Value {
    let mut schema = object_schema(properties, required);
    schema["not"] = json!({"required": [left, right]});
    compact_json(schema)
}

fn content_scope_names() -> Vec<&'static str> {
    vec!["all", "transcript", "calls", "outputs"]
}

fn event_type_names() -> Vec<&'static str> {
    vec![
        EventType::Message.as_str(),
        EventType::ToolCall.as_str(),
        EventType::ToolOutput.as_str(),
        EventType::CommandStarted.as_str(),
        EventType::CommandOutput.as_str(),
        EventType::CommandFinished.as_str(),
        EventType::Artifact.as_str(),
        EventType::Summary.as_str(),
        EventType::Notice.as_str(),
    ]
}

#[cfg(test)]
mod request_id_tests {
    use ctx_history_core::CaptureProvider;
    use serde_json::{json, Value};

    use super::{
        encoded_json_string_bytes, handle_protocol_message, request_id_is_accepted, search_request,
        tool_definitions, McpServerIdentity, McpToolKind, RequestDescriptor,
        MCP_MAX_ENCODED_REQUEST_ID_BYTES, PROVIDER_ROOT_SELECTOR_PATTERN,
    };
    use crate::tool_backend::{
        OpaqueMcpProxyError, ToolBackend, ToolExecutionError, ToolOperation, ToolOutcome,
        ToolSearchBackend,
    };

    struct UnusedBackend;

    impl ToolBackend for UnusedBackend {
        fn execute(&self, _operation: ToolOperation) -> Result<ToolOutcome, ToolExecutionError> {
            panic!("request-ID validation must run before the backend")
        }

        fn proxy_companion_mcp(&self, _request: &[u8]) -> Result<Vec<u8>, OpaqueMcpProxyError> {
            panic!("request-ID validation must run before the backend")
        }

        fn parse_provider(&self, _value: &str) -> Option<CaptureProvider> {
            panic!("request-ID validation must run before the backend")
        }

        fn provider_names(&self) -> Vec<&'static str> {
            Vec::new()
        }
    }

    #[test]
    fn encoded_request_id_boundary_is_exact() {
        let accepted = "x".repeat(MCP_MAX_ENCODED_REQUEST_ID_BYTES - 2);
        let rejected = "x".repeat(MCP_MAX_ENCODED_REQUEST_ID_BYTES - 1);
        assert_eq!(
            encoded_json_string_bytes(&accepted),
            MCP_MAX_ENCODED_REQUEST_ID_BYTES
        );
        assert_eq!(
            serde_json::to_vec(&accepted).unwrap().len(),
            MCP_MAX_ENCODED_REQUEST_ID_BYTES
        );
        assert!(request_id_is_accepted(Some(&Value::String(
            accepted.clone()
        ))));
        assert!(!request_id_is_accepted(Some(&Value::String(
            rejected.clone()
        ))));

        let escaped = "\"\\\n\u{0001}雪";
        assert_eq!(
            encoded_json_string_bytes(escaped),
            serde_json::to_vec(escaped).unwrap().len()
        );
        assert!(request_id_is_accepted(Some(&json!(u64::MAX))));
        assert!(!request_id_is_accepted(Some(&Value::Null)));

        let mut initialized = true;
        let accepted_response = handle_protocol_message(
            json!({"jsonrpc": "2.0", "id": accepted, "method": "ping"}),
            RequestDescriptor::Ping,
            &mut initialized,
            McpServerIdentity {
                name: "ctx",
                version: "test",
            },
            &UnusedBackend,
            |_| panic!("request-ID validation must run before text rendering"),
        )
        .value
        .unwrap();
        assert_eq!(
            serde_json::to_vec(&accepted_response["id"]).unwrap().len(),
            MCP_MAX_ENCODED_REQUEST_ID_BYTES
        );

        let rejected_response = handle_protocol_message(
            json!({"jsonrpc": "2.0", "id": rejected, "method": "ping"}),
            RequestDescriptor::Ping,
            &mut initialized,
            McpServerIdentity {
                name: "ctx",
                version: "test",
            },
            &UnusedBackend,
            |_| panic!("request-ID validation must run before text rendering"),
        )
        .value
        .unwrap();
        assert_eq!(rejected_response["id"], Value::Null);
        assert_eq!(rejected_response["error"]["code"], -32600);
    }

    #[test]
    fn core_tool_manifest_contains_no_companion_owned_definition() {
        let definitions = tool_definitions(Vec::new());
        assert!(definitions.iter().all(|tool| {
            !McpToolKind::from_tool_name(tool.get("name").and_then(Value::as_str))
                .is_companion_owned()
        }));
    }

    #[test]
    fn search_root_and_group_arrays_are_typed_and_forwarded_across_backends() {
        for (backend, expected_backend) in [
            ("lexical", ToolSearchBackend::Lexical),
            ("semantic", ToolSearchBackend::Semantic),
            ("hybrid", ToolSearchBackend::Hybrid),
        ] {
            let request = search_request(
                &json!({
                    "query": "fixture",
                    "source_roots": ["personal", "archive"],
                    "source_groups": ["work"],
                    "backend": backend,
                }),
                &UnusedBackend,
            )
            .unwrap();
            assert_eq!(request.source_roots, ["personal", "archive"]);
            assert_eq!(request.source_groups, ["work"]);
            assert_eq!(request.backend, Some(expected_backend));
        }

        let error = search_request(
            &json!({"query": "fixture", "source_roots": ["personal", 7]}),
            &UnusedBackend,
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("source_roots entries must be strings"));

        let definitions = tool_definitions(Vec::new());
        let search = definitions
            .iter()
            .find(|tool| tool["name"] == "search")
            .unwrap();
        assert_eq!(
            search["inputSchema"]["properties"]["source_roots"]["maxItems"],
            64
        );
        assert_eq!(
            search["inputSchema"]["properties"]["source_groups"]["items"]["maxLength"],
            64
        );
        for key in ["source_roots", "source_groups"] {
            assert_eq!(
                search["inputSchema"]["properties"][key]["items"]["pattern"],
                PROVIDER_ROOT_SELECTOR_PATTERN
            );
        }
    }

    #[test]
    fn search_root_and_group_schema_matches_the_runtime_token_grammar() {
        for value in ["a", "A0_-", &"x".repeat(64)] {
            for key in ["source_roots", "source_groups"] {
                let mut arguments = json!({"query": "fixture"});
                arguments[key] = json!([value]);
                assert!(
                    search_request(&arguments, &UnusedBackend).is_ok(),
                    "{key} should accept {value:?}"
                );
            }
        }

        for value in ["", "bad.root", " spaced ", "café", &"x".repeat(65)] {
            for key in ["source_roots", "source_groups"] {
                let mut arguments = json!({"query": "fixture"});
                arguments[key] = json!([value]);
                let error = search_request(&arguments, &UnusedBackend).unwrap_err();
                let rendered = error.to_string();
                assert!(
                    rendered.contains("ASCII letters, digits"),
                    "{key} unexpectedly accepted {value:?}: {error}"
                );
                if !value.is_empty() {
                    assert!(
                        !rendered.contains(value),
                        "{key} rejection leaked selector content: {rendered}"
                    );
                }
            }
        }

        let too_many = vec!["root"; 65];
        for key in ["source_roots", "source_groups"] {
            let mut arguments = json!({"query": "fixture"});
            arguments[key] = json!(&too_many);
            let error = search_request(&arguments, &UnusedBackend).unwrap_err();
            assert!(error.to_string().contains("maximum of 64 entries"));
        }
    }
}
