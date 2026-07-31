use std::{
    fs,
    path::{Path, PathBuf},
};

use serde_json::{json, Value};

use super::*;
use crate::test_support_paths::tempdir;

#[test]
fn source_backed_cold_projection_and_exact_locator() {
    const TAIL_TERM: &str = "qodertailneedle";

    let temp = tempdir().unwrap();
    let root = temp.path().join(".qoder/projects");
    let transcript = transcript_path(&root);
    let source_record = tool_call("source-backed-tool", TAIL_TERM);
    let expected_body = source_record
        .pointer("/message/content")
        .unwrap()
        .to_string();
    write_transcript(&transcript, &[header("qoder-life"), source_record.clone()]);
    let mut expected_record = serde_json::to_vec(&source_record).unwrap();
    expected_record.push(b'\n');

    super::super::source_backed::assert_source_backed_fixture(
        qoder_source_backed_adapter(),
        &root,
        "qoder-life",
        &expected_body,
        TAIL_TERM,
        &expected_record,
        None,
        "qoder-life",
        "primary",
        true,
        "913b9e24a1f1f906b0dba46552964ac0ab2e9c01acb548b9696bbb1ca89d7183",
    );
}

fn transcript_path(root: &Path) -> PathBuf {
    root.join("sanitized-workspace/transcript/qoder-life.jsonl")
}

fn header(session_id: &str) -> Value {
    json!({
        "type": "session_meta",
        "sessionId": session_id,
        "uuid": format!("{session_id}-meta"),
        "timestamp": "2026-07-25T12:00:00Z",
        "cwd": "/workspace/qoder",
        "data": {
            "meta_type": "session_info",
            "content": {"mode": "agent", "session_type": "assistant"}
        }
    })
}

fn tool_call(id: &str, tail_term: &str) -> Value {
    json!({
        "type": "assistant",
        "sessionId": "qoder-life",
        "uuid": id,
        "timestamp": "2026-07-25T12:00:01Z",
        "cwd": "/workspace/qoder",
        "message": {
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "qoder-call",
                "name": "write_file",
                "input": {
                    "padding": "q".repeat(17_000),
                    "zz_tail": tail_term,
                },
            }],
        },
        "model": "qoder-agent",
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
