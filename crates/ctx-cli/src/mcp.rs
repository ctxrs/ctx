use std::{io, path::PathBuf};

use anyhow::Result;
use clap::{Args, Subcommand};
use ctx_agent_application::mcp::{
    serve_stdio as serve_mcp_stdio, McpTelemetry, McpUsagePort, ProductIdentity,
};
use ctx_agent_integrations::{
    mcp::McpToolKind,
    tool_backend::{ToolBackend, ToolUsageFacts},
};
use ctx_client_observability::local_usage::{McpInvocation, McpUsageRecorder};
use serde_json::Value;

#[cfg(test)]
use {
    ctx_agent_integrations::mcp::{
        handle_protocol_message, McpHandled, McpServerIdentity, McpUsage, RequestDescriptor,
    },
    serde_json::json,
    std::path::Path,
};

pub(crate) mod text;
#[cfg(test)]
mod unified_tests;

use crate::{operation_descriptor::observed_mcp_product_operation, tool_backend::LocalToolBackend};

#[derive(Debug, Args)]
pub(crate) struct McpArgs {
    #[command(subcommand)]
    command: McpCommand,
}

#[derive(Debug, Subcommand)]
enum McpCommand {
    #[command(
        about = "Serve ctx tools over stdio",
        long_about = "Serve ctx tools over newline-delimited stdio JSON-RPC. By default, tools execute locally. With --server NAME, expose status, lexical search, exact event retrieval, and paginated session logs from the saved shared collection.\n\nExample:\n  printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{},\"clientInfo\":{\"name\":\"client\",\"version\":\"0\"}}}' | ctx mcp serve"
    )]
    Serve(McpServeArgs),
}

#[derive(Debug, Args)]
struct McpServeArgs {
    /// Local-only graph snapshot; defaults to the nearest ancestor's .graf/index.db.
    #[arg(long, value_name = "PATH")]
    graph_db: Option<PathBuf>,
}

pub(crate) fn run(args: McpArgs, data_root: PathBuf) -> Result<()> {
    match args.command {
        McpCommand::Serve(args) => serve_stdio(data_root, args.graph_db),
    }
}

pub(crate) fn run_remote(
    args: McpArgs,
    backend: crate::remote_history::RemoteBackend,
    data_root: PathBuf,
) -> Result<()> {
    let McpCommand::Serve(args) = args.command;
    anyhow::ensure!(
        args.graph_db.is_none(),
        "--graph-db is local-only; it cannot be combined with --server"
    );
    let stdin = io::stdin();
    let stdout = io::stdout();
    serve_remote_stdio_with_telemetry(
        &mut stdin.lock(),
        &mut stdout.lock(),
        &backend,
        crate::engine_telemetry::mcp(&data_root, true),
    )
}

#[cfg(test)]
pub(crate) fn serve_remote_stdio(
    input: &mut impl io::BufRead,
    output: &mut impl io::Write,
    backend: &impl ToolBackend,
) -> Result<()> {
    serve_remote_stdio_with_telemetry(
        input,
        output,
        backend,
        McpTelemetry::start(false, |_| Ok(())).for_remote(),
    )
}

fn serve_remote_stdio_with_telemetry(
    input: &mut impl io::BufRead,
    output: &mut impl io::Write,
    backend: &impl ToolBackend,
    telemetry: McpTelemetry,
) -> Result<()> {
    serve_mcp_stdio(
        input,
        output,
        ProductIdentity {
            name: "ctx",
            version: env!("CARGO_PKG_VERSION"),
        },
        backend,
        &ctx_agent_application::mcp::render_generic_tool_text,
        &mut RemoteUsage,
        telemetry,
    )
    .map_err(|failure| failure.into_error())
}

struct RemoteUsage;

impl McpUsagePort for RemoteUsage {
    fn record_delivered(
        &mut self,
        _operation: McpToolKind,
        _usage: ToolUsageFacts,
        _response: &Value,
        _encoded_response_bytes: usize,
        _duration: std::time::Duration,
    ) {
    }
}

fn serve_stdio(data_root: PathBuf, graph_db: Option<PathBuf>) -> Result<()> {
    let graph_db = graph_database_at_startup(graph_db);
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut stdin = stdin.lock();
    let mut stdout = stdout.lock();
    let mut control =
        crate::observability_composition::LocalUsageControlAuthority::new(data_root.clone());
    let recorder = McpUsageRecorder::start(
        crate::observability_composition::local_usage_storage_authority(&data_root),
        move || control.snapshot(),
    );
    let mut usage = LocalUsagePort { recorder };
    let backend = LocalToolBackend::new(data_root.clone()).with_graph_db(graph_db);
    let telemetry = product_telemetry(data_root);
    serve_mcp_stdio(
        &mut stdin,
        &mut stdout,
        ProductIdentity {
            name: "ctx",
            version: env!("CARGO_PKG_VERSION"),
        },
        &backend,
        &text::render_tool_text,
        &mut usage,
        telemetry,
    )
    .map_err(|failure| failure.into_error())
}

fn graph_database_at_startup(explicit: Option<PathBuf>) -> Result<Option<PathBuf>, String> {
    ctx_graph::optional_database(explicit.as_deref())
        .and_then(|path| match path {
            Some(path) if !path.is_absolute() => Ok(Some(std::env::current_dir()?.join(path))),
            path => Ok(path),
        })
        .map_err(|error| format!("{error:#}"))
}

struct LocalUsagePort {
    recorder: McpUsageRecorder,
}

impl McpUsagePort for LocalUsagePort {
    fn record_delivered(
        &mut self,
        operation: McpToolKind,
        usage: ToolUsageFacts,
        response: &Value,
        encoded_response_bytes: usize,
        duration: std::time::Duration,
    ) {
        self.recorder.record_delivered(duration, || {
            let operation = observed_mcp_product_operation(operation)?;
            let mut invocation = McpInvocation::from_operation(operation);
            invocation.bind_tool_usage(crate::observability_product::mcp_tool_usage(usage));
            let completion = crate::observability_product::mcp_completion_facts(
                operation,
                response,
                encoded_response_bytes,
            );
            Some((invocation, completion))
        });
    }
}

fn product_telemetry(data_root: PathBuf) -> McpTelemetry {
    crate::engine_telemetry::mcp(&data_root, false)
}

#[cfg(test)]
fn handle_message(
    message: Value,
    data_root: &Path,
    initialized: &mut bool,
) -> (McpHandled<Option<Value>>, Option<McpInvocation>) {
    let backend = LocalToolBackend::new(data_root.to_path_buf());
    let descriptor = RequestDescriptor::from_message(&message);
    let handled = handle_protocol_message(
        message,
        descriptor,
        initialized,
        McpServerIdentity {
            name: "ctx",
            version: env!("CARGO_PKG_VERSION"),
        },
        &backend,
        text::render_tool_text,
    );
    let invocation = handled.usage.clone().and_then(usage_invocation);
    (handled, invocation)
}

#[cfg(test)]
fn usage_invocation(usage: McpUsage) -> Option<McpInvocation> {
    let operation = observed_mcp_product_operation(usage.operation)?;
    let mut invocation = McpInvocation::from_operation(operation);
    invocation.bind_tool_usage(crate::observability_product::mcp_tool_usage(usage.facts));
    Some(invocation)
}

#[cfg(test)]
fn query_events_mcp_result_for_test(arguments: &Value, data_root: &Path) -> Result<Value> {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": McpToolKind::QueryEvents.tool_name(),
            "arguments": arguments,
        },
    });
    let (handled, _) = handle_message(request, data_root, &mut true);
    let result = handled
        .value
        .and_then(|response| response.get("result").cloned())
        .ok_or_else(|| anyhow::anyhow!("MCP query_events returned no result"))?;
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        return Err(anyhow::anyhow!(serde_json::to_string(&result)?));
    }
    Ok(result)
}

#[cfg(test)]
pub(crate) fn render_tool_text_for_test(value: &Value) -> String {
    text::render_tool_text(value)
}

#[cfg(test)]
mod tests {
    use super::{query_events_mcp_result_for_test, render_tool_text_for_test};
    use ctx_history_capture::{
        provider_source_for_path, refresh_source_backed_generation,
        register_landed_source_backed_route, SourceBackedProviderRegistry,
        SourceBackedRouteSelection,
    };
    use ctx_history_core::CaptureProvider;
    use ctx_history_index::WriterOptions;
    use serde_json::json;
    use std::fs;
    use tempfile::tempdir;

    const QUERY_EVENTS_CANARY: &str = "final-host-query-events-canary";

    fn write_query_events_fixture(data_root: &std::path::Path) {
        let sessions = data_root.join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        let records = [
            json!({
                "timestamp": "2026-08-11T12:00:00Z",
                "type": "session_meta",
                "payload": {
                    "id": "019fa000-0000-7000-8000-0000000000d1",
                    "timestamp": "2026-08-11T12:00:00Z",
                    "cwd": "/workspace/query-events",
                    "originator": "codex_cli_rs",
                    "cli_version": "0.1.0",
                    "source": "cli",
                    "model_provider": "openai"
                }
            }),
            json!({
                "timestamp": "2026-08-11T12:00:01Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": QUERY_EVENTS_CANARY}]
                }
            }),
        ];
        fs::write(
            sessions.join("query-events.jsonl"),
            records
                .iter()
                .map(|record| format!("{}\n", serde_json::to_string(record).unwrap()))
                .collect::<String>(),
        )
        .unwrap();
        let mut registry = SourceBackedProviderRegistry::new();
        register_landed_source_backed_route(
            &mut registry,
            provider_source_for_path(CaptureProvider::Codex, sessions),
            SourceBackedRouteSelection::ExplicitManual,
        )
        .unwrap();
        refresh_source_backed_generation(
            data_root.join("search/lexical"),
            &registry,
            WriterOptions::default(),
        )
        .unwrap();
    }

    #[test]
    fn query_events_mcp_transport_keeps_addressable_event_lineage_and_text_equal_to_show() {
        let temp = tempdir().unwrap();
        write_query_events_fixture(temp.path());

        let result = query_events_mcp_result_for_test(
            &json!({"content": "full", "limit": 100}),
            temp.path(),
        )
        .unwrap();
        let page = result
            .get("structuredContent")
            .expect("MCP query_events returns the event page as structured content");
        assert_eq!(page["payload_type"], "event_range_page");
        let event = page["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|event| event["text"] == QUERY_EVENTS_CANARY)
            .expect("MCP query_events keeps the imported event addressable");
        let event_id = event["ctx_event_id"].as_str().unwrap();
        let (shown, _) = ctx_history_cli::mcp_show_event_application(
            temp.path(),
            event_id,
            0,
            0,
            None,
            1024 * 1024,
        )
        .unwrap();
        let shown = &shown["event"];
        assert_eq!(event["session_relationship"], shown["session_relationship"]);
        assert_eq!(event["event_origin"], shown["event_origin"]);
        assert_eq!(event["text"], shown["text"]);
        assert_eq!(
            render_tool_text_for_test(page),
            format!(
                "ctx query_events\nevents: {}\ngeneration_id: {}\nterminal: true\ntruncated: false\n",
                page["events"].as_array().unwrap().len(),
                page["generation_id"].as_str().unwrap()
            )
        );
    }

    #[test]
    fn mcp_renderer_renders_unresolved_and_cyclic_lineage_exactly() {
        for (state, selected_depth) in [("unresolved", 1), ("cyclic", 2)] {
            let value = json!({
                "payload_type": "event_window",
                "ctx_event_id": "aaaaaaaa",
                "ctx_session_id": "bbbbbbbb",
                "events": [],
                "copied_lineage": {
                    "schema_version": 2,
                    "resolution": {"state": state},
                    "selected_depth": selected_depth,
                    "observed_count": 0,
                    "returned": 0,
                    "occurrences": [],
                    "truncated": false
                }
            });
            assert_eq!(
                render_tool_text_for_test(&value),
                format!(
                    "ctx show event\nctx_event_id: aaaaaaaa\nctx_session_id: bbbbbbbb\nevents: 0\n\ncopied lineage\nresolution: {state}, selected_depth={selected_depth}\ninherited_sessions: 0\n"
                )
            );
        }
    }
}
