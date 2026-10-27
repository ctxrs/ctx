//! Sanitized native captures; scenario and sanitization scope accompany the data.
use super::*;
use std::path::Path;

#[test]
fn authentic_headless_text_captures_preserve_tools_and_repeat() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/provider-history/authentic-headless/v1");
    for (provider, name, sessions, calls) in [
        (Provider::Continue, "continue", 1, 4),
        (Provider::QwenCode, "qwen-code", 1, 4),
    ] {
        let out = tempfile::tempdir().unwrap();
        copy_tree(&root.join(name), &out.path().join("source"));
        let options = Options::new(
            vec![Input {
                provider,
                path: out.path().join("source"),
            }],
            out.path().join("bundle"),
        );
        let first = export(&options).unwrap();
        assert_eq!(first.sessions, sessions);
        assert_eq!(first.gaps, 0);
        let manifest: Value =
            serde_json::from_slice(&std::fs::read(options.out.join("bundle.json")).unwrap())
                .unwrap();
        let mut tools = 0;
        let mut results = 0;
        for session in manifest["sessions"].as_array().unwrap() {
            let bytes =
                std::fs::read_to_string(options.out.join(session["path"].as_str().unwrap()))
                    .unwrap();
            for line in bytes.lines() {
                let r: Value = serde_json::from_str(line).unwrap();
                tools += usize::from(r["type"] == "tool_call");
                results += usize::from(r["type"] == "tool_result");
                if let Some(meta) = r["_meta"].as_object() {
                    assert!(!meta.contains_key(&format!("{}/source", provider.domain())));
                }
                if provider == Provider::Continue && r["type"] == "tool_result" {
                    assert!(r["native"]["record"].is_object());
                }
            }
        }
        assert_eq!(tools, calls);
        assert_eq!(results, calls);
        assert_eq!(export(&options).unwrap().unchanged, sessions);
    }
}

fn copy_tree(source: &Path, target: &Path) {
    std::fs::create_dir_all(target).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target.join(entry.file_name()));
        } else {
            std::fs::copy(entry.path(), target.join(entry.file_name())).unwrap();
        }
    }
}

#[test]
fn grok_native_task_kind_is_a_vendor_kind_and_status_is_grouped() {
    let (_dir, options) = fixture(
        Provider::GrokBuild,
        &[
            json!({"sessionId":"native-kind","update":{"sessionUpdate":"tool_call","toolCallId":"task-call","kind":"task","status":"pending","title":"Review"}}),
        ],
    );
    export(&options).unwrap();
    let r = records(&options);
    let call = r.iter().find(|r| r["type"] == "tool_call").unwrap();
    assert_eq!(call["kind"], "_ai.x.grok-build/task");
    assert_eq!(
        call["_meta"]["ai.x.grok-build/remainingFields"]["status"],
        "pending"
    );
}

#[test]
fn codebuddy_fork_owner_and_function_records_use_distinct_native_ids() {
    let mut rows = vec![
        json!({"type":"message","id":"copied","sessionId":"parent","role":"user","content":"copied prompt"});
        70
    ];
    rows.extend([
        json!({"type":"session-meta","id":"meta","sessionId":"fork"}),
        json!({"type":"function_call","id":"call-row","sessionId":"fork","callId":"tool-id","name":"Read","arguments":"{\"path\":\"greeting.py\"}"}),
        json!({"type":"function_call_result","id":"result-row","sessionId":"fork","callId":"tool-id","status":"completed","output":{"type":"text","text":"hello"}}),
    ]);
    let (_dir, options) = fixture(Provider::CodeBuddy, &rows);
    export(&options).unwrap();
    let r = records(&options);
    assert_eq!(r[0]["sessionId"], "fork");
    let call = r.iter().find(|r| r["type"] == "tool_call").unwrap();
    assert_eq!(call["callId"], "tool-id");
    assert_eq!(call["name"], "Read");
    let result = r.iter().find(|r| r["type"] == "tool_result").unwrap();
    assert_eq!(result["callId"], "tool-id");
    assert_eq!(result["status"], "completed");
    assert_eq!(result["raw"]["output"]["text"], "hello");
    assert_eq!(result["capture"]["content"]["state"], "absent");
}
