//! Copilot native JSONL generation qualification.

use std::{
    fs::{self, OpenOptions},
    io::{BufWriter, Write},
    path::Path,
};

use ctx_history_capture_composition::{
    build_automatic_source_backed_registry_from_report_with_probes,
    refresh_source_backed_generation, DiscoveryContext, DiscoveryPlatform, DiscoveryPlatformDirs,
    DiscoveryReport, ProviderImportSupport, SourceBackedProviderRegistry,
};
use ctx_history_core::{CaptureProvider, CoreRecord, ScannedSourceCounts, TypedKey};
use ctx_history_index::{VerifiedIndex, WriterOptions};
use ctx_history_provider_native_jsonl::COPILOT_CLI_SOURCE_FORMAT;
use ctx_history_source_io::MAX_PROVIDER_JSONL_LINE_BYTES;

use crate::{fixture_provider_source_at, test_provider_probes, test_support_paths::tempdir};

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
    let build = build_automatic_source_backed_registry_from_report_with_probes(
        &test_provider_probes(),
        &context,
        &temp.join("ctx-data"),
        DiscoveryReport {
            sources: vec![fixture_provider_source_at(
                CaptureProvider::CopilotCli,
                COPILOT_CLI_SOURCE_FORMAT,
                ProviderImportSupport::Native,
                root,
            )],
            issues: Vec::new(),
        },
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

fn invocation_identity(record: &CoreRecord) -> Option<(&str, &str)> {
    record
        .content
        .activity
        .as_ref()?
        .invocation
        .as_ref()
        .and_then(|invocation| Some((invocation.server.as_deref()?, invocation.tool.as_str())))
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
}

#[test]
fn copilot_route_enforces_independent_exact_identity_component_boundaries() {
    const MAX_ACTIVITY_IDENTITY_COMPONENT_BYTES: usize = 64 * 1024;
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let exact_server = "s".repeat(MAX_ACTIVITY_IDENTITY_COMPONENT_BYTES);
    let oversized_server = "s".repeat(MAX_ACTIVITY_IDENTITY_COMPONENT_BYTES.saturating_add(1));
    let exact_tool = "t".repeat(MAX_ACTIVITY_IDENTITY_COMPONENT_BYTES);
    let oversized_tool = "t".repeat(MAX_ACTIVITY_IDENTITY_COMPONENT_BYTES.saturating_add(1));

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
        invocation_identity(record_by_native_id(&records, "server-exact-start")),
        Some((exact_server.as_str(), "lookup"))
    );
    assert_eq!(
        invocation_identity(record_by_native_id(&records, "tool-exact-start")),
        Some(("server", exact_tool.as_str()))
    );
    assert_eq!(
        invocation_identity(record_by_native_id(&records, "server-over-start")),
        None
    );
    assert_eq!(
        invocation_identity(record_by_native_id(&records, "tool-over-start")),
        None
    );
    for native_id in [
        "server-exact-complete",
        "tool-exact-complete",
        "server-over-complete",
        "tool-over-complete",
    ] {
        assert!(record_by_native_id(&records, native_id)
            .content
            .activity
            .as_ref()
            .is_some_and(|activity| activity.result.is_some()));
    }
    assert!(records
        .iter()
        .all(|record| record.validate_contract().is_ok()));
}

#[test]
fn copilot_activity_append_replay_preserves_stable_event_ids() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let index_path = temp.path().join("index");
    let cold_path = temp.path().join("cold");
    write_session(
        &root,
        "stable",
        "stable-session",
        &[
            tool_start("first-start", "first-call", "first-server", "first-tool"),
            tool_completion("first-complete", "first-call", true),
        ],
    );
    let registry = copilot_registry(&root, temp.path());

    refresh_source_backed_generation(&index_path, &registry, WriterOptions::default()).unwrap();
    let initial = core_records(&VerifiedIndex::open(&index_path).unwrap());
    let initial_start = record_by_native_id(&initial, "first-start");
    let initial_complete = record_by_native_id(&initial, "first-complete");
    let initial_start_id = initial_start.event_id;
    let initial_complete_id = initial_complete.event_id;
    let initial_start_activity = initial_start.content.activity.clone();
    let initial_complete_activity = initial_complete.content.activity.clone();

    let events_path = root.join("stable").join("events.jsonl");
    let mut writer = BufWriter::new(OpenOptions::new().append(true).open(events_path).unwrap());
    writeln!(
        writer,
        "{}",
        tool_start(
            "second-start",
            "second-call",
            "second-server",
            "second-tool"
        )
    )
    .unwrap();
    writeln!(
        writer,
        "{}",
        tool_completion("second-complete", "second-call", false)
    )
    .unwrap();
    writer.flush().unwrap();

    let appended =
        refresh_source_backed_generation(&index_path, &registry, WriterOptions::default()).unwrap();
    assert!(appended.failed_routes.is_empty());
    assert!(appended.logical_source_failures.is_empty());
    let appended_generation = appended.commit.generation_id.clone();
    let appended_records = core_records(&VerifiedIndex::open(&index_path).unwrap());
    let first_start = record_by_native_id(&appended_records, "first-start");
    let first_complete = record_by_native_id(&appended_records, "first-complete");
    assert_eq!(first_start.event_id, initial_start_id);
    assert_eq!(first_complete.event_id, initial_complete_id);
    assert_eq!(first_start.content.activity, initial_start_activity);
    assert_eq!(first_complete.content.activity, initial_complete_activity);
    assert_eq!(
        invocation_identity(record_by_native_id(&appended_records, "second-start")),
        Some(("second-server", "second-tool"))
    );
    assert!(record_by_native_id(&appended_records, "second-complete")
        .content
        .activity
        .as_ref()
        .and_then(|activity| activity.result.as_ref())
        .is_some());
    let appended_snapshot = appended_records
        .iter()
        .map(|record| serde_json::to_vec(record).unwrap())
        .collect::<Vec<_>>();

    let replay =
        refresh_source_backed_generation(&index_path, &registry, WriterOptions::default()).unwrap();
    assert_eq!(replay.commit.generation_id, appended_generation);

    refresh_source_backed_generation(&cold_path, &registry, WriterOptions::default()).unwrap();
    let cold_records = core_records(&VerifiedIndex::open(&cold_path).unwrap());
    assert_eq!(
        cold_records
            .iter()
            .map(|record| serde_json::to_vec(record).unwrap())
            .collect::<Vec<_>>(),
        appended_snapshot
    );
}
