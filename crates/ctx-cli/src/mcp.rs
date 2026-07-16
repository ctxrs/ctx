use std::{
    io::{self, BufRead, Write},
    path::{Path, PathBuf},
    time::Duration,
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
use ctx_protocol::{
    AgentHistoryErrorBody, AgentHistoryErrorCode, JsonObject,
    SEARCH_MAX_ANALYZED_TOKENS_PER_CLAUSE, SEARCH_MAX_CLAUSES, SEARCH_MAX_CLAUSE_BYTES,
    SEARCH_MAX_LITERAL_BYTES, SEARCH_MAX_QUERY_JSON_BYTES, SEARCH_MAX_RESULTS,
    SEARCH_MAX_SERIALIZED_RESPONSE_BYTES, SEARCH_MAX_TOTAL_CLAUSE_BYTES, SEARCH_MIN_LITERAL_BYTES,
};
use serde_json::{json, Value};
use uuid::Uuid;

mod text;

use text::render_tool_text;

use super::{
    cli_supported_provider, compact_json, config, config::CONFIG_FILE,
    discovered_plugin_sources_json, discovered_sources, event_window, event_window_json,
    raw_sql_result_json, search_filters, session_transcript_json, sources_json, OutputFormat,
    ProviderArg, RefreshArg, SearchBackendArg, SearchDto, SearchFilterInput, SearchRefreshReport,
    SourceIdentityFilterArgs, TranscriptMode, MAX_EVENT_WINDOW,
};
use crate::commands::search::resolve_search_backend;
use crate::search_query_input::parse_search_query_value;
use crate::semantic::{
    daemon_report, search_packet_file_filter_with_backend, search_packet_query_with_backend,
    semantic_worker_report_cached, semantic_worker_report_configured_json,
};
use crate::store_util::open_existing_store_read_only;

const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
const MCP_SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &[MCP_PROTOCOL_VERSION, "2025-06-18"];
const MCP_MAX_LINE_BYTES: usize = 1024 * 1024;
const MCP_MAX_SESSION_EVENTS: usize = 200;

enum McpInputLine {
    Line(String),
    InvalidUtf8,
    TooLarge,
}

#[derive(Debug, Args)]
pub(crate) struct McpArgs {
    #[command(subcommand)]
    command: McpCommand,
}

#[derive(Debug, Subcommand)]
enum McpCommand {
    #[command(
        about = "Serve a read-only MCP server over stdio",
        long_about = "Serve a read-only MCP server over newline-delimited stdio JSON-RPC.\n\nExample:\n  printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{},\"clientInfo\":{\"name\":\"client\",\"version\":\"0\"}}}' | ctx mcp serve"
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
    let mut initialized = false;

    while let Some(input) = read_mcp_input_line(&mut stdin)? {
        let response = match input {
            McpInputLine::Line(line) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                handle_line(line, &data_root, &mut initialized)
            }
            McpInputLine::InvalidUtf8 => Some(error_response(
                Value::Null,
                -32700,
                "Parse error",
                Some(json!({ "error": "MCP message is not valid UTF-8" })),
            )),
            McpInputLine::TooLarge => Some(error_response(
                Value::Null,
                -32700,
                "Parse error",
                Some(json!({
                    "error": format!("MCP message exceeds max line bytes ({MCP_MAX_LINE_BYTES})")
                })),
            )),
        };
        if let Some(response) = response {
            writeln!(stdout, "{}", serde_json::to_string(&response)?)?;
            stdout.flush()?;
        }
    }
    Ok(())
}

fn read_mcp_input_line(reader: &mut impl BufRead) -> Result<Option<McpInputLine>> {
    let mut buffer = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            if buffer.is_empty() {
                return Ok(None);
            }
            break;
        }
        if let Some(newline_index) = available.iter().position(|byte| *byte == b'\n') {
            let bytes_to_consume = newline_index + 1;
            if buffer.len().saturating_add(bytes_to_consume) > MCP_MAX_LINE_BYTES {
                reader.consume(bytes_to_consume);
                return Ok(Some(McpInputLine::TooLarge));
            }
            buffer.extend_from_slice(&available[..bytes_to_consume]);
            reader.consume(bytes_to_consume);
            break;
        }

        let bytes_to_consume = available.len();
        if buffer.len().saturating_add(bytes_to_consume) > MCP_MAX_LINE_BYTES {
            reader.consume(bytes_to_consume);
            discard_until_newline(reader)?;
            return Ok(Some(McpInputLine::TooLarge));
        }
        buffer.extend_from_slice(available);
        reader.consume(bytes_to_consume);
    }

    Ok(Some(match String::from_utf8(buffer) {
        Ok(line) => McpInputLine::Line(line),
        Err(_) => McpInputLine::InvalidUtf8,
    }))
}

fn discard_until_newline(reader: &mut impl BufRead) -> Result<()> {
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(());
        }
        let bytes_to_consume = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|index| index + 1)
            .unwrap_or(available.len());
        let found_newline = bytes_to_consume <= available.len()
            && available
                .get(bytes_to_consume.saturating_sub(1))
                .is_some_and(|byte| *byte == b'\n');
        reader.consume(bytes_to_consume);
        if found_newline {
            return Ok(());
        }
    }
}

fn handle_line(line: &str, data_root: &Path, initialized: &mut bool) -> Option<Value> {
    let message = match serde_json::from_str::<Value>(line) {
        Ok(message) => message,
        Err(err) => {
            return Some(error_response(
                Value::Null,
                -32700,
                "Parse error",
                Some(json!({ "error": err.to_string() })),
            ));
        }
    };
    handle_message(message, data_root, initialized)
}

fn handle_message(message: Value, data_root: &Path, initialized: &mut bool) -> Option<Value> {
    let Some(object) = message.as_object() else {
        return Some(error_response(Value::Null, -32600, "Invalid Request", None));
    };
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        let id = object.get("id").cloned().unwrap_or(Value::Null);
        return Some(error_response(id, -32600, "Invalid Request", None));
    }
    let id = message
        .as_object()
        .and_then(|object| object.get("id"))
        .cloned();
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return id.map(|id| error_response(id, -32600, "Invalid Request", None));
    };
    if matches!(id, Some(Value::Null | Value::Array(_) | Value::Object(_))) {
        return Some(error_response(Value::Null, -32600, "Invalid Request", None));
    }
    if id.is_none() {
        if method == "notifications/initialized" {
            *initialized = true;
        }
        return None;
    }
    let id = id?;
    let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
    if !params.is_object() {
        return Some(error_response(
            id,
            -32602,
            "Invalid params",
            Some(json!({ "error": "params must be an object" })),
        ));
    }
    if method != "initialize" && !*initialized {
        return Some(error_response(
            id,
            -32002,
            "Server not initialized",
            Some(json!({ "error": "send initialize before calling ctx MCP tools" })),
        ));
    }
    let is_search_tool_call =
        method == "tools/call" && params.get("name").and_then(Value::as_str) == Some("search");
    let result = match method {
        "initialize" => {
            *initialized = true;
            Ok(initialize_result(&params))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_definitions() })),
        "tools/call" => handle_tools_call(params, data_root),
        _ => Err(json_rpc_error(-32601, "Method not found", None)),
    };
    Some(match result {
        Ok(result) if is_search_tool_call => bounded_search_rpc_response(id, result),
        Ok(result) => success_response(id, result),
        Err(error) => {
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
        }
    })
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
        "instructions": "Read-only access to the local ctx index. Tool output may include absolute paths, source metadata, snippets, transcript text, and raw SQL query results; MCP hosts may log or forward it. This minimal server supports initialize, ping, tools/list, and tools/call over newline-delimited stdio. It does not expose MCP resources or prompts, and tools do not import provider history, write provider files, or write repositories."
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

fn handle_tools_call(params: Value, data_root: &Path) -> Result<Value, Value> {
    let name = params.get("name").and_then(Value::as_str).ok_or_else(|| {
        json_rpc_error(
            -32602,
            "Invalid params",
            Some(json!({ "error": "tools/call requires params.name" })),
        )
    })?;
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !arguments.is_object() {
        return Err(json_rpc_error(
            -32602,
            "Invalid params",
            Some(json!({ "error": "tools/call params.arguments must be an object" })),
        ));
    }

    let result = match name {
        "status" => {
            validate_argument_keys(&arguments, &[])?;
            tool_status(data_root)
        }
        "sources" => {
            validate_argument_keys(&arguments, &[])?;
            tool_sources(data_root)
        }
        "search" => {
            validate_argument_keys(
                &arguments,
                &[
                    "query",
                    "limit",
                    "provider",
                    "history_source",
                    "provider_key",
                    "source_id",
                    "source_format",
                    "workspace",
                    "since",
                    "primary_only",
                    "include_subagents",
                    "event_type",
                    "file",
                    "session",
                    "events",
                    "include_current_session",
                    "backend",
                ],
            )?;
            tool_search(&arguments, data_root)
        }
        "sql" => {
            validate_argument_keys(
                &arguments,
                &[
                    "sql",
                    "max_rows",
                    "max_columns",
                    "max_value_bytes",
                    "max_sql_bytes",
                    "timeout_ms",
                ],
            )?;
            tool_sql(&arguments, data_root)
        }
        "show_session" => {
            validate_argument_keys(&arguments, &["ctx_session_id", "mode"])?;
            tool_show_session(&arguments, data_root)
        }
        "show_event" => {
            validate_argument_keys(&arguments, &["ctx_event_id", "before", "after", "window"])?;
            tool_show_event(&arguments, data_root)
        }
        _ => {
            return Err(json_rpc_error(
                -32602,
                "Invalid params",
                Some(json!({ "error": format!("unknown tool {name}") })),
            ))
        }
    };

    Ok(match result {
        Ok(value) => tool_result(value),
        Err(err) if name == "search" => search_tool_error_result(err),
        Err(err) => tool_error_result(err),
    })
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
        completed_with_rejections_catalog_sessions,
        terminal_rejected_catalog_sessions,
        stale_catalog_sessions,
        source_import_files,
        indexed_source_import_files,
        pending_source_import_files,
        failed_source_import_files,
        completed_with_rejections_source_import_files,
        terminal_rejected_source_import_files,
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
            catalog_counts.completed_with_rejections,
            catalog_counts.rejected,
            catalog_counts.stale,
            source_import_file_counts.total,
            source_import_file_counts.indexed,
            source_import_file_counts.pending,
            source_import_file_counts.failed,
            source_import_file_counts.completed_with_rejections,
            source_import_file_counts.rejected,
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
    let completed_with_rejections_inventory_units = completed_with_rejections_catalog_sessions
        .saturating_add(completed_with_rejections_source_import_files);
    let terminal_rejected_inventory_units =
        terminal_rejected_catalog_sessions.saturating_add(terminal_rejected_source_import_files);
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
        "completed_with_rejections_inventory_units": completed_with_rejections_inventory_units,
        "terminal_rejected_inventory_units": terminal_rejected_inventory_units,
        "stale_inventory_units": stale_inventory_units,
        "cataloged_sessions": cataloged_sessions,
        "indexed_catalog_sessions": indexed_catalog_sessions,
        "pending_catalog_sessions": pending_catalog_sessions,
        "failed_catalog_sessions": failed_catalog_sessions,
        "completed_with_rejections_catalog_sessions": completed_with_rejections_catalog_sessions,
        "terminal_rejected_catalog_sessions": terminal_rejected_catalog_sessions,
        "stale_catalog_sessions": stale_catalog_sessions,
        "source_import_files": source_import_files,
        "indexed_source_import_files": indexed_source_import_files,
        "pending_source_import_files": pending_source_import_files,
        "failed_source_import_files": failed_source_import_files,
        "completed_with_rejections_source_import_files": completed_with_rejections_source_import_files,
        "terminal_rejected_source_import_files": terminal_rejected_source_import_files,
        "stale_source_import_files": stale_source_import_files,
        "semantic": semantic,
        "daemon": daemon,
        "local_only": true,
        "read_only": true,
    }))
}

fn tool_sources(data_root: &Path) -> Result<Value> {
    let sources = discovered_sources();
    let mut source_values = sources_json(&sources);
    source_values.extend(discovered_plugin_sources_json(data_root)?);
    Ok(json!({
        "schema_version": 1,
        "sources": source_values,
        "read_only": true,
    }))
}

fn tool_search(arguments: &Value, data_root: &Path) -> Result<Value> {
    let file = optional_string(arguments, "file")?.map(PathBuf::from);
    let query = arguments
        .get("query")
        .map(parse_search_query_value)
        .transpose()?;
    if query.is_none() && file.is_none() {
        return Err(anyhow!("search needs a ctx-search-v1 query or file filter"));
    }
    let limit = optional_usize(arguments, "limit")?.unwrap_or(20);
    if !(1..=SEARCH_MAX_RESULTS).contains(&limit) {
        return Err(anyhow!("limit must be between 1 and {SEARCH_MAX_RESULTS}"));
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
    let config = config::AppConfig::load(data_root)?;
    let backend = resolve_search_backend(optional_search_backend(arguments, "backend")?, &config)?;
    let store = open_existing_store(data_root)?;
    let events = optional_bool(arguments, "events")?.unwrap_or(false) || session.is_some();
    let include_current_session =
        optional_bool(arguments, "include_current_session")?.unwrap_or(false);

    let options = ctx_history_search::PacketOptions {
        limit,
        filters: search_filters(
            SearchFilterInput {
                session,
                provider,
                source_identity: SourceIdentityFilterArgs {
                    history_source,
                    provider_key,
                    source_id,
                    source_format,
                },
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
    let (packet, retrieval) = if let Some(query) = query.as_ref() {
        search_packet_query_with_backend(
            &store,
            data_root,
            query,
            &options,
            backend,
            config.semantic_search_enabled(),
            RefreshArg::Off,
            false,
        )?
    } else {
        search_packet_file_filter_with_backend(&store, &options, backend, false)?
    };
    let refresh = SearchRefreshReport::skipped(RefreshArg::Off, "skipped");
    let mut result = SearchDto::packet(&store, &packet, &refresh, &retrieval, None);
    if let Some(object) = result.as_object_mut() {
        object.insert("schema_version".to_owned(), json!(2));
        object.insert("query".to_owned(), json!(query));
        object.remove("query_spec");
        object.insert(
            "query_execution".to_owned(),
            serde_json::to_value(&packet.query_execution)
                .context("serialize search execution diagnostics")?,
        );
    }
    Ok(result)
}

fn tool_sql(arguments: &Value, data_root: &Path) -> Result<Value> {
    let store = open_existing_store(data_root)?;
    let sql = optional_string(arguments, "sql")?.ok_or_else(|| anyhow!("sql is required"))?;
    let max_rows = optional_usize(arguments, "max_rows")?.unwrap_or(RAW_SQL_DEFAULT_MAX_ROWS);
    let max_columns =
        optional_usize(arguments, "max_columns")?.unwrap_or(RAW_SQL_DEFAULT_MAX_COLUMNS);
    let max_value_bytes =
        optional_usize(arguments, "max_value_bytes")?.unwrap_or(RAW_SQL_DEFAULT_MAX_VALUE_BYTES);
    let max_sql_bytes =
        optional_usize(arguments, "max_sql_bytes")?.unwrap_or(RAW_SQL_DEFAULT_MAX_SQL_BYTES);
    let timeout_ms = optional_usize(arguments, "timeout_ms")?
        .map(|value| u64::try_from(value).map_err(|_| anyhow!("timeout_ms is too large")))
        .transpose()?
        .unwrap_or_else(|| duration_millis_u64(RAW_SQL_DEFAULT_TIMEOUT));
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

fn tool_show_session(arguments: &Value, data_root: &Path) -> Result<Value> {
    let store = open_existing_store(data_root)?;
    let session_id = required_uuid(arguments, "ctx_session_id")?;
    let mode = optional_transcript_mode(arguments, "mode")?.unwrap_or(TranscriptMode::Lite);
    let session = store.get_session(session_id)?;
    let mut events = store.events_for_session_limited(session.id, MCP_MAX_SESSION_EVENTS + 1)?;
    let truncated = events.len() > MCP_MAX_SESSION_EVENTS;
    if truncated {
        events.truncate(MCP_MAX_SESSION_EVENTS);
    }
    let mut value = session_transcript_json(&store, &session, &events, mode, OutputFormat::Json);
    if truncated {
        if let Some(object) = value.as_object_mut() {
            object.insert(
                "truncated".to_owned(),
                json!({
                    "events": true,
                    "max_events": MCP_MAX_SESSION_EVENTS,
                }),
            );
        }
    }
    Ok(value)
}

fn tool_show_event(arguments: &Value, data_root: &Path) -> Result<Value> {
    let store = open_existing_store(data_root)?;
    let event_id = required_uuid(arguments, "ctx_event_id")?;
    let before = optional_usize(arguments, "before")?.unwrap_or(0);
    let after = optional_usize(arguments, "after")?.unwrap_or(0);
    let window = optional_usize(arguments, "window")?;
    if before > MAX_EVENT_WINDOW
        || after > MAX_EVENT_WINDOW
        || window.is_some_and(|window| window > MAX_EVENT_WINDOW)
    {
        return Err(anyhow!(
            "show_event before/after/window must be {MAX_EVENT_WINDOW} or less"
        ));
    }
    let event = store.get_event(event_id)?;
    let events = event_window(&store, &event, before, after, window)?;
    Ok(event_window_json(
        &store,
        &event,
        &events,
        OutputFormat::Json,
    ))
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

fn tool_result(structured: Value) -> Value {
    let text = render_tool_text(&structured);
    json!({
        "content": [
            {
                "type": "text",
                "text": text,
            }
        ],
        "structuredContent": structured,
    })
}

fn bounded_search_rpc_response(id: Value, result: Value) -> Value {
    let Some(mut structured) = result.get("structuredContent").cloned() else {
        return bounded_untrimmable_search_response(id, result);
    };
    if structured
        .get("results")
        .and_then(Value::as_array)
        .is_none()
    {
        return bounded_untrimmable_search_response(id, result);
    }
    let result_count = structured
        .get("results")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    match render_search_rpc_response(&id, &mut structured) {
        Ok((result, bytes)) if bytes <= SEARCH_MAX_SERIALIZED_RESPONSE_BYTES => return result,
        Ok(_) => {}
        Err(error) => return bounded_search_error_rpc_response(id, error),
    }

    let mut smallest = 0_usize;
    let mut largest = result_count;
    let mut best = None;
    while smallest <= largest {
        let keep = smallest + (largest - smallest) / 2;
        let mut candidate = structured.clone();
        truncate_search_transport(&mut candidate, keep);
        match render_search_rpc_response(&id, &mut candidate) {
            Ok((result, bytes)) if bytes <= SEARCH_MAX_SERIALIZED_RESPONSE_BYTES => {
                best = Some(result);
                smallest = keep.saturating_add(1);
            }
            Ok(_) => {
                if keep == 0 {
                    break;
                }
                largest = keep - 1;
            }
            Err(error) => {
                return bounded_search_error_rpc_response(id, error);
            }
        }
    }

    best.unwrap_or_else(|| {
        bounded_search_error_rpc_response(
            id,
            anyhow!(
                "bounded MCP search metadata exceeds the {SEARCH_MAX_SERIALIZED_RESPONSE_BYTES}-byte response cap"
            ),
        )
    })
}

fn bounded_untrimmable_search_response(id: Value, result: Value) -> Value {
    let response = success_response(id.clone(), result);
    match framed_response_bytes(&response) {
        Ok(bytes) if bytes <= SEARCH_MAX_SERIALIZED_RESPONSE_BYTES => response,
        Ok(_) => bounded_search_error_rpc_response(
            id,
            anyhow!(
                "bounded MCP search response exceeds the {SEARCH_MAX_SERIALIZED_RESPONSE_BYTES}-byte response cap"
            ),
        ),
        Err(error) => bounded_search_error_rpc_response(id, error),
    }
}

fn bounded_search_error_rpc_response(id: Value, error: anyhow::Error) -> Value {
    let response = success_response(id.clone(), search_tool_error_result(error));
    if framed_response_bytes(&response)
        .is_ok_and(|bytes| bytes <= SEARCH_MAX_SERIALIZED_RESPONSE_BYTES)
    {
        return response;
    }

    let response = success_response(
        id,
        search_tool_error_result(anyhow!(
            "bounded MCP search error exceeded the response cap"
        )),
    );
    if framed_response_bytes(&response)
        .is_ok_and(|bytes| bytes <= SEARCH_MAX_SERIALIZED_RESPONSE_BYTES)
    {
        return response;
    }
    error_response(
        Value::Null,
        -32603,
        "bounded MCP search response exceeded its byte cap",
        None,
    )
}

fn framed_response_bytes(response: &Value) -> Result<usize> {
    Ok(serde_json::to_vec(response)
        .context("serialize bounded MCP search response")?
        .len()
        .saturating_add(1))
}

fn render_search_rpc_response(id: &Value, structured: &mut Value) -> Result<(Value, usize)> {
    for _ in 0..8 {
        let response = success_response(id.clone(), tool_result(structured.clone()));
        let serialized_bytes = framed_response_bytes(&response)?;
        let Some(consumed) = structured
            .get_mut("query_execution")
            .and_then(Value::as_object_mut)
            .and_then(|execution| execution.get_mut("consumed"))
            .and_then(Value::as_object_mut)
        else {
            return Ok((response, serialized_bytes));
        };
        if consumed
            .get("serialized_response_bytes")
            .and_then(Value::as_u64)
            == Some(serialized_bytes as u64)
        {
            return Ok((response, serialized_bytes));
        }
        consumed.insert(
            "serialized_response_bytes".to_owned(),
            json!(serialized_bytes),
        );
    }
    Err(anyhow!(
        "bounded MCP search response byte accounting did not converge"
    ))
}

fn truncate_search_transport(structured: &mut Value, keep: usize) {
    let Some(results) = structured.get_mut("results").and_then(Value::as_array_mut) else {
        return;
    };
    let keep = keep.min(results.len());
    let removed_results = results.len().saturating_sub(keep);
    let removed_text_bytes = results[keep..]
        .iter()
        .map(search_result_text_bytes)
        .fold(0_usize, usize::saturating_add);
    results.truncate(keep);
    if removed_results == 0 {
        return;
    }

    if let Some(pagination) = structured
        .get_mut("pagination")
        .and_then(Value::as_object_mut)
    {
        pagination.insert("has_more".to_owned(), Value::Bool(true));
    }
    if let Some(truncation) = structured
        .get_mut("truncation")
        .and_then(Value::as_object_mut)
    {
        truncation.insert("truncated".to_owned(), Value::Bool(true));
        truncation.insert("reason".to_owned(), json!("serialized_response_bytes"));
        let omitted = truncation
            .get("omitted_results")
            .and_then(Value::as_u64)
            .unwrap_or_default()
            .saturating_add(removed_results as u64);
        truncation.insert("omitted_results".to_owned(), json!(omitted));
    }
    let Some(execution) = structured
        .get_mut("query_execution")
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    execution.insert("truncated".to_owned(), Value::Bool(true));
    let reasons = execution
        .entry("truncation_reasons".to_owned())
        .or_insert_with(|| json!([]));
    if let Some(reasons) = reasons.as_array_mut() {
        let reason = json!("serialized_response_bytes");
        if !reasons.contains(&reason) {
            reasons.push(reason);
        }
    }
    if let Some(consumed) = execution.get_mut("consumed").and_then(Value::as_object_mut) {
        let returned = consumed
            .get("returned_results")
            .and_then(Value::as_u64)
            .unwrap_or_default()
            .saturating_sub(removed_results as u64);
        consumed.insert("returned_results".to_owned(), json!(returned));
        let returned_text_bytes = consumed
            .get("returned_text_bytes")
            .and_then(Value::as_u64)
            .unwrap_or_default()
            .saturating_sub(removed_text_bytes as u64);
        consumed.insert("returned_text_bytes".to_owned(), json!(returned_text_bytes));
    }
}

fn search_result_text_bytes(result: &Value) -> usize {
    result
        .get("title")
        .and_then(Value::as_str)
        .map_or(0, str::len)
        .saturating_add(
            result
                .get("snippet")
                .and_then(Value::as_str)
                .map_or(0, str::len),
        )
}

fn tool_error_result(err: anyhow::Error) -> Value {
    let error = err.to_string();
    json!({
        "isError": true,
        "content": [
            {
                "type": "text",
                "text": error.clone(),
            }
        ],
        "structuredContent": {
            "error": error,
        }
    })
}

fn search_tool_error_result(err: anyhow::Error) -> Value {
    let message = err.to_string();
    let mut error = AgentHistoryErrorBody::new(
        AgentHistoryErrorCode::InvalidRequest,
        message.clone(),
        false,
    );
    let mut query_execution = None;

    if let Some(search_error) = err
        .chain()
        .find_map(|source| source.downcast_ref::<ctx_history_search::SearchError>())
    {
        match search_error {
            ctx_history_search::SearchError::Query(_)
            | ctx_history_search::SearchError::Envelope(_)
            | ctx_history_search::SearchError::InvalidSemanticCandidateId(_) => {}
            ctx_history_search::SearchError::SemanticNotReady { readiness } => {
                error.code = AgentHistoryErrorCode::BackendUnavailable;
                error.retryable = true;
                let mut details = JsonObject::new();
                details.insert("readiness".to_owned(), json!(readiness));
                details.insert(
                    "search_error_code".to_owned(),
                    json!("explicit_semantic_unavailable"),
                );
                error.details = Some(details);
            }
            ctx_history_search::SearchError::TimedOut { diagnostics, .. } => {
                error.code = AgentHistoryErrorCode::Timeout;
                error.retryable = true;
                let mut details = JsonObject::new();
                details.insert(
                    "search_error_code".to_owned(),
                    json!(ctx_history_search::SEARCH_BUDGET_EXHAUSTED_ERROR_CODE),
                );
                error.details = Some(details);
                query_execution = Some(json!(diagnostics.as_ref()));
            }
            _ => {
                error.code = AgentHistoryErrorCode::Unknown;
            }
        }
    } else if message.contains("store is not initialized") {
        error.code = AgentHistoryErrorCode::NotInitialized;
    }

    let structured = compact_json(json!({
        "schema_version": 2,
        "payload_type": "search_error",
        "error": error,
        "query_execution": query_execution,
    }));
    json!({
        "isError": true,
        "content": [
            {
                "type": "text",
                "text": message,
            }
        ],
        "structuredContent": structured,
    })
}

fn lexical_search_clause_schema() -> Value {
    json!({
        "oneOf": [
            object_schema(json!({
                "all": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": SEARCH_MAX_CLAUSE_BYTES,
                    "description": format!("All analyzed words must match the same indexed event. Runtime limits this value to {SEARCH_MAX_CLAUSE_BYTES} UTF-8 bytes and {SEARCH_MAX_ANALYZED_TOKENS_PER_CLAUSE} analyzed tokens.")
                }
            }), vec!["all"]),
            object_schema(json!({
                "phrase": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": SEARCH_MAX_CLAUSE_BYTES,
                    "description": format!("Analyzed words must match in order at adjacent positions. Runtime limits this value to {SEARCH_MAX_CLAUSE_BYTES} UTF-8 bytes and {SEARCH_MAX_ANALYZED_TOKENS_PER_CLAUSE} analyzed tokens.")
                }
            }), vec!["phrase"]),
            object_schema(json!({
                "literal": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": SEARCH_MAX_LITERAL_BYTES,
                    "description": format!("Match punctuation-preserving contiguous text. Runtime requires {SEARCH_MIN_LITERAL_BYTES}..={SEARCH_MAX_LITERAL_BYTES} UTF-8 bytes and an indexed anchor; JSON Schema string lengths count Unicode code points.")
                }
            }), vec!["literal"])
        ]
    })
}

fn semantic_search_clause_schema() -> Value {
    object_schema(
        json!({
            "semantic": {
                "type": "string",
                "minLength": 1,
                "maxLength": SEARCH_MAX_CLAUSE_BYTES,
                "description": format!("Retrieve bounded vector candidates without model acquisition or backfill. Runtime limits this value to {SEARCH_MAX_CLAUSE_BYTES} UTF-8 bytes.")
            }
        }),
        vec!["semantic"],
    )
}

fn any_search_clause_schema() -> Value {
    let lexical = lexical_search_clause_schema();
    let lexical_variants = lexical
        .get("oneOf")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut variants = lexical_variants;
    variants.push(semantic_search_clause_schema());
    json!({ "oneOf": variants })
}

fn search_query_schema() -> Value {
    let mut schema = object_schema(
        json!({
            "version": { "type": "string", "const": "ctx-search-v1" },
            "any": {
                "type": "array",
                "maxItems": SEARCH_MAX_CLAUSES,
                "items": any_search_clause_schema(),
                "contains": semantic_search_clause_schema(),
                "minContains": 0,
                "maxContains": 1,
                "description": "Alternatives. At most one semantic clause may appear here."
            },
            "must": {
                "type": "array",
                "maxItems": SEARCH_MAX_CLAUSES,
                "items": lexical_search_clause_schema(),
                "description": "Every lexical clause is required."
            },
            "must_not": {
                "type": "array",
                "maxItems": SEARCH_MAX_CLAUSES,
                "items": lexical_search_clause_schema(),
                "description": "A candidate matching any lexical clause is excluded."
            }
        }),
        vec!["version"],
    );
    if let Some(object) = schema.as_object_mut() {
        object.insert(
            "description".to_owned(),
            json!(format!(
                "Canonical ctx-search-v1 query. Runtime allows {SEARCH_MAX_CLAUSES} clauses and {SEARCH_MAX_TOTAL_CLAUSE_BYTES} total clause bytes; adapters do not reinterpret it."
            )),
        );
        object.insert(
            "anyOf".to_owned(),
            json!([
                {"required": ["any"], "properties": {"any": {"minItems": 1}}},
                {"required": ["must"], "properties": {"must": {"minItems": 1}}}
            ]),
        );
    }
    schema
}

fn tool_definitions() -> Vec<Value> {
    let mut tools = vec![
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
            "description": "Search the existing local ctx index with one bounded ctx-search-v1 query and optional metadata filters. any clauses are alternatives, must clauses are required, and must_not clauses exclude. The tool is read-only and never refreshes, downloads a model, or starts indexing.",
            "inputSchema": object_schema(json!({
                "query": search_query_schema(),
                "limit": { "type": "integer", "minimum": 1, "maximum": SEARCH_MAX_RESULTS, "default": 20 },
                "provider": { "type": "string", "enum": provider_names() },
                "history_source": { "type": "string", "description": "Custom history source selector as plugin/source or provider_key/source_id." },
                "provider_key": { "type": "string", "description": "Custom history provider_key." },
                "source_id": { "type": "string", "description": "Custom history source_id." },
                "source_format": { "type": "string", "description": "Custom history source_format." },
                "workspace": { "type": "string", "description": "Workspace path or name text." },
                "since": { "type": "string", "description": "RFC3339 timestamp or day window such as 30d." },
                "primary_only": { "type": "boolean", "default": false, "description": "Include only primary-agent sessions." },
                "include_subagents": { "type": "boolean", "default": false, "description": "Include subagent sessions in addition to primary-agent sessions." },
                "event_type": { "type": "string", "enum": event_type_names() },
                "file": { "type": "string", "description": "Optional indexed touched-file path filter. Required only when query is omitted." },
                "session": { "type": "string", "description": "ctx session id." },
                "events": { "type": "boolean", "default": false },
                "include_current_session": { "type": "boolean", "default": false, "description": "Include the active Codex session tree when CODEX_THREAD_ID is set." },
                "backend": { "type": "string", "enum": ["hybrid", "semantic", "lexical"], "description": "Execution backend. Backend choice never changes clause meaning." }
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
                "mode": { "type": "string", "enum": ["full", "lite", "log"], "default": "lite" }
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
                "window": { "type": "integer", "minimum": 0 }
            }), vec!["ctx_event_id"]),
            "annotations": { "readOnlyHint": true },
        }),
    ];
    if let Some(search) = tools
        .iter_mut()
        .find(|tool| tool.get("name").and_then(Value::as_str) == Some("search"))
    {
        search["inputSchema"]["anyOf"] = json!([
            {"required": ["query"]},
            {"required": ["file"]}
        ]);
        search["inputSchema"]["description"] = json!(format!(
            "Search input is limited to {SEARCH_MAX_QUERY_JSON_BYTES} serialized query bytes and {SEARCH_MAX_CLAUSES} total query clauses."
        ));
    }
    tools
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

fn optional_string(arguments: &Value, key: &str) -> Result<Option<String>> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(anyhow!("{key} must be a string")),
    }
}

fn duration_millis_u64(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn optional_bool(arguments: &Value, key: &str) -> Result<Option<bool>> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(anyhow!("{key} must be a boolean")),
    }
}

fn optional_usize(arguments: &Value, key: &str) -> Result<Option<usize>> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(value)) => {
            let value = value
                .as_u64()
                .ok_or_else(|| anyhow!("{key} must be a non-negative integer"))?;
            usize::try_from(value)
                .map(Some)
                .map_err(|_| anyhow!("{key} is too large"))
        }
        Some(_) => Err(anyhow!("{key} must be a non-negative integer")),
    }
}

fn required_uuid(arguments: &Value, key: &str) -> Result<Uuid> {
    optional_uuid(arguments, key)?.ok_or_else(|| anyhow!("{key} is required"))
}

fn optional_uuid(arguments: &Value, key: &str) -> Result<Option<Uuid>> {
    optional_string(arguments, key)?
        .map(|value| Uuid::parse_str(&value).with_context(|| format!("invalid {key}")))
        .transpose()
}

fn optional_provider(arguments: &Value, key: &str) -> Result<Option<ProviderArg>> {
    let Some(provider) = optional_string(arguments, key)? else {
        return Ok(None);
    };
    ProviderArg::parse_name(&provider)
        .filter(|provider| cli_supported_provider(provider.capture_provider()))
        .map(Some)
        .ok_or_else(|| anyhow!("provider must be one of {}", provider_names().join(", ")))
}

fn optional_search_backend(arguments: &Value, key: &str) -> Result<Option<SearchBackendArg>> {
    let Some(value) = optional_string(arguments, key)? else {
        return Ok(None);
    };
    match value.as_str() {
        "hybrid" => Ok(Some(SearchBackendArg::Hybrid)),
        "lexical" => Ok(Some(SearchBackendArg::Lexical)),
        "semantic" => Ok(Some(SearchBackendArg::Semantic)),
        _ => Err(anyhow!("backend must be one of hybrid, semantic, lexical")),
    }
}

fn validate_argument_keys(arguments: &Value, allowed: &[&str]) -> std::result::Result<(), Value> {
    let Some(object) = arguments.as_object() else {
        return Err(json_rpc_error(
            -32602,
            "Invalid params",
            Some(json!({ "error": "tools/call params.arguments must be an object" })),
        ));
    };
    if let Some(key) = object
        .keys()
        .find(|key| !allowed.iter().any(|allowed| allowed == &key.as_str()))
    {
        return Err(json_rpc_error(
            -32602,
            "Invalid params",
            Some(json!({ "error": format!("unknown argument {key}") })),
        ));
    }
    Ok(())
}

fn optional_transcript_mode(arguments: &Value, key: &str) -> Result<Option<TranscriptMode>> {
    let Some(mode) = optional_string(arguments, key)? else {
        return Ok(None);
    };
    match mode.as_str() {
        "full" => Ok(Some(TranscriptMode::Full)),
        "lite" => Ok(Some(TranscriptMode::Lite)),
        "log" => Ok(Some(TranscriptMode::Log)),
        _ => Err(anyhow!("mode must be one of full, lite, log")),
    }
}

fn success_response(id: Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
}

fn error_response(id: Value, code: i64, message: &str, data: Option<Value>) -> Value {
    compact_json(json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message,
            "data": data,
        }
    }))
}

fn json_rpc_error(code: i64, message: &str, data: Option<Value>) -> Value {
    compact_json(json!({
        "code": code,
        "message": message,
        "data": data,
    }))
}

#[cfg(test)]
mod search_contract_tests {
    use super::*;

    #[test]
    fn timeout_error_keeps_typed_snake_case_diagnostics() {
        let diagnostics = ctx_history_search::SearchExecutionDiagnostics {
            timed_out: true,
            truncated: true,
            ..ctx_history_search::SearchExecutionDiagnostics::default()
        };
        let result = search_tool_error_result(anyhow::Error::new(
            ctx_history_search::SearchError::TimedOut {
                timeout_ms: 10_000,
                diagnostics: Box::new(diagnostics),
            },
        ));
        let structured = &result["structuredContent"];
        assert_eq!(structured["schema_version"], 2);
        assert_eq!(structured["error"]["code"], "timeout");
        assert_eq!(
            structured["error"]["details"]["search_error_code"],
            ctx_history_search::SEARCH_BUDGET_EXHAUSTED_ERROR_CODE
        );
        assert_eq!(structured["query_execution"]["timed_out"], true);
        assert_eq!(
            structured["query_execution"]["query_version"],
            ctx_protocol::SEARCH_QUERY_VERSION
        );
    }

    #[test]
    fn search_tool_result_caps_combined_text_and_structured_payload() {
        let results = (0..200)
            .map(|index| {
                json!({
                    "title": format!("result {index}"),
                    "snippet": "x".repeat(20_000),
                })
            })
            .collect::<Vec<_>>();
        let id = json!("i".repeat(MCP_MAX_LINE_BYTES / 2));
        let result = bounded_search_rpc_response(
            id,
            tool_result(json!({
                "results": results,
                "pagination": {"has_more": false},
                "truncation": {"truncated": false, "omitted_results": 0},
                "query_execution": {
                    "consumed": {
                        "returned_results": 200,
                        "returned_text_bytes": 4_000_000,
                        "serialized_response_bytes": 0,
                    },
                    "truncated": false,
                    "truncation_reasons": [],
                },
            })),
        );

        let serialized_bytes = serde_json::to_vec(&result).unwrap().len() + 1;
        assert!(serialized_bytes <= SEARCH_MAX_SERIALIZED_RESPONSE_BYTES);
        let structured = &result["result"]["structuredContent"];
        assert_eq!(structured["query_execution"]["truncated"], true);
        assert_eq!(structured["pagination"]["has_more"], true);
        let returned_results = structured["results"].as_array().unwrap().len() as u64;
        assert_eq!(
            structured["query_execution"]["consumed"]["returned_results"],
            returned_results
        );
        assert_eq!(
            structured["truncation"]["omitted_results"],
            200 - returned_results
        );
        assert_eq!(
            structured["query_execution"]["consumed"]["serialized_response_bytes"],
            serialized_bytes
        );
        assert!(structured["query_execution"]["truncation_reasons"]
            .as_array()
            .unwrap()
            .contains(&json!("serialized_response_bytes")));
    }

    #[test]
    fn search_error_response_is_capped_after_json_rpc_framing() {
        let result = bounded_search_rpc_response(
            json!("error-request"),
            search_tool_error_result(anyhow!(
                "x".repeat(SEARCH_MAX_SERIALIZED_RESPONSE_BYTES.saturating_mul(2))
            )),
        );

        assert!(framed_response_bytes(&result).unwrap() <= SEARCH_MAX_SERIALIZED_RESPONSE_BYTES);
        assert_eq!(result["result"]["isError"], true);
    }
}
