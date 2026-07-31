use std::{
    fs,
    path::{Path, PathBuf},
};

use serde_json::{json, Value};

use super::*;
use crate::test_support_paths::tempdir;

#[test]
fn source_backed_cold_projection_and_exact_locator() {
    const TAIL_TERM: &str = "antigravitytailneedle";

    let temp = tempdir().unwrap();
    let root = temp.path().join("brain");
    let transcript = transcript_path(&root);
    let source_record = tool_call(0, TAIL_TERM);
    let expected_body = json!({
        "content": source_record.get("content").unwrap(),
        "tool_calls": source_record.get("tool_calls").unwrap(),
    })
    .to_string();
    write_transcript(&transcript, std::slice::from_ref(&source_record));
    let mut expected_record = serde_json::to_vec(&source_record).unwrap();
    expected_record.push(b'\n');

    super::super::source_backed::assert_source_backed_fixture(
        antigravity_source_backed_adapter(),
        &root,
        "agy-life",
        &expected_body,
        TAIL_TERM,
        &expected_record,
        None,
        "agy-life",
        "primary",
        true,
        "40914e1730adb5d141a1533b20ad3e7e906380f707511fb973be0f367c93615b",
    );
}

fn transcript_path(root: &Path) -> PathBuf {
    root.join("agy-life")
        .join(".system_generated/logs/transcript_full.jsonl")
}

fn tool_call(step: u64, tail_term: &str) -> Value {
    json!({
        "step_index": step,
        "source": "planner",
        "type": "CODE_ACTION",
        "status": "ok",
        "created_at": format!("2026-07-25T12:00:{step:02}Z"),
        "content": "apply the complete structured edit",
        "tool_calls": [{
            "name": "write_to_file",
            "args": {
                "padding": "a".repeat(17_000),
                "zz_tail": tail_term,
            },
        }],
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
