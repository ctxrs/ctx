use std::{
    fs,
    path::{Path, PathBuf},
};

use serde_json::{json, Value};

use super::*;
use crate::test_support_paths::tempdir;

#[test]
fn source_backed_cold_projection_and_exact_locator() {
    const TAIL_TERM: &str = "tabninetailneedle";

    let temp = tempdir().unwrap();
    let root = temp.path().join(".tabnine/agent");
    let transcript = transcript_path(&root);
    let source_record = tool_call("source-backed-tool", TAIL_TERM);
    let expected_body = json!({
        "toolCalls": source_record.get("toolCalls").unwrap(),
    })
    .to_string();
    write_transcript(
        &transcript,
        &[header("tabnine-life"), source_record.clone()],
    );
    let mut expected_record = serde_json::to_vec(&source_record).unwrap();
    expected_record.push(b'\n');

    super::super::source_backed::assert_source_backed_fixture(
        tabnine_source_backed_adapter(),
        &root,
        "tabnine-life",
        &expected_body,
        TAIL_TERM,
        &expected_record,
        None,
        "tabnine-life",
        "primary",
        true,
        "a5cd51c9c37456dde096091dc863db5141938b43b2f3a15840df796e9d9a0224",
    );
}

fn transcript_path(root: &Path) -> PathBuf {
    root.join("tmp/project/chats/session-tabnine-life.jsonl")
}

fn header(session_id: &str) -> Value {
    json!({
        "sessionId": session_id,
        "projectHash": "tabnine-nativepath-project",
        "startTime": "2026-07-25T12:00:00Z",
        "lastUpdated": "2026-07-25T12:00:59Z",
        "kind": "main",
        "directories": ["/workspace/tabnine"],
    })
}

fn tool_call(id: &str, tail_term: &str) -> Value {
    json!({
        "id": id,
        "timestamp": "2026-07-25T12:00:01Z",
        "type": "tabnine",
        "toolCalls": [{
            "id": "tabnine-call",
            "name": "write_file",
            "args": {
                "padding": "t".repeat(17_000),
                "zz_tail": tail_term,
            },
        }],
        "model": "tabnine-agent",
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
