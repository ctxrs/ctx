use std::{
    fs,
    path::{Path, PathBuf},
};

use serde_json::{json, Value};

use super::*;
use crate::test_support_paths::tempdir;

#[test]
fn source_backed_cold_projection_and_exact_locator() {
    const SENTINEL: &str = "FACTORY_DROID_SOURCE_BACKED_SENTINEL";

    let temp = tempdir().unwrap();
    let root = temp.path().join(".factory/sessions");
    let transcript = transcript_path(&root);
    let source_record = message("source-backed-user", "user", SENTINEL);
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
        SENTINEL,
        &expected_record,
        Some("droid-parent"),
        "droid-parent",
        "subagent",
        false,
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

fn message(id: &str, role: &str, text: &str) -> Value {
    json!({
        "type": "message",
        "id": id,
        "timestamp": "2026-07-25T12:00:01Z",
        "message": {
            "role": role,
            "content": [{"type": "text", "text": text}],
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
