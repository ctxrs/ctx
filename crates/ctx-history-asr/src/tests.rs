//! Authored semantic fixtures; these are not native harness captures.
use super::*;
use serde_json::{json, Value};
fn fixture(provider: Provider, rows: &[Value]) -> (tempfile::TempDir, Options) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("source.jsonl");
    let mut bytes = String::new();
    for row in rows {
        bytes.push_str(&row.to_string());
        bytes.push('\n');
    }
    std::fs::write(&path, bytes).unwrap();
    let options = Options::new(vec![Input { provider, path }], dir.path().join("bundle"));
    (dir, options)
}
fn records(options: &Options) -> Vec<Value> {
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(options.out.join("bundle.json")).unwrap()).unwrap();
    std::fs::read_to_string(
        options
            .out
            .join(manifest["sessions"][0]["path"].as_str().unwrap()),
    )
    .unwrap()
    .lines()
    .map(|l| serde_json::from_str(l).unwrap())
    .collect()
}
#[test]
fn claude_folds_usage_and_preserves_unknown_authorship() {
    let (_dir, options) = fixture(
        Provider::Claude,
        &[
            json!({"type":"user","uuid":"u","sessionId":"test","parentUuid":null,"message":{"role":"user","content":"hello\n"}}),
            json!({"type":"assistant","uuid":"a","sessionId":"test","parentUuid":"u","message":{"id":"response","role":"assistant","content":[{"type":"text","text":"one"}],"usage":{"input_tokens":1,"output_tokens":2}}}),
            json!({"type":"assistant","uuid":"b","sessionId":"test","parentUuid":"a","message":{"id":"response","role":"assistant","content":[{"type":"text","text":"two"}],"usage":{"input_tokens":1,"output_tokens":4},"stop_reason":"end_turn"}}),
        ],
    );
    export(&options).unwrap();
    let r = records(&options);
    let m = r.iter().find(|v| v["messageId"] == "response").unwrap();
    assert_eq!(m["content"].as_array().unwrap().len(), 2);
    let inf = r.iter().find(|v| v["type"] == "inference").unwrap();
    assert_eq!(inf["usage"]["output"]["total"], 4);
    assert!(r
        .iter()
        .find(|v| v["messageId"] == "u")
        .unwrap()
        .get("author")
        .is_none());
    assert_eq!(export(&options).unwrap().unchanged, 1);
    assert_eq!(records(&options), r);
}
#[test]
fn codex_provenance_usage_and_incremental_ids() {
    let (_dir, options) = fixture(
        Provider::Codex,
        &[
            json!({"type":"session_meta","ordinal":0,"payload":{"id":"test","source":"cli","cwd":"/synthetic"}}),
            json!({"type":"turn_context","ordinal":1,"payload":{"turn_id":"t","model":"requested-model"}}),
            json!({"type":"response_item","ordinal":2,"payload":{"type":"message","id":"m","role":"user","content":[{"type":"input_text","text":"instructions"},{"type":"input_text","text":"question"}],"internal_chat_message_metadata_passthrough":{"turn_id":"t","content_item_kinds":["agents_md.instructions","user.text"]}}}),
            json!({"type":"token_usage_record","ordinal":3,"payload":{"response_id":"response","usage":{"input_tokens":10,"output_tokens":0}}}),
        ],
    );
    export(&options).unwrap();
    let before = records(&options);
    let m = before.iter().find(|v| v["messageId"] == "m").unwrap();
    assert_eq!(m["content"][0]["author"], "harness");
    assert_eq!(m["content"][1]["author"], "human");
    let inf = before.iter().find(|v| v["type"] == "inference").unwrap();
    assert_eq!(inf["model"]["requestedSource"], "turn_config");
    assert!(inf["model"].get("id").is_none());
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&options.inputs[0].path)
        .unwrap();
    writeln!(
        file,
        "{}",
        json!({"type":"event_msg","ordinal":4,"payload":{"type":"task_complete","turn_id":"t"}})
    )
    .unwrap();
    export(&options).unwrap();
    let after = records(&options);
    assert_eq!(&after[..before.len()], &before);
}

mod incremental;
mod mappings;
mod sqlite;
mod warp;
