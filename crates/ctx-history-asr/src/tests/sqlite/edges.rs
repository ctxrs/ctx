//! Authored snapshot, precision, identity and privacy regressions.
use super::*;

#[test]
fn unrelated_sqlite_update_preserves_capture_and_manifest_bytes() {
    let (_d, o, db) = database(Provider::Crush, CRUSH);
    crush_message(&db, "u", "user", json!([]), 1000);
    export(&o).unwrap();
    let before = std::fs::read(stream_path(&o)).unwrap();
    let manifest = std::fs::read(o.out.join("bundle.json")).unwrap();
    db.execute_batch("CREATE TABLE unrelated(v TEXT);INSERT INTO unrelated VALUES('changed')")
        .unwrap();
    let summary = export(&o).unwrap();
    assert_eq!(summary.snapshots, 0);
    assert_eq!(std::fs::read(stream_path(&o)).unwrap(), before);
    assert_eq!(std::fs::read(o.out.join("bundle.json")).unwrap(), manifest);
}
#[test]
fn embedded_tool_ids_survive_part_insertions() {
    let (_d, o, db) = database(Provider::Crush, CRUSH);
    let call = json!({"type":"tool_call","data":{"id":"call","name":"read","input":"{}"}});
    let result = json!({"type":"tool_result","data":{"tool_call_id":"call","content":"ok","is_error":false}});
    crush_message(&db, "a", "assistant", json!([call, result]), 1000);
    export(&o).unwrap();
    let before = records(&o);
    let ids: Vec<_> = before
        .iter()
        .filter(|v| v["type"] == "tool_call" || v["type"] == "tool_result")
        .map(|v| v["id"].clone())
        .collect();
    db.execute(
        "UPDATE messages SET parts=?1",
        [json!([{"type":"text","data":{"text":"inserted"}},call,result]).to_string()],
    )
    .unwrap();
    export(&o).unwrap();
    let after = records(&o);
    assert_eq!(
        ids,
        after
            .iter()
            .filter(|v| v["type"] == "tool_call" || v["type"] == "tool_result")
            .map(|v| v["id"].clone())
            .collect::<Vec<_>>()
    );
}
#[test]
fn native_json_duplicates_and_unrepresentable_numbers_stay_exact() {
    let (_d, o, db) = database(Provider::Goose, GOOSE);
    let huge = r#"[{"type":"toolRequest","id":"c","toolCall":{"status":"success","value":{"name":"n","arguments":{"n":18446744073709551617}}}}]"#;
    let duplicate = r#"[{"type":"text","text":"first","text":"second"}]"#;
    for (id, raw) in [("large", huge), ("duplicate", duplicate)] {
        db.execute("INSERT INTO messages(message_id,session_id,role,content_json,created_timestamp) VALUES(?1,'20260101_1','assistant',?2,1)",params![id,raw]).unwrap();
    }
    assert_eq!(export(&o).unwrap().gaps, 1);
    let rows = records(&o);
    let native: Vec<Value> = rows
        .iter()
        .filter(|v| v["type"] == "gap")
        .map(|v| v["native"]["record"].clone())
        .collect();
    assert_eq!(native[0]["content_json"], duplicate);
    let call = rows.iter().find(|r| r["type"] == "tool_call").unwrap();
    assert_eq!(call["inputText"], r#"{"n":18446744073709551617}"#);
    assert!(call.get("input").is_none());
}
#[test]
fn goose_message_usage_is_attributed_only_when_ledger_is_absent() {
    let (_d, o, db) = database(Provider::Goose, GOOSE);
    goose_message(
        &db,
        Some("m"),
        "assistant",
        json!([]),
        json!({"usage":{"inputTokens":10,"outputTokens":3,"totalTokens":13,"elapsedMs":25,"cost":0.01,"costSource":"estimated"},"inference":{"provider":"p","requestedModel":"alias","resolvedModel":"served"}}),
        1,
    );
    export(&o).unwrap();
    let rows = records(&o);
    let inf = rows.iter().find(|v| v["type"] == "inference").unwrap();
    assert_eq!(
        inf["model"],
        json!({"provider":"p","requested":"alias","id":"served"})
    );
    assert_eq!(inf["usage"]["total"], 13);
    assert_eq!(rows[1]["inferenceId"], inf["inferenceId"]);
    assert_eq!(inf["latency"]["durationMs"], 25);
    db.execute_batch(
        "INSERT INTO usage_ledger(id,session_id,input_tokens) VALUES(1,'20260101_1',10)",
    )
    .unwrap();
    export(&o).unwrap();
    let rows = records(&o);
    assert_eq!(rows.iter().filter(|v| v["type"] == "inference").count(), 1);
    assert!(rows[1].get("inferenceId").is_none());
}
#[test]
fn goose_thread_mirrors_preserve_secret_redaction_without_duplicate_calls() {
    let (_d, o, db) = database(Provider::Goose, GOOSE);
    db.execute_batch("CREATE TABLE threads(id TEXT PRIMARY KEY,name TEXT);INSERT INTO threads VALUES('lane','Unassigned lane');CREATE TABLE thread_messages(id INTEGER PRIMARY KEY,session_id TEXT,message_id TEXT,role TEXT,content_json TEXT,created_timestamp INTEGER,metadata_json TEXT)").unwrap();
    let q = json!([{"type":"actionRequired","data":{"actionType":"elicitation","id":"q","requested_schema":{"properties":{"p":{"format":"password"}}}}},{"type":"toolRequest","id":"c","toolCall":{"status":"success","value":{"name":"read","arguments":{}}}}]);
    let a = json!([{"type":"actionRequired","data":{"actionType":"elicitationResponse","id":"q","action":"accept","user_data":{"p":"AUTHORED-THREAD-SECRET"}}},{"type":"toolResponse","id":"c","toolResult":{"status":"success","value":{"content":[]}}}]);
    goose_message(&db, Some("q"), "assistant", q, json!({}), 1);
    goose_message(&db, Some("a"), "user", a.clone(), json!({}), 2);
    db.execute_batch("INSERT INTO thread_messages SELECT id,session_id,message_id,role,content_json,created_timestamp,metadata_json FROM messages").unwrap();
    db.execute(
        "INSERT INTO thread_messages VALUES(3,NULL,'orphan','user',?1,3,'{}')",
        [a.to_string()],
    )
    .unwrap();
    export(&o).unwrap();
    let rows = records(&o);
    assert_eq!(
        rows.iter().find(|v| v["type"] == "tool_result").unwrap()["linkage"],
        "exact"
    );
    assert!(rows
        .iter()
        .any(|v| v["type"] == "xyz.block.goose/thread_message"));
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(o.out.join("bundle.json")).unwrap()).unwrap();
    assert_eq!(
        manifest["_meta"]["xyz.block.goose/unassigned"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    fn inspect(path: &std::path::Path) {
        for entry in std::fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                inspect(&entry.path());
            } else if entry.path().extension().is_none_or(|e| e != "sqlite") {
                assert!(
                    !String::from_utf8_lossy(&std::fs::read(entry.path()).unwrap())
                        .contains("AUTHORED-THREAD-SECRET")
                );
            }
        }
    }
    inspect(&o.out);
}
#[test]
fn large_binary_and_native_metadata_are_externalized() {
    use base64::Engine;
    let (_d, o, db) = database(Provider::Crush, CRUSH);
    let data = base64::engine::general_purpose::STANDARD.encode(vec![42u8; 70000]);
    crush_message(
        &db,
        "u",
        "user",
        json!([{"type":"binary","data":{"MIMEType":"image/png","Data":data}}]),
        1000,
    );
    export(&o).unwrap();
    let rows = records(&o);
    assert!(rows[1]["content"][0]["blob"].is_object());
    assert!(rows[1]["native"]["blob"].is_object());
    assert!(!std::fs::read_to_string(stream_path(&o))
        .unwrap()
        .contains(&data));
}

#[test]
fn crush_initial_usage_defaults_do_not_claim_a_report() {
    let (_dir, options, db) = database(Provider::Crush, CRUSH);
    db.execute_batch("UPDATE sessions SET prompt_tokens=0,completion_tokens=0,cost=0")
        .unwrap();
    export(&options).unwrap();
    assert!(!records(&options)
        .iter()
        .any(|r| r["type"] == "usage_summary"));
    db.execute_batch("UPDATE sessions SET prompt_tokens=2")
        .unwrap();
    export(&options).unwrap();
    let rows = records(&options);
    let usage = rows.iter().find(|r| r["type"] == "usage_summary").unwrap();
    assert_eq!(usage["usage"]["input"]["total"], 2);
    assert_eq!(usage["usage"]["output"]["total"], 0);
}
