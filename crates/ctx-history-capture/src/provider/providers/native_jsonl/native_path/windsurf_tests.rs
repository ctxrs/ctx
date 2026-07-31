use std::{
    fs,
    path::{Path, PathBuf},
};

use serde_json::{json, Value};

use super::*;
use crate::test_support_paths::tempdir;

#[test]
fn source_backed_cold_projection_and_exact_locator() {
    const TAIL_TERM: &str = "windsurftailneedle";

    let temp = tempdir().unwrap();
    let root = temp.path().join("transcripts");
    let transcript = transcript_path(&root);
    let source_record = code_action(0, TAIL_TERM);
    let expected_body = source_record.get("code_action").unwrap().to_string();
    write_transcript(&transcript, std::slice::from_ref(&source_record));
    let mut expected_record = serde_json::to_vec(&source_record).unwrap();
    expected_record.push(b'\n');

    super::super::source_backed::assert_source_backed_fixture(
        windsurf_source_backed_adapter(),
        &root,
        "windsurf-hook-trajectory",
        &expected_body,
        TAIL_TERM,
        &expected_record,
        None,
        "windsurf-hook-trajectory",
        "primary",
        true,
        "7e3cca82c92cea15497b4ff3734c2ad22636e8445c5283dfb493d39d061d191a",
    );
}

fn transcript_path(root: &Path) -> PathBuf {
    root.join("windsurf-hook-trajectory.jsonl")
}

fn code_action(step: u64, tail_term: &str) -> Value {
    json!({
        "status": "done",
        "type": "code_action",
        "timestamp": format!("2026-07-25T12:00:{step:02}Z"),
        "code_action": {
            "path": "src/complete.rs",
            "arguments": {
                "padding": "s".repeat(17_000),
                "zz_tail": tail_term,
            },
        },
    })
}

#[test]
fn fallback_normalization_retains_selected_text_beyond_16k() {
    const TAIL_TERM: &str = "windsurffallbacktailneedle";

    let value = json!({
        "type": "summary",
        "summary": "x".repeat(17_000),
        "details": {"zz_tail": TAIL_TERM},
    });
    let body = windsurf_event_text(&value, "summary");

    assert!(body.contains(TAIL_TERM));
    assert!(body.split_once(TAIL_TERM).unwrap().0.chars().count() > 16 * 1024);
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
