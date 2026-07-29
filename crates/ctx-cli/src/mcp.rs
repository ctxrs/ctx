use std::{
    io::{self, BufRead, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
use clap::{Args, Subcommand};
use ctx_history_core::{database_path, EventType};
use ctx_history_store::{
    RawSqlOptions, Store, RAW_SQL_DEFAULT_MAX_COLUMNS, RAW_SQL_DEFAULT_MAX_ROWS,
    RAW_SQL_DEFAULT_MAX_SQL_BYTES, RAW_SQL_DEFAULT_MAX_VALUE_BYTES, RAW_SQL_DEFAULT_TIMEOUT,
    RAW_SQL_MAX_COLUMNS_CAP, RAW_SQL_MAX_ROWS_CAP, RAW_SQL_MAX_SQL_BYTES_CAP, RAW_SQL_MAX_TIMEOUT,
    RAW_SQL_MAX_VALUE_BYTES_CAP,
};
use serde_json::{json, Value};

mod arguments;
mod input;
mod pro;
mod response;
mod response_bound;
mod show;
mod telemetry;
mod text;

use arguments::{
    allowed_tool_arguments, duration_millis_u64, optional_bool, optional_f32, optional_provider,
    optional_search_backend, optional_string, optional_transcript_mode, optional_usize,
    validate_argument_keys, validate_search_filter_arguments,
};
use input::{read_mcp_input_line, McpInputLine};
use pro::{
    pro_blame_tool, required_blame_target, tool_pro_blame, tool_pro_status,
    MCP_BLAME_MAX_OUTPUT_BYTES,
};
use response::{
    error_response, invalid_request_response, invalid_tool_request, json_rpc_error,
    success_response, tool_error_result, tool_result,
};
use response_bound::{
    bound_blame_mcp_response, bound_complete_content_mcp_response, is_blame_tool_call,
    is_complete_content_tool_call,
};
use show::{tool_show_event, tool_show_session};
use telemetry::{McpHandled, McpTelemetry, RequestDescriptor};
use text::render_tool_text;

use super::{
    compact_json, config, config::CONFIG_FILE, discovered_plugin_sources_json, raw_sql_result_json,
    search_filters, search_has_intent, sources_json, ProviderArg, RefreshArg, SearchDto,
    SearchFilterInput, SearchIntentInput, SearchRefreshReport, SourceIdentityFilterArgs,
    TranscriptMode, MAX_EVENT_WINDOW, MAX_SEARCH_LIMIT,
};
use crate::analytics::{McpErrorClassV1, McpStopReasonV1, Outcome};
use crate::commands::search::resolve_search_backend;
use crate::complete_content::MCP_COMPLETE_CONTENT_MAX_OUTPUT_BYTES;
use crate::local_usage::{McpInvocation, McpUsageRecorder};
use crate::provider_sources::{discovered_sources_report, discovery_report_issues_json};
use crate::semantic::{
    daemon_report, search_packet_with_backend, semantic_worker_report_cached,
    semantic_worker_report_configured_json,
};
use crate::store_util::open_existing_store_read_only;

const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
const MCP_SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &[MCP_PROTOCOL_VERSION, "2025-06-18"];
const MCP_MAX_LINE_BYTES: usize = 1024 * 1024;
const MCP_MAX_SESSION_EVENTS: usize = 200;

#[derive(Debug, Args)]
pub(crate) struct McpArgs {
    #[command(subcommand)]
    command: McpCommand,
}

#[derive(Debug, Subcommand)]
enum McpCommand {
    #[command(
        about = "Serve local ctx tools over stdio",
        long_about = "Serve local ctx tools over newline-delimited stdio JSON-RPC. Blame may perform bounded local catch-up that updates the canonical Core index, writes the encrypted derived Pro graph, and writes the projection acknowledgement. It never writes provider history or repositories. pro_status remains read-only.\n\nExample:\n  printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{},\"clientInfo\":{\"name\":\"client\",\"version\":\"0\"}}}' | ctx mcp serve"
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
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut stdin = stdin.lock();
    let mut stdout = stdout.lock();
    let mut telemetry = McpTelemetry::start(data_root.clone());
    let mut usage_recorder = McpUsageRecorder::start(data_root.clone());
    let started = Instant::now();
    let mut initialized = false;

    let result = serve_stdio_loop(
        &data_root,
        &mut stdin,
        &mut stdout,
        &mut initialized,
        &mut telemetry,
        &mut usage_recorder,
    );
    let (reason, outcome) = match &result {
        Ok(()) => (McpStopReasonV1::Eof, Outcome::Success),
        Err(failure) => (failure.reason, Outcome::Failure),
    };
    telemetry.stop(reason, outcome, started.elapsed());
    result.map_err(|failure| failure.error)
}

struct McpServeFailure {
    reason: McpStopReasonV1,
    error: anyhow::Error,
}

fn serve_stdio_loop(
    data_root: &Path,
    stdin: &mut impl BufRead,
    stdout: &mut impl Write,
    initialized: &mut bool,
    telemetry: &mut McpTelemetry,
    usage_recorder: &mut McpUsageRecorder,
) -> std::result::Result<(), McpServeFailure> {
    loop {
        let input = read_mcp_input_line(stdin).map_err(|error| McpServeFailure {
            reason: McpStopReasonV1::StdinReadError,
            error,
        })?;
        let Some(input) = input else {
            return Ok(());
        };
        let request_started = Instant::now();
        let (handled, descriptor, usage_invocation) = match input {
            McpInputLine::Line(line) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                match serde_json::from_str::<Value>(line) {
                    Ok(message) => {
                        let descriptor = RequestDescriptor::from_message(&message);
                        let (handled, usage_invocation) =
                            handle_message(message, data_root, initialized);
                        (handled, descriptor, usage_invocation)
                    }
                    Err(err) => (
                        McpHandled::plain(Some(error_response(
                            Value::Null,
                            -32700,
                            "Parse error",
                            Some(json!({ "error": err.to_string() })),
                        ))),
                        RequestDescriptor::InvalidJson,
                        None,
                    ),
                }
            }
            McpInputLine::InvalidUtf8 => (
                McpHandled::plain(Some(error_response(
                    Value::Null,
                    -32700,
                    "Parse error",
                    Some(json!({ "error": "MCP message is not valid UTF-8" })),
                ))),
                RequestDescriptor::InvalidUtf8,
                None,
            ),
            McpInputLine::TooLarge => (
                McpHandled::plain(Some(error_response(
                    Value::Null,
                    -32700,
                    "Parse error",
                    Some(json!({
                        "error": format!("MCP message exceeds max line bytes ({MCP_MAX_LINE_BYTES})")
                    })),
                ))),
                RequestDescriptor::LineTooLarge,
                None,
            ),
        };
        let McpHandled {
            value: response,
            pro_event,
        } = handled;
        if let Some(response) = response {
            let encoded = serde_json::to_string(&response).map_err(|error| {
                telemetry.record_response_failure(
                    descriptor,
                    request_started.elapsed(),
                    McpErrorClassV1::ResponseSerialize,
                );
                McpServeFailure {
                    reason: McpStopReasonV1::ResponseSerializeError,
                    error: error.into(),
                }
            })?;
            writeln!(stdout, "{encoded}").map_err(|error| {
                telemetry.record_response_failure(
                    descriptor,
                    request_started.elapsed(),
                    McpErrorClassV1::ResponseWrite,
                );
                McpServeFailure {
                    reason: McpStopReasonV1::StdoutWriteError,
                    error: error.into(),
                }
            })?;
            stdout.flush().map_err(|error| {
                telemetry.record_response_failure(
                    descriptor,
                    request_started.elapsed(),
                    McpErrorClassV1::ResponseFlush,
                );
                McpServeFailure {
                    reason: McpStopReasonV1::StdoutFlushError,
                    error: error.into(),
                }
            })?;
            let duration = request_started.elapsed();
            if let Some(invocation) = usage_invocation {
                let serialized_response_bytes = encoded.len().saturating_add(1);
                usage_recorder.record_delivered(
                    invocation,
                    &response,
                    duration,
                    serialized_response_bytes,
                );
            }
            telemetry.record_delivered(descriptor, Some(&response), duration);
            if let Some(event) = pro_event {
                telemetry.submit_pro_event(event);
            }
        } else {
            telemetry.record_delivered(descriptor, None, request_started.elapsed());
        }
    }
}

fn handle_message(
    message: Value,
    data_root: &Path,
    initialized: &mut bool,
) -> (McpHandled<Option<Value>>, Option<McpInvocation>) {
    let Some(object) = message.as_object() else {
        return (
            McpHandled::plain(Some(error_response(
                Value::Null,
                -32600,
                "Invalid Request",
                None,
            ))),
            None,
        );
    };
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return (
            McpHandled::plain(Some(invalid_request_response(object.get("id")))),
            None,
        );
    }
    let bound_complete_content = is_complete_content_tool_call(&message);
    let bound_blame = is_blame_tool_call(&message);
    let id = message
        .as_object()
        .and_then(|object| object.get("id"))
        .cloned();
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return (
            McpHandled::plain(Some(invalid_request_response(id.as_ref()))),
            None,
        );
    };
    if matches!(
        id,
        Some(Value::Null | Value::Bool(_) | Value::Array(_) | Value::Object(_))
    ) {
        return (
            McpHandled::plain(Some(invalid_request_response(None))),
            None,
        );
    }
    let Some(id) = id else {
        if method == "notifications/initialized" {
            *initialized = true;
        }
        return (McpHandled::plain(None), None);
    };
    let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
    if !params.is_object() {
        return (
            McpHandled::plain(Some(error_response(
                id,
                -32602,
                "Invalid params",
                Some(json!({ "error": "params must be an object" })),
            ))),
            None,
        );
    }
    if method != "initialize" && !*initialized {
        return (
            McpHandled::plain(Some(error_response(
                id,
                -32002,
                "Server not initialized",
                Some(json!({ "error": "send initialize before calling ctx MCP tools" })),
            ))),
            None,
        );
    }
    let (result, usage_invocation) = match method {
        "initialize" => {
            *initialized = true;
            (Ok(McpHandled::plain(initialize_result(&params))), None)
        }
        "ping" => (Ok(McpHandled::plain(json!({}))), None),
        "tools/list" => (
            Ok(McpHandled::plain(json!({ "tools": tool_definitions() }))),
            None,
        ),
        "tools/call" => handle_tools_call(params, data_root),
        _ => (Err(json_rpc_error(-32601, "Method not found", None)), None),
    };
    let response_id = id.clone();
    let McpHandled {
        value: response,
        pro_event,
    } = match result {
        Ok(handled) => McpHandled {
            value: success_response(id, handled.value),
            pro_event: handled.pro_event,
        },
        Err(error) => McpHandled::plain({
            if let Some(object) = error.as_object() {
                let code = object.get("code").and_then(Value::as_i64).unwrap_or(-32603);
                let message = object
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("Internal error");
                let data = object.get("data").cloned();
                error_response(id, code, message, data)
            } else {
                error_response(id, -32603, "Internal error", Some(error))
            }
        }),
    };
    (
        McpHandled {
            value: Some(if bound_complete_content {
                bound_complete_content_mcp_response(
                    response,
                    response_id,
                    MCP_COMPLETE_CONTENT_MAX_OUTPUT_BYTES,
                )
            } else if bound_blame {
                bound_blame_mcp_response(response, response_id, MCP_BLAME_MAX_OUTPUT_BYTES)
            } else {
                response
            }),
            pro_event,
        },
        usage_invocation,
    )
}

fn initialize_result(params: &Value) -> Value {
    let protocol_version = negotiate_protocol_version(params);
    json!({
        "protocolVersion": protocol_version,
        "capabilities": {
            "tools": {
                "listChanged": false
            }
        },
        "serverInfo": {
            "name": "ctx",
            "version": env!("CARGO_PKG_VERSION")
        },
        "instructions": "Local access to the ctx index and optional Pro work graph. Tool output may include absolute paths, source metadata, snippets, transcript text, and raw SQL query results; MCP hosts may log or forward it. Blame may perform bounded local catch-up that updates the canonical Core index, writes the encrypted derived Pro graph, and writes the projection acknowledgement. It never writes provider history or repositories. pro_status and the other tools are read-only."
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

fn handle_tools_call(
    params: Value,
    data_root: &Path,
) -> (Result<McpHandled<Value>, Value>, Option<McpInvocation>) {
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return (
            Err(json_rpc_error(
                -32602,
                "Invalid params",
                Some(json!({ "error": "tools/call requires params.name" })),
            )),
            None,
        );
    };
    let Some(allowed_arguments) = allowed_tool_arguments(name) else {
        return (
            Err(json_rpc_error(
                -32602,
                "Invalid params",
                Some(json!({ "error": format!("unknown tool {name}") })),
            )),
            None,
        );
    };
    let mut invocation =
        McpInvocation::recognized(name).expect("allowed MCP tools have local usage operations");
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !arguments.is_object() {
        return (
            Err(json_rpc_error(
                -32602,
                "Invalid params",
                Some(json!({ "error": "tools/call params.arguments must be an object" })),
            )),
            Some(invocation),
        );
    }

    if let Err(error) = validate_argument_keys(&arguments, allowed_arguments) {
        return (
            Ok(McpHandled::plain(tool_error_result(error))),
            Some(invocation),
        );
    }

    let handled = match name {
        "status" => McpHandled::plain(tool_status(data_root)),
        "sources" => McpHandled::plain(tool_sources(data_root)),
        "search" => McpHandled::plain(tool_search(&arguments, data_root)),
        "sql" => McpHandled::plain(tool_sql(&arguments, data_root)),
        "show_session" => McpHandled::plain(tool_show_session(&arguments, data_root)),
        "show_event" => McpHandled::plain(tool_show_event(&arguments, data_root)),
        "pro_status" => tool_pro_status(data_root),
        "blame" => {
            let parsed_target = required_blame_target(&arguments);
            if let Ok(target) = &parsed_target {
                invocation.bind_blame_target(target);
            }
            tool_pro_blame(&arguments, data_root, parsed_target)
        }
        _ => unreachable!("known tool was validated above"),
    };

    (
        Ok(McpHandled {
            value: match handled.value {
                Ok(value) => tool_result(value),
                Err(err) => tool_error_result(err),
            },
            pro_event: handled.pro_event,
        }),
        Some(invocation),
    )
}

fn tool_status(data_root: &Path) -> Result<Value> {
    let db_path = database_path(data_root.to_path_buf());
    let initialized = db_path.exists();
    let config = config::AppConfig::load(data_root)?;
    let (
        indexed_items,
        indexed_sessions,
        indexed_events,
        indexed_sources,
        cataloged_sessions,
        indexed_catalog_sessions,
        pending_catalog_sessions,
        failed_catalog_sessions,
        stale_catalog_sessions,
        source_import_files,
        indexed_source_import_files,
        pending_source_import_files,
        failed_source_import_files,
        stale_source_import_files,
        semantic,
        daemon,
    ) = if initialized {
        let store = open_existing_store_read_only(&db_path, "ctx mcp status")?;
        let catalog_counts = store.catalog_session_counts()?;
        let source_import_file_counts = store.source_import_file_counts()?;
        let indexed_counts = store.indexed_history_counts()?;
        let semantic_report = semantic_worker_report_cached(data_root, Some(&store))?;
        let daemon = daemon_report(data_root, &semantic_report);
        let semantic = semantic_worker_report_configured_json(&config, &semantic_report);
        (
            indexed_counts.items(),
            indexed_counts.sessions,
            indexed_counts.events,
            store.capture_source_count()?,
            catalog_counts.total,
            catalog_counts.indexed,
            catalog_counts.pending,
            catalog_counts.failed,
            catalog_counts.stale,
            source_import_file_counts.total,
            source_import_file_counts.indexed,
            source_import_file_counts.pending,
            source_import_file_counts.failed,
            source_import_file_counts.stale,
            semantic,
            daemon,
        )
    } else {
        let semantic_report = semantic_worker_report_cached(data_root, None)?;
        let daemon = daemon_report(data_root, &semantic_report);
        (
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            semantic_worker_report_configured_json(&config, &semantic_report),
            daemon,
        )
    };
    let inventory_units = cataloged_sessions.saturating_add(source_import_files);
    let pending_inventory_units =
        pending_catalog_sessions.saturating_add(pending_source_import_files);
    let failed_inventory_units = failed_catalog_sessions.saturating_add(failed_source_import_files);
    let stale_inventory_units = stale_catalog_sessions.saturating_add(stale_source_import_files);

    Ok(json!({
        "schema_version": 1,
        "initialized": initialized,
        "data_root": data_root,
        "database_path": db_path,
        "config_path": data_root.join(CONFIG_FILE),
        "indexed_items": indexed_items,
        "indexed_sessions": indexed_sessions,
        "indexed_events": indexed_events,
        "indexed_sources": indexed_sources,
        "inventory_units": inventory_units,
        "pending_inventory_units": pending_inventory_units,
        "failed_inventory_units": failed_inventory_units,
        "stale_inventory_units": stale_inventory_units,
        "cataloged_sessions": cataloged_sessions,
        "indexed_catalog_sessions": indexed_catalog_sessions,
        "pending_catalog_sessions": pending_catalog_sessions,
        "failed_catalog_sessions": failed_catalog_sessions,
        "stale_catalog_sessions": stale_catalog_sessions,
        "source_import_files": source_import_files,
        "indexed_source_import_files": indexed_source_import_files,
        "pending_source_import_files": pending_source_import_files,
        "failed_source_import_files": failed_source_import_files,
        "stale_source_import_files": stale_source_import_files,
        "semantic": semantic,
        "daemon": daemon,
        "local_only": true,
        "read_only": true,
    }))
}

fn tool_sources(data_root: &Path) -> Result<Value> {
    let report = discovered_sources_report();
    let mut source_values = sources_json(&report.sources);
    source_values.extend(discovered_plugin_sources_json(data_root)?);
    let (issues, issues_truncated) = discovery_report_issues_json(&report);
    Ok(json!({
        "schema_version": 1,
        "sources": source_values,
        "issues": issues,
        "issues_truncated": issues_truncated,
        "read_only": true,
    }))
}

fn tool_search(arguments: &Value, data_root: &Path) -> Result<Value> {
    let query = optional_string(arguments, "query")?.unwrap_or_default();
    let limit = optional_usize(arguments, "limit")?.unwrap_or(20);
    if !(1..=MAX_SEARCH_LIMIT).contains(&limit) {
        return Err(invalid_tool_request(format!(
            "limit must be between 1 and {MAX_SEARCH_LIMIT}"
        )));
    }
    let provider = optional_provider(arguments, "provider")?;
    let history_source = optional_string(arguments, "history_source")?;
    let provider_key = optional_string(arguments, "provider_key")?;
    let source_id = optional_string(arguments, "source_id")?;
    let source_format = optional_string(arguments, "source_format")?;
    let session = optional_string(arguments, "session")?;
    let workspace = optional_string(arguments, "workspace")?;
    let since = optional_string(arguments, "since")?;
    let primary_only = optional_bool(arguments, "primary_only")?.unwrap_or(false);
    let include_subagents = optional_bool(arguments, "include_subagents")?.unwrap_or(false);
    let event_type = optional_string(arguments, "event_type")?;
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
    if !search_has_intent(SearchIntentInput {
        query: Some(&query),
        terms: &[],
        file: file.as_deref(),
    }) {
        return Err(invalid_tool_request("search needs a query or file"));
    }
    if crate::commands::source_index::index_is_available(data_root) {
        return crate::commands::source_index::mcp_search(
            crate::commands::source_index::SourceSearchRequest {
                query,
                terms: Vec::new(),
                limit,
                provider: provider.map(ProviderArg::capture_provider),
                history_source: source_identity.history_source,
                provider_key: source_identity.provider_key,
                source_id: source_identity.source_id,
                source_format: source_identity.source_format,
                workspace,
                since,
                primary_only,
                include_subagents,
                event_type,
                file,
                session,
                events,
                include_current_session,
                backend,
                refresh: RefreshArg::Off,
            },
            data_root,
        );
    }
    validate_search_filter_arguments(
        provider.as_ref(),
        &source_identity,
        session.as_deref(),
        since.as_deref(),
        event_type.as_deref(),
    )?;
    let config = config::AppConfig::load(data_root)?;
    let backend = resolve_search_backend(backend, &config)?;
    let store = open_existing_store(data_root)?;

    let options = ctx_history_search::PacketOptions {
        limit,
        filters: search_filters(
            SearchFilterInput {
                session,
                provider,
                source_identity,
                workspace,
                since,
                primary_only,
                include_subagents,
                event_type,
                file,
                include_current_session,
            },
            Some(&store),
        )?,
        result_mode: if events {
            ctx_history_search::SearchResultMode::Events
        } else {
            ctx_history_search::SearchResultMode::Sessions
        },
        ..ctx_history_search::PacketOptions::default()
    };
    let (packet, retrieval) = search_packet_with_backend(
        &store,
        data_root,
        &query,
        &[],
        &options,
        backend,
        config.semantic_search_enabled(),
        semantic_weight,
        RefreshArg::Off,
        false,
    )?;
    let refresh = SearchRefreshReport::skipped(RefreshArg::Off, "skipped");
    Ok(SearchDto::packet(
        &store,
        &packet,
        &refresh,
        &retrieval,
        Some(&query),
    ))
}

fn tool_sql(arguments: &Value, data_root: &Path) -> Result<Value> {
    let sql = optional_string(arguments, "sql")?
        .ok_or_else(|| invalid_tool_request("sql is required"))?;
    let max_rows = optional_usize(arguments, "max_rows")?.unwrap_or(RAW_SQL_DEFAULT_MAX_ROWS);
    let max_columns =
        optional_usize(arguments, "max_columns")?.unwrap_or(RAW_SQL_DEFAULT_MAX_COLUMNS);
    let max_value_bytes =
        optional_usize(arguments, "max_value_bytes")?.unwrap_or(RAW_SQL_DEFAULT_MAX_VALUE_BYTES);
    let max_sql_bytes =
        optional_usize(arguments, "max_sql_bytes")?.unwrap_or(RAW_SQL_DEFAULT_MAX_SQL_BYTES);
    let timeout_ms = optional_usize(arguments, "timeout_ms")?
        .map(|value| {
            u64::try_from(value).map_err(|_| invalid_tool_request("timeout_ms is too large"))
        })
        .transpose()?
        .unwrap_or_else(|| duration_millis_u64(RAW_SQL_DEFAULT_TIMEOUT));
    let store = open_existing_store(data_root)?;
    let result = store.raw_sql_query(
        &sql,
        RawSqlOptions {
            max_rows,
            max_columns,
            max_value_bytes,
            max_sql_bytes,
            timeout: Duration::from_millis(timeout_ms),
        },
    )?;
    Ok(raw_sql_result_json(&result))
}

fn open_existing_store(data_root: &Path) -> Result<Store> {
    let db_path = database_path(data_root.to_path_buf());
    if !db_path.exists() {
        return Err(anyhow!(
            "ctx store is not initialized at {}; run `ctx setup` or `ctx import` first",
            db_path.display()
        ));
    }
    Store::open_read_only(&db_path)
        .with_context(|| format!("open read-only ctx store {}", db_path.display()))
}

fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "status",
            "title": "Status",
            "description": "Return local ctx index status without writing to provider history or repositories.",
            "inputSchema": object_schema(json!({}), vec![]),
            "annotations": { "readOnlyHint": true },
        }),
        json!({
            "name": "sources",
            "title": "Sources",
            "description": "List discovered local agent history sources.",
            "inputSchema": object_schema(json!({}), vec![]),
            "annotations": { "readOnlyHint": true },
        }),
        json!({
            "name": "search",
            "title": "Search",
            "description": "Search the existing local ctx index by query text or touched-file path. This does not refresh or import provider history.",
            "inputSchema": object_schema(json!({
                "query": { "type": "string", "description": "Non-empty text query. Required unless file is provided." },
                "limit": { "type": "integer", "minimum": 1, "maximum": MAX_SEARCH_LIMIT, "default": 20 },
                "provider": { "type": "string", "enum": provider_names() },
                "history_source": { "type": "string", "description": "Custom history source selector as plugin/source or provider_key/source_id." },
                "provider_key": { "type": "string", "description": "Custom history provider_key." },
                "source_id": { "type": "string", "description": "Custom history source_id." },
                "source_format": { "type": "string", "description": "Custom history source_format." },
                "workspace": { "type": "string", "description": "Workspace path or name text." },
                "since": { "type": "string", "description": "RFC3339 timestamp or day window such as 30d." },
                "include_subagents": { "type": "boolean", "default": false, "description": "Include subagent sessions in addition to primary-agent sessions." },
                "event_type": { "type": "string", "enum": event_type_names() },
                "file": { "type": "string", "description": "Indexed touched-file path. Required unless query is provided." },
                "session": { "type": "string", "description": "ctx session id." },
                "events": { "type": "boolean", "default": false },
                "include_current_session": { "type": "boolean", "default": false, "description": "Include the active Codex session tree when CODEX_THREAD_ID is set." },
                "backend": { "type": "string", "enum": ["hybrid", "semantic", "lexical"], "description": "Optional backend override. Defaults to lexical unless local semantic search is enabled in ctx config, then hybrid." },
                "semantic_weight": { "type": "number", "minimum": 0.0, "maximum": 1.0, "default": 0.35 }
            }), vec![]),
            "annotations": { "readOnlyHint": true },
        }),
        json!({
            "name": "sql",
            "title": "SQL",
            "description": "Run one read-only SQL statement against the existing local ctx index. Prefer stable ctx_* views for scripts.",
            "inputSchema": object_schema(json!({
                "sql": { "type": "string", "description": "Single read-only SQL statement." },
                "max_rows": { "type": "integer", "minimum": 1, "maximum": RAW_SQL_MAX_ROWS_CAP, "default": RAW_SQL_DEFAULT_MAX_ROWS },
                "max_columns": { "type": "integer", "minimum": 1, "maximum": RAW_SQL_MAX_COLUMNS_CAP, "default": RAW_SQL_DEFAULT_MAX_COLUMNS },
                "max_value_bytes": { "type": "integer", "minimum": 1, "maximum": RAW_SQL_MAX_VALUE_BYTES_CAP, "default": RAW_SQL_DEFAULT_MAX_VALUE_BYTES },
                "max_sql_bytes": { "type": "integer", "minimum": 1, "maximum": RAW_SQL_MAX_SQL_BYTES_CAP, "default": RAW_SQL_DEFAULT_MAX_SQL_BYTES },
                "timeout_ms": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": duration_millis_u64(RAW_SQL_MAX_TIMEOUT),
                    "default": duration_millis_u64(RAW_SQL_DEFAULT_TIMEOUT)
                }
            }), vec!["sql"]),
            "annotations": { "readOnlyHint": true },
        }),
        json!({
            "name": "show_session",
            "title": "Show Session",
            "description": "Return an indexed session transcript by ctx session id.",
            "inputSchema": object_schema(json!({
                "ctx_session_id": { "type": "string" },
                "mode": { "type": "string", "enum": ["full", "lite", "log"], "default": "lite" },
                "content": { "type": "string", "enum": ["indexed", "complete"], "default": "indexed", "description": "Complete explicitly reads verified local provider sources and caps the final serialized JSON-RPC response at 8 MiB. MCP hosts may log or forward returned transcript content." }
            }), vec!["ctx_session_id"]),
            "annotations": { "readOnlyHint": true },
        }),
        json!({
            "name": "show_event",
            "title": "Show Event",
            "description": "Return an indexed event and optional surrounding event window by ctx event id.",
            "inputSchema": object_schema(json!({
                "ctx_event_id": { "type": "string" },
                "before": { "type": "integer", "minimum": 0, "default": 0 },
                "after": { "type": "integer", "minimum": 0, "default": 0 },
                "window": { "type": "integer", "minimum": 0 },
                "content": { "type": "string", "enum": ["indexed", "complete"], "default": "indexed", "description": "Complete explicitly reads verified local provider sources and caps the final serialized JSON-RPC response at 8 MiB. MCP hosts may log or forward returned transcript content." }
            }), vec!["ctx_event_id"]),
            "annotations": { "readOnlyHint": true },
        }),
        json!({
            "name": "pro_status",
            "title": "Pro Status",
            "description": "Inspect local ctx Pro readiness, protocol capabilities, and nonsecret access state/deadlines.",
            "inputSchema": object_schema(json!({}), vec![]),
            "annotations": { "readOnlyHint": true },
        }),
        pro_blame_tool(),
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

fn provider_names() -> Vec<&'static str> {
    ProviderArg::mcp_names()
}

fn event_type_names() -> Vec<&'static str> {
    vec![
        EventType::Message.as_str(),
        EventType::ToolCall.as_str(),
        EventType::ToolOutput.as_str(),
        EventType::CommandStarted.as_str(),
        EventType::CommandOutput.as_str(),
        EventType::CommandFinished.as_str(),
        EventType::FileTouched.as_str(),
        EventType::VcsChange.as_str(),
        EventType::Artifact.as_str(),
        EventType::Summary.as_str(),
        EventType::Notice.as_str(),
    ]
}
