use super::super::nativepath::WarpNativeCounters;
use super::*;
use crate::{record_evidence::RecordDigest, OutputOutcome};
use ctx_history_core::{
    CoreRecord, EventRole, EventType, McpFailureKind, McpJsonCapture, McpPayloadOmissionReason,
    McpTerminalStatus, McpTextCapture, MAX_CORE_CONTENT_BYTES,
};
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

fn invocation_record<'a>(
    records: &'a [CoreRecord],
    call_id: &str,
    server: &str,
    tool: &str,
) -> &'a CoreRecord {
    records
        .iter()
        .find(|record| {
            record
                .content
                .mcp_exchange
                .as_ref()
                .is_some_and(|exchange| {
                    exchange.provider_call_id == call_id
                        && exchange.invocation.as_ref().is_some_and(|invocation| {
                            invocation.server == server && invocation.tool == tool
                        })
                })
        })
        .unwrap()
}

fn response_record<'a>(
    records: &'a [CoreRecord],
    call_id: &str,
    server: &str,
    tool: &str,
) -> &'a CoreRecord {
    records
        .iter()
        .find(|record| {
            record
                .content
                .mcp_exchange
                .as_ref()
                .is_some_and(|exchange| {
                    exchange.provider_call_id == call_id && exchange.response.is_some()
                })
                && record
                    .mcp_tool_call
                    .as_ref()
                    .is_some_and(|identity| identity.server == server && identity.tool == tool)
        })
        .unwrap()
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
                mcp_response: None,
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
                && event.mcp_invocation.as_ref().is_some_and(|invocation| {
                    invocation.server_id == "11111111-1111-4111-8111-111111111111"
                        && invocation.tool_name == "shared_tool"
                })
        })
        .unwrap()
        .clone();
    let invocation_event = events
        .iter()
        .find(|event| {
            event.event_type == EventType::ToolCall
                && event.mcp_invocation.as_ref().is_some_and(|invocation| {
                    invocation.server_id == "11111111-1111-4111-8111-111111111111"
                        && invocation.tool_name == "shared_tool"
                })
        })
        .unwrap()
        .clone();
    let oversized_response_event = attributed_event.clone();
    let compact_invocation_event = invocation_event.clone();
    let compact_unavailable_response_event = attributed_event.clone();
    let attributed = core_record(&source, &lineage, attributed_event.clone()).unwrap();
    let mut unattributed_event = attributed_event;
    unattributed_event.mcp_attribution = false;
    unattributed_event.mcp_invocation = None;
    unattributed_event.mcp_response = None;
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

    let exchanges = records
        .iter()
        .filter(|record| record.content.mcp_exchange.is_some())
        .count();
    assert_eq!(exchanges, 12);
    let mut invocation_links = records
        .iter()
        .filter_map(|record| {
            let exchange = record.content.mcp_exchange.as_ref()?;
            let invocation = exchange.invocation.as_ref()?;
            Some((
                exchange.provider_call_id.clone(),
                invocation.server.clone(),
                invocation.tool.clone(),
            ))
        })
        .collect::<Vec<_>>();
    let mut response_links = records
        .iter()
        .filter_map(|record| {
            let exchange = record.content.mcp_exchange.as_ref()?;
            exchange.response.as_ref()?;
            let identity = record.mcp_tool_call.as_ref()?;
            Some((
                exchange.provider_call_id.clone(),
                identity.server.clone(),
                identity.tool.clone(),
            ))
        })
        .collect::<Vec<_>>();
    invocation_links.sort_unstable();
    response_links.sort_unstable();
    assert_eq!(invocation_links, response_links);

    let success_call = invocation_record(
        &records,
        "success",
        "11111111-1111-4111-8111-111111111111",
        "shared_tool",
    );
    let success_invocation = success_call
        .content
        .mcp_exchange
        .as_ref()
        .unwrap()
        .invocation
        .as_ref()
        .unwrap();
    assert_eq!(
        success_invocation.arguments,
        McpJsonCapture::Present {
            value: serde_json::json!({"side": "a"}),
        }
    );
    let final_call = invocation_record(
        &records,
        "final-id",
        "11111111-1111-4111-8111-111111111111",
        "final_tool",
    );
    assert_eq!(
        final_call
            .content
            .mcp_exchange
            .as_ref()
            .unwrap()
            .invocation
            .as_ref()
            .unwrap()
            .arguments,
        McpJsonCapture::Present {
            value: serde_json::json!({"first": "one", "second": true}),
        }
    );

    let success = response_record(
        &records,
        "success",
        "11111111-1111-4111-8111-111111111111",
        "shared_tool",
    );
    let success_response = success
        .content
        .mcp_exchange
        .as_ref()
        .unwrap()
        .response
        .as_ref()
        .unwrap();
    assert_eq!(success_response.status, McpTerminalStatus::Succeeded);
    assert_eq!(success_response.failure_kind, None);
    assert_eq!(success_response.duration_ns, None);
    assert_eq!(success_response.text, McpTextCapture::NormalizedBody);
    assert_eq!(success_response.payload, McpJsonCapture::Unavailable);
    assert_eq!(
        success.content.meaningful_text(),
        "first\nresource text\nlast"
    );

    let failure = response_record(
        &records,
        "error",
        "22222222-2222-4222-8222-222222222222",
        "shared_tool",
    );
    let failure_response = failure
        .content
        .mcp_exchange
        .as_ref()
        .unwrap()
        .response
        .as_ref()
        .unwrap();
    assert_eq!(failure_response.status, McpTerminalStatus::Failed);
    assert_eq!(failure_response.failure_kind, Some(McpFailureKind::Unknown));
    assert_eq!(failure_response.text, McpTextCapture::NormalizedBody);
    assert_eq!(
        failure_response.payload,
        McpJsonCapture::Present {
            value: serde_json::json!({"error": {"message": "sanitized tool error"}}),
        }
    );

    let cancelled = response_record(
        &records,
        "cancel",
        "11111111-1111-4111-8111-111111111111",
        "cancel_tool",
    );
    let cancelled_response = cancelled
        .content
        .mcp_exchange
        .as_ref()
        .unwrap()
        .response
        .as_ref()
        .unwrap();
    assert_eq!(cancelled_response.status, McpTerminalStatus::Cancelled);
    assert_eq!(cancelled_response.failure_kind, None);
    assert_eq!(cancelled_response.text, McpTextCapture::Absent);
    assert_eq!(cancelled_response.payload, McpJsonCapture::Absent);
    assert_eq!(cancelled.content.meaningful_text(), "cancel");

    let binary = response_record(
        &records,
        "nontext",
        "22222222-2222-4222-8222-222222222222",
        "binary_tool",
    );
    let binary_response = binary
        .content
        .mcp_exchange
        .as_ref()
        .unwrap()
        .response
        .as_ref()
        .unwrap();
    assert_eq!(binary_response.status, McpTerminalStatus::Succeeded);
    assert_eq!(binary_response.text, McpTextCapture::Absent);
    assert_eq!(binary.content.meaningful_text(), "call_mcp_tool");
    assert_eq!(binary_response.payload, McpJsonCapture::Unavailable);
    assert!(records
        .iter()
        .all(|record| !record
            .content
            .mcp_exchange
            .as_ref()
            .is_some_and(|exchange| serde_json::to_string(exchange)
                .unwrap()
                .contains("c2FuaXRpemVk"))));

    let bodyless = response_record(
        &records,
        "success",
        "22222222-2222-4222-8222-222222222222",
        "reused_tool",
    );
    let bodyless_response = bodyless
        .content
        .mcp_exchange
        .as_ref()
        .unwrap()
        .response
        .as_ref()
        .unwrap();
    assert_eq!(bodyless_response.status, McpTerminalStatus::Succeeded);
    assert_eq!(bodyless_response.text, McpTextCapture::Absent);
    assert_eq!(bodyless.content.meaningful_text(), "call_mcp_tool");
    assert_eq!(
        bodyless_response.payload,
        McpJsonCapture::Present {
            value: serde_json::json!({"success": {"results": []}}),
        }
    );
    assert!(records.iter().all(|record| {
        record.content.mcp_exchange.as_ref().is_none_or(|exchange| {
            exchange.response.is_none()
                || (exchange.invocation.is_none()
                    && !serde_json::to_string(exchange)
                        .unwrap()
                        .contains("\"arguments\""))
        })
    }));
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

    let mut oversized_invocation_event = invocation_event;
    oversized_invocation_event
        .mcp_invocation
        .as_mut()
        .unwrap()
        .args = serde_json::json!({"oversized": "x".repeat(MAX_CORE_CONTENT_BYTES)});
    let oversized_invocation = core_record(&source, &lineage, oversized_invocation_event).unwrap();
    assert_eq!(oversized_invocation.event_id, success_call.event_id);
    assert_eq!(
        oversized_invocation.content.meaningful_text(),
        success_call.content.meaningful_text()
    );
    let McpJsonCapture::Omitted {
        reason,
        observed_encoded_bytes,
    } = &oversized_invocation
        .content
        .mcp_exchange
        .as_ref()
        .unwrap()
        .invocation
        .as_ref()
        .unwrap()
        .arguments
    else {
        panic!("oversized Warp arguments were not explicitly omitted");
    };
    assert_eq!(*reason, McpPayloadOmissionReason::SizeLimit);
    assert!(observed_encoded_bytes
        .is_some_and(|bytes| bytes > u64::try_from(MAX_CORE_CONTENT_BYTES).unwrap()));

    let mut oversized_response_event = oversized_response_event;
    let retained_response_text = oversized_response_event.lexical_body.clone();
    oversized_response_event
        .mcp_response
        .as_mut()
        .unwrap()
        .payload = McpJsonCapture::Present {
        value: serde_json::json!({"oversized": "x".repeat(MAX_CORE_CONTENT_BYTES)}),
    };
    let oversized_response = core_record(&source, &lineage, oversized_response_event).unwrap();
    assert_eq!(
        oversized_response.content.meaningful_text(),
        retained_response_text
    );
    let response = oversized_response
        .content
        .mcp_exchange
        .as_ref()
        .unwrap()
        .response
        .as_ref()
        .unwrap();
    assert_eq!(response.text, McpTextCapture::NormalizedBody);
    let McpJsonCapture::Omitted {
        reason,
        observed_encoded_bytes,
    } = &response.payload
    else {
        panic!("oversized Warp response payload was not explicitly omitted");
    };
    assert_eq!(*reason, McpPayloadOmissionReason::SizeLimit);
    assert!(observed_encoded_bytes
        .is_some_and(|bytes| bytes > u64::try_from(MAX_CORE_CONTENT_BYTES).unwrap()));

    let invocation_structured_bytes =
        serde_json::to_vec(success_call.content.structured_content.as_ref().unwrap())
            .unwrap()
            .len();
    let mut compact_invocation_event = compact_invocation_event;
    compact_invocation_event.lexical_body =
        "x".repeat(MAX_CORE_CONTENT_BYTES - invocation_structured_bytes);
    let compact_invocation = core_record(&source, &lineage, compact_invocation_event).unwrap();
    assert_eq!(compact_invocation.event_id, success_call.event_id);
    assert_eq!(
        compact_invocation.content.meaningful_text().len(),
        MAX_CORE_CONTENT_BYTES - invocation_structured_bytes
    );
    assert_eq!(
        compact_invocation.content.encoded_content_bytes().unwrap(),
        MAX_CORE_CONTENT_BYTES
    );
    assert!(compact_invocation.content.mcp_exchange.is_none());

    let response_structured_bytes =
        serde_json::to_vec(attributed.content.structured_content.as_ref().unwrap())
            .unwrap()
            .len();
    let mut compact_unavailable_response_event = compact_unavailable_response_event;
    compact_unavailable_response_event.lexical_body =
        "x".repeat(MAX_CORE_CONTENT_BYTES - response_structured_bytes);
    compact_unavailable_response_event
        .mcp_response
        .as_mut()
        .unwrap()
        .payload = McpJsonCapture::Unavailable;
    let compact_unavailable_response =
        core_record(&source, &lineage, compact_unavailable_response_event).unwrap();
    assert_eq!(compact_unavailable_response.event_id, attributed.event_id);
    assert_eq!(
        compact_unavailable_response.content.meaningful_text().len(),
        MAX_CORE_CONTENT_BYTES - response_structured_bytes
    );
    assert_eq!(
        compact_unavailable_response
            .content
            .encoded_content_bytes()
            .unwrap(),
        MAX_CORE_CONTENT_BYTES
    );
    assert!(compact_unavailable_response.content.mcp_exchange.is_none());
}
