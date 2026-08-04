use super::super::nativepath::WarpNativeCounters;
use super::*;
use crate::{record_evidence::RecordDigest, OutputOutcome};
use ctx_history_core::{EventRole, EventType};
use rusqlite::OpenFlags;

#[derive(Default)]
struct FixtureSink {
    pages: Vec<WarpNativePage>,
}

impl WarpNativeSink for FixtureSink {
    fn push_page(&mut self, page: WarpNativePage) -> CaptureResult<()> {
        self.pages.push(page);
        Ok(())
    }
}

#[test]
fn scan_accounting_includes_unknown_message_units() {
    let native_scan = WarpNativeSourceBackedScan {
        source_integrity_digest: "00".repeat(32),
        counters: WarpNativeCounters {
            sessions_retained: 1,
            ignored_messages: 1,
            ..WarpNativeCounters::default()
        },
    };
    assert_eq!(accounted_ignored_records(&native_scan, 1).unwrap(), 2);
}

#[test]
fn core_projection_keeps_success_failure_unknown_and_large_result_bodies_once() {
    let selection = WarpSourceSelectionV0::new("/tmp", "/tmp/warp.db", "surface").unwrap();
    let source = warp_source_key(&selection).unwrap();
    let lineage = WarpSessionLineage {
        parent_conversation_id: None,
        root_conversation_id: "conversation".to_owned(),
    };
    for (index, (outcome, expected)) in [
        (OutputOutcome::Success, "success"),
        (OutputOutcome::Failure, "failure"),
        (OutputOutcome::Unknown, "unknown"),
    ]
    .into_iter()
    .enumerate()
    {
        let body = if index == 0 {
            format!(
                "warp-core-head-{}-warp-core-tail",
                "x".repeat(8 * 1024 * 1024)
            )
        } else {
            format!("{expected} complete Warp result")
        };
        let record = core_record(
            &source,
            &lineage,
            WarpNativeEvent {
                identity: super::WarpNativeEventIdentity {
                    conversation_id: "conversation".to_owned(),
                    task_id: format!("task-{index}"),
                    message: WarpNativeMessageIdentity::MessageOrdinal(0),
                },
                native_order: super::WarpNativeOrder {
                    provider_event_index: u64::try_from(index).unwrap(),
                    legacy_provider_event_index: Some(u64::try_from(index).unwrap()),
                    task_rowid: i64::try_from(index + 1).unwrap(),
                    task_key: format!("task-{index}"),
                    message_ordinal: 0,
                },
                event_type: EventType::ToolOutput,
                role: Some(EventRole::Tool),
                kind: "run_shell_command",
                request_id: Some(format!("request-{index}")),
                result_outcome: Some(outcome),
                call_id: Some(format!("call-{index}")),
                mcp_invocation: None,
                mcp_attribution: false,
                occurred_at: None,
                lexical_body: body.clone(),
                source_record_digest: RecordDigest::from_text("warp source row"),
            },
        )
        .unwrap();
        assert_eq!(record.content.meaningful_text(), body);
        let structured = record.content.structured_content.as_ref().unwrap();
        assert_eq!(
            structured
                .pointer("/provider_native_result/result_outcome")
                .and_then(serde_json::Value::as_str),
            Some(expected)
        );
        assert!(!structured.to_string().contains("complete Warp result"));
        assert!(!structured.to_string().contains("warp-core-head-"));
    }
}

#[test]
fn sanitized_mcp_fixture_projects_only_unique_qualified_terminal_pairs() {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/provider-history/warp/v1/warp-mcp.sqlite");
    let connection =
        Connection::open_with_flags(fixture, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let mut sink = FixtureSink::default();
    let scan = scan_warp_source_backed_connection(&connection, &mut sink).unwrap();
    assert_eq!(scan.counters.malformed_task_cells, 1);

    let selection = WarpSourceSelectionV0::new("/tmp", "/tmp/warp.db", "surface").unwrap();
    let source = warp_source_key(&selection).unwrap();
    let lineage = WarpSessionLineage {
        parent_conversation_id: None,
        root_conversation_id: "warp-mcp-sanitized-conversation".to_owned(),
    };
    let events = sink
        .pages
        .into_iter()
        .flat_map(|page| page.events)
        .collect::<Vec<_>>();
    assert!(events
        .iter()
        .filter(|event| event.event_type == EventType::ToolCall)
        .all(|event| !event.mcp_attribution));

    let attributed_event = events
        .iter()
        .find(|event| {
            event.mcp_attribution
                && event
                    .mcp_invocation
                    .as_ref()
                    .is_some_and(|invocation| invocation.tool_name == "shared_tool")
        })
        .unwrap()
        .clone();
    let attributed = core_record(&source, &lineage, attributed_event.clone()).unwrap();
    let mut unattributed_event = attributed_event;
    unattributed_event.mcp_attribution = false;
    unattributed_event.mcp_invocation = None;
    let unattributed = core_record(&source, &lineage, unattributed_event).unwrap();
    assert_eq!(attributed.event_id, unattributed.event_id);
    assert_eq!(attributed.native_event_id, unattributed.native_event_id);

    let records = events
        .into_iter()
        .map(|event| core_record(&source, &lineage, event).unwrap())
        .collect::<Vec<_>>();
    let mut observed = records
        .iter()
        .filter_map(|record| {
            record.mcp_tool_call.as_ref().map(|attribution| {
                (
                    attribution.server.as_str(),
                    attribution.tool.as_str(),
                    record.content.meaningful_text(),
                )
            })
        })
        .collect::<Vec<_>>();
    observed.sort_unstable();
    assert_eq!(
        observed,
        vec![
            (
                "11111111-1111-4111-8111-111111111111",
                "cancel_tool",
                "cancel",
            ),
            (
                "11111111-1111-4111-8111-111111111111",
                "final_tool",
                "first result\nsecond result",
            ),
            (
                "11111111-1111-4111-8111-111111111111",
                "shared_tool",
                "first\nresource text\nlast",
            ),
            (
                "22222222-2222-4222-8222-222222222222",
                "binary_tool",
                "call_mcp_tool",
            ),
            (
                "22222222-2222-4222-8222-222222222222",
                "reused_tool",
                "call_mcp_tool",
            ),
            (
                "22222222-2222-4222-8222-222222222222",
                "shared_tool",
                "sanitized tool error",
            ),
        ]
    );
    assert!(records
        .iter()
        .filter(|record| record.event_type == EventType::ToolCall.as_str())
        .all(|record| record.mcp_tool_call.is_none()));
    assert!(records.iter().all(|record| {
        record
            .content
            .structured_content
            .as_ref()
            .is_none_or(|value| {
                value
                    .pointer("/provider_native_tool/mcp_invocation")
                    .is_none()
                    && !value.to_string().contains("\"side\":\"a\"")
            })
    }));
}
