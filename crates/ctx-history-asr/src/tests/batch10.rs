//! Authored semantic cases, not native producer captures.
use super::*;
use std::{fs, path::Path};
fn write(path: &Path, value: &Value) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
}
fn document(provider: Provider, value: Value) -> (tempfile::TempDir, Options) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(if provider == Provider::RovoDev {
        "source/session_context.json"
    } else {
        "source/session.json"
    });
    write(&path, &value);
    let out = dir.path().join("bundle");
    (dir, Options::new(vec![Input { provider, path }], out))
}
fn openhands(events: &[Value], state: Value) -> (tempfile::TempDir, Options) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("conversation");
    for (i, event) in events.iter().enumerate() {
        write(
            &path.join(format!(
                "events/event-{i:05}-{}.json",
                event["id"].as_str().unwrap()
            )),
            event,
        );
    }
    write(&path.join("base_state.json"), &state);
    let out = dir.path().join("bundle");
    (
        dir,
        Options::new(
            vec![Input {
                provider: Provider::OpenHands,
                path,
            }],
            out,
        ),
    )
}
fn msg(id: &str, parent: Option<&str>, role: &str, text: &str) -> Value {
    json!({"kind":"MessageEvent","id":id,"parent_id":parent,"source":if role=="user"{"user"}else{"agent"},"timestamp":"2026-10-01T12:00:00.123456Z","llm_message":{"role":role,"content":[{"type":"text","text":text}]}})
}
#[test]
fn batch10_continue_rewrite_preserves_native_ids_but_not_seq_and_keeps_old_capture() {
    let (_dir, o) = document(
        Provider::Continue,
        json!({"sessionId":"continue-example","history":[{"message":{"id":"a","role":"user","content":" exact \r\n"}},{"message":{"id":"b","role":"assistant","content":"answer"}}],"usage":{"promptTokens":10,"completionTokens":0,"totalCost":0.0}}),
    );
    export(&o).unwrap();
    let before = records(&o);
    let manifest = fs::read(o.out.join("bundle.json")).unwrap();
    let old: Value = serde_json::from_slice(&manifest).unwrap();
    let oldpath = o.out.join(old["sessions"][0]["path"].as_str().unwrap());
    let oldbytes = fs::read(&oldpath).unwrap();
    assert_eq!(before[1]["content"][0]["text"], " exact \r\n");
    assert!(before[1].get("ts").is_none());
    assert!(before[1].get("idScope").is_none());
    assert_eq!(export(&o).unwrap().unchanged, 1);
    assert_eq!(manifest, fs::read(o.out.join("bundle.json")).unwrap());
    write(
        &o.inputs[0].path,
        &json!({"sessionId":"continue-example","history":[{"message":{"id":"new","role":"user","content":"inserted"}},{"message":{"id":"a","role":"user","content":"rewritten"}},{"message":{"id":"b","role":"assistant","content":"answer"}}]}),
    );
    assert_eq!(export(&o).unwrap().snapshots, 1);
    let after = records(&o);
    let a = after.iter().find(|r| r["messageId"] == "a").unwrap();
    assert_eq!(a["id"], before[1]["id"]);
    assert_ne!(a["seq"], before[1]["seq"]);
    assert!(after[0]["observedAt"].is_string());
    assert_eq!(fs::read(oldpath).unwrap(), oldbytes);
    assert!(after.iter().all(|r| r.get("supersedes").is_none()));
}
#[test]
fn batch10_continue_compaction_calls_usage_and_unknown_enum() {
    let (_dir, o) = document(
        Provider::Continue,
        json!({"sessionId":"continue-tools","mode":"future-native-mode","chatModelTitle":"chosen","history":[{"message":{"role":"assistant","content":"","toolCalls":[{"id":"c","function":{"name":"shell","arguments":"{\"n\":9007199254740993}"}}]},"toolCallStates":[{"toolCallId":"c","toolCall":{"id":"c","function":{"name":"shell","arguments":"{\"n\":9007199254740993}"}},"status":"done","output":[{"content":"output"}]}],"promptLogs":[{"modelTitle":"requested","modelProvider":"example","prompt":"prompt","completion":"completion"}],"conversationSummary":"summary"}],"usage":{"promptTokens":1,"completionTokens":2,"totalTokens":3,"totalCost":0.0}}),
    );
    export(&o).unwrap();
    let r = records(&o);
    let calls = r
        .iter()
        .filter(|v| v["type"] == "tool_call")
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["inputText"], "{\"n\":9007199254740993}");
    assert!(calls[0].get("input").is_none());
    let result = r.iter().find(|v| v["type"] == "tool_result").unwrap();
    assert!(result.get("content").is_none());
    assert_eq!(result["capture"]["content"]["state"], "absent");
    assert_eq!(result["status"], "completed");
    assert_eq!(result["linkage"], "exact");
    assert_eq!(r.iter().filter(|v| v["type"] == "compaction").count(), 1);
    assert_eq!(r.iter().filter(|v| v["type"] == "inference").count(), 1);
    let config = r.iter().find(|v| v["type"] == "config_change").unwrap();
    assert_eq!(config["mode"], "future-native-mode");
    assert_eq!(config["snapshot"], true);
}
#[test]
fn batch10_auggie_summary_is_not_a_message_and_no_timestamp_is_fabricated() {
    let (_dir, o) = document(
        Provider::Auggie,
        json!({"sessionId":"auggie-example","created":"2026-10-01T00:00:00Z","chatHistory":[{"sequenceId":1,"finishedAt":"2026-10-01T01:00:00Z","exchange":{"request_id":"request-1","request_message":"ask","response_text":"answer"}},{"isHistorySummary":true,"exchange":{"response_text":"summary"}}]}),
    );
    export(&o).unwrap();
    let r = records(&o);
    assert_eq!(r.iter().filter(|r| r["type"] == "message").count(), 2);
    assert_eq!(r.iter().filter(|r| r["type"] == "compaction").count(), 1);
    assert!(r
        .iter()
        .filter(|r| r["type"] == "message")
        .all(|r| r.get("ts").is_none()));
}
#[test]
fn batch10_codebuddy_cli_and_ide_keep_native_text() {
    let (_dir, o) = fixture(
        Provider::CodeBuddy,
        &[
            json!({"sessionId":"codebuddy-cli","id":"m","role":"user","content":"<user_query> exact \n</user_query>"}),
        ],
    );
    export(&o).unwrap();
    assert_eq!(
        records(&o)[1]["content"][0]["text"],
        "<user_query> exact \n</user_query>"
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("project/native-session/index.json");
    write(&path, &json!({"messages":[{"id":"m","role":"user"}]}));
    write(
        &path.with_file_name("messages/m.json"),
        &json!({"message":"{\"content\":\"exact IDE text\"}"}),
    );
    let o = Options::new(
        vec![Input {
            provider: Provider::CodeBuddy,
            path,
        }],
        dir.path().join("bundle"),
    );
    export(&o).unwrap();
    assert_eq!(records(&o)[1]["messageId"], "m");
    assert_eq!(records(&o)[1]["content"][0]["text"], "exact IDE text");
}
#[test]
fn batch10_rovo_duplicate_call_ids_are_unpaired_and_metadata_is_last() {
    let (_dir, o) = document(
        Provider::RovoDev,
        json!({"session_id":"rovo-example","message_history":[{"id":"a","role":"assistant","parts":[{"kind":"tool_use","id":"duplicate","name":"read","input":{"path":"a"}}]},{"id":"b","role":"assistant","parts":[{"kind":"tool_use","id":"duplicate","name":"read","input":{"path":"b"}}]},{"id":"c","role":"tool","tool_call_id":"duplicate","content":"result","exit_code":0,"signal":9}]}),
    );
    write(
        &o.inputs[0].path.with_file_name("metadata.json"),
        &json!({"session_id":"rovo-example","title":"latest"}),
    );
    export(&o).unwrap();
    let r = records(&o);
    let result = r.iter().find(|v| v["type"] == "tool_result").unwrap();
    assert_eq!(result["linkage"], "ambiguous");
    assert!(result.get("callId").is_none());
    assert_eq!(result["exec"]["exitCode"], 0);
    assert!(result["exec"].get("signal").is_none());
    assert_eq!(r.last().unwrap()["title"], "latest");
}
#[test]
fn batch10_openhands_forgets_individual_entries_and_state_is_tail() {
    let events = vec![
        msg("a", None, "user", "first"),
        msg("b", Some("a"), "assistant", "second"),
        json!({"kind":"Condensation","id":"compact","parent_id":"b","forgotten_event_ids":["a"],"summary":"short","summary_offset":0,"llm_response_id":"summary-call"}),
        msg("c", Some("compact"), "user", "third"),
    ];
    let (_dir, o) = openhands(
        &events,
        json!({"leaf_event_id":"c","confirmation_policy":{"kind":"AlwaysConfirm"},"secret_registry":{"s":"sensitive-test-value"}}),
    );
    export(&o).unwrap();
    let r = records(&o);
    let a = r.iter().find(|v| v["messageId"] == "a").unwrap();
    let c = r.iter().find(|v| v["type"] == "compaction").unwrap();
    assert_eq!(c["replaces"]["ids"], json!([a["id"]]));
    assert!(c["replaces"].get("throughId").is_none());
    assert_eq!(
        c["insertAt"],
        json!({"beforeId":r.iter().find(|v|v["messageId"]=="b").unwrap()["id"]})
    );
    assert!(r.last().unwrap()["assembly"]["mode"] == "snapshot");
    assert!(!serde_json::to_string(&r)
        .unwrap()
        .contains("sensitive-test-value"));
}
#[test]
fn batch10_openhands_parallel_calls_are_mechanical_but_new_prompt_branches() {
    let action = |id: &str, parent: &str, call: &str| json!({"kind":"ActionEvent","id":id,"parent_id":parent,"llm_response_id":"response","thought":[],"tool_call_id":call,"tool_name":"read","tool_call":{"id":call,"name":"read","arguments":"{}"}});
    let events = vec![
        msg("u", None, "user", "prompt"),
        action("a", "u", "c1"),
        action("b", "u", "c2"),
        msg("edit", Some("u"), "user", "edited prompt"),
    ];
    let (_dir, o) = openhands(&events, json!({"leaf_event_id":"edit"}));
    export(&o).unwrap();
    let r = records(&o);
    let calls = r
        .iter()
        .filter(|v| v["type"] == "tool_call")
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 2);
    assert!(calls[1].get("branchesFrom").is_none());
    let edit = r.iter().find(|v| v["messageId"] == "edit").unwrap();
    assert!(edit["branchesFrom"]["record"].is_string());
}
#[test]
fn batch10_large_integer_native_data_is_preserved_as_json_text() {
    let (_dir, o) = document(
        Provider::Continue,
        json!({"sessionId":"large-number","history":[]}),
    );
    fs::write(&o.inputs[0].path,"{\"sessionId\":\"large-number\",\"history\":[{\"message\":{\"role\":\"user\",\"content\":\"body\"},\"value\":184467440737095516170}]}").unwrap();
    export(&o).unwrap();
    let bytes = serde_json::to_string(&records(&o)).unwrap();
    assert!(bytes.contains("184467440737095516170"));
    assert!(bytes.contains("numeric_record"));
}
#[test]
fn batch10_document_cursor_bounds_each_item_not_whole_session() {
    let history = (0..100)
        .map(|i| json!({"message":{"role":"user","content":format!("{i}{}","x".repeat(100))}}))
        .collect::<Vec<_>>();
    let (_dir, mut o) = document(
        Provider::Continue,
        json!({"sessionId":"many-items","history":history}),
    );
    o.max_record_bytes = 1024;
    assert_eq!(export(&o).unwrap().sessions, 1);
    assert_eq!(
        records(&o)
            .iter()
            .filter(|r| r["type"] == "message")
            .count(),
        100
    );
}

#[test]
fn batch10_openhands_native_offsets_include_system_and_summary_entries() {
    let (_dir, o) = openhands(
        &[
            json!({"kind":"SystemPromptEvent","id":"system","system_prompt":{"type":"text","text":"rules"},"tools":[{"kind":"TerminalTool","description":"terminal"}]}),
            msg("a", None, "user", "keep"),
            msg("b", None, "assistant", "forget"),
            json!({"kind":"Condensation","id":"c","forgotten_event_ids":["b"],"summary":"first summary","summary_offset":1}),
            json!({"kind":"Condensation","id":"d","forgotten_event_ids":["c-summary"],"summary":"new summary","summary_offset":1}),
        ],
        json!({}),
    );
    export(&o).unwrap();
    let r = records(&o);
    let c = r.iter().find(|r| r["compactionId"] == "c").unwrap();
    let d = r.iter().find(|r| r["compactionId"] == "d").unwrap();
    assert!(c["insertAt"]["beforeId"].is_string());
    assert_eq!(d["replaces"]["ids"], json!([c["id"]]));
    let t = r.iter().find(|r| r["kind"] == "tool_definitions").unwrap();
    assert_eq!(t["tools"], json!([]));
    assert_eq!(t["capture"]["tools"]["state"], "truncated");
}
#[test]
fn batch10_openhands_metrics_revise_response_and_preserve_unknown_cache_accounting() {
    let mut a = msg("a", None, "assistant", "hello");
    a["llm_response_id"] = json!("response");
    let (_dir, o) = openhands(
        &[a],
        json!({"stats":{"usage_to_metrics":{"agent":{"token_usages":[{"response_id":"response","model":"native-model","prompt_tokens":12,"completion_tokens":4,"cache_read_tokens":2}],"response_latencies":[{"response_id":"response","latency":0.25}],"accumulated_token_usage":{"prompt_tokens":12,"completion_tokens":4},"accumulated_cost":0.01}}}}),
    );
    export(&o).unwrap();
    let r = records(&o);
    let inf = r
        .iter()
        .filter(|r| r["type"] == "inference")
        .collect::<Vec<_>>();
    assert_eq!(inf.len(), 2);
    assert_eq!(inf[1]["supersedes"], inf[0]["id"]);
    assert_eq!(inf[1]["usage"]["input"]["includesCache"], Value::Null);
    assert_eq!(inf[1]["latency"]["durationMs"], 250.0);
    assert_eq!(inf[1]["model"]["requested"], "native-model");
    assert!(inf[1]["model"].get("id").is_none());
    assert!(r
        .windows(2)
        .all(|r| r[0]["seq"].as_u64() < r[1]["seq"].as_u64()));
}
#[test]
fn batch10_nested_rovo_document_and_large_sdk_image() {
    let (_dir, o) = document(
        Provider::RovoDev,
        json!({"session_context":{"message_history":[{"id":"u","role":"user","parts":[{"kind":"text","text":"nested"}]}]}}),
    );
    export(&o).unwrap();
    assert!(records(&o)
        .iter()
        .any(|r| r["content"][0]["text"] == "nested"));
    use base64::Engine;
    let data = base64::engine::general_purpose::STANDARD.encode(vec![42u8; 70_000]);
    let mut m = msg("image", None, "user", "");
    m["llm_message"]["content"] =
        json!([{"type":"image","image_urls":[format!("data:image/png;base64,{data}")]}]);
    let (_dir, o) = openhands(&[m], json!({}));
    export(&o).unwrap();
    let r = records(&o);
    let image = &r.iter().find(|r| r["type"] == "message").unwrap()["content"][0];
    assert_eq!(image["type"], "image");
    assert!(image.get("data").is_none());
    assert_eq!(image["blob"]["size"], 70_000);
}
#[test]
fn batch10_decimal_unsafe_numbers_preserve_lexical_value() {
    for number in ["9007199254740992.0", "9.007199254740993e15", "1e309"] {
        assert!(crate::batch10::unsafe_native_number(number));
    }
    assert!(!crate::batch10::unsafe_native_number("100.0"));
}

#[test]
fn batch10_openhands_forgets_all_projections_and_accepts_discovered_event_paths() {
    let (_dir, mut o) = openhands(
        &[
            msg("u", None, "user", "question"),
            json!({"kind":"ActionEvent","id":"action","parent_id":"u","thought":[{"type":"text","text":"thinking"}],"tool_call_id":"call","tool_call":{"id":"call","name":"echo","arguments":"{}"}}),
            json!({"kind":"Condensation","id":"c","forgotten_event_ids":["action"],"summary":"summary","summary_offset":1}),
        ],
        json!({}),
    );
    let root = o.inputs[0].path.clone();
    o.inputs.push(Input {
        provider: Provider::OpenHands,
        path: root.join("events/event-00000-u.json"),
    });
    export(&o).unwrap();
    let r = records(&o);
    let c = r.iter().find(|r| r["type"] == "compaction").unwrap();
    assert_eq!(c["replaces"]["ids"].as_array().unwrap().len(), 2);
    let previous = fs::read(o.out.join("bundle.json")).unwrap();
    assert_eq!(export(&o).unwrap().unchanged, 1);
    assert_eq!(fs::read(o.out.join("bundle.json")).unwrap(), previous);
}
#[test]
fn batch10_distinct_documents_with_colliding_session_ids_are_not_merged() {
    let (_dir, mut o) = document(Provider::Continue, json!({"sessionId":"same","history":[]}));
    let second = o.inputs[0].path.with_file_name("second.json");
    write(&second, &json!({"sessionId":"same","history":[]}));
    o.inputs.push(Input {
        provider: Provider::Continue,
        path: second,
    });
    assert!(export(&o)
        .unwrap_err()
        .to_string()
        .contains("multiple native sources"));
    assert!(!o.out.join("bundle.json").exists());
}

#[test]
fn batch10_rovo_nested_result_id_is_exact_and_conflicting_aliases_stay_unpaired() {
    let (_dir, o) = document(
        Provider::RovoDev,
        json!({"session_id":"rovo-nested","message_history":[{"id":"a","role":"assistant","parts":[{"kind":"tool_use","id":"call","name":"read","input":{}}]},{"id":"b","role":"tool","parts":[{"kind":"tool_result","tool_use_id":"call","status":"completed","content":"nested result"}]},{"id":"c","role":"tool","call_id":"different","parts":[{"kind":"tool_result","tool_use_id":"call","content":"conflicting IDs"}]}]}),
    );
    export(&o).unwrap();
    let r = records(&o);
    let results = r
        .iter()
        .filter(|r| r["type"] == "tool_result")
        .collect::<Vec<_>>();
    assert_eq!(results[0]["callId"], "call");
    assert_eq!(results[0]["status"], "completed");
    assert!(results[1].get("callId").is_none());
}
