use serde_json::{json, Value};

use super::{
    arguments::{optional_string, optional_usize, validate_argument_keys},
    object_schema,
    response::{success_response, tool_error_result, tool_result_with_text},
    response_bound::serialized_json_line_bytes,
    McpHandled, MCP_PRESENTATION_MAX_OUTPUT_BYTES,
};
use crate::tool_backend::{
    GraphDirection, GraphOperation, GraphOptions, ToolBackend, ToolBackendError, ToolOutcome,
    UnifiedErrorCode, UnifiedToolKind, UnifiedToolOperation, MAX_OUTPUT_INPUT_BYTES,
    OUTPUT_ENCODINGS,
};

impl UnifiedToolKind {
    pub(super) const fn allowed_arguments(self) -> &'static [&'static str] {
        match self {
            Self::GraphQuery => &["query", "depth", "limit", "direction", "relation"],
            Self::GraphShow => &["symbol"],
            Self::GraphCallers | Self::GraphCallees => &["symbol", "depth", "limit"],
            Self::GraphImpact => &["symbol", "depth", "limit", "relation"],
            Self::GraphPath => &[
                "source",
                "target",
                "depth",
                "limit",
                "direction",
                "relation",
            ],
            Self::GraphStats => &[],
            Self::OutputCompact => &["text"],
            Self::OutputRestore => &["text", "encoding"],
        }
    }
}

pub(super) fn definitions() -> Vec<Value> {
    UnifiedToolKind::ALL.into_iter().map(|kind| {
        let (description, required) = match kind {
            UnifiedToolKind::GraphQuery => (
                "Search the selected local graph snapshot. Does not index or refresh source files.",
                vec!["query"],
            ),
            UnifiedToolKind::GraphShow => (
                "Resolve one graph symbol, preserving ambiguity errors; use an exact node ID to disambiguate.",
                vec!["symbol"],
            ),
            UnifiedToolKind::GraphCallers => (
                "Follow recorded incoming calls in the selected graph snapshot.", vec!["symbol"],
            ),
            UnifiedToolKind::GraphCallees => (
                "Follow recorded outgoing calls in the selected graph snapshot.", vec!["symbol"],
            ),
            UnifiedToolKind::GraphImpact => (
                "Follow incoming dependencies for potential impact. Reachability is not proof of runtime behavior.",
                vec!["symbol"],
            ),
            UnifiedToolKind::GraphPath => (
                "Find a bounded shortest path between two graph symbols. Inspect truncation before treating absence as proof.",
                vec!["source", "target"],
            ),
            UnifiedToolKind::GraphStats => (
                "Read graph counts and generation without indexing, migration, or history setup.", vec![],
            ),
            UnifiedToolKind::OutputCompact => (
                "Compact supplied UTF-8 text locally with Sift. Returns explicit encoding and token counts. Text codecs preserve bytes; JSON codecs preserve values, not source whitespace. No files, retention, commands, or providers.",
                vec!["text"],
            ),
            UnifiedToolKind::OutputRestore => (
                "Restore supplied compact text using its explicit encoding, locally. Text codecs preserve bytes; JSON codecs preserve values, not source whitespace. Oversized output is rejected, never truncated.",
                vec!["text", "encoding"],
            ),
        };
        let mut properties = serde_json::Map::new();
        for key in kind.allowed_arguments() {
            let schema = match *key {
                "depth" => json!({"type": "integer", "minimum": 0, "maximum": 6, "default": default_depth(kind)}),
                "limit" => json!({"type": "integer", "minimum": 1, "maximum": 200, "default": 100}),
                "direction" => json!({"type": "string", "enum": ["incoming", "outgoing", "both"], "default": default_direction(kind)}),
                "encoding" => json!({"type": "string", "enum": OUTPUT_ENCODINGS}),
                "text" => json!({"type": "string", "maxLength": MAX_OUTPUT_INPUT_BYTES, "description": "At most 262144 UTF-8 bytes."}),
                _ => json!({"type": "string", "minLength": 1, "maxLength": 1024, "description": "Nonempty; at most 1024 UTF-8 bytes."}),
            };
            properties.insert((*key).to_owned(), schema);
        }
        json!({
            "name": kind.tool_name(),
            "description": description,
            "inputSchema": object_schema(Value::Object(properties), required),
            "annotations": {"readOnlyHint": true, "destructiveHint": false, "openWorldHint": false},
        })
    }).collect()
}

fn default_depth(kind: UnifiedToolKind) -> usize {
    match kind {
        UnifiedToolKind::GraphImpact => 3,
        UnifiedToolKind::GraphPath => 6,
        _ => 1,
    }
}

fn default_direction(kind: UnifiedToolKind) -> &'static str {
    if kind == UnifiedToolKind::GraphPath {
        "outgoing"
    } else {
        "both"
    }
}

fn required_string(
    arguments: &Value,
    key: &str,
    max: usize,
    nonempty: bool,
) -> Result<String, ToolBackendError> {
    let value = optional_string(arguments, key)?
        .ok_or_else(|| ToolBackendError::invalid_request(format!("{key} is required")))?;
    if value.len() > max || (nonempty && value.trim().is_empty()) {
        return Err(ToolBackendError::invalid_request(format!(
            "{key} must {}be at most {max} UTF-8 bytes",
            if nonempty { "be nonempty and " } else { "" },
        )));
    }
    Ok(value)
}

fn options(kind: UnifiedToolKind, arguments: &Value) -> Result<GraphOptions, ToolBackendError> {
    let depth = optional_usize(arguments, "depth")?.unwrap_or(default_depth(kind));
    let limit = optional_usize(arguments, "limit")?.unwrap_or(100);
    if depth > 6 || !(1..=200).contains(&limit) {
        return Err(ToolBackendError::invalid_request(
            "depth must be 0..6 and limit must be 1..200",
        ));
    }
    let direction = match optional_string(arguments, "direction")?
        .as_deref()
        .unwrap_or(default_direction(kind))
    {
        "incoming" => GraphDirection::Incoming,
        "outgoing" => GraphDirection::Outgoing,
        "both" => GraphDirection::Both,
        _ => {
            return Err(ToolBackendError::invalid_request(
                "direction must be incoming, outgoing, or both",
            ))
        }
    };
    let relation = optional_string(arguments, "relation")?;
    if relation
        .as_ref()
        .is_some_and(|value| value.trim().is_empty() || value.len() > 1024)
    {
        return Err(ToolBackendError::invalid_request(
            "relation must be nonempty and at most 1024 UTF-8 bytes",
        ));
    }
    Ok(GraphOptions {
        depth: depth as u32,
        limit,
        direction,
        relation,
    })
}

fn parse(
    kind: UnifiedToolKind,
    arguments: &Value,
) -> Result<UnifiedToolOperation, ToolBackendError> {
    if !arguments.is_object() {
        return Err(ToolBackendError::invalid_request(
            "tools/call params.arguments must be an object",
        ));
    }
    validate_argument_keys(arguments, kind.allowed_arguments())?;
    let symbol = || required_string(arguments, "symbol", 1024, true);
    let graph = match kind {
        UnifiedToolKind::GraphQuery => GraphOperation::Query {
            query: required_string(arguments, "query", 1024, true)?,
            options: options(kind, arguments)?,
        },
        UnifiedToolKind::GraphShow => GraphOperation::Show { symbol: symbol()? },
        UnifiedToolKind::GraphCallers => GraphOperation::Callers {
            symbol: symbol()?,
            options: options(kind, arguments)?,
        },
        UnifiedToolKind::GraphCallees => GraphOperation::Callees {
            symbol: symbol()?,
            options: options(kind, arguments)?,
        },
        UnifiedToolKind::GraphImpact => GraphOperation::Impact {
            symbol: symbol()?,
            options: options(kind, arguments)?,
        },
        UnifiedToolKind::GraphPath => GraphOperation::Path {
            source: required_string(arguments, "source", 1024, true)?,
            target: required_string(arguments, "target", 1024, true)?,
            options: options(kind, arguments)?,
        },
        UnifiedToolKind::GraphStats => GraphOperation::Stats,
        UnifiedToolKind::OutputCompact => {
            return Ok(UnifiedToolOperation::OutputCompact {
                text: required_string(arguments, "text", MAX_OUTPUT_INPUT_BYTES, false)?,
            })
        }
        UnifiedToolKind::OutputRestore => {
            let text = required_string(arguments, "text", MAX_OUTPUT_INPUT_BYTES, false)?;
            let encoding = required_string(arguments, "encoding", 32, true)?;
            if !OUTPUT_ENCODINGS.contains(&encoding.as_str()) {
                return Err(ToolBackendError::invalid_request(
                    "unsupported output encoding",
                ));
            }
            return Ok(UnifiedToolOperation::OutputRestore { text, encoding });
        }
    };
    Ok(UnifiedToolOperation::Graph(graph))
}

pub(super) fn handle<B: ToolBackend>(
    kind: UnifiedToolKind,
    params: &Value,
    backend: &B,
    render_text: &impl Fn(&Value) -> String,
) -> McpHandled<Value> {
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let result = parse(kind, &arguments)
        .map_err(Into::into)
        .and_then(|operation| backend.execute_unified(operation));
    let result = match result {
        Ok(ToolOutcome {
            structured,
            compact,
            text,
            ..
        }) => {
            let text = text.unwrap_or_else(|| render_text(compact.as_ref().unwrap_or(&structured)));
            tool_result_with_text(structured, text)
        }
        Err(failure) => tool_error_result(*failure.error),
    };
    // These operations have no history usage or analytics authority.
    McpHandled::plain(result)
}

pub(super) fn bound_response(response: Value, id: Value) -> Value {
    if serialized_json_line_bytes(&response)
        .is_ok_and(|size| size <= MCP_PRESENTATION_MAX_OUTPUT_BYTES)
    {
        return response;
    }
    success_response(id, tool_error_result(ToolBackendError::Unified {
        code: UnifiedErrorCode::OutputLimit,
        detail: "response exceeds the MCP output limit; reduce graph depth/limit or output input size".to_owned(),
    }))
}

#[cfg(test)]
#[path = "unified_tests.rs"]
mod tests;
