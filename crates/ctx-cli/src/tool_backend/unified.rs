//! In-process, read-only graph and stateless output tool composition.
use super::{
    GraphDirection, GraphOperation, GraphOptions, ToolBackendError, ToolExecutionError,
    ToolOutcome, ToolUsageFacts, UnifiedErrorCode, UnifiedToolOperation, MAX_OUTPUT_INPUT_BYTES,
    MAX_OUTPUT_TEXT_BYTES,
};
use ctx_client_observability::analytics as telemetry;
use ctx_graph_core::{
    model::{Direction, QueryOptions},
    observation as graph_facts,
    query::{ImpactOptions, SearchOptions},
    store::Store,
};
use serde_json::{json, Value};
use std::{path::Path, time::Instant};

pub(super) fn execute(
    graph_db: Result<Option<&Path>, &str>,
    operation: UnifiedToolOperation,
) -> Result<ToolOutcome, ToolExecutionError> {
    match operation {
        UnifiedToolOperation::Graph(operation) => graph(graph_db, operation),
        UnifiedToolOperation::OutputCompact { text } => compact(&text),
        UnifiedToolOperation::OutputRestore { text, encoding } => restore(&text, &encoding),
    }
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
        // The engine owns traversal work and graph JSON accounting.
        token_budget: Some(64_000),
        ..SearchOptions::default()
    }
}
fn graph(
    path: Result<Option<&Path>, &str>,
    operation: GraphOperation,
) -> Result<ToolOutcome, ToolExecutionError> {
    use graph_facts::{GraphInvocation, GraphObservation, GraphOperation as O, GraphPhase};
    let started = Instant::now();
    let kind = match &operation {
        GraphOperation::Query { .. } => O::Search,
        GraphOperation::Show { .. } => O::Show,
        GraphOperation::Callers { .. } => O::Callers,
        GraphOperation::Callees { .. } => O::Callees,
        GraphOperation::Impact { .. } => O::Impact,
        GraphOperation::Path { .. } => O::Path,
        GraphOperation::Stats => O::Stats,
    };
    let mut facts = GraphObservation::new(kind, GraphInvocation::UnifiedMcp);
    let result = (|| {
        facts.phase = GraphPhase::Discover;
        let path =
            path.map_err(|error| (error, graph_facts::GraphFailureKind::Unknown))
                .and_then(|path| {
                    path.ok_or((
                        "no graph database found at server startup",
                        graph_facts::GraphFailureKind::MissingIndex,
                    ))
                })
                .map_err(|(error, kind)| {
                    facts.fail(kind);
                    failure(
                        UnifiedErrorCode::GraphUnavailable,
                        format!(
                    "{error}; start ctx mcp serve in an indexed project or use --graph-db PATH"),
                    )
                })?;
        facts.phase = GraphPhase::Open;
        let store = Store::open_read_only(path).map_err(|error| {
            graph_facts::failed(&mut facts, &error);
            failure(UnifiedErrorCode::GraphUnavailable, format!(
                "cannot read the selected graph snapshot: {error:#}; run ctx graph index first or select --graph-db PATH"))
        })?;
        facts.phase = GraphPhase::Query;
        let query_started = Instant::now();
        let queried = graph_query(&store, operation, &mut facts, query_started);
        facts
            .query_duration
            .get_or_insert_with(|| query_started.elapsed());
        let result = queried.map_err(|error| {
            graph_facts::failed(&mut facts, &error);
            failure(UnifiedErrorCode::GraphQuery, format!("{error:#}"))
        })?;
        let text = serde_json::to_string_pretty(&result).map_err(|error| {
            facts.fail(graph_facts::GraphFailureKind::Serialize);
            ToolBackendError::internal(error.to_string())
        })?;
        let mut outcome = ToolOutcome::plain(result);
        outcome.text = Some(format!("ctx graph\n{text}\n"));
        Ok(outcome)
    })();
    facts.duration = Some(started.elapsed());
    with_usage(
        result,
        ToolUsageFacts {
            graph: crate::engine_telemetry::graph_completed(facts),
            ..Default::default()
        },
    )
}
fn graph_query(
    store: &Store,
    operation: GraphOperation,
    facts: &mut graph_facts::GraphObservation,
    started: Instant,
) -> anyhow::Result<Value> {
    match operation {
        GraphOperation::Query { query, options } => {
            let result = store.query_extended(&query, &graph_options(options))?;
            graph_facts::search(facts, &result);
            graph_value(facts, started, result)
        }
        GraphOperation::Show { symbol } => {
            let options = graph_options(GraphOptions {
                depth: 0,
                limit: 1,
                direction: GraphDirection::Both,
                relation: None,
            });
            let result = store.neighbors_resolved(&symbol, &options)?;
            graph_facts::search(facts, &result);
            graph_value(facts, started, result)
        }
        GraphOperation::Callers { symbol, options } => {
            let mut options = graph_options(options);
            options.graph.direction = Direction::Incoming;
            options.graph.relation = Some("calls".to_owned());
            let result = store.neighbors_extended(&symbol, &options)?;
            graph_facts::search(facts, &result);
            graph_value(facts, started, result)
        }
        GraphOperation::Callees { symbol, options } => {
            let mut options = graph_options(options);
            options.graph.direction = Direction::Outgoing;
            options.graph.relation = Some("calls".to_owned());
            let result = store.neighbors_extended(&symbol, &options)?;
            graph_facts::search(facts, &result);
            graph_value(facts, started, result)
        }
        GraphOperation::Impact { symbol, options } => {
            let result = store.impact_extended(
                &symbol,
                &ImpactOptions {
                    search: graph_options(options),
                    ..ImpactOptions::default()
                },
            )?;
            graph_facts::search(facts, &result);
            graph_value(facts, started, result)
        }
        GraphOperation::Path {
            source,
            target,
            options,
        } => {
            let result = store.path_extended(&source, &target, &graph_options(options))?;
            graph_facts::path(facts, &result);
            graph_value(facts, started, result)
        }
        GraphOperation::Stats => {
            let result = store.stats()?;
            facts.stats(&result);
            graph_value(facts, started, result)
        }
    }
}
fn graph_value(
    facts: &mut graph_facts::GraphObservation,
    started: Instant,
    result: impl serde::Serialize,
) -> anyhow::Result<Value> {
    // Capture the typed engine result before JSON projection can fail.
    facts.query_duration = Some(started.elapsed());
    facts.execution_succeeded = Some(true);
    facts.phase = graph_facts::GraphPhase::Render;
    Ok(serde_json::to_value(result)?)
}
fn with_usage(
    result: Result<ToolOutcome, ToolBackendError>,
    usage: ToolUsageFacts,
) -> Result<ToolOutcome, ToolExecutionError> {
    match result {
        Ok(mut outcome) => {
            outcome.usage.merge(usage);
            Ok(outcome)
        }
        Err(error) => Err(ToolExecutionError {
            error: Box::new(error),
            usage: Box::new(usage),
        }),
    }
}
fn validate_text(text: &str) -> Result<(), ToolBackendError> {
    if text.len() > MAX_OUTPUT_INPUT_BYTES {
        return Err(ToolBackendError::invalid_request(format!(
            "text must be at most {MAX_OUTPUT_INPUT_BYTES} UTF-8 bytes"
        )));
    }
    Ok(())
}
fn compact(text: &str) -> Result<ToolOutcome, ToolExecutionError> {
    let started = Instant::now();
    let mut sample = output_sample(
        telemetry::SiftOperation::Compact,
        telemetry::SiftMode::Lossless,
    );
    let result = (|| {
        validate_text(text)?;
        let compactor = sift::Compactor::new().map_err(|error| {
            sample.cohort.failure_phase = Some(telemetry::SiftPhase::Codec);
            sample.cohort.failure_kind = Some(telemetry::SiftFailureKind::Tokenizer);
            ToolBackendError::internal(error.to_string())
        })?;
        let compacted = compactor.compact(text);
        // Candidate pairs stay in the carrier until the transport finalizer:
        // failed encoding/writing/flushing removes them before publication.
        sample.bytes = Some(telemetry::PresentedTotals {
            samples: 1,
            input: text.len() as u64,
            output: compacted.text.len() as u64,
        });
        sample.tokens = Some(telemetry::PresentedTotals {
            samples: 1,
            input: compacted.input_tokens as u64,
            output: compacted.output_tokens as u64,
        });
        let encoding = serde_json::to_value(compacted.encoding)
            .map_err(|error| ToolBackendError::internal(error.to_string()))?;
        output_result(
            compacted.text,
            json!({"encoding":encoding, "input_tokens":compacted.input_tokens, "output_tokens":compacted.output_tokens}),
        )
    })();
    output_usage(result, sample, started)
}
fn restore(text: &str, encoding: &str) -> Result<ToolOutcome, ToolExecutionError> {
    let started = Instant::now();
    let mut sample = output_sample(
        telemetry::SiftOperation::Restore,
        telemetry::SiftMode::Restore,
    );
    let result = (|| {
        validate_text(text)?;
        let encoding = serde_json::from_value::<sift::Encoding>(json!(encoding))
            .map_err(|_| ToolBackendError::invalid_request("unsupported output encoding"))?;
        let restored = sift::restore(encoding, text).map_err(|error| {
            sample.cohort.failure_phase = Some(telemetry::SiftPhase::Codec);
            sample.cohort.failure_kind = Some(telemetry::SiftFailureKind::InvalidInput);
            failure(UnifiedErrorCode::OutputDecode, error.to_string())
        })?;
        sample.bytes = Some(telemetry::PresentedTotals {
            samples: 1,
            input: text.len() as u64,
            output: restored.len() as u64,
        });
        sample.cohort.missingness = Some(telemetry::SiftMissingness::ViewNotTokenized);
        output_result(restored, json!({"encoding":"raw"}))
    })();
    output_usage(result, sample, started)
}
fn output_sample(
    operation: telemetry::SiftOperation,
    mode: telemetry::SiftMode,
) -> telemetry::SiftSummaryV1 {
    let mut sample = telemetry::SiftSummaryV1::new(operation, mode, std::time::Duration::ZERO);
    sample.observed = 1;
    sample.cohort = telemetry::SiftCohort {
        entry: Some(telemetry::SiftEntry::Mcp),
        terminal: Some(telemetry::SiftTerminal::Invocation),
        outcome: Some(telemetry::SiftTerminalOutcome::Success),
        delivery: Some(telemetry::SiftDelivery::NotAttempted),
        child: Some(telemetry::SiftChildOutcome::NotApplicable),
        ..Default::default()
    };
    sample
}
fn output_usage(
    result: Result<ToolOutcome, ToolBackendError>,
    mut sample: telemetry::SiftSummaryV1,
    started: Instant,
) -> Result<ToolOutcome, ToolExecutionError> {
    let mut latency = [0; 14];
    latency[telemetry::native_duration_index(started.elapsed())] = 1;
    sample.latency = Some(latency);
    if let Err(error) = &result {
        sample.execution_failed = 1;
        sample.cohort.outcome = Some(telemetry::SiftTerminalOutcome::Failure);
        if sample.cohort.failure_phase.is_none() {
            let invalid = matches!(error, ToolBackendError::InvalidRequest { .. });
            sample.cohort.failure_phase = Some(if invalid {
                telemetry::SiftPhase::Arguments
            } else {
                telemetry::SiftPhase::Render
            });
            sample.cohort.failure_kind = Some(if invalid {
                telemetry::SiftFailureKind::InvalidInput
            } else {
                telemetry::SiftFailureKind::Other
            });
        }
        sample.bytes = None;
        sample.tokens = None;
    }
    if sample.bytes.is_some() {
        sample.complete = 1;
    } else {
        sample.unmeasured = 1;
    }
    with_usage(
        result,
        ToolUsageFacts {
            sift: Some(sample),
            ..Default::default()
        },
    )
}
fn output_result(text: String, mut metadata: Value) -> Result<ToolOutcome, ToolBackendError> {
    if text.len() > MAX_OUTPUT_TEXT_BYTES {
        return Err(failure(UnifiedErrorCode::OutputLimit, format!("restored/compacted text exceeds {MAX_OUTPUT_TEXT_BYTES} UTF-8 bytes; use smaller input")));
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
#[cfg(test)]
mod telemetry_tests;
