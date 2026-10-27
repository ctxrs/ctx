//! Authored cases; native writer evidence is documented separately.
use super::*;
use std::fs;

fn task(provider: Provider, ui: Value, api: Value) -> (tempfile::TempDir, Options) {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("tasks/native-task");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("ui_messages.json"), ui.to_string()).unwrap();
    fs::write(
        source.join("api_conversation_history.json"),
        api.to_string(),
    )
    .unwrap();
    let mut options = Options::new(
        vec![Input {
            provider,
            path: source,
        }],
        dir.path().join("out"),
    );
    options.namespace = Some("synthetic-installation".into());
    (dir, options)
}
fn ui_text(rows: &[Value], text: &str) -> Value {
    rows.iter()
        .find(|v| v.pointer("/displayContent/0/text").and_then(Value::as_str) == Some(text))
        .unwrap()
        .clone()
}
#[test]
fn task_rewrite_keeps_timestamp_identity_with_capture_local_order() {
    for provider in [Provider::Cline, Provider::Roo] {
        let (_dir, options) = task(
            provider,
            json!([
                {"ts":1000,"type":"say","say":"text","text":"partial","partial":true},
                {"ts":2000,"type":"say","say":"text","text":"second"}
            ]),
            json!([{"role":"user","content":"question"}]),
        );
        export(&options).unwrap();
        let before = records(&options);
        let original = ui_text(&before, "partial");
        assert!(original.get("idScope").is_none());
        assert!(original.get("assembly").is_none());
        assert_eq!(original["capture"]["content"]["state"], "absent");
        let manifest = fs::read(options.out.join("bundle.json")).unwrap();
        let old: Value = serde_json::from_slice(&manifest).unwrap();
        let old_path = options
            .out
            .join(old["sessions"][0]["path"].as_str().unwrap());
        let old_bytes = fs::read(&old_path).unwrap();
        fs::write(
            options.inputs[0].path.join("ui_messages.json"),
            json!([
                {"ts":2000,"type":"say","say":"text","text":"second"},
                {"ts":1000,"type":"say","say":"text","text":"complete","partial":false}
            ])
            .to_string(),
        )
        .unwrap();
        assert_eq!(export(&options).unwrap().snapshots, 1);
        let after = records(&options);
        let completed = ui_text(&after, "complete");
        assert_eq!(completed["id"], original["id"]);
        assert_ne!(completed["seq"], original["seq"]);
        assert_ne!(after[0]["observedAt"], before[0]["observedAt"]);
        assert!(completed.get("supersedes").is_none());
        assert_eq!(fs::read(old_path).unwrap(), old_bytes);
        assert_eq!(export(&options).unwrap().unchanged, 1);
        assert_eq!(records(&options), after);
        let moved = options.inputs[0]
            .path
            .with_file_name("relocated")
            .join("native-task");
        fs::create_dir_all(moved.parent().unwrap()).unwrap();
        fs::rename(&options.inputs[0].path, &moved).unwrap();
        let mut relocated = options.clone();
        relocated.inputs[0].path = moved;
        assert_eq!(export(&relocated).unwrap().snapshots, 0);
        assert_eq!(records(&relocated), after);
    }
}
#[test]
fn task_collisions_are_not_merged_or_paired_by_order() {
    let (_dir, options) = task(
        Provider::Roo,
        json!([
            {"ts":1000,"type":"say","say":"text","text":"first"},
            {"ts":1000,"type":"say","say":"text","text":"collision"}
        ]),
        json!([
            {"role":"assistant","content":[{"type":"tool_use","id":"same","name":"read","input":{}},{"type":"tool_use","id":"same","name":"read","input":{}}]},
            {"role":"user","content":[{"type":"tool_result","tool_use_id":"same","content":"result"}]}
        ]),
    );
    export(&options).unwrap();
    let r = records(&options);
    assert_eq!(ui_text(&r, "first")["idScope"], "capture");
    assert_ne!(ui_text(&r, "first")["id"], ui_text(&r, "collision")["id"]);
    assert_eq!(r.iter().filter(|v| v["type"] == "tool_call").count(), 2);
    let result = r.iter().find(|v| v["type"] == "tool_result").unwrap();
    assert_eq!(result["linkage"], "ambiguous");
    assert!(result.get("callId").is_none());
    // Even an empty native store cannot silently share another store's session key.
    let empty = options.out.parent().unwrap().join("other/native-task");
    fs::create_dir_all(&empty).unwrap();
    fs::write(empty.join("ui_messages.json"), "[]").unwrap();
    let mut duplicate = options.clone();
    duplicate.inputs.push(Input {
        provider: Provider::Roo,
        path: empty,
    });
    assert!(export(&duplicate)
        .unwrap_err()
        .to_string()
        .contains("multiple native stores"));
}
#[test]
fn task_large_integer_input_is_exact_json_text() {
    let (_dir, options) = task(
        Provider::Cline,
        json!([{"ts":1000,"type":"say","say":"api_req_started","text":"{\"cost\":9007199254740993}"}]),
        json!([{"role":"assistant","content":[{"type":"tool_use","id":"large","name":"read","input":{"value":9007199254740993u64}}]}]),
    );
    export(&options).unwrap();
    let r = records(&options);
    let call = r.iter().find(|v| v["type"] == "tool_call").unwrap();
    assert!(call.get("input").is_none());
    assert_eq!(call["inputText"], "{\"value\":9007199254740993}");
    let hash = call["native"]["blob"]["blob"]
        .as_str()
        .unwrap()
        .strip_prefix("sha256:")
        .unwrap();
    let native = fs::read_to_string(options.out.join("blobs/sha256").join(hash)).unwrap();
    assert!(native.contains("9007199254740993"));
    let inference = r.iter().find(|v| v["type"] == "inference").unwrap();
    assert!(inference.get("cost").is_none());
    assert!(inference.to_string().contains("9007199254740993"));
}
#[test]
fn task_incomplete_rewrite_preserves_published_bundle() {
    let (_dir, options) = task(
        Provider::Cline,
        json!([]),
        json!([{"role":"user","content":"valid"}]),
    );
    export(&options).unwrap();
    let before = fs::read(options.out.join("bundle.json")).unwrap();
    fs::write(
        options.inputs[0].path.join("api_conversation_history.json"),
        b"[{\"role\":\"user\"",
    )
    .unwrap();
    assert!(export(&options).is_err());
    assert_eq!(fs::read(options.out.join("bundle.json")).unwrap(), before);
}
#[test]
fn task_fallback_metadata_and_namespace() {
    let (_dir, mut options) = task(
        Provider::Roo,
        json!([]),
        json!([{"role":"user","content":"fallback"}]),
    );
    let dir = &options.inputs[0].path;
    fs::rename(
        dir.join("api_conversation_history.json"),
        dir.join("claude_messages.json"),
    )
    .unwrap();
    fs::write(
        dir.join("history_item.json"),
        json!({"id":"native-task","task":"Title","tokensIn":0,"tokensOut":2}).to_string(),
    )
    .unwrap();
    options.namespace = None;
    assert!(export(&options)
        .unwrap_err()
        .to_string()
        .contains("namespace"));
    options.namespace = Some("synthetic-installation".into());
    export(&options).unwrap();
    let r = records(&options);
    assert_eq!(r[0]["namespace"], "synthetic-installation");
    assert_eq!(
        r.iter().find(|v| v["type"] == "message").unwrap()["idScope"],
        "capture"
    );
    assert_eq!(
        r.iter().find(|v| v["type"] == "usage_summary").unwrap()["usage"]["input"]["total"],
        0
    );
}
#[test]
fn task_validation_corpus() {
    let Some(out) = std::env::var_os("TEST_UNDECLARED_OUTPUTS_DIR") else {
        return;
    };
    for provider in [Provider::Cline, Provider::Roo] {
        let (_dir, mut options) = task(
            provider,
            json!([
                {"ts":1000,"type":"say","say":"text","text":"Displayed","partial":false,"lastCheckpointHash":"abc123"},
                {"ts":2000,"type":"say","say":"api_req_started","text":"{\"tokensIn\":12,\"tokensOut\":3,\"cacheReads\":2,\"cost\":0.01}"},
                {"ts":3000,"type":"ask","ask":"followup","text":"What next?"},
                {"ts":4000,"type":"ask","ask":"command","text":"pwd"}
            ]),
            json!([
                {"role":"user","content":"Question"},
                {"role":"assistant","content":[{"type":"text","text":"Working"},{"type":"tool_use","id":"call1","name":"read_file","input":{"path":"note.txt"}}]},
                {"role":"user","content":[{"type":"tool_result","tool_use_id":"call1","content":"file body","is_error":false}]}
            ]),
        );
        options.out = PathBuf::from(&out).join(provider.agent());
        export(&options).unwrap();
    }
}

#[test]
fn native_call_ids_and_message_ids_survive_insertions() {
    let api = json!([{"role":"assistant","id":"native-message","content":"Hello"},{"role":"assistant","content":[{"type":"tool_use","id":"native-call","name":"read","input":{}}]}]);
    let (_dir, options) = task(Provider::Roo, json!([]), api.clone());
    export(&options).unwrap();
    let before = records(&options);
    let call = before.iter().find(|v| v["type"] == "tool_call").unwrap();
    assert!(call.get("idScope").is_none());
    assert_eq!(
        before
            .iter()
            .find(|v| v["messageId"] == "native-message")
            .unwrap()["idSource"],
        "native"
    );
    let mut changed = api.as_array().unwrap().clone();
    changed.insert(0, json!({"role":"user","content":"Inserted"}));
    fs::write(
        options.inputs[0].path.join("api_conversation_history.json"),
        json!(changed).to_string(),
    )
    .unwrap();
    export(&options).unwrap();
    let after = records(&options);
    assert_eq!(
        call["id"],
        after.iter().find(|v| v["type"] == "tool_call").unwrap()["id"]
    );
}

#[test]
fn task_large_binary_is_blobbed_without_mutating_native_metadata() {
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(vec![0u8; 65537]);
    let image =
        json!({"type":"image","source":{"type":"base64","media_type":"image/png","data":encoded}});
    let (_dir, options) = task(
        Provider::Cline,
        json!([]),
        json!([{"role":"user","content":[image.clone()]}]),
    );
    export(&options).unwrap();
    let r = records(&options);
    let m = r.iter().find(|v| v["type"] == "message").unwrap();
    assert!(m["content"][0].get("data").is_none());
    assert_eq!(m["content"][0]["blob"]["size"], 65537);
    let source = m.pointer("/native/blob").unwrap();
    let hash = source["blob"]
        .as_str()
        .unwrap()
        .strip_prefix("sha256:")
        .unwrap();
    let saved: Value =
        serde_json::from_slice(&fs::read(options.out.join("blobs/sha256").join(hash)).unwrap())
            .unwrap();
    assert_eq!(saved["content"][0], image);
}

#[test]
fn task_secret_answers_are_removed_before_native_blobs_and_context_metadata() {
    let secret = "SYNTHETIC_SECRET_ANSWER_9831";
    let (_dir, options) = task(
        Provider::Cline,
        json!([]),
        json!([
            {"role":"assistant","content":[{"type":"tool_use","id":"question","name":"request_user_input","input":{"questions":[{"id":"token","question":"Token?","secret":true}]}}]},
            {"role":"user","content":[{"type":"tool_result","tool_use_id":"question","content":json!({"answers":{"token":secret}}).to_string()}]},
            {"role":"assistant","content":format!("{} {secret}","x".repeat(70000))}
        ]),
    );
    fs::write(
        options.inputs[0].path.join("context_history.json"),
        json!([[1,{"answer":secret}]]).to_string(),
    )
    .unwrap();
    export(&options).unwrap();
    fn check(path: &std::path::Path, secret: &str) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                check(&path, secret)
            } else {
                let bytes = fs::read(path).unwrap();
                assert!(!bytes.windows(secret.len()).any(|s| s == secret.as_bytes()));
            }
        }
    }
    check(&options.out, secret);
}

#[test]
fn task_over_limit_and_malformed_rows_are_explicit_gaps() {
    let (_dir, mut options) = task(Provider::Cline, json!([]), json!([]));
    options.max_record_bytes = 1024;
    fs::write(options.inputs[0].path.join("api_conversation_history.json"),format!("[{{\"role\":\"user\",\"content\":\"{}\"}},{{bad}},{{\"role\":\"user\",\"content\":\"survives\"}}]","x".repeat(2000))).unwrap();
    assert_eq!(export(&options).unwrap().gaps, 2);
    let r = records(&options);
    assert!(r
        .iter()
        .any(|v| v["type"] == "gap" && v["reason"] == "conversion_loss"));
    assert!(r
        .iter()
        .any(|v| v["type"] == "gap" && v["reason"] == "unparseable"));
    assert!(r
        .iter()
        .any(|v| v.pointer("/content/0/text") == Some(&json!("survives"))));
}
