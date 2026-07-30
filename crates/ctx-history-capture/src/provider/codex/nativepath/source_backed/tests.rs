use std::{
    fs::{self, OpenOptions},
    io::Write,
};

use ctx_history_core::{
    EventType, LocatorRevisionPolicy, NativeRecordCoordinate, SourceRecordLocator,
};

use super::*;

mod hydration;
mod lifecycle;
mod projection;

fn assert_no_legacy_operations(counters: CodexSourceBackedCountersV0) {
    assert_eq!(counters.scanner_legacy_body_json_serializations, 0);
    assert_eq!(counters.scanner_legacy_row_json_serializations, 0);
    assert_eq!(counters.scanner_legacy_json_serialized_bytes, 0);
    assert_eq!(counters.scanner_legacy_normalized_payload_hashes, 0);
    assert_eq!(counters.scanner_legacy_file_touch_rows, 0);
    assert_eq!(counters.scanner_legacy_complete_content_locators, 0);
    assert_eq!(counters.scanner_legacy_duplicate_preview_allocations, 0);
    assert_eq!(counters.scanner_legacy_page_owner_json_serializations, 0);
    assert_eq!(
        counters.scanner_legacy_page_identity_owner_json_serializations,
        0
    );
    assert_eq!(
        counters.scanner_legacy_page_identity_row_json_serializations,
        0
    );
}

fn search_event_ids(index: &VerifiedIndex, query: &str) -> Vec<StableEntityId> {
    index
        .search_event_candidates(query, 32)
        .unwrap()
        .into_iter()
        .map(|candidate| candidate.event.event_id)
        .collect()
}

fn session_path(sessions: &Path, native_session_id: &str) -> PathBuf {
    sessions.join(format!("rollout-{native_session_id}.jsonl"))
}

fn write_session(sessions: &Path, native_session_id: &str, events: &[String]) {
    let mut contents = format!("{}\n", session_meta(native_session_id));
    for event in events {
        contents.push_str(event);
        contents.push('\n');
    }
    fs::write(session_path(sessions, native_session_id), contents).unwrap();
}

fn session_meta(native_session_id: &str) -> String {
    serde_json::json!({
        "timestamp": "2026-07-28T12:00:00Z",
        "type": "session_meta",
        "payload": {
            "id": native_session_id,
            "timestamp": "2026-07-28T12:00:00Z",
            "cwd": "/tmp/source-backed",
            "originator": "codex_cli_rs",
            "cli_version": "0.1.0",
            "source": "cli",
            "model_provider": "openai"
        }
    })
    .to_string()
}

fn message(role: &str, text: &str) -> String {
    serde_json::json!({
        "timestamp": "2026-07-28T12:00:01Z",
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": role,
            "content": [{
                "type": "input_text",
                "text": text
            }]
        }
    })
    .to_string()
}

fn tool_call_with_patch(call_id: &str) -> String {
    serde_json::json!({
            "timestamp": "2026-07-28T12:00:02Z",
            "type": "response_item",
            "payload": {
                "type": "custom_tool_call",
                "name": "apply_patch",
                "call_id": call_id,
                "input": "*** Begin Patch\n*** Update File: src/source_backed.rs\n@@\n-old\n+new\n*** End Patch\n"
            }
        })
        .to_string()
}

fn failed_tool_output(call_id: &str) -> String {
    serde_json::json!({
        "timestamp": "2026-07-28T12:00:03Z",
        "type": "response_item",
        "payload": {
            "type": "custom_tool_call_output",
            "call_id": call_id,
            "output": "Process exited with code 7\nfailure body stays source-backed"
        }
    })
    .to_string()
}
