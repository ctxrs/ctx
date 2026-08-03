use std::{
    cell::RefCell,
    io::{self, Write},
    path::Path,
    rc::Rc,
    sync::{Arc, Mutex},
};

use clap::{CommandFactory, Parser};
use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSource, CoreRecord, EventIdentityInput,
    NativeItemKey, NativeSessionKey, ScannedSourceCounts, SessionIdentityInput, SourceAnchor,
    SourceKey, SourceObservation, TypedKey,
};
use ctx_history_index::{GenerationWriter, WriterOptions};
use serde_json::{json, Value};

use super::*;
use crate::{
    analytics::{RenderFormat, ShowTelemetry, TargetKind},
    cli::CommandRoot,
    ui::{RenderContext, StreamKind, TestContext},
    Cli, ShowTarget,
};

fn parse_events(arguments: &[&str]) -> ShowEventsArgs {
    let cli = Cli::try_parse_from(arguments).expect("event query arguments should parse");
    let CommandRoot::Show(show) = cli.command else {
        panic!("expected show command");
    };
    let ShowTarget::Events(events) = show.target else {
        panic!("expected events target");
    };
    *events
}

#[test]
fn all_domain_is_explicit_and_defaults_when_omitted() {
    let defaulted = parse_events(&["ctx", "show", "events"]);
    assert!(defaulted.since.is_none() && defaulted.until.is_none());
    assert_eq!(defaulted.limit, DEFAULT_EVENT_QUERY_LIMIT);
}

#[test]
fn unreleased_aliases_and_page_budget_flags_are_rejected() {
    for flag in [
        "--all",
        "--parent",
        "--root",
        "--max-items",
        "--page-items",
        "--max-bytes",
        "--byte-budget",
    ] {
        assert!(
            Cli::try_parse_from(["ctx", "show", "events", flag]).is_err(),
            "unexpectedly accepted {flag}"
        );
    }
}

#[test]
fn every_core_filter_and_canonical_relationship_flag_maps_to_selection() {
    let id = "01234567-89ab-4def-8123-456789abcdef";
    let args = parse_events(&[
        "ctx",
        "show",
        "events",
        "--provider",
        "codex",
        "--provider",
        "claude",
        "--source",
        id,
        "--history-source",
        "plugin/source",
        "--provider-key",
        "plugin",
        "--source-id",
        "source",
        "--source-format",
        "future-format",
        "--provider-session",
        "native-session",
        "--session",
        id,
        "--parent-session",
        id,
        "--root-session",
        id,
        "--branch",
        "main",
        "--workspace",
        "workspace",
        "--event-type",
        "future-event",
        "--role",
        "assistant",
        "--agent-type",
        "future-agent",
        "--scope",
        "subagent",
        "--file",
        "src/lib.rs",
        "--direction",
        "descending",
    ]);
    let selection = selection_from_args(&args).unwrap();
    let filters = selection.filters();
    assert_eq!(filters.providers, ["claude", "codex"]);
    assert_eq!(filters.source_identity.unwrap().to_string(), id);
    assert_eq!(filters.history_source.as_deref(), Some("plugin/source"));
    assert_eq!(filters.provider_key.as_deref(), Some("plugin"));
    assert_eq!(filters.source_id.as_deref(), Some("source"));
    assert_eq!(filters.source_format.as_deref(), Some("future-format"));
    assert_eq!(
        filters.provider_session_id.as_deref(),
        Some("native-session")
    );
    assert_eq!(filters.session_id.unwrap().to_string(), id);
    assert_eq!(filters.parent_session_id.unwrap().to_string(), id);
    assert_eq!(filters.root_session_id.unwrap().to_string(), id);
    assert_eq!(filters.branch.as_deref(), Some("main"));
    assert_eq!(filters.workspace.as_deref(), Some("workspace"));
    assert_eq!(filters.event_type.as_deref(), Some("future-event"));
    assert_eq!(filters.role.as_deref(), Some("assistant"));
    assert_eq!(filters.agent_type.as_deref(), Some("future-agent"));
    assert_eq!(filters.scope, CoreEventRangeScope::Subagent);
    assert_eq!(filters.file.as_deref(), Some("src/lib.rs"));
    assert_eq!(filters.direction, CoreEventRangeDirection::Descending);
}

#[test]
fn wire_cap_covers_worst_case_core_json_expansion() {
    assert_eq!(
        MAX_EVENT_QUERY_WIRE_RECORD_BYTES,
        MAX_ENCODED_CORE_RECORD_BYTES * 6 + 1024 * 1024
    );
    const { assert!(MAX_EVENT_QUERY_WIRE_RECORD_BYTES < 512 * 1024 * 1024) };
}

#[test]
fn machine_errors_are_typed_for_ranges_cursors_and_resource_limits() {
    let cursor = decode_cursor("not+base64").unwrap_err();
    assert_eq!(
        event_query_error_value(&cursor)["error_code"],
        "invalid_cursor"
    );
    let range = selection(
        Some("2026-08-01T00:00:00Z"),
        None,
        CoreEventRangeFilters::default(),
    )
    .unwrap_err();
    assert_eq!(
        event_query_error_value(&range)["error_code"],
        "invalid_range"
    );
    let resource = validated_limit(0).unwrap_err();
    assert_eq!(
        event_query_error_value(&resource)["error_code"],
        "resource_limit"
    );
}

#[test]
fn help_exposes_only_the_compact_show_events_route() {
    let command = Cli::command();
    assert!(command
        .get_subcommands()
        .all(|subcommand| subcommand.get_name() != "export"));
    let help = Cli::try_parse_from(["ctx", "show", "events", "--help"])
        .unwrap_err()
        .to_string();
    for expected in ["--since", "--until", "--parent-session", "--root-session"] {
        assert!(help.contains(expected), "missing {expected} from help");
    }
    for removed in [
        "--all",
        "--parent ",
        "--root ",
        "--max-items",
        "--page-items",
        "--max-bytes",
        "--byte-budget",
    ] {
        assert!(!help.contains(removed), "unexpected {removed} in help");
    }
}

fn test_source() -> SourceKey {
    SourceKey::derive(
        "codex",
        "codex_session_test",
        "session",
        1,
        SourceAnchor::provider_native(
            "session-file",
            TypedKey::utf8("event-query-fixture").unwrap(),
        )
        .unwrap(),
    )
    .unwrap()
}

fn test_record(source: &SourceKey, nonce: u64, body: &str) -> CoreRecord {
    let native_session_key =
        NativeSessionKey::native_id("session", TypedKey::utf8("session").unwrap()).unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: "thread",
        native_session_key: &native_session_key,
    })
    .unwrap();
    let native_item_key = NativeItemKey::native_id("message", TypedKey::U64(nonce)).unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .unwrap();
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        session_id,
        source.clone(),
        nonce,
        if nonce == 1 {
            "future_provider_event"
        } else {
            "message"
        },
        "codex",
        true,
        "event-query-test-v1",
        body,
    )
    .unwrap();
    record.occurred_at_unix_ms = Some(1_700_000_000_000 + i64::try_from(nonce).unwrap());
    record.provider_session_id = Some("provider-session".to_owned());
    record.native_event_id = Some(TypedKey::U64(nonce));
    record.role = Some("assistant".to_owned());
    record.workspace = Some("/workspace/ctx".to_owned());
    record.branch = Some("main".to_owned());
    record.cwd = Some("/workspace/ctx".to_owned());
    record
        .metadata
        .insert("future_payload".to_owned(), json!({ "kept": nonce }));
    record.validate_contract().unwrap();
    record
}

fn publish_fixture(data_root: &Path, bodies: &[String]) {
    let source = test_source();
    let index_root = data_root.join("search/lexical");
    let records = bodies
        .iter()
        .enumerate()
        .map(|(index, body)| test_record(&source, index as u64, body))
        .collect::<Vec<_>>();
    let observation = SourceObservation::new(source.clone(), "regular-file-v1", vec![1]).unwrap();
    let certificate = CertifiedSource::certify(
        observation.clone(),
        observation,
        "event-query-test-v1",
        [1; 32],
        ScannedSourceCounts {
            complete_records: records.len() as u64,
            retained_records: records.len() as u64,
            indexed_documents: records.len() as u64,
            certified_bytes: records.len() as u64,
            ..ScannedSourceCounts::default()
        },
    )
    .unwrap();
    let mut writer = GenerationWriter::open(&index_root, WriterOptions::default()).unwrap();
    writer.begin_source(source).unwrap();
    for record in records {
        writer.add_core_record(record).unwrap();
    }
    writer.certify_source(certificate).unwrap();
    writer.commit(|_| true).unwrap();
}

fn all_selection(direction: CoreEventRangeDirection) -> CoreEventRangeSelection {
    CoreEventRangeSelection::all(CoreEventRangeFilters {
        direction,
        ..CoreEventRangeFilters::default()
    })
    .unwrap()
}

fn request(
    direction: CoreEventRangeDirection,
    limit: usize,
    content: EventContentProjection,
) -> EventQueryWireRequest {
    EventQueryWireRequest::new(
        json!({ "kind": "all" }),
        json!({}),
        direction,
        content,
        limit,
    )
}

fn page(
    data_root: &Path,
    selection: &CoreEventRangeSelection,
    cursor: Option<&CoreEventRangeCursor>,
    request: &EventQueryWireRequest,
) -> Value {
    event_range_page_value(data_root, selection, cursor, request, None).unwrap()
}

#[test]
fn json_page_has_protocol_receipt_and_correct_truncation_semantics() {
    let temp = tempfile::tempdir().unwrap();
    let bodies = (0..101).map(|index| index.to_string()).collect::<Vec<_>>();
    publish_fixture(temp.path(), &bodies);
    let selection = all_selection(CoreEventRangeDirection::Ascending);

    let normal = page(
        temp.path(),
        &selection,
        None,
        &request(
            CoreEventRangeDirection::Ascending,
            1_000,
            EventContentProjection::Full,
        ),
    );
    assert_eq!(normal["payload_type"], "event_range_page");
    assert_eq!(
        normal["events"].as_array().unwrap().len(),
        EVENT_QUERY_PAGE_ITEMS
    );
    assert_eq!(normal["terminal"], false);
    assert_eq!(normal["truncated"], false);
    assert!(normal["next_cursor"].is_string());
    assert_eq!(normal["domain"], json!({ "kind": "all" }));
    assert_eq!(normal["direction"], "ascending");
    assert_eq!(normal["content"], "full");
    assert_eq!(normal["limit"], 1_000);
    assert!(normal.get("limits").is_none());
    assert_eq!(normal["usage"]["items"], EVENT_QUERY_PAGE_ITEMS);
    assert_eq!(normal["usage"]["pages"], 1);
    assert_eq!(normal["usage"]["oversized_singleton"], false);
    assert_eq!(
        normal["usage"]["bytes"].as_u64().unwrap() as usize,
        serde_json::to_vec(&normal).unwrap().len() + 1
    );
    assert_eq!(normal["frontier"]["generation_id"], normal["generation_id"]);
    assert_eq!(normal["frontier"]["cursor"], normal["next_cursor"]);

    let limited = page(
        temp.path(),
        &selection,
        None,
        &request(
            CoreEventRangeDirection::Ascending,
            1,
            EventContentProjection::Full,
        ),
    );
    assert_eq!(limited["terminal"], false);
    assert_eq!(limited["truncated"], true);
    assert!(limited["next_cursor"].is_string());

    let one = tempfile::tempdir().unwrap();
    publish_fixture(one.path(), &["only".to_owned()]);
    let terminal = page(
        one.path(),
        &selection,
        None,
        &request(
            CoreEventRangeDirection::Ascending,
            10,
            EventContentProjection::Full,
        ),
    );
    assert_eq!(terminal["terminal"], true);
    assert_eq!(terminal["truncated"], false);
    assert!(terminal["next_cursor"].is_null());
}

#[test]
fn event_projection_is_canonical_complete_and_not_duplicated() {
    let temp = tempfile::tempdir().unwrap();
    publish_fixture(temp.path(), &["body".to_owned(), "unknown".to_owned()]);
    let selection = all_selection(CoreEventRangeDirection::Ascending);
    let value = page(
        temp.path(),
        &selection,
        None,
        &request(
            CoreEventRangeDirection::Ascending,
            10,
            EventContentProjection::Full,
        ),
    );
    let event = &value["events"][1];
    assert_eq!(event["event_type"], "future_provider_event");
    assert_eq!(event["sequence"], 1);
    assert!(event["occurred_at_ms"].is_number());
    assert_eq!(event["agent_scope"], "primary");
    assert!(event["source"].is_object());
    assert_eq!(event["parser_revision"], "event-query-test-v1");
    assert!(event["normalization_revision"].is_number());
    assert_eq!(event["metadata"]["future_payload"]["kept"], 1);
    for field in [
        "repository_candidate_evidence",
        "repository_bindings",
        "repository_abstentions",
        "repository_file_invocation_evidence",
        "repository_file_observations",
        "repository_vcs_observations",
    ] {
        assert!(event.get(field).is_some(), "missing {field}");
    }
    assert_eq!(event["citations"].as_array().unwrap().len(), 1);
    assert!(event.get("citation").is_none());
    assert!(event.get("event_sequence").is_none());
    assert!(event.get("occurred_at_unix_ms").is_none());
    assert!(event.get("scope").is_none());
    assert!(event.get("core_record").is_none());
    assert!(event.get("source_identity").is_none());

    let none = page(
        temp.path(),
        &selection,
        None,
        &request(
            CoreEventRangeDirection::Ascending,
            10,
            EventContentProjection::None,
        ),
    );
    assert!(none["events"][0]["text"].is_null());
    assert!(none["events"][0]["structured_content"].is_null());
    assert_eq!(none["events"][0]["content"]["policy_status"], "selected");
}

#[derive(Default)]
struct StreamState {
    bytes: Vec<u8>,
    flush_offsets: Vec<usize>,
}

#[derive(Clone, Default)]
struct TrackingWriter(Rc<RefCell<StreamState>>);

impl Write for TrackingWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0.borrow_mut().bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut state = self.0.borrow_mut();
        let offset = state.bytes.len();
        state.flush_offsets.push(offset);
        Ok(())
    }
}

#[test]
fn jsonl_flushes_each_event_before_fetching_the_next_page_and_completes_once() {
    let temp = tempfile::tempdir().unwrap();
    let bodies = (0..101).map(|index| index.to_string()).collect::<Vec<_>>();
    publish_fixture(temp.path(), &bodies);
    let selection = all_selection(CoreEventRangeDirection::Ascending);
    let index = open_event_range_index(temp.path(), None).unwrap();
    let request = request(
        CoreEventRangeDirection::Ascending,
        1_000,
        EventContentProjection::Full,
    );
    let mut writer = TrackingWriter::default();
    let observed = Rc::clone(&writer.0);
    let mut page_flush_counts = Vec::new();
    let count = write_jsonl_pages(&index, &selection, None, &request, &mut writer, || {
        page_flush_counts.push(observed.borrow().flush_offsets.len())
    })
    .unwrap();
    assert_eq!(count, 101);
    assert_eq!(page_flush_counts, [100, 101]);

    let state = writer.0.borrow();
    let lines = state
        .bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 102);
    for (ordinal, line) in lines[..101].iter().enumerate() {
        assert_eq!(line["record_type"], "event_range_event");
        assert_eq!(line["ordinal"], ordinal);
    }
    let completion = &lines[101];
    assert_eq!(completion["record_type"], "event_range_completion");
    assert_eq!(completion["terminal"], true);
    assert_eq!(completion["truncated"], false);
    assert_eq!(completion["usage"]["items"], 101);
    assert_eq!(completion["usage"]["pages"], 2);
    assert_eq!(completion["usage"]["bytes"], state.bytes.len());
    assert_eq!(completion["usage"]["oversized_singleton_pages"], 0);
    assert!(completion.get("limits").is_none());
    assert_eq!(completion["domain"], json!({ "kind": "all" }));
    assert_eq!(state.flush_offsets.len(), 102);
    let json_page = page(temp.path(), &selection, None, &request);
    assert_eq!(lines[0]["event"], json_page["events"][0]);
}

#[test]
fn cursor_direction_filters_and_mcp_page_share_one_wire_shape() {
    let temp = tempfile::tempdir().unwrap();
    publish_fixture(
        temp.path(),
        &["zero".to_owned(), "one".to_owned(), "two".to_owned()],
    );
    let selection = all_selection(CoreEventRangeDirection::Descending);
    let descending_request = request(
        CoreEventRangeDirection::Descending,
        DEFAULT_EVENT_QUERY_LIMIT as usize,
        EventContentProjection::Text,
    );
    let cli_page = page(temp.path(), &selection, None, &descending_request);
    let sequences = cli_page["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["sequence"].as_u64().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(sequences, [2, 1, 0]);

    let mcp_page = crate::mcp::query_events_for_test(
        &json!({
            "direction": "descending",
            "content": "text",
            "limit": DEFAULT_EVENT_QUERY_LIMIT,
        }),
        temp.path(),
    )
    .unwrap();
    assert_eq!(mcp_page, cli_page);

    let first_request = request(
        CoreEventRangeDirection::Ascending,
        1,
        EventContentProjection::None,
    );
    let ascending = all_selection(CoreEventRangeDirection::Ascending);
    let first = page(temp.path(), &ascending, None, &first_request);
    let cursor = decode_cursor(first["next_cursor"].as_str().unwrap()).unwrap();
    let second = page(temp.path(), &ascending, Some(&cursor), &first_request);
    assert_ne!(
        first["events"][0]["ctx_event_id"],
        second["events"][0]["ctx_event_id"]
    );
    let mismatched = CoreEventRangeSelection::all(CoreEventRangeFilters {
        providers: vec!["claude".to_owned()],
        ..CoreEventRangeFilters::default()
    })
    .unwrap();
    assert!(matches!(
        event_range_page_value(
            temp.path(),
            &mismatched,
            Some(&cursor),
            &first_request,
            None
        ),
        Err(EventQueryError::Range(
            CoreEventRangeError::CursorSelectionMismatch
        ))
    ));
}

#[test]
fn full_projection_admits_a_valid_oversized_singleton_under_the_wire_cap() {
    let temp = tempfile::tempdir().unwrap();
    let body = "\0".repeat(200_000);
    publish_fixture(temp.path(), std::slice::from_ref(&body));
    let value = page(
        temp.path(),
        &all_selection(CoreEventRangeDirection::Ascending),
        None,
        &request(
            CoreEventRangeDirection::Ascending,
            10,
            EventContentProjection::Full,
        ),
    );
    assert_eq!(value["events"][0]["text"], body);
    assert_eq!(value["usage"]["oversized_singleton"], true);
    assert!(value["usage"]["bytes"].as_u64().unwrap() as usize > EVENT_QUERY_PAGE_BYTES);
    assert!(
        value["usage"]["bytes"].as_u64().unwrap() as usize <= MAX_EVENT_QUERY_WIRE_RECORD_BYTES
    );
}

#[test]
fn mcp_rejects_near_limit_escape_heavy_record_with_typed_error() {
    let temp = tempfile::tempdir().unwrap();
    let hard_cap = mcp_event_query_core_record_bytes(
        crate::presentation_limit::MCP_PRESENTATION_MAX_OUTPUT_BYTES,
    );
    let body = "\0".repeat(hard_cap / 6);
    let encoded_core_bytes = test_record(&test_source(), 0, &body)
        .encode_stored()
        .unwrap()
        .len();
    assert!(encoded_core_bytes > hard_cap);
    assert!(encoded_core_bytes < hard_cap + 4_096);
    publish_fixture(temp.path(), std::slice::from_ref(&body));

    let error = crate::mcp::query_events_for_test(&json!({}), temp.path()).unwrap_err();
    let error = error.downcast_ref::<EventQueryError>().unwrap();
    let value = event_query_error_value(error);
    assert_eq!(value["error_code"], "output_limit_exceeded");
    assert_eq!(value["retryable"], true);
    assert_eq!(
        value["recommendation"],
        "use CLI JSONL with ctx show events"
    );
}

#[derive(Clone, Default)]
struct SharedBytes(Arc<Mutex<Vec<u8>>>);

impl SharedBytes {
    fn bytes(&self) -> Vec<u8> {
        self.0.lock().unwrap().clone()
    }
}

impl Write for SharedBytes {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct BrokenPipeWriter;

impl Write for BrokenPipeWriter {
    fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(io::ErrorKind::BrokenPipe, "consumer closed"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn broken_pipe_is_quiet_and_successful() {
    let temp = tempfile::tempdir().unwrap();
    publish_fixture(temp.path(), &["body".to_owned()]);
    let stderr = SharedBytes::default();
    let stderr_copy = stderr.clone();
    let mut ui = crate::ui::Ui::with_writers(
        BrokenPipeWriter,
        RenderContext::for_test(TestContext::pipe(StreamKind::Stdout)),
        stderr,
        RenderContext::for_test(TestContext::pipe(StreamKind::Stderr)),
    );
    let mut telemetry = ShowTelemetry {
        target_kind: TargetKind::Events,
        transcript_mode: None,
        output_format: RenderFormat::Jsonl,
        writes_out_file: false,
        provider_lookup: false,
        window: None,
        events_returned: None,
    };
    let mut usage = crate::local_usage::CliUsage::excluded();
    let args = parse_events(&["ctx", "show", "events", "--format", "jsonl"]);
    assert!(run(
        args,
        temp.path().to_path_buf(),
        &mut telemetry,
        &mut usage,
        &mut ui
    )
    .is_ok());
    assert!(stderr_copy.bytes().is_empty());
}
