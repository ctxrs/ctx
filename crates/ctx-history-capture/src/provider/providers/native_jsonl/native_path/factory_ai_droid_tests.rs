use std::{
    fs,
    path::{Path, PathBuf},
};

use serde_json::{json, Value};

use super::*;
use crate::test_support_paths::tempdir;

#[test]
fn source_backed_cold_projection_and_exact_locator() {
    const TAIL_TERM: &str = "factorydroidtailneedle";

    let temp = tempdir().unwrap();
    let root = temp.path().join(".factory/sessions");
    let transcript = transcript_path(&root);
    let source_record = tool_call("source-backed-tool", TAIL_TERM);
    let expected_body = source_record
        .pointer("/message/content")
        .unwrap()
        .to_string();
    write_transcript(
        &transcript,
        &[
            child_header("droid-life", "droid-parent"),
            source_record.clone(),
        ],
    );
    let mut expected_record = serde_json::to_vec(&source_record).unwrap();
    expected_record.push(b'\n');

    super::super::source_backed::assert_source_backed_fixture(
        factory_droid_source_backed_adapter(),
        &root,
        "droid-life",
        &expected_body,
        TAIL_TERM,
        &expected_record,
        Some("droid-parent"),
        "droid-parent",
        "subagent",
        false,
        "6b324614c0b96f21a92b22e9232c880fa1cc37520f7d02778538e3aa902f427f",
    );
}

fn transcript_path(root: &Path) -> PathBuf {
    root.join("project/droid-life.jsonl")
}

fn child_header(session_id: &str, parent: &str) -> Value {
    json!({
        "type": "session_start",
        "sessionId": session_id,
        "timestamp": "2026-07-25T12:00:00Z",
        "cwd": "/workspace/factory",
        "model": "factory/droid",
        "callingSessionId": parent,
        "decompSessionType": "worker",
        "decompMissionId": "mission-1",
    })
}

fn tool_call(id: &str, tail_term: &str) -> Value {
    json!({
        "type": "message",
        "id": id,
        "timestamp": "2026-07-25T12:00:01Z",
        "message": {
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "factory-call",
                "name": "write_file",
                "input": {
                    "padding": "f".repeat(17_000),
                    "zz_tail": tail_term,
                },
            }],
        },
        "model": "factory/droid",
    })
}

fn write_transcript(path: &Path, records: &[Value]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = Vec::new();
    for record in records {
        serde_json::to_writer(&mut bytes, record).unwrap();
        bytes.push(b'\n');
    }
    fs::write(path, bytes).unwrap();
}
