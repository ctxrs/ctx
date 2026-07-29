use std::{
    fs,
    path::{Path, PathBuf},
};

use serde_json::{json, Value};

use super::*;
use crate::test_support_paths::tempdir;

#[test]
fn source_backed_cold_projection_and_exact_locator() {
    const SENTINEL: &str = "COPILOT_SOURCE_BACKED_SENTINEL";

    let temp = tempdir().unwrap();
    let root = temp.path().join(".copilot/session-state");
    let transcript = transcript_path(&root);
    let source_record = message("source-backed-user", "user.message", SENTINEL);
    write_transcript(
        &transcript,
        &[header("copilot-life"), source_record.clone()],
    );
    let mut expected_record = serde_json::to_vec(&source_record).unwrap();
    expected_record.push(b'\n');

    super::super::source_backed::assert_source_backed_fixture(
        copilot_source_backed_adapter(),
        &root,
        "copilot-life",
        SENTINEL,
        &expected_record,
        None,
        "copilot-life",
        "primary",
        true,
    );
}

fn transcript_path(root: &Path) -> PathBuf {
    root.join("copilot-life/events.jsonl")
}

fn header(session_id: &str) -> Value {
    json!({
        "id": format!("{session_id}-start"),
        "timestamp": "2026-07-25T12:00:00Z",
        "type": "session.start",
        "data": {
            "sessionId": session_id,
            "startTime": "2026-07-25T12:00:00Z",
            "selectedModel": "gpt-5-mini",
            "context": { "cwd": "/workspace/copilot" },
        },
    })
}

fn message(id: &str, kind: &str, content: &str) -> Value {
    json!({
        "id": id,
        "timestamp": "2026-07-25T12:00:01Z",
        "type": kind,
        "data": { "content": content },
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
