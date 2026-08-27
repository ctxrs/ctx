use std::{io, path::PathBuf};

use anyhow::Result;
use clap::{Args, Subcommand};
use ctx_agent_application::mcp::{
    serve_stdio as serve_mcp_stdio, McpTelemetry, McpUsagePort, ProductIdentity,
};
use ctx_agent_integrations::{mcp::McpToolKind, tool_backend::ToolUsageFacts};
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

use super::config;
use crate::{
    analytics::PublicEventV1, operation_descriptor::observed_mcp_product_operation,
    tool_backend::LocalToolBackend,
};

#[derive(Debug, Args)]
pub(crate) struct McpArgs {
    #[command(subcommand)]
    command: McpCommand,
}

#[derive(Debug, Subcommand)]
enum McpCommand {
    #[command(
        about = "Serve local ctx tools over stdio",
        long_about = "Serve local ctx tools over newline-delimited stdio JSON-RPC. Core tool calls execute locally; companion-owned tool calls are forwarded as bounded opaque request and response bytes.\n\nExample:\n  printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{},\"clientInfo\":{\"name\":\"client\",\"version\":\"0\"}}}' | ctx mcp serve"
    )]
    Serve(McpServeArgs),
}

#[derive(Debug, Args)]
struct McpServeArgs {}

pub(crate) fn run(args: McpArgs, data_root: PathBuf) -> Result<()> {
    match args.command {
        McpCommand::Serve(_) => serve_stdio(data_root),
    }
}

fn serve_stdio(data_root: PathBuf) -> Result<()> {
    let daemon_config = config::AppConfig::load(&data_root)?;
    if daemon_config.automatic_indexing_enabled()
        && crate::semantic::daemon_autostart_suppression_reason().is_none()
    {
        let _ = crate::semantic::autostart_daemon_and_wait(
            &data_root,
            &daemon_config,
            crate::DaemonTriggerCommandArg::Search,
        );
    }
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
    let backend = LocalToolBackend::new(data_root.clone());
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

    fn record_companion_blame_delivered(
        &mut self,
        failed: bool,
        encoded_response_bytes: usize,
        duration: std::time::Duration,
    ) {
        self.recorder
            .record_companion_blame_delivered(failed, encoded_response_bytes, duration);
    }
}

fn product_telemetry(data_root: PathBuf) -> McpTelemetry {
    let enabled = config::AppConfig::load(&data_root).is_ok_and(|config| config.analytics.enabled);
    McpTelemetry::start(enabled, move |events: &[PublicEventV1]| {
        let Ok(config) = config::AppConfig::load(&data_root) else {
            return Ok(());
        };
        if !config.analytics.enabled {
            return Ok(());
        }
        crate::analytics::send_batch(&data_root, &config, events);
        Ok(())
    })
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
pub(crate) fn query_events_for_test(arguments: &Value, data_root: &Path) -> Result<Value> {
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
    let structured = result
        .get("structuredContent")
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("MCP query_events returned no structured content"))?;
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        return Err(anyhow::anyhow!(serde_json::to_string(&structured)?));
    }
    Ok(structured)
}

#[cfg(test)]
pub(crate) fn render_tool_text_for_test(value: &Value) -> String {
    text::render_tool_text(value)
}

#[cfg(test)]
mod tests {
    use super::{query_events_for_test, render_tool_text_for_test};
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

        let page =
            query_events_for_test(&json!({"content": "full", "limit": 100}), temp.path()).unwrap();
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
            render_tool_text_for_test(&page),
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
