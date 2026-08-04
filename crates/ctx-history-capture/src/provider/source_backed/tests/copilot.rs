use std::{
    fs::OpenOptions,
    io::{BufWriter, Write},
    path::Path,
};

use ctx_history_core::{McpToolCallAttribution, MAX_MCP_TOOL_CALL_ATTRIBUTION_COMPONENT_BYTES};

use super::*;
use crate::{DiscoveryPlatformDirs, COPILOT_CLI_SOURCE_FORMAT, MAX_PROVIDER_JSONL_LINE_BYTES};

fn session_header(session_id: &str) -> String {
    serde_json::json!({
        "type": "session.start",
        "id": format!("{session_id}-header"),
        "timestamp": "2026-08-03T12:00:00Z",
        "data": {
            "sessionId": session_id,
            "startTime": "2026-08-03T12:00:00Z",
            "context": {"cwd": "/workspace/sanitized"}
        }
    })
    .to_string()
}

fn tool_start(native_id: &str, call_id: &str, server: &str, tool: &str) -> String {
    serde_json::json!({
        "type": "tool.execution_start",
        "id": native_id,
        "timestamp": "2026-08-03T12:00:01Z",
        "data": {
            "toolCallId": call_id,
            "mcpServerName": server,
            "mcpToolName": tool
        }
    })
    .to_string()
}

fn tool_completion(native_id: &str, call_id: &str, success: bool) -> String {
    serde_json::json!({
        "type": "tool.execution_complete",
        "id": native_id,
        "timestamp": "2026-08-03T12:00:02Z",
        "data": {"toolCallId": call_id, "success": success}
    })
    .to_string()
}

fn write_session(root: &Path, directory: &str, session_id: &str, events: &[String]) {
    let session = root.join(directory);
    fs::create_dir_all(&session).unwrap();
    let path = session.join("events.jsonl");
    let file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)
        .unwrap();
    let mut writer = BufWriter::new(file);
    writeln!(writer, "{}", session_header(session_id)).unwrap();
    for event in events {
        writeln!(writer, "{event}").unwrap();
    }
    writer.flush().unwrap();
}

fn copilot_registry(root: &Path, temp: &Path) -> SourceBackedProviderRegistry {
    let context = DiscoveryContext::new(
        temp.join("home"),
        temp.join("cwd"),
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    );
    let build = build_automatic_source_backed_registry_from_parts(
        &context,
        &temp.join("ctx-data"),
        vec![fixture_provider_source_at(
            CaptureProvider::CopilotCli,
            COPILOT_CLI_SOURCE_FORMAT,
            ProviderImportSupport::Native,
            root,
        )],
        Vec::new(),
    );
    assert_eq!(build.executable_route_count(), 1);
    assert!(
        build.issues.is_empty(),
        "unexpected issues: {:#?}",
        build.issues
    );
    build.registry
}

fn core_records(index: &VerifiedIndex) -> Vec<CoreRecord> {
    let mut records = Vec::new();
    for source in &index.manifest().sources {
        let source_key = source.observation().source();
        let page = index.source_event_page(source_key, None, 256).unwrap();
        assert!(page.next_cursor.is_none());
        for item in page.items {
            records.push(
                index
                    .core_record_by_id(item.event_id.as_uuid())
                    .unwrap()
                    .unwrap(),
            );
        }
    }
    records
}

fn native_id(record: &CoreRecord) -> &str {
    let Some(TypedKey::Composite(parts)) = record.native_event_id.as_ref() else {
        panic!("Copilot record has no composite native identity");
    };
    let Some(TypedKey::Utf8(native_id)) = parts.first() else {
        panic!("Copilot record has no native event id");
    };
    native_id
}

fn record_by_native_id<'a>(records: &'a [CoreRecord], expected: &str) -> &'a CoreRecord {
    records
        .iter()
        .find(|record| native_id(record) == expected)
        .unwrap_or_else(|| panic!("missing published Copilot record {expected}"))
}

fn attribution(server: &str, tool: &str) -> Option<McpToolCallAttribution> {
    Some(McpToolCallAttribution {
        server: server.to_owned(),
        tool: tool.to_owned(),
    })
}

#[test]
fn copilot_route_discards_physical_line_above_shared_ceiling_and_publishes_neighbors() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let session = root.join("oversized");
    fs::create_dir_all(&session).unwrap();
    let path = session.join("events.jsonl");
    let file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&path)
        .unwrap();
    let mut writer = BufWriter::new(file);
    writeln!(writer, "{}", session_header("oversized-session")).unwrap();
    writeln!(
        writer,
        "{}",
        tool_start("before-start", "before", "clean", "before")
    )
    .unwrap();
    writeln!(
        writer,
        "{}",
        tool_completion("before-complete", "before", true)
    )
    .unwrap();

    let prefix = br#"{"type":"tool.execution_start","id":"oversized-start","data":{"toolCallId":"oversized","mcpServerName":"oversized","mcpToolName":"lookup"},"padding":""#;
    let suffix = br#""}"#;
    let padding_bytes = MAX_PROVIDER_JSONL_LINE_BYTES
        .saturating_add(1)
        .checked_sub(prefix.len().saturating_add(suffix.len()))
        .unwrap();
    assert_eq!(
        prefix
            .len()
            .saturating_add(padding_bytes)
            .saturating_add(suffix.len()),
        MAX_PROVIDER_JSONL_LINE_BYTES.saturating_add(1)
    );
    writer.write_all(prefix).unwrap();
    let chunk = [b'x'; 64 * 1024];
    let mut remaining = padding_bytes;
    while remaining >= chunk.len() {
        writer.write_all(&chunk).unwrap();
        remaining -= chunk.len();
    }
    writer.write_all(&chunk[..remaining]).unwrap();
    writer.write_all(suffix).unwrap();
    writer.write_all(b"\n").unwrap();

    writeln!(
        writer,
        "{}",
        tool_start("after-start", "after", "clean", "after")
    )
    .unwrap();
    writeln!(
        writer,
        "{}",
        tool_completion("after-complete", "after", false)
    )
    .unwrap();
    writer.flush().unwrap();
    assert!(fs::metadata(&path).unwrap().len() > MAX_PROVIDER_JSONL_LINE_BYTES as u64);

    let registry = copilot_registry(&root, temp.path());
    let index_path = temp.path().join("index");
    let receipt =
        refresh_source_backed_generation(&index_path, &registry, WriterOptions::default()).unwrap();
    assert!(receipt.failed_routes.is_empty());
    assert!(receipt.logical_source_failures.is_empty());
    assert_eq!(receipt.successful_route_ids.len(), 1);
    assert_eq!(receipt.sources.len(), 1);
    assert_eq!(
        receipt.sources[0].counts(),
        ScannedSourceCounts {
            complete_records: 6,
            retained_records: 5,
            rejected_records: 1,
            ignored_records: 0,
            indexed_documents: 5,
            certified_bytes: fs::metadata(path).unwrap().len(),
        }
    );

    let records = core_records(&VerifiedIndex::open(&index_path).unwrap());
    assert_eq!(records.len(), 5);
    for expected in [
        "before-start",
        "before-complete",
        "after-start",
        "after-complete",
    ] {
        assert!(records.iter().any(|record| native_id(record) == expected));
    }
    assert!(!records
        .iter()
        .any(|record| native_id(record) == "oversized-start"));
    assert!(records.iter().all(|record| record.mcp_tool_call.is_none()));
}

#[test]
fn copilot_route_enforces_independent_exact_identity_component_boundaries() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let exact_server = "s".repeat(MAX_MCP_TOOL_CALL_ATTRIBUTION_COMPONENT_BYTES);
    let oversized_server =
        "s".repeat(MAX_MCP_TOOL_CALL_ATTRIBUTION_COMPONENT_BYTES.saturating_add(1));
    let exact_tool = "t".repeat(MAX_MCP_TOOL_CALL_ATTRIBUTION_COMPONENT_BYTES);
    let oversized_tool =
        "t".repeat(MAX_MCP_TOOL_CALL_ATTRIBUTION_COMPONENT_BYTES.saturating_add(1));

    write_session(
        &root,
        "server-exact",
        "server-exact-session",
        &[
            tool_start(
                "server-exact-start",
                "server-exact",
                &exact_server,
                "lookup",
            ),
            tool_completion("server-exact-complete", "server-exact", true),
        ],
    );
    write_session(
        &root,
        "tool-exact",
        "tool-exact-session",
        &[
            tool_start("tool-exact-start", "tool-exact", "server", &exact_tool),
            tool_completion("tool-exact-complete", "tool-exact", false),
        ],
    );
    write_session(
        &root,
        "server-over",
        "server-over-session",
        &[
            tool_start(
                "server-over-start",
                "server-over",
                &oversized_server,
                "lookup",
            ),
            tool_completion("server-over-complete", "server-over", true),
        ],
    );
    write_session(
        &root,
        "tool-over",
        "tool-over-session",
        &[
            tool_start("tool-over-start", "tool-over", "server", &oversized_tool),
            tool_completion("tool-over-complete", "tool-over", false),
        ],
    );

    let registry = copilot_registry(&root, temp.path());
    let index_path = temp.path().join("index");
    let receipt =
        refresh_source_backed_generation(&index_path, &registry, WriterOptions::default()).unwrap();
    assert!(receipt.failed_routes.is_empty());
    assert!(receipt.logical_source_failures.is_empty());
    assert!(receipt.record_rejections.is_empty());
    assert_eq!(receipt.sources.len(), 4);

    let records = core_records(&VerifiedIndex::open(&index_path).unwrap());
    assert_eq!(records.len(), 12);
    assert_eq!(
        record_by_native_id(&records, "server-exact-complete").mcp_tool_call,
        attribution(&exact_server, "lookup")
    );
    assert_eq!(
        record_by_native_id(&records, "tool-exact-complete").mcp_tool_call,
        attribution("server", &exact_tool)
    );
    assert_eq!(
        record_by_native_id(&records, "server-over-complete").mcp_tool_call,
        None
    );
    assert_eq!(
        record_by_native_id(&records, "tool-over-complete").mcp_tool_call,
        None
    );
    assert!(records
        .iter()
        .all(|record| record.validate_contract().is_ok()));
}
