//! Authored v3.1 boundary cases; these complement native-history qualification.
use super::*;
use std::io::Write;

#[test]
fn exact_native_json_numbers_do_not_change_workspace_json_behavior() {
    use ctx_history_asr_core::numbers::{decode, safe, text};
    for raw in [
        r#"{"large":18446744073709551617}"#,
        r#"{"large":-18446744073709551617}"#,
        r#"{"$ctx.asr.exactJson":"a native key"}"#,
        r#"{"nested":[{"$ctx.asr.exactJson":"18446744073709551617"}]}"#,
    ] {
        let value = decode(raw).unwrap();
        assert!(!safe(&value));
        assert_eq!(text(&value), raw);
    }
    assert_eq!(serde_json::from_str::<Value>("0.25").unwrap(), json!(0.25));
    let marker = r#"{"$serde_json::private::Number":"001"}"#;
    assert_eq!(
        serde_json::from_str::<Value>(marker).unwrap()["$serde_json::private::Number"],
        "001"
    );
    assert!(decode(r#"{"large":1,"large":18446744073709551617}"#).is_none());
    let (_dir, o) = fixture(Provider::Claude, &[]);
    std::fs::write(&o.inputs[0].path, concat!(r#"{"type":"assistant","sessionId":"test","uuid":"a","message":{"id":"m","content":[{"type":"tool_use","id":"c","name":"test","input":{"large":18446744073709551617}}]}}"#, "\n")).unwrap();
    export(&o).unwrap();
    let rows = records(&o);
    let call = rows.iter().find(|v| v["type"] == "tool_call").unwrap();
    assert_eq!(call["inputText"], r#"{"large":18446744073709551617}"#);
    assert!(call.get("input").is_none());
}

#[test]
fn claude_growth_preserves_bytes_and_rewrites_start_a_capture() {
    let (_dir, o) = fixture(
        Provider::Claude,
        &[
            json!({"type":"user","sessionId":"test","uuid":"u","message":{"content":"start"}}),
            json!({"type":"assistant","sessionId":"test","uuid":"a","parentUuid":"u","message":{"id":"response","content":[{"type":"text","text":"one"}],"usage":{"output_tokens":1}}}),
        ],
    );
    export(&o).unwrap();
    let before = records(&o);
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&o.inputs[0].path)
        .unwrap();
    for (uuid, text, count) in [("b", "two", 2), ("c", "three", 3)] {
        writeln!(f,"{}",json!({"type":"assistant","sessionId":"test","uuid":uuid,"parentUuid":"a","message":{"id":"response","content":[{"type":"text","text":text}],"usage":{"output_tokens":count}}})).unwrap();
        assert_eq!(export(&o).unwrap().snapshots, 0);
        let rows = records(&o);
        assert_eq!(&rows[..before.len()], &before);
        let latest = rows
            .iter()
            .rev()
            .find(|r| r["messageId"] == "response")
            .unwrap();
        assert_eq!(latest["content"].as_array().unwrap().len(), count);
        assert!(latest["supersedes"].is_string());
        assert_eq!(export(&o).unwrap().unchanged, 1);
    }
    writeln!(f,"{}",json!({"type":"assistant","sessionId":"test","uuid":"a","parentUuid":"u","message":{"id":"response","content":[{"type":"text","text":"rewritten"}],"usage":{"output_tokens":9}}})).unwrap();
    assert_eq!(export(&o).unwrap().snapshots, 1);
    let rows = records(&o);
    assert!(rows[0]["observedAt"].is_string());
    let m = rows.iter().find(|r| r["messageId"] == "response").unwrap();
    assert_eq!(m["content"][0]["text"], "rewritten");
    assert_eq!(m["content"][1]["text"], "two");
    assert_eq!(m["content"][2]["text"], "three");
    assert_eq!(
        rows.iter().find(|r| r["type"] == "inference").unwrap()["usage"]["output"]["total"],
        9
    );
}

#[test]
fn pilot_exact_numbers_and_large_binaries() {
    use base64::Engine;
    for provider in [Provider::Claude, Provider::Codex] {
        let number = json!({"large":u64::MAX});
        let data = base64::engine::general_purpose::STANDARD.encode(vec![42u8; 65537]);
        let rows = if provider == Provider::Claude {
            vec![
                json!({"type":"user","sessionId":"test","uuid":"u","message":{"content":[{"type":"image","source":{"type":"base64","media_type":"image/png","data":data}},{"type":"resource","resource":{"uri":"test:binary","blob":data}}]}}),
                json!({"type":"assistant","sessionId":"test","uuid":"a","message":{"id":"m","content":[{"type":"tool_use","id":"c","name":"test","input":number}]}}),
            ]
        } else {
            vec![
                json!({"type":"session_meta","payload":{"id":"test","source":"cli"}}),
                json!({"type":"response_item","ordinal":9007199254740990u64,"payload":{"type":"message","id":"u","role":"user","content":[{"type":"input_image","image_url":format!("data:image/png;base64,{data}")}]}}),
                json!({"type":"response_item","payload":{"type":"function_call","call_id":"c","name":"test","arguments":number.to_string()}}),
            ]
        };
        let (_dir, o) = fixture(provider, &rows);
        export(&o).unwrap();
        let r = records(&o);
        let call = r.iter().find(|r| r["type"] == "tool_call").unwrap();
        assert_eq!(call["inputText"], number.to_string());
        assert!(call.get("input").is_none());
        let image = r.iter().find(|r| r["role"] == "user").unwrap()["content"][0].clone();
        assert!(image.get("data").is_none());
        let digest = image["blob"]["blob"]
            .as_str()
            .unwrap()
            .strip_prefix("sha256:")
            .unwrap();
        assert_eq!(
            std::fs::read(o.out.join("blobs/sha256").join(digest)).unwrap(),
            vec![42u8; 65537]
        );
        assert!(r.iter().all(|r| r["seq"].as_u64().unwrap() < 20));
        if provider == Provider::Claude {
            let resource =
                &r.iter().find(|r| r["role"] == "user").unwrap()["content"][1]["resource"];
            assert!(resource.get("blob").is_none());
            assert_eq!(resource["blobRef"]["size"], 65537);
        }
        if provider == Provider::Codex {
            let m = r.iter().find(|r| r["messageId"] == "u").unwrap();
            assert_eq!(
                m["_meta"]["com.openai.codex/position"]["ordinal"],
                9007199254740990u64
            );
            assert!(m["_meta"]["com.openai.codex/position"]["segmentId"].is_string());
        }
    }
}

#[test]
fn codex_native_trigger_and_uri_contracts() {
    let (_dir, o) = fixture(
        Provider::Codex,
        &[
            json!({"type":"session_meta","payload":{"id":"test","source":"cli","cwd":"/synthetic"}}),
            json!({"type":"response_item","payload":{"type":"function_call","call_id":"call","name":"new_context","arguments":"{}","cell_id":"host"}}),
            json!({"type":"event_msg","payload":{"type":"item_completed","item":{"type":"CommandExecution","id":"exec","cwd":"file:///synthetic/a%20b","status":"completed"}}}),
            json!({"type":"event_msg","payload":{"type":"item_completed","item":{"type":"DynamicToolCall","id":"dynamic","error":"failed natively","status":"failed"}}}),
            json!({"type":"compacted","payload":{"window_id":"w","message":"summary","replacement_history":[{"type":"message","role":"developer","content":"fresh"}]}}),
            json!({"type":"event_msg","payload":{"type":"turn_aborted","reason":"future_native_reason"}}),
        ],
    );
    export(&o).unwrap();
    let r = records(&o);
    let call = r.iter().find(|r| r["callId"] == "call").unwrap();
    assert!(call.get("parentCallId").is_none());
    let compact = r.iter().find(|r| r["type"] == "compaction").unwrap();
    assert!(compact.get("trigger").is_none());
    assert_eq!(
        compact["_meta"]["com.openai.codex/compaction"]["replacement_history"][0]["content"],
        "fresh"
    );
    let execution = r
        .iter()
        .find(|r| r["type"] == "tool_result" && r["callId"] == "exec")
        .unwrap();
    assert_eq!(execution["exec"]["cwd"]["path"], "a b");
    let dynamic = r
        .iter()
        .find(|r| r["type"] == "tool_result" && r["callId"] == "dynamic")
        .unwrap();
    assert_eq!(dynamic["error"]["message"], "failed natively");
    let state = r.iter().find(|r| r["type"] == "state").unwrap();
    assert_eq!(state["stopReason"], "cancelled");
    assert_eq!(
        state["_meta"]["com.openai.codex/abortReason"],
        "future_native_reason"
    );
}

#[test]
fn codex_reused_ordinals_preserve_distinct_rows_and_appends() {
    let (_dir, o) = fixture(
        Provider::Codex,
        &[
            json!({"type":"session_meta","ordinal":0,"payload":{"id":"test","source":"cli"}}),
            json!({"type":"event_msg","ordinal":7,"payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}}),
            json!({"type":"turn_context","ordinal":7,"payload":{"turn_id":"synthetic-turn","model":"synthetic-model"}}),
            json!({"type":"event_msg","ordinal":7,"payload":{"type":"task_started","turn_id":"synthetic-turn"}}),
            json!({"type":"response_item","ordinal":7,"payload":{"type":"message","id":"synthetic-message","role":"user","content":[{"type":"input_text","text":"synthetic input"}]}}),
        ],
    );
    export(&o).unwrap();
    let before = records(&o);
    let ids: std::collections::HashSet<_> =
        before.iter().map(|r| r["id"].as_str().unwrap()).collect();
    assert_eq!(ids.len(), before.len());
    for (kind, line) in [
        ("usage_summary", 2),
        ("config_change", 3),
        ("state", 4),
        ("message", 5),
    ] {
        let row = before.iter().find(|r| r["type"] == kind).unwrap();
        let position = &row["_meta"]["com.openai.codex/position"];
        assert_eq!(position["ordinal"], 7);
        assert_eq!(position["line"], line);
    }
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(o.out.join("bundle.json")).unwrap()).unwrap();
    let path = o
        .out
        .join(manifest["sessions"][0]["path"].as_str().unwrap());
    let bytes = std::fs::read(&path).unwrap();
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&o.inputs[0].path)
        .unwrap();
    writeln!(f, "{}", json!({"type":"event_msg","ordinal":7,"payload":{"type":"task_complete","turn_id":"synthetic-turn"}})).unwrap();
    assert_eq!(export(&o).unwrap().snapshots, 0);
    assert!(std::fs::read(path).unwrap().starts_with(&bytes));
    let after = records(&o);
    let ids: std::collections::HashSet<_> =
        after.iter().map(|r| r["id"].as_str().unwrap()).collect();
    assert_eq!(ids.len(), after.len());
    assert_eq!(&after[..before.len()], &before);
}
