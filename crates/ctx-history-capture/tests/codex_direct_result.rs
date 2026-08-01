use std::{fs, path::Path};

use ctx_history_capture::{
    provider_source_for_path, refresh_source_backed_generation,
    register_landed_source_backed_route, SourceBackedProviderRegistry, SourceBackedRefreshReceipt,
    SourceBackedRouteSelection,
};
use ctx_history_core::CaptureProvider;
use ctx_history_index::{VerifiedIndex, WriterOptions};
use serde_json::{json, Value};

fn write_session(root: &Path, native_session_id: &str, events: &[Value]) {
    fs::create_dir_all(root).unwrap();
    let mut lines = vec![json!({
        "timestamp": "2026-08-01T12:00:00Z",
        "type": "session_meta",
        "payload": {
            "id": native_session_id,
            "timestamp": "2026-08-01T12:00:00Z",
            "cwd": "/workspace",
            "source": "cli"
        }
    })];
    lines.extend_from_slice(events);
    let mut contents = lines
        .into_iter()
        .map(|line| serde_json::to_string(&line).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    contents.push('\n');
    fs::write(
        root.join(format!("rollout-{native_session_id}.jsonl")),
        contents,
    )
    .unwrap();
}

fn publish_codex_sessions(session_root: &Path, index_root: &Path) -> SourceBackedRefreshReceipt {
    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut registry,
        provider_source_for_path(CaptureProvider::Codex, session_root.to_path_buf()),
        SourceBackedRouteSelection::ExplicitManual,
    )
    .unwrap();
    refresh_source_backed_generation(index_root, &registry, WriterOptions::default()).unwrap()
}

fn mcp_result(call_id: &str, result: Value) -> Value {
    json!({
        "timestamp": "2026-08-01T12:00:01Z",
        "type": "event_msg",
        "payload": {
            "type": "mcp_tool_call_end",
            "call_id": call_id,
            "invocation": {
                "server": "example",
                "tool": "read",
                "arguments": {"path": "/workspace/result.txt"}
            },
            "duration": {"secs": 1, "nanos": 7},
            "result": result
        }
    })
}

#[test]
fn over_8_mib_mcp_result_is_admitted_once_and_indexable() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    let native_session_id = "019fa000-0000-7000-8000-000000000024";
    let result_tail = "mcp_large_result_tail_indexable";
    let full_result = format!("{} {result_tail}", "x".repeat(9 * 1024 * 1024));
    assert!(full_result.len() > 8 * 1024 * 1024);
    write_session(
        &sessions,
        native_session_id,
        &[mcp_result(
            "exec-mcp-large",
            json!({
                "Ok": {
                    "content": [{"type": "text", "text": full_result}],
                    "isError": false,
                    "_meta": {"surface": "fixture"}
                }
            }),
        )],
    );

    publish_codex_sessions(&sessions, &index);
    let verified = VerifiedIndex::open(&index).unwrap();
    let candidate = verified
        .search_event_candidates(result_tail, 10)
        .unwrap()
        .into_iter()
        .next()
        .expect("large result tail is indexed");
    let core = verified
        .core_record_by_id(candidate.event.event_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(
        core.content.normalized_body.as_deref(),
        Some(full_result.as_str())
    );
    let structured = core.content.structured_content.as_ref().unwrap();
    assert_eq!(
        structured["provider_native_tool_result"]["result_variant"],
        "Ok"
    );
    assert_eq!(
        structured["provider_native_tool_result"]["result_metadata"]["_meta"]["surface"],
        "fixture"
    );
    let encoded = serde_json::to_string(structured).unwrap();
    assert!(encoded.len() < 4 * 1024);
    assert!(!encoded.contains(result_tail));
}

#[test]
fn malformed_mcp_results_are_rejected_without_hiding_later_valid_content() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    let native_session_id = "019fa000-0000-7000-8000-000000000025";
    let rejected_marker = "rejectonlyzzqv7421";
    let tool_error_marker = "toolerrorokqjx8137";
    let protocol_error_marker = "protocolerrorokmzn9264";
    let valid_marker = "later_valid_content_is_indexed";
    let malformed = [
        json!({
            "Ok": {"content": [{"type": "text", "text": rejected_marker}]},
            "Err": "ambiguous wrapper"
        }),
        json!({"Unknown": {"content": []}}),
        json!({"Ok": null}),
        json!({"Ok": {"content": "not an array"}}),
        json!({"Ok": {"content": [{"type": "text"}]}}),
        json!({"Ok": {"content": [], "isError": "not a boolean"}}),
        json!({"Err": null}),
    ];
    let mut events = malformed
        .into_iter()
        .enumerate()
        .map(|(index, result)| mcp_result(&format!("exec-mcp-malformed-{index}"), result))
        .collect::<Vec<_>>();
    events.push(mcp_result(
        "exec-mcp-tool-error",
        json!({
            "Ok": {
                "content": [{"type": "text", "text": tool_error_marker}],
                "isError": true
            }
        }),
    ));
    events.push(mcp_result(
        "exec-mcp-protocol-error",
        json!({"Err": protocol_error_marker}),
    ));
    events.push(json!({
        "timestamp": "2026-08-01T12:00:02Z",
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": valid_marker}]
        }
    }));
    write_session(&sessions, native_session_id, &events);

    let receipt = publish_codex_sessions(&sessions, &index);
    assert_eq!(receipt.commit.indexed_documents, 3);
    let verified = VerifiedIndex::open(&index).unwrap();
    assert!(verified
        .search_event_candidates(rejected_marker, 10)
        .unwrap()
        .is_empty());
    assert_eq!(
        verified
            .search_event_candidates(tool_error_marker, 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        verified
            .search_event_candidates(protocol_error_marker, 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        verified
            .search_event_candidates(valid_marker, 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn redacted_real_shape_fixture_is_admitted_with_linkage_and_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join("rollout-redacted-mcp-direct-result.jsonl"),
        include_str!(
            "../src/provider/codex/nativepath/tests/fixtures/mcp_tool_call_end_direct_result.jsonl"
        ),
    )
    .unwrap();

    let receipt = publish_codex_sessions(&sessions, &index);
    assert_eq!(receipt.commit.indexed_documents, 1);
    let verified = VerifiedIndex::open(&index).unwrap();
    let candidate = verified
        .search_event_candidates("REAL_SHAPE_DIRECT_RESULT", 10)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let core = verified
        .core_record_by_id(candidate.event.event_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(
        core.content.normalized_body.as_deref(),
        Some("REAL_SHAPE_DIRECT_RESULT")
    );
    let native = &core.content.structured_content.as_ref().unwrap()["provider_native_tool_result"];
    assert_eq!(native["call_id"], "exec-redacted-real-shape");
    assert_eq!(native["result_variant"], "Ok");
    assert_eq!(native["result_metadata"]["isError"], false);
    assert_eq!(
        native["result_metadata"]["_meta"]["codex/toolSurface"]["kind"],
        "browserUse"
    );
    assert_eq!(native["invocation"]["server"], "node_repl");
}
