use std::{
    fs,
    path::{Path, PathBuf},
};

use serde_json::{json, Value};

use super::*;
use crate::test_support_paths::tempdir;

#[test]
fn source_backed_cold_projection_and_exact_locator() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("brain");
    let transcript = transcript_path(&root);
    let sentinel = format!(
        "ANTIGRAVITY_SOURCE_BACKED_SENTINEL {}antigravity-tail",
        "full-body ".repeat(400)
    );
    let source_record = record(0, "USER_INPUT", &sentinel);
    write_transcript(&transcript, std::slice::from_ref(&source_record));
    let mut expected_record = serde_json::to_vec(&source_record).unwrap();
    expected_record.push(b'\n');

    super::super::source_backed::assert_source_backed_fixture(
        antigravity_source_backed_adapter(),
        &root,
        "agy-life",
        &sentinel,
        &expected_record,
        None,
        "agy-life",
        "primary",
        true,
        "5d13599441e0003221f51f2fa833f230dac8ca8cb1a09f13e5629855d90a8182",
    );
}

fn transcript_path(root: &Path) -> PathBuf {
    root.join("agy-life")
        .join(".system_generated/logs/transcript_full.jsonl")
}

fn record(step: u64, kind: &str, content: &str) -> Value {
    json!({
        "step_index": step,
        "source": if kind == "USER_INPUT" { "user" } else { "planner" },
        "type": kind,
        "status": "ok",
        "created_at": format!("2026-07-25T12:00:{step:02}Z"),
        "content": content,
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
