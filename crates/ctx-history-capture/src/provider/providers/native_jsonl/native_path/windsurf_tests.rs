use std::{
    fs,
    path::{Path, PathBuf},
};

use serde_json::{json, Value};

use super::*;
use crate::test_support_paths::tempdir;

#[test]
fn source_backed_cold_projection_and_exact_locator() {
    const SENTINEL: &str = "WINDSURF_SOURCE_BACKED_SENTINEL";

    let temp = tempdir().unwrap();
    let root = temp.path().join("transcripts");
    let transcript = transcript_path(&root);
    let source_record = user_input(0, SENTINEL);
    write_transcript(&transcript, std::slice::from_ref(&source_record));
    let mut expected_record = serde_json::to_vec(&source_record).unwrap();
    expected_record.push(b'\n');

    super::super::source_backed::assert_source_backed_fixture(
        windsurf_source_backed_adapter(),
        &root,
        "windsurf-hook-trajectory",
        SENTINEL,
        &expected_record,
        None,
        "windsurf-hook-trajectory",
        "primary",
        true,
        "e0190d935691dcdfb294672fd2a821f694be6edaf3b81d848959fa7df56c9125",
    );
}

fn transcript_path(root: &Path) -> PathBuf {
    root.join("windsurf-hook-trajectory.jsonl")
}

fn user_input(step: u64, content: &str) -> Value {
    json!({
        "status": "done",
        "type": "user_input",
        "timestamp": format!("2026-07-25T12:00:{step:02}Z"),
        "user_input": {"user_response": content},
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
