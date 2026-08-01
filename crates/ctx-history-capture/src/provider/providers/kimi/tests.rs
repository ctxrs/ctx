use std::{
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
};

use serde_json::{json, Value};

use crate::{test_support_paths::tempdir, CaptureError};

use super::layout::{
    KimiWireLayout, KIMI_WIRE_LAYOUT_MAX_AGGREGATE_BYTES, KIMI_WIRE_LAYOUT_MAX_INDEX_ENTRIES,
};

const SUCCESS_BODY: &str = "KIMI_SUCCESS_BODY_RETAINED_IN_CORE";
const FAILURE_BODY: &str = "KIMI_FAILURE_DIAGNOSTIC";

fn kimi_wire_fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".kimi-code");
    let session_dir = root.join("sessions/work/session-1");
    let agent_dir = session_dir.join("agents/main");
    fs::create_dir_all(&agent_dir).unwrap();
    fs::write(
        root.join("session_index.jsonl"),
        format!(
            "{}\n",
            json!({
                "sessionId": "session-1",
                "sessionDir": session_dir,
                "workDir": "/workspace/kimi"
            })
        ),
    )
    .unwrap();
    fs::write(
        session_dir.join("state.json"),
        json!({
            "createdAt": "2026-07-17T12:00:00Z",
            "updatedAt": "2026-07-17T12:00:10Z",
            "title": "Kimi NativePath",
            "agents": {"main": {"type": "main"}}
        })
        .to_string(),
    )
    .unwrap();
    let wire = agent_dir.join("wire.jsonl");
    write_records(
        &wire,
        &[
            json!({"type": "metadata", "created_at": 1_784_289_600_000_i64}),
            message("fresh"),
            output("success", 0, SUCCESS_BODY),
            output("failure", 17, FAILURE_BODY),
        ],
    );
    (temp, root, wire)
}

fn message(text: &str) -> Value {
    json!({
        "type": "turn.prompt",
        "time": 1_784_289_600_001_i64,
        "input": text
    })
}

fn output(call_id: &str, exit_code: i64, content: &str) -> Value {
    json!({
        "type": "context.append_loop_event",
        "time": 1_784_289_600_002_i64 + exit_code,
        "event": {
            "type": "tool.result",
            "toolName": "bash",
            "call_id": call_id,
            "exit_code": exit_code,
            "output": content
        }
    })
}

fn write_records(path: &Path, records: &[Value]) {
    let contents = records
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(path, contents).unwrap();
}

#[test]
fn kimi_wire_observation_detects_auxiliary_state_changes() {
    let (_temp, _root, wire) = kimi_wire_fixture();
    let observation = super::source::KimiWireObservation::read(&wire).unwrap();
    assert!(observation.revalidate(&wire).unwrap());
    let session_dir = wire.parent().unwrap().parent().unwrap().parent().unwrap();
    fs::write(
        session_dir.join("state.json"),
        json!({
            "createdAt": "2026-07-17T12:00:00Z",
            "title": "changed state",
            "agents": {"main": {"type": "main"}}
        })
        .to_string(),
    )
    .unwrap();
    assert!(!observation.revalidate(&wire).unwrap());
}

#[test]
fn kimi_layout_derives_one_exact_root_despite_malicious_nesting() {
    let (temp, root, wire) = kimi_wire_fixture();
    let session_dir = wire.parent().unwrap().parent().unwrap().parent().unwrap();
    fs::write(
        session_dir.join("session_index.jsonl"),
        format!(
            "{}\n",
            json!({"sessionId": "session-1", "workDir": "/malicious/nearby"})
        ),
    )
    .unwrap();

    let mut layout = KimiWireLayout::read(&wire).unwrap();
    assert_eq!(
        layout.take_index_entry().unwrap().work_dir.as_deref(),
        Some("/workspace/kimi")
    );

    let invalid_wire = temp
        .path()
        .join("not-sessions/work/session-1/agents/main/wire.jsonl");
    fs::create_dir_all(invalid_wire.parent().unwrap()).unwrap();
    fs::write(&invalid_wire, "{}\n").unwrap();
    assert!(matches!(
        KimiWireLayout::read(&invalid_wire),
        Err(CaptureError::InvalidProviderTranscriptPath { .. })
    ));
    assert!(root.join("session_index.jsonl").is_file());
}

#[test]
fn kimi_layout_bounds_remain_exact() {
    let (_temp, root, wire) = kimi_wire_fixture();
    let index_path = root.join("session_index.jsonl");
    OpenOptions::new()
        .write(true)
        .open(&index_path)
        .unwrap()
        .set_len(KIMI_WIRE_LAYOUT_MAX_AGGREGATE_BYTES as u64)
        .unwrap();
    assert!(KimiWireLayout::read(&wire).is_ok());

    let mut index = fs::read(&index_path).unwrap();
    index.truncate(
        index
            .iter()
            .position(|byte| *byte == b'\n')
            .unwrap()
            .saturating_add(1),
    );
    index.extend(std::iter::repeat_n(
        b'\n',
        KIMI_WIRE_LAYOUT_MAX_INDEX_ENTRIES,
    ));
    fs::write(index_path, index).unwrap();
    assert!(KimiWireLayout::read(&wire).is_err());
}
