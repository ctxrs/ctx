//! Authored transactional regressions plus the existing sanitized fx 0.0.6 captures.
use super::*;
use std::{fs, io::Write};
fn frame(seq: u64, kind: &str, payload: Value) -> Value {
    json!({"schema_version":1,"log_generation":"11111111111111111111111111111111","seq":seq,"event_id":format!("{seq:032x}"),"timestamp_ms":seq,"kind":kind,"payload":payload})
}
fn turn(text: &str) -> Value {
    json!({"conversation_language":"en","total_input_tokens":2,"total_output_tokens":3,"turn":{"kind":"assistant","user":{"text":"Question","images":[]},"assistant":text,"execution":{"schema_version":1,"tool_steps":[],"files":[]}}})
}
fn fx() -> (tempfile::TempDir, Options) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session");
    fs::create_dir_all(&path).unwrap();
    fs::write(path.join("authority.json"),json!({"schema_version":1,"session_id":"session","authority_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","storage_format":"event_log_v1","source":"native_create"}).to_string()).unwrap();
    let start = frame(
        1,
        "session_started",
        json!({"id":"session","created_at_ms":1,"origin_workspace_root":"/workspace","workspace_root":"/workspace","conversation_language":"en","preferences":{"provider":"gateway","model":"native-model","effort":"high","fast_mode":false}}),
    );
    let log = format!(
        "{}\n{}\n",
        start,
        frame(2, "history_turn_committed", turn("Committed"))
    );
    fs::write(path.join("events.jsonl"), log).unwrap();
    commit(&path, 2);
    let options = Options::new(
        vec![Input {
            provider: Provider::Fx,
            path,
        }],
        dir.path().join("out"),
    );
    (dir, options)
}
fn commit(path: &std::path::Path, seq: u64) {
    let bytes = fs::metadata(path.join("events.jsonl")).unwrap().len();
    fs::write(path.join("commit.11111111111111111111111111111111.json"),json!({"schema_version":1,"session_id":"session","log_generation":"11111111111111111111111111111111","through_seq":seq,"through_event_id":format!("{seq:032x}"),"through_event_log_bytes":bytes}).to_string()).unwrap();
}

fn replacement(path: &std::path::Path, start: u64, history: Value) -> String {
    use base64::Engine;
    use sha2::{Digest, Sha256};
    let state = json!({"id":"session","origin_workspace_root":"/workspace","workspace_root":"/workspace","created_at_ms":1,"updated_at_ms":start+2,"conversation_language":"en","preferences":{"provider":"gateway","model":"native-model","effort":"high","fast_mode":false},"history":history,"total_input_tokens":2,"total_output_tokens":3,"context_history_start":0,"permission_state":{"schema_version":2,"next_generation":1,"rules":[]}});
    let bytes = serde_json::to_vec(&state).unwrap();
    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let hash = format!("{:x}", Sha256::digest(&bytes));
    let replacement_id = format!("{start:032x}");
    let mut log = fs::OpenOptions::new()
        .append(true)
        .open(path.join("events.jsonl"))
        .unwrap();
    writeln!(log,"{}",frame(start,"state_replacement_started",json!({"replacement_id":replacement_id,"reason":"compaction","encoded_bytes":bytes.len(),"sha256":hash,"chunk_count":1}))).unwrap();
    writeln!(log,"{}",frame(start+1,"state_replacement_chunk",json!({"replacement_id":replacement_id,"chunk_index":0,"raw_bytes":bytes.len(),"chunk_sha256":hash,"base64":encoded}))).unwrap();
    writeln!(log,"{}",frame(start+2,"state_replacement_committed",json!({"replacement_id":replacement_id,"encoded_bytes":bytes.len(),"sha256":hash,"chunk_count":1}))).unwrap();
    commit(path, start + 2);
    encoded
}

#[test]
fn fx_secret_answers_in_replaced_history_do_not_escape_in_encoded_audit_chunks() {
    let (_dir, options) = fx();
    let secret = "SYNTHETIC_SECRET_FX_5271";
    let mut history = turn("Answer")["turn"].clone();
    history["execution"]["tool_steps"] = json!([{"assistant":null,"tool_calls":[{"id":"secret-call","name":"request_user_input","arguments_json":json!({"questions":[{"id":"token","question":"Token?","secret":true}]}).to_string(),"provider_result":null}],"tool_results":[{"tool_call_id":"secret-call","tool_name":"request_user_input","status":"success","output":json!({"answers":{"token":secret}}).to_string(),"output_handle":null,"preview":null,"output_bytes":100,"stored_output_bytes":100,"truncated":false,"provider_native":false,"created_at_ms":1}]}]);
    let encoded = replacement(&options.inputs[0].path, 3, json!([history]));
    replacement(
        &options.inputs[0].path,
        6,
        json!([{"kind":"compacted_summary","summary":"Later summary","removed_turn_count":1,"compaction_count":1}]),
    );
    export(&options).unwrap();
    let r = records(&options);
    let all = serde_json::to_string(&r).unwrap();
    assert!(!all.contains(secret));
    assert!(!all.contains(&encoded));
    assert!(all.contains("secret-bearing replacement payload omitted"));
    assert!(r.iter().any(|v| v["type"] == "compaction"));
    if let Some(out) = std::env::var_os("TEST_UNDECLARED_OUTPUTS_DIR") {
        let mut options = options.clone();
        options.out = PathBuf::from(out).join("fx-secret-replacement");
        export(&options).unwrap();
    }
}

#[test]
fn fx_native_tool_identity_survives_replacement_and_duplicate_ids_stay_ambiguous() {
    let (_dir, options) = fx();
    let mut payload = turn("Done");
    payload["turn"]["execution"]["schema_version"] = json!(3);
    payload["turn"]["execution"]["tool_steps"] = json!([{"assistant":null,"tool_calls":[{"id":"native-call","name":"command","arguments_json":"{}","provider_result":null}],"tool_results":[{"tool_call_id":"native-call","tool_name":"command","status":"failure","output":"failed","output_handle":null,"preview":null,"output_bytes":6,"stored_output_bytes":6,"truncated":false,"provider_native":false,"created_at_ms":1,"command_process_presentation":{"kind":"exit_code","value":9007199254740993i64}}]}]);
    let result = &mut payload["turn"]["execution"]["tool_steps"][0]["tool_results"][0];
    result["permission_feedback"] = json!([]);
    result["committed_file_presentation"] = Value::Null;
    result["command_output_replay"] = Value::Null;
    let mut log = fs::OpenOptions::new()
        .append(true)
        .open(options.inputs[0].path.join("events.jsonl"))
        .unwrap();
    writeln!(
        log,
        "{}",
        frame(3, "history_turn_committed", payload.clone())
    )
    .unwrap();
    commit(&options.inputs[0].path, 3);
    export(&options).unwrap();
    let before = records(&options);
    let result = before.iter().find(|v| v["type"] == "tool_result").unwrap();
    assert!(result.get("exec").is_none());
    assert!(result
        .pointer("/_meta/com.vercel.fx~1sourceJson")
        .unwrap()
        .as_str()
        .unwrap()
        .contains("9007199254740993"));
    replacement(
        &options.inputs[0].path,
        4,
        json!([turn("Prefix")["turn"], payload["turn"]]),
    );
    export(&options).unwrap();
    let after = records(&options);
    for kind in ["tool_call", "tool_result"] {
        let old = before.iter().find(|v| v["type"] == kind).unwrap();
        let new = after.iter().find(|v| v["type"] == kind).unwrap();
        assert_eq!(old["id"], new["id"]);
        assert!(new.get("idScope").is_none());
    }
    replacement(
        &options.inputs[0].path,
        7,
        json!([payload["turn"], payload["turn"]]),
    );
    export(&options).unwrap();
    let ambiguous = records(&options);
    assert_eq!(
        ambiguous
            .iter()
            .filter(|v| v["type"] == "tool_call")
            .count(),
        2
    );
    for result in ambiguous.iter().filter(|v| v["type"] == "tool_result") {
        assert!(result.get("callId").is_none());
        assert_eq!(result["linkage"], "ambiguous");
        assert_eq!(result["idScope"], "capture");
    }
}
#[test]
fn fx_ignores_uncommitted_tails_and_preserves_committed_identity() {
    let (_dir, options) = fx();
    export(&options).unwrap();
    let before = records(&options);
    let mut log = fs::OpenOptions::new()
        .append(true)
        .open(options.inputs[0].path.join("events.jsonl"))
        .unwrap();
    writeln!(
        log,
        "{}",
        frame(3, "history_turn_committed", turn("Pending"))
    )
    .unwrap();
    assert_eq!(export(&options).unwrap().snapshots, 0);
    assert_eq!(records(&options), before);
    commit(&options.inputs[0].path, 3);
    assert_eq!(export(&options).unwrap().snapshots, 1);
    let after = records(&options);
    let old = before
        .iter()
        .find(|v| v.pointer("/content/0/text") == Some(&json!("Committed")))
        .unwrap();
    let new = after
        .iter()
        .find(|v| v.pointer("/content/0/text") == Some(&json!("Committed")))
        .unwrap();
    assert_eq!(old["id"], new["id"]);
    assert!(old.get("idScope").is_none());
    assert!(after
        .iter()
        .any(|v| v.pointer("/content/0/text") == Some(&json!("Pending"))));
}
#[test]
fn fx_pending_or_bad_watermark_does_not_replace_bundle() {
    let (_dir, options) = fx();
    export(&options).unwrap();
    let before = fs::read(options.out.join("bundle.json")).unwrap();
    fs::write(options.inputs[0].path.join("commit.pending.json"), "{}").unwrap();
    assert!(export(&options).is_err());
    assert_eq!(fs::read(options.out.join("bundle.json")).unwrap(), before);
    fs::remove_file(options.inputs[0].path.join("commit.pending.json")).unwrap();
    commit(&options.inputs[0].path, 3);
    assert!(export(&options).is_err());
    assert_eq!(fs::read(options.out.join("bundle.json")).unwrap(), before);
}
#[test]
fn fx_native_v3_fixtures_and_validation_corpus() {
    let temp = tempfile::tempdir().unwrap();
    let root = PathBuf::from("tests/fixtures/provider-history/fx/v0.0.6");
    assert!(
        root.exists(),
        "native fx fixture data must be declared in Bazel"
    );
    let out = std::env::var_os("TEST_UNDECLARED_OUTPUTS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| temp.path().to_owned());
    let options = Options::new(
        vec![Input {
            provider: Provider::Fx,
            path: root,
        }],
        out.join("fx-native"),
    );
    let summary = export(&options).unwrap();
    assert_eq!(summary.sessions, 2);
    assert_eq!(summary.gaps, 0);
    assert_eq!(export(&options).unwrap().unchanged, 2);
}

#[test]
fn fx_committed_state_replacement_uses_capture_scope_and_keeps_log() {
    use base64::Engine;
    use sha2::{Digest, Sha256};
    let (_dir, options) = fx();
    export(&options).unwrap();
    let state = json!({"id":"session","origin_workspace_root":"/workspace","workspace_root":"/workspace","created_at_ms":1,"updated_at_ms":5,"conversation_language":"en","preferences":{"provider":"gateway","model":"native-model","effort":"high","fast_mode":false},"history":[{"kind":"compacted_summary","summary":"Summary","removed_turn_count":1,"compaction_count":1}],"total_input_tokens":2,"total_output_tokens":3,"context_history_start":0,"permission_state":{"schema_version":2,"next_generation":1,"rules":[]}});
    let bytes = serde_json::to_vec(&state).unwrap();
    let hash = format!("{:x}", Sha256::digest(&bytes));
    let replacement = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let mut log = fs::OpenOptions::new()
        .append(true)
        .open(options.inputs[0].path.join("events.jsonl"))
        .unwrap();
    writeln!(log,"{}",frame(3,"state_replacement_started",json!({"replacement_id":replacement,"reason":"compaction","encoded_bytes":bytes.len(),"sha256":hash,"chunk_count":1}))).unwrap();
    writeln!(log,"{}",frame(4,"state_replacement_chunk",json!({"replacement_id":replacement,"chunk_index":0,"raw_bytes":bytes.len(),"chunk_sha256":hash,"base64":base64::engine::general_purpose::STANDARD.encode(&bytes)}))).unwrap();
    // A complete but uncommitted replacement is not visible.
    export(&options).unwrap();
    assert!(!records(&options).iter().any(|v| v["type"] == "compaction"));
    writeln!(log,"{}",frame(5,"state_replacement_committed",json!({"replacement_id":replacement,"encoded_bytes":bytes.len(),"sha256":hash,"chunk_count":1}))).unwrap();
    commit(&options.inputs[0].path, 5);
    export(&options).unwrap();
    let r = records(&options);
    let c = r.iter().find(|v| v["type"] == "compaction").unwrap();
    assert_eq!(c["idScope"], "capture");
    assert_eq!(c["summary"][0]["text"], "Summary");
    assert_eq!(c["replaces"]["state"], "unknown");
    assert_eq!(
        r.iter()
            .filter(|v| v["type"] == "com.vercel.fx/committed_event")
            .count(),
        5
    );
    assert!(!r
        .iter()
        .any(|v| v.pointer("/content/0/text") == Some(&json!("Committed"))));
    if let Some(out) = std::env::var_os("TEST_UNDECLARED_OUTPUTS_DIR") {
        let mut options = options.clone();
        options.out = PathBuf::from(out).join("fx-replacement");
        export(&options).unwrap();
    }
}
