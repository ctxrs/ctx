use ctx_history_core::{EventRole, EventType};
use serde_json::{json, Value};

use super::normalization::{
    enrich_tool_output, normalize_node, DevinNativeEvent, DevinNodeDisposition,
};

fn normalize(value: Value) -> Vec<DevinNativeEvent> {
    normalize_node(&value.to_string(), None).unwrap().events
}

fn kinds(events: &[DevinNativeEvent]) -> Vec<(EventType, EventRole)> {
    events
        .iter()
        .map(|event| (event.event_type, event.role))
        .collect()
}

#[test]
fn a_user_node_becomes_one_message() {
    let events = normalize(json!({"role": "user", "content": "  hello  "}));
    assert_eq!(kinds(&events), [(EventType::Message, EventRole::User)]);
    assert_eq!(events[0].text, "hello");
}

#[test]
fn an_assistant_node_fans_out_reasoning_content_and_every_tool_call() {
    let events = normalize(json!({
        "role": "assistant",
        "content": "answer",
        "thinking": {"thinking": "reasoning", "signature": "sig", "signature_type": "kind"},
        "tool_calls": [
            {"id": "exec_0", "name": "exec", "arguments": {"command": "echo hi"}},
            {"id": "read_0", "name": "read", "arguments": {"file_path": "/tmp/a.txt"}},
        ],
    }));
    assert_eq!(
        kinds(&events),
        [
            (EventType::Summary, EventRole::Assistant),
            (EventType::Message, EventRole::Assistant),
            (EventType::ToolCall, EventRole::Assistant),
            (EventType::ToolCall, EventRole::Assistant),
        ]
    );
    assert_eq!(events[0].text, "reasoning");
    assert_eq!(events[1].text, "answer");

    // A command tool contributes the command it ran.
    assert_eq!(events[2].text, "echo hi");
    assert_eq!(events[2].command.as_deref(), Some("echo hi"));
    assert_eq!(events[2].provider_call_id.as_deref(), Some("exec_0"));
    assert_eq!(events[2].tool_name.as_deref(), Some("exec"));

    // A non-command tool retains its arguments rather than a rendered
    // description, and its literal file path becomes a touch.
    assert!(events[3].text.contains("/tmp/a.txt"));
    assert_eq!(events[3].command, None);
    assert_eq!(events[3].file_paths, ["/tmp/a.txt"]);
}

#[test]
fn the_reasoning_signature_is_never_copied_anywhere() {
    let events = normalize(json!({
        "role": "assistant",
        "content": "answer",
        "thinking": {
            "thinking": "reasoning",
            "signature": "SECRET-ATTESTATION",
            "signature_type": "SECRET-KIND",
        },
    }));
    let rendered = events
        .iter()
        .map(|event| {
            format!(
                "{}{}",
                event.text,
                event
                    .structured_content
                    .as_ref()
                    .map(Value::to_string)
                    .unwrap_or_default()
            )
        })
        .collect::<String>();
    assert!(rendered.contains("reasoning"));
    assert!(!rendered.contains("SECRET-ATTESTATION"), "{rendered}");
    assert!(!rendered.contains("SECRET-KIND"), "{rendered}");
    assert!(!rendered.contains("signature"), "{rendered}");
}

#[test]
fn a_system_node_is_a_notice_unless_the_row_records_a_compaction() {
    let notice = normalize(json!({"role": "system", "content": "environment"}));
    assert_eq!(kinds(&notice), [(EventType::Notice, EventRole::System)]);

    let summary = normalize_node(
        &json!({"role": "system", "content": "continuing"}).to_string(),
        Some(42),
    )
    .unwrap()
    .events;
    assert_eq!(kinds(&summary), [(EventType::Summary, EventRole::System)]);
    assert_eq!(summary[0].text, "continuing");
}

#[test]
fn a_tool_node_falls_back_to_recorded_terminal_output_and_reads_its_timing() {
    let events = normalize(json!({
        "role": "tool",
        "content": "",
        "tool_call_id": "exec_0",
        "metadata": {"extensions": {
            "chisel/terminal_output": {"text": "stdout\n", "cwd": "/work"},
            "chisel/tool_call_timing": {
                "finished_at": "2026-08-19T19:20:07.467967400Z",
                "duration_ms": 3,
            },
        }},
    }));
    assert_eq!(kinds(&events), [(EventType::ToolOutput, EventRole::Tool)]);
    assert_eq!(events[0].text, "stdout");
    assert_eq!(events[0].provider_call_id.as_deref(), Some("exec_0"));
    assert_eq!(events[0].workdir.as_deref(), Some("/work"));
    assert_eq!(events[0].duration_ms, Some(3));
    assert_eq!(
        events[0].completed_at.as_deref(),
        Some("2026-08-19T19:20:07.467967400Z")
    );
}

#[test]
fn an_empty_tool_result_is_preserved_but_absent_result_text_is_not() {
    let events = normalize(json!({
        "role": "tool",
        "content": "",
        "tool_call_id": "exec_empty",
        "metadata": {"extensions": {
            "chisel/terminal_output": {"text": "", "cwd": "/tmp/devin-fixture"},
            "chisel/tool_call_timing": {
                "finished_at": "2026-08-19T19:20:07.467967400Z",
                "duration_ms": 2,
            },
        }},
    }));
    assert_eq!(kinds(&events), [(EventType::ToolOutput, EventRole::Tool)]);
    assert_eq!(events[0].text, "");
    assert_eq!(events[0].provider_call_id.as_deref(), Some("exec_empty"));
    assert_eq!(events[0].workdir.as_deref(), Some("/tmp/devin-fixture"));
    assert_eq!(events[0].duration_ms, Some(2));

    let absent = normalize_node(
        &json!({
            "role": "tool",
            "tool_call_id": "exec_absent",
            "metadata": {"extensions": {
                "chisel/terminal_output": {"cwd": "/tmp/devin-fixture"},
            }},
        })
        .to_string(),
        None,
    )
    .unwrap();
    assert!(absent.events.is_empty());
    assert_eq!(absent.disposition, Some(DevinNodeDisposition::Empty));
}

#[test]
fn a_node_the_reader_cannot_audit_is_refused_and_an_empty_one_is_ignored() {
    for payload in [
        json!({"role": "future_role", "content": "text"}),
        json!({"content": "no role"}),
        json!(["not", "an", "object"]),
        json!("bare string"),
    ] {
        let node = normalize_node(&payload.to_string(), None).unwrap();
        assert_eq!(
            node.disposition,
            Some(DevinNodeDisposition::Unsupported),
            "{payload}"
        );
        assert!(node.events.is_empty());
    }

    let node = normalize_node(r#"{"role":"user","content":"   "}"#, None).unwrap();
    assert_eq!(node.disposition, Some(DevinNodeDisposition::Empty));

    // Malformed JSON is refused rather than partially read.
    assert_eq!(
        normalize_node("{not json", None).unwrap().disposition,
        Some(DevinNodeDisposition::Unsupported)
    );
}

#[test]
fn a_duplicate_json_key_is_refused_rather_than_resolved() {
    let node = normalize_node(r#"{"role":"user","content":"a","content":"b"}"#, None).unwrap();
    assert_eq!(node.disposition, Some(DevinNodeDisposition::Unsupported));
}

#[test]
fn acp_enrichment_is_additive_and_never_overwrites_the_node() {
    let mut event = normalize(json!({
        "role": "tool",
        "content": "harness text",
        "tool_call_id": "edit_1",
    }))
    .remove(0);
    let call = json!({
        "toolCallId": "edit_1",
        "locations": [{"path": "/tmp/note.txt"}],
        "rawInput": {"file_path": "/tmp/other.txt"},
        "_meta": {"cognition.ai/inferenceToolName": "edit"},
    });
    let update = json!({
        "toolCallId": "edit_1",
        "status": "completed",
        "content": [{"type": "content", "content": {"type": "text", "text": "acp text"}}],
        "_meta": {"cognition.ai/cwd": "/tmp/devin-fixture"},
    });
    enrich_tool_output(&mut event, Some(&call), Some(&update));

    // The harness text wins; ACP only fills what was absent.
    assert_eq!(event.text, "harness text");
    assert_eq!(event.status.as_deref(), Some("completed"));
    assert_eq!(event.workdir.as_deref(), Some("/tmp/devin-fixture"));
    assert_eq!(event.tool_name.as_deref(), Some("edit"));
    assert_eq!(event.file_paths, ["/tmp/note.txt", "/tmp/other.txt"]);

    // An unfinished call contributes no status.
    let mut pending = normalize(json!({"role": "tool", "content": "x"})).remove(0);
    enrich_tool_output(&mut pending, None, Some(&json!({"status": "in_progress"})));
    assert_eq!(pending.status, None);
}

#[test]
fn acp_enrichment_supplies_text_to_an_empty_normalized_result() {
    let mut event = normalize(json!({
        "role": "tool",
        "content": "",
        "tool_call_id": "exec_0",
        "metadata": {"extensions": {"chisel/terminal_output": {
            "text": "",
            "cwd": "/tmp/devin-fixture",
        }}},
    }))
    .remove(0);
    assert_eq!(event.text, "");
    assert_eq!(event.provider_call_id.as_deref(), Some("exec_0"));

    let call = json!({
        "toolCallId": "exec_0",
        "rawInput": {"command": "false"},
        "_meta": {"cognition.ai/inferenceToolName": "exec"},
    });
    let update = json!({
        "toolCallId": "exec_0",
        "status": "failed",
        "content": [{"type": "content", "content": {"type": "text", "text": "acp only"}}],
    });
    let expected_acp = json!({
        "acp": {
            "tool_call": call.clone(),
            "tool_call_update": update.clone(),
        },
    });
    enrich_tool_output(&mut event, Some(&call), Some(&update));

    assert_eq!(event.text, "acp only");
    assert_eq!(event.status.as_deref(), Some("failed"));
    assert_eq!(event.tool_name.as_deref(), Some("exec"));
    assert_eq!(event.structured_content.as_ref(), Some(&expected_acp));
}

#[test]
fn every_fixture_node_normalizes_without_error() {
    let conn = super::schema_tests::fixture_connection();
    let mut statement = conn
        .prepare(
            "select chat_message, json_extract(metadata, '$.summarized_from') \
             from message_nodes order by session_id, node_id",
        )
        .unwrap();
    let mut rows = statement.query([]).unwrap();
    let mut totals = std::collections::BTreeMap::<String, usize>::new();
    let mut unsupported = 0;
    let mut nodes = 0;
    while let Some(row) = rows.next().unwrap() {
        let chat_message: String = row.get(0).unwrap();
        let summarized_from: Option<i64> = row.get(1).unwrap();
        let node = normalize_node(&chat_message, summarized_from).unwrap();
        nodes += 1;
        if node.disposition == Some(DevinNodeDisposition::Unsupported) {
            unsupported += 1;
        }
        for event in &node.events {
            *totals
                .entry(format!("{:?}/{:?}", event.event_type, event.role))
                .or_default() += 1;
        }
    }
    assert_eq!(nodes, 114);
    assert_eq!(unsupported, 0, "no recorded node should be unreadable");
    // Every audited role appears, which is what makes the fixture a useful
    // oracle for the projection.
    for expected in [
        "Message/User",
        "Message/Assistant",
        "Summary/Assistant",
        "Summary/System",
        "Notice/System",
        "ToolCall/Assistant",
        "ToolOutput/Tool",
    ] {
        assert!(
            totals.contains_key(expected),
            "missing {expected}: {totals:?}"
        );
    }
}
