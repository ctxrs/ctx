use std::{
    fs,
    path::{Path, PathBuf},
};

use serde_json::{json, Value};

use super::*;

#[test]
fn source_backed_cold_projection_and_exact_locator() {
    const SENTINEL: &str = "QWEN_CODE_SOURCE_BACKED_SENTINEL";

    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join(".qwen/projects");
    let transcript = transcript_path(&root);
    let source_record = message("qwen-life", "source-backed-user", "user", SENTINEL);
    write_transcript(&transcript, std::slice::from_ref(&source_record));
    let mut expected_record = serde_json::to_vec(&source_record).unwrap();
    expected_record.push(b'\n');

    super::super::source_backed::assert_source_backed_fixture(
        qwen_code_source_backed_adapter(),
        &root,
        "qwen-life",
        SENTINEL,
        &expected_record,
        None,
        "qwen-life",
        "primary",
        true,
        "ff13570b6860d2e7b42b48d1255150490bc74d5ecff07a462742ab9d1de664ed",
    );
}

fn transcript_path(root: &Path) -> PathBuf {
    root.join("sanitized-workspace/chats/qwen-life.jsonl")
}

fn message(session_id: &str, id: &str, kind: &str, content: &str) -> Value {
    json!({
        "uuid": id,
        "sessionId": session_id,
        "timestamp": "2026-07-25T12:00:01Z",
        "type": kind,
        "cwd": "/workspace/qwen",
        "message": {
            "role": kind,
            "content": [{"type": "text", "text": content}]
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
