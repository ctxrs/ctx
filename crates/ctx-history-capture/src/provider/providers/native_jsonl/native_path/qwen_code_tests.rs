use std::{
    fs,
    path::{Path, PathBuf},
};

use serde_json::{json, Value};

use super::*;

#[test]
fn source_backed_cold_projection_and_exact_locator() {
    const TAIL_TERM: &str = "qwencodetailneedle";

    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join(".qwen/projects");
    let transcript = transcript_path(&root);
    let source_record = tool_call("qwen-life", "source-backed-tool", TAIL_TERM);
    let expected_body = source_record
        .pointer("/message/content")
        .unwrap()
        .to_string();
    write_transcript(&transcript, std::slice::from_ref(&source_record));
    let mut expected_record = serde_json::to_vec(&source_record).unwrap();
    expected_record.push(b'\n');

    super::super::source_backed::assert_source_backed_fixture(
        qwen_code_source_backed_adapter(),
        &root,
        "qwen-life",
        &expected_body,
        TAIL_TERM,
        &expected_record,
        None,
        "qwen-life",
        "primary",
        true,
        "1549f87e6b69136ab9c7a9a1f913801cee705ffdafeb08379cf744a6fc5a9d1d",
    );
}

fn transcript_path(root: &Path) -> PathBuf {
    root.join("sanitized-workspace/chats/qwen-life.jsonl")
}

fn tool_call(session_id: &str, id: &str, tail_term: &str) -> Value {
    json!({
        "uuid": id,
        "sessionId": session_id,
        "timestamp": "2026-07-25T12:00:01Z",
        "type": "assistant",
        "cwd": "/workspace/qwen",
        "message": {
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "qwen-call",
                "name": "write_file",
                "input": {
                    "padding": "w".repeat(17_000),
                    "zz_tail": tail_term,
                },
            }]
        },
        "model": "qwen3-coder",
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
