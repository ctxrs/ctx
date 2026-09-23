//! In-process, read-only graph and stateless output tool composition.

use std::path::Path;

use graf::{
    model::{Direction, QueryOptions},
    query::{ImpactOptions, SearchOptions},
    store::Store,
};
use serde_json::{json, Value};

use super::{
    GraphDirection, GraphOperation, GraphOptions, ToolBackendError, ToolExecutionError,
    ToolOutcome, UnifiedErrorCode, UnifiedToolOperation, MAX_OUTPUT_INPUT_BYTES,
    MAX_OUTPUT_TEXT_BYTES,
};

pub(super) fn execute(
    graph_db: Result<Option<&Path>, &str>,
    operation: UnifiedToolOperation,
) -> Result<ToolOutcome, ToolExecutionError> {
    match operation {
        UnifiedToolOperation::Graph(operation) => graph(graph_db, operation),
        UnifiedToolOperation::OutputCompact { text } => compact(&text),
        UnifiedToolOperation::OutputRestore { text, encoding } => restore(&text, &encoding),
    }
    .map_err(Into::into)
}

fn failure(code: UnifiedErrorCode, detail: impl Into<String>) -> ToolBackendError {
    ToolBackendError::Unified {
        code,
        detail: detail.into(),
    }
}

fn graph_options(options: GraphOptions) -> SearchOptions {
    SearchOptions {
        graph: QueryOptions {
            depth: options.depth,
            limit: options.limit,
            direction: match options.direction {
                GraphDirection::Incoming => Direction::Incoming,
                GraphDirection::Outgoing => Direction::Outgoing,
                GraphDirection::Both => Direction::Both,
            },
            relation: options.relation,
        },
        // The engine accounts for graph JSON, preserves truncation reasons,
        // and applies its own traversal/SQLite work budgets.
        token_budget: Some(64_000),
        ..SearchOptions::default()
    }
}

fn graph(
    path: Result<Option<&Path>, &str>,
    operation: GraphOperation,
) -> Result<ToolOutcome, ToolBackendError> {
    let path = path
        .and_then(|path| path.ok_or("no graph database found at server startup"))
        .map_err(|error| {
            failure(
                UnifiedErrorCode::GraphUnavailable,
                format!(
                    "{error}; start ctx mcp serve in an indexed project or use --graph-db PATH"
                ),
            )
        })?;
    let store = Store::open_read_only(path).map_err(|error| failure(
        UnifiedErrorCode::GraphUnavailable,
        format!("cannot read the selected graph snapshot: {error:#}; run ctx graph index first or select --graph-db PATH"),
    ))?;
    let result = (|| -> anyhow::Result<Value> {
        Ok(match operation {
            GraphOperation::Query { query, options } => {
                serde_json::to_value(store.query_extended(&query, &graph_options(options))?)?
            }
            GraphOperation::Show { symbol } => {
                let options = graph_options(GraphOptions {
                    depth: 0,
                    limit: 1,
                    direction: GraphDirection::Both,
                    relation: None,
                });
                serde_json::to_value(store.neighbors_resolved(&symbol, &options)?)?
            }
            GraphOperation::Callers { symbol, options } => {
                let mut options = graph_options(options);
                options.graph.direction = Direction::Incoming;
                options.graph.relation = Some("calls".to_owned());
                serde_json::to_value(store.neighbors_extended(&symbol, &options)?)?
            }
            GraphOperation::Callees { symbol, options } => {
                let mut options = graph_options(options);
                options.graph.direction = Direction::Outgoing;
                options.graph.relation = Some("calls".to_owned());
                serde_json::to_value(store.neighbors_extended(&symbol, &options)?)?
            }
            GraphOperation::Impact { symbol, options } => {
                serde_json::to_value(store.impact_extended(
                    &symbol,
                    &ImpactOptions {
                        search: graph_options(options),
                        ..ImpactOptions::default()
                    },
                )?)?
            }
            GraphOperation::Path {
                source,
                target,
                options,
            } => serde_json::to_value(store.path_extended(
                &source,
                &target,
                &graph_options(options),
            )?)?,
            GraphOperation::Stats => serde_json::to_value(store.stats()?)?,
        })
    })()
    .map_err(|error| failure(UnifiedErrorCode::GraphQuery, format!("{error:#}")))?;
    // Keep all native fields in both projections, including unresolved edges,
    // generation, found, seeds, and truncation reasons for text-only clients.
    let text = serde_json::to_string_pretty(&result)
        .map_err(|error| ToolBackendError::internal(error.to_string()))?;
    let mut outcome = ToolOutcome::plain(result);
    outcome.text = Some(format!("ctx graph\n{text}\n"));
    Ok(outcome)
}

fn validate_text(text: &str) -> Result<(), ToolBackendError> {
    if text.len() > MAX_OUTPUT_INPUT_BYTES {
        return Err(ToolBackendError::invalid_request(format!(
            "text must be at most {MAX_OUTPUT_INPUT_BYTES} UTF-8 bytes"
        )));
    }
    Ok(())
}

fn compact(text: &str) -> Result<ToolOutcome, ToolBackendError> {
    validate_text(text)?;
    let compactor =
        sift::Compactor::new().map_err(|error| ToolBackendError::internal(error.to_string()))?;
    let compacted = compactor.compact(text);
    let encoding = serde_json::to_value(compacted.encoding)
        .map_err(|error| ToolBackendError::internal(error.to_string()))?;
    output_result(
        compacted.text,
        json!({
            "encoding": encoding,
            "input_tokens": compacted.input_tokens,
            "output_tokens": compacted.output_tokens,
        }),
    )
}

fn restore(text: &str, encoding: &str) -> Result<ToolOutcome, ToolBackendError> {
    validate_text(text)?;
    let encoding = serde_json::from_value::<sift::Encoding>(json!(encoding))
        .map_err(|_| ToolBackendError::invalid_request("unsupported output encoding"))?;
    // Sift validates frame lengths and caps decoder amplification before allocation.
    let restored = sift::restore(encoding, text)
        .map_err(|error| failure(UnifiedErrorCode::OutputDecode, error.to_string()))?;
    output_result(restored, json!({"encoding": "raw"}))
}

fn output_result(text: String, mut metadata: Value) -> Result<ToolOutcome, ToolBackendError> {
    if text.len() > MAX_OUTPUT_TEXT_BYTES {
        return Err(failure(
            UnifiedErrorCode::OutputLimit,
            format!(
            "restored/compacted text exceeds {MAX_OUTPUT_TEXT_BYTES} UTF-8 bytes; use smaller input"
        ),
        ));
    }
    let mut rendered = format!(
        "ctx sift\nencoding: {}\n",
        metadata["encoding"].as_str().unwrap_or("raw")
    );
    if let (Some(input), Some(output)) = (
        metadata["input_tokens"].as_u64(),
        metadata["output_tokens"].as_u64(),
    ) {
        rendered.push_str(&format!("tokens: {input} -> {output}\n"));
    }
    rendered.push('\n');
    rendered.push_str(&text);
    metadata["text"] = json!(text);
    let mut outcome = ToolOutcome::plain(metadata);
    outcome.text = Some(rendered);
    Ok(outcome)
}
