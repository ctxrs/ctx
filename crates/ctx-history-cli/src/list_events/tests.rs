use std::{
    cell::RefCell,
    io::{self, Write},
    path::Path,
    rc::Rc,
    sync::{Arc, Mutex},
};

use ctx_history_core::{
    derive_event_id, derive_session_id, ActivityInvocation, ActivityJsonCapture, ActivityResult,
    ActivityTextCapture, AgentScope, CertifiedSource, CoreActivity, CoreRecord, EventIdentityInput,
    LiteralFactKind, NativeItemKey, NativeSessionKey, ProviderDeclaredFact, ScannedSourceCounts,
    SessionIdentityInput, SourceAnchor, SourceKey, SourceObservation, TypedKey,
    CORE_ACTIVITY_REVISION,
};
use ctx_history_index::{GenerationWriter, WriterOptions};
use serde_json::{json, Value};

use super::*;
use crate::{
    analytics::{RenderFormat, ShowTelemetry, TargetKind},
    test_query_authority::publish_empty_generation,
    ui::{RenderContext, StreamKind, TestContext},
};

#[test]
fn wire_cap_covers_worst_case_core_json_expansion() {
    assert_eq!(
        MAX_EVENT_QUERY_WIRE_RECORD_BYTES,
        ctx_history_core::MAX_ENCODED_CORE_RECORD_BYTES * 6 + 1024 * 1024
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
fn compound_invalid_list_requests_preserve_selection_cursor_limit_precedence() {
    let temp = tempfile::tempdir().unwrap();
    let mut output = Vec::new();
    let selection_error = execute(
        ListEventsArgs {
            since: Some("2026-08-01T00:00:00Z".to_owned()),
            cursor: Some("not+base64".to_owned()),
            limit: 0,
            ..ListEventsArgs::default()
        },
        temp.path(),
        &mut output,
    )
    .unwrap_err();
    assert_eq!(
        event_query_error_value(&selection_error)["error_code"],
        "invalid_range"
    );

    let cursor_error = execute(
        ListEventsArgs {
            cursor: Some("not+base64".to_owned()),
            limit: 0,
            ..ListEventsArgs::default()
        },
        temp.path(),
        &mut output,
    )
    .unwrap_err();
    assert_eq!(
        event_query_error_value(&cursor_error)["error_code"],
        "invalid_cursor"
    );
}

#[test]
fn list_gateway_opens_a_verified_empty_generation() {
    let temp = tempfile::tempdir().unwrap();
    let generation_id = publish_empty_generation(temp.path());
    let index = open_event_range_index(temp.path(), None).unwrap();
    assert_eq!(index.generation_id(), generation_id);
    assert_eq!(index.document_count(), 0);
}

#[test]
fn list_cursor_opens_its_exact_verified_retained_generation() {
    let temp = tempfile::tempdir().unwrap();
    let retained_generation = publish_empty_generation(temp.path());
    publish_fixture(temp.path(), &["active nonempty successor".to_owned()]);
    let active = open_event_range_index(temp.path(), None).unwrap();
    assert_ne!(active.generation_id(), retained_generation);
    let selection = all_selection(CoreEventRangeDirection::Ascending);
    let event = ctx_history_read_application::PinnedHistoryQuery::new(&active, None)
        .list_events_page(&ctx_history_read_application::ListEventsPageRequest {
            selection: selection.clone(),
            cursor: None,
            limit: 1,
            page_items: 1,
            byte_budget: EVENT_QUERY_PAGE_BYTES,
            strict_budget: None,
        })
        .unwrap()
        .page
        .items
        .into_iter()
        .next()
        .unwrap();
    let cursor = selection.cursor_for(&retained_generation, &event).unwrap();

    let retained = open_event_range_index(temp.path(), Some(&cursor)).unwrap();
    assert_eq!(retained.generation_id(), retained_generation);
    assert_eq!(retained.document_count(), 0);
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
        source.clone(),
        nonce,
        if nonce == 1 {
            "future_provider_event"
        } else {
            "message"
        },
        "event-query-test-v1",
        body,
    )
    .unwrap();
    record.occurred_at_unix_ms = Some(1_700_000_000_000 + i64::try_from(nonce).unwrap());
    record.provider_session_id = Some("provider-session".to_owned());
    record.native_event_id = Some(TypedKey::U64(nonce));
    record.role = Some("assistant".to_owned());
    record.agent_scope = Some(AgentScope::Primary);
    record.content.structured_content = Some(json!({"future_payload": {"kept": nonce}}));
    record.validate_contract().unwrap();
    record
}

fn test_activity(payload: Value) -> CoreActivity {
    CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id: Some(TypedKey::utf8("native-call-呼び出し-🦀").unwrap()),
        invocation: Some(ActivityInvocation {
            protocol: Some("mcp".to_owned()),
            server: Some("mcp-サーバー".to_owned()),
            tool: "検索-tool".to_owned(),
            arguments: ActivityJsonCapture::Present {
                value: json!({
                    "snake_key": ["雪", null, {"camelKey": true}],
                    "nested": {"deep_null": null},
                }),
            },
            started_at_unix_ms: Some(11),
        }),
        result: Some(ActivityResult {
            status: Some("provider::failed".to_owned()),
            completed_at_unix_ms: Some(22),
            duration_ns: Some(42),
            text: ActivityTextCapture::Omitted {
                reason: "provider_size_limit".to_owned(),
                observed_bytes: Some(70_000),
            },
            structured_content: ActivityJsonCapture::Present { value: payload },
        }),
        facts: [
            (LiteralFactKind::Workspace, "/workspace/ctx"),
            (LiteralFactKind::Branch, "main"),
            (LiteralFactKind::SessionCwd, "/workspace/ctx"),
        ]
        .into_iter()
        .map(|(kind, value)| ProviderDeclaredFact {
            kind,
            value: value.to_owned(),
        })
        .collect(),
    }
}

fn publish_fixture(data_root: &Path, bodies: &[String]) {
    let source = test_source();
    let records = bodies
        .iter()
        .enumerate()
        .map(|(index, body)| test_record(&source, index as u64, body))
        .collect::<Vec<_>>();
    publish_records(data_root, source, records);
}

fn publish_records(data_root: &Path, source: SourceKey, records: Vec<CoreRecord>) {
    let index_root = data_root.join("search/lexical");
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
    let mut writer = GenerationWriter::open(&index_root, WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
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
    assert_eq!(event["structured_content"]["future_payload"]["kept"], 1);
    for field in [
        "metadata",
        "repository_candidate_evidence",
        "repository_bindings",
        "repository_abstentions",
        "repository_file_invocation_evidence",
        "repository_file_observations",
        "repository_vcs_observations",
    ] {
        assert!(event.get(field).is_none(), "retired field leaked: {field}");
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

#[test]
fn activity_is_lossless_ordered_and_full_projection_only_across_json_and_jsonl() {
    let temp = tempfile::tempdir().unwrap();
    let source = test_source();
    let payload = json!({
        "result_key": ["完了", null, {"mixedCase": [false, 3]}],
        "nested_object": {"snake_key": null},
    });
    let activity = test_activity(payload.clone());
    let exact_activity = serde_json::to_value(&activity).unwrap();
    let mut captured = test_record(&source, 0, "response body remains unchanged");
    captured.content.activity = Some(activity);
    captured.validate_contract().unwrap();
    let absent = test_record(&source, 1, "no exchange");
    publish_records(temp.path(), source, vec![captured, absent]);

    let selection = all_selection(CoreEventRangeDirection::Ascending);
    for projection in [
        EventContentProjection::Full,
        EventContentProjection::Text,
        EventContentProjection::None,
    ] {
        let request = request(CoreEventRangeDirection::Ascending, 10, projection);
        let json_page = page(temp.path(), &selection, None, &request);
        assert!(json_page["events"][1].get("activity").is_none());
        if projection == EventContentProjection::Full {
            assert_eq!(json_page["events"][0]["activity"], exact_activity);
            assert_eq!(
                json_page["events"][0]["activity"]["result"]["structured_content"]["value"],
                payload
            );
            assert!(
                json_page["events"][0]["activity"]["result"]["structured_content"]["value"]
                    ["result_key"][1]
                    .is_null()
            );
        } else {
            assert!(json_page["events"][0].get("activity").is_none());
        }

        let mut jsonl = Vec::new();
        write_jsonl_pages(
            temp.path(),
            selection.clone(),
            None,
            &request,
            &mut jsonl,
            || {},
        )
        .unwrap();
        let first_line: Value =
            serde_json::from_slice(jsonl.split(|byte| *byte == b'\n').next().unwrap()).unwrap();
        assert_eq!(
            first_line["event"].get("activity"),
            (projection == EventContentProjection::Full).then_some(&exact_activity)
        );
    }
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
    let request = request(
        CoreEventRangeDirection::Ascending,
        1_000,
        EventContentProjection::Full,
    );
    let mut writer = TrackingWriter::default();
    let observed = Rc::clone(&writer.0);
    let mut page_flush_counts = Vec::new();
    let count = write_jsonl_pages(
        temp.path(),
        selection.clone(),
        None,
        &request,
        &mut writer,
        || page_flush_counts.push(observed.borrow().flush_offsets.len()),
    )
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
    let args = ListEventsArgs {
        format: EventQueryFormat::Jsonl,
        ..ListEventsArgs::default()
    };
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

#[test]
fn run_writes_typed_resource_errors_only_to_the_selected_machine_stderr() {
    let temp = tempfile::tempdir().unwrap();
    let stdout = SharedBytes::default();
    let stdout_copy = stdout.clone();
    let stderr = SharedBytes::default();
    let stderr_copy = stderr.clone();
    let mut ui = crate::ui::Ui::with_writers(
        stdout,
        RenderContext::for_test(TestContext::pipe(StreamKind::Stdout)),
        stderr,
        RenderContext::for_test(TestContext::pipe(StreamKind::Stderr)),
    );
    let mut telemetry = ShowTelemetry {
        target_kind: TargetKind::Events,
        transcript_mode: None,
        output_format: RenderFormat::Json,
        writes_out_file: false,
        provider_lookup: false,
        window: None,
        events_returned: None,
    };
    let mut usage = crate::local_usage::CliUsage::excluded();
    let args = ListEventsArgs {
        limit: 0,
        ..ListEventsArgs::default()
    };

    assert!(run(
        args,
        temp.path().to_path_buf(),
        &mut telemetry,
        &mut usage,
        &mut ui
    )
    .is_err());
    assert!(stdout_copy.bytes().is_empty());
    let stderr = stderr_copy.bytes();
    let value: Value = serde_json::from_slice(&stderr).unwrap();
    assert_eq!(value["error_code"], "resource_limit");
    assert_eq!(value["retryable"], false);
}
