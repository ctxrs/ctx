//! Authored SQLite/protobuf semantic cases, not native harness captures.
use super::*;
use rusqlite::{params, Connection};
pub(super) fn database(
    provider: Provider,
    schema: &str,
) -> (tempfile::TempDir, Options, Connection) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("source.db");
    let db = Connection::open(&path).unwrap();
    db.execute_batch(schema).unwrap();
    let mut options = Options::new(vec![Input { provider, path }], dir.path().join("bundle"));
    if provider == Provider::Goose {
        options.namespace = Some("authored-installation".into());
    }
    (dir, options, db)
}
const CRUSH:&str="CREATE TABLE sessions(id TEXT PRIMARY KEY,title TEXT,created_at INTEGER,prompt_tokens INTEGER,completion_tokens INTEGER,cost REAL);INSERT INTO sessions VALUES('s','Authored session',1000,10,3,0.1);CREATE TABLE messages(id TEXT PRIMARY KEY,session_id TEXT,role TEXT,parts TEXT,created_at INTEGER,model TEXT,is_summary_message INTEGER);CREATE TABLE files(id TEXT PRIMARY KEY,session_id TEXT,path TEXT,content TEXT,version INTEGER,created_at INTEGER);";
const GOOSE:&str="CREATE TABLE sessions(id TEXT PRIMARY KEY,name TEXT,parent_session_id TEXT,working_dir TEXT,input_tokens INTEGER,output_tokens INTEGER,total_tokens INTEGER,accumulated_input_tokens INTEGER,accumulated_output_tokens INTEGER,accumulated_total_tokens INTEGER,model_config_json TEXT,goose_mode TEXT);INSERT INTO sessions(id,name) VALUES('20260101_1','Authored session');CREATE TABLE messages(id INTEGER PRIMARY KEY AUTOINCREMENT,message_id TEXT,session_id TEXT,role TEXT,content_json TEXT,created_timestamp INTEGER,metadata_json TEXT);CREATE TABLE usage_ledger(id INTEGER PRIMARY KEY,session_id TEXT,created_timestamp INTEGER,model TEXT,input_tokens INTEGER,output_tokens INTEGER,total_tokens INTEGER,cache_read_tokens INTEGER,cache_write_tokens INTEGER,cost REAL,cost_source TEXT,is_compaction INTEGER);";
fn goose_message(
    db: &Connection,
    id: Option<&str>,
    role: &str,
    parts: Value,
    metadata: Value,
    time: i64,
) {
    db.execute("INSERT INTO messages(message_id,session_id,role,content_json,created_timestamp,metadata_json) VALUES(?1,'20260101_1',?2,?3,?4,?5)",params![id,role,parts.to_string(),time,metadata.to_string()]).unwrap();
}
fn crush_message(db: &Connection, id: &str, role: &str, parts: Value, time: i64) {
    db.execute(
        "INSERT INTO messages VALUES(?1,'s',?2,?3,?4,NULL,0)",
        params![id, role, parts.to_string(), time],
    )
    .unwrap();
}
fn stream_path(options: &Options) -> std::path::PathBuf {
    let v: Value =
        serde_json::from_slice(&std::fs::read(options.out.join("bundle.json")).unwrap()).unwrap();
    options.out.join(v["sessions"][0]["path"].as_str().unwrap())
}
#[test]
fn crush_snapshot_rewrite_truncation_and_file_versions() {
    let (_d, o, db) = database(Provider::Crush, CRUSH);
    crush_message(
        &db,
        "u",
        "user",
        json!([{"type":"text","data":{"text":"prompt"}}]),
        1000,
    );
    crush_message(
        &db,
        "a",
        "assistant",
        json!([{"type":"text","data":{"text":"answer"}},{"type":"finish","data":{"reason":"end_turn"}}]),
        2000,
    );
    db.execute_batch("INSERT INTO files VALUES('f0','s','/synthetic/a','before',0,1000),('f1','s','/synthetic/a','after',1,2000);").unwrap();
    export(&o).unwrap();
    let first = records(&o);
    let old_path = stream_path(&o);
    let old_bytes = std::fs::read(&old_path).unwrap();
    assert_eq!(export(&o).unwrap().unchanged, 1);
    assert_eq!(records(&o), first);
    let net = first.iter().find(|v| v["type"] == "file_change").unwrap();
    assert_eq!(net["basis"], "net");
    let checkpoint = first.iter().find(|v| v["type"] == "checkpoint").unwrap();
    assert!(checkpoint.get("files").is_none());
    assert_eq!(
        checkpoint["_meta"]["sh.charm.crush/file"]["path"]["path"],
        "/synthetic/a"
    );
    assert!(checkpoint["_meta"]["sh.charm.crush/file"]["blob"].is_object());
    assert_eq!(net["precision"], "exact");
    assert!(net.get("cause").is_none());
    assert_eq!(first.last().unwrap()["type"], "usage_summary");
    let id = first.iter().find(|v| v["messageId"] == "a").unwrap()["id"].clone();
    db.execute(
        "UPDATE messages SET parts=?1 WHERE id='a'",
        [json!([{"type":"text","data":{"text":"rewritten"}}]).to_string()],
    )
    .unwrap();
    assert_eq!(export(&o).unwrap().snapshots, 1);
    assert_eq!(std::fs::read(&old_path).unwrap(), old_bytes);
    let after = records(&o);
    assert_eq!(
        after.iter().find(|v| v["messageId"] == "a").unwrap()["id"],
        id
    );
    assert_ne!(after[0]["observedAt"], first[0]["observedAt"]);
    db.execute("DELETE FROM messages WHERE id='u'", []).unwrap();
    export(&o).unwrap();
    let after = records(&o);
    assert!(!after.iter().any(|v| v["messageId"] == "u"));
    assert!(!after.iter().any(|v| v["type"] == "tombstone"));
    assert!(after
        .iter()
        .filter(|v| v["type"] == "message")
        .all(|v| v.get("assembly").is_none()));
}
#[test]
fn crush_duplicate_calls_and_large_integer_arguments() {
    let (_d, o, db) = database(Provider::Crush, CRUSH);
    crush_message(
        &db,
        "a",
        "assistant",
        json!([{"type":"tool_call","data":{"id":"same","name":"mcp_a_b_c","input":"{\"n\":9007199254740993}"}},{"type":"tool_call","data":{"id":"same","name":"other","input":"{}"}}]),
        1000,
    );
    crush_message(
        &db,
        "r",
        "tool",
        json!([{"type":"tool_result","data":{"tool_call_id":"same","content":"body","is_error":false}}]),
        2000,
    );
    export(&o).unwrap();
    let rows = records(&o);
    let calls: Vec<_> = rows.iter().filter(|v| v["type"] == "tool_call").collect();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["inputText"], "{\"n\":9007199254740993}");
    assert!(calls[0].get("input").is_none());
    assert_eq!(calls[0]["provider"]["identity"], "unknown");
    let result = rows.iter().find(|v| v["type"] == "tool_result").unwrap();
    assert_eq!(result["linkage"], "ambiguous");
    assert!(result.get("callId").is_none());
    assert_eq!(result["nativeCallId"], "same");
}
#[test]
fn goose_reinserted_rows_keep_native_identity_and_native_display_order() {
    let (_d, o, db) = database(Provider::Goose, GOOSE);
    let text = json!([{"type":"text","text":"later"}]);
    goose_message(
        &db,
        Some("b"),
        "assistant",
        text.clone(),
        json!({"agentVisible":true,"userVisible":true}),
        20,
    );
    export(&o).unwrap();
    let before = records(&o);
    let b = before.iter().find(|v| v["messageId"] == "b").unwrap();
    db.execute("DELETE FROM messages", []).unwrap();
    goose_message(
        &db,
        Some("a"),
        "user",
        json!([{"type":"text","text":"earlier"}]),
        json!({}),
        10,
    );
    goose_message(
        &db,
        Some("b"),
        "assistant",
        text,
        json!({"agentVisible":true,"userVisible":true}),
        20,
    );
    assert_eq!(export(&o).unwrap().snapshots, 1);
    let after = records(&o);
    let later = after.iter().find(|v| v["messageId"] == "b").unwrap();
    assert_eq!(later["id"], b["id"]);
    assert_ne!(later["seq"], b["seq"]);
    assert_eq!(after[1]["messageId"], "a");
    assert_eq!(later["tsPrecision"], "s");
    assert_eq!(after[0]["namespace"], "authored-installation");
    db.execute("DELETE FROM messages WHERE created_timestamp>=20", [])
        .unwrap();
    export(&o).unwrap();
    assert!(!records(&o).iter().any(|v| v["messageId"] == "b"));
}
#[test]
fn goose_ledger_is_not_joined_by_time_and_settings_are_last() {
    let (_d, o, db) = database(Provider::Goose, GOOSE);
    goose_message(
        &db,
        Some("m"),
        "assistant",
        json!([{"type":"text","text":"answer"}]),
        json!({"inference":{"requestedModel":"requested","resolvedModel":"served"}}),
        20,
    );
    db.execute_batch("INSERT INTO usage_ledger VALUES(9,'20260101_1',1,'served',10,3,13,4,2,0.2,'estimated',1);UPDATE sessions SET goose_mode='auto',model_config_json='{\"model_name\":\"current-model\"}',accumulated_input_tokens=10,accumulated_output_tokens=3,accumulated_total_tokens=13;").unwrap();
    export(&o).unwrap();
    let rows = records(&o);
    let infer = rows.iter().find(|v| v["type"] == "inference").unwrap();
    assert_eq!(infer["purpose"], "compaction");
    assert_eq!(infer["cost"]["source"], "harness_estimated");
    assert_eq!(infer["usage"]["input"]["includesCache"], Value::Null);
    assert_eq!(infer["usage"]["input"]["cacheRead"], 4);
    assert!(rows[1].get("inferenceId").is_none());
    let config = rows
        .iter()
        .position(|v| v["type"] == "config_change")
        .unwrap();
    let usage = rows.iter().position(|v| v["type"] == "inference").unwrap();
    assert!(config > usage);
    assert_eq!(rows[config]["snapshot"], true);
}
#[test]
fn goose_secret_form_answer_is_absent_from_all_streams() {
    let (_d, o, db) = database(Provider::Goose, GOOSE);
    goose_message(
        &db,
        Some("q"),
        "assistant",
        json!([{"type":"actionRequired","data":{"actionType":"elicitation","id":"request","message":"Credential","requested_schema":{"type":"object","properties":{"token":{"type":"string","format":"password"}}}}}]),
        json!({}),
        1,
    );
    goose_message(
        &db,
        Some("r"),
        "user",
        json!([{"type":"actionRequired","data":{"actionType":"elicitationResponse","id":"request","user_data":{"token":"AUTHORED-SECRET-DO-NOT-EXPORT"},"action":"accept"}}]),
        json!({}),
        2,
    );
    export(&o).unwrap();
    let body = std::fs::read_to_string(stream_path(&o)).unwrap();
    assert!(!body.contains("AUTHORED-SECRET-DO-NOT-EXPORT"));
    assert!(records(&o).iter().any(|v| v["type"] == "input_request"
        && v["requestedSchema"]["properties"]["token"]["format"] == "password"));
}
#[test]
fn goose_missing_namespace_and_colliding_databases_do_not_merge() {
    let (_d, mut o, _db) = database(Provider::Goose, GOOSE);
    o.namespace = None;
    assert!(export(&o).unwrap_err().to_string().contains("namespace"));
    o.namespace = Some("installation".into());
    let second = o.inputs[0].path.with_file_name("other.db");
    std::fs::copy(&o.inputs[0].path, &second).unwrap();
    o.inputs.push(Input {
        provider: Provider::Goose,
        path: second,
    });
    assert!(export(&o).unwrap_err().to_string().contains("colliding"));
}
#[test]
fn lingma_summaries_are_partial_and_duplicate_request_ids_never_merge() {
    let (_d,o,db)=database(Provider::Lingma,"CREATE TABLE chat_record(session_id TEXT,request_id TEXT,chat_prompt TEXT,summary TEXT,error_result TEXT,gmt_create INTEGER,extra TEXT)");
    db.execute_batch("INSERT INTO chat_record VALUES('s','r','prompt','answer summary',NULL,1700000000000,'{\"context\":1}'),('s','r','other','another summary','{}',1700000001000,NULL)").unwrap();
    export(&o).unwrap();
    let rows = records(&o);
    let messages: Vec<_> = rows.iter().filter(|v| v["type"] == "message").collect();
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[0]["ts"], "2023-11-14T22:13:20.000Z");
    assert_eq!(messages[0]["tsPrecision"], "ms");
    assert_ne!(messages[0]["messageId"], messages[2]["messageId"]);
    assert!(messages.iter().all(|v| v["idScope"] == "capture"));
    let summary = messages[1];
    assert_eq!(summary["capture"]["content"]["state"], "summarized");
    assert!(summary.get("ts").is_none());
    assert_eq!(rows[0]["agent"]["name"], "com.aliyun.lingma");
}
#[test]
fn sqlite_bounds_wal_and_multiple_sessions() {
    let (_d, mut o, db) = database(Provider::Crush, CRUSH);
    db.execute_batch("PRAGMA journal_mode=WAL;INSERT INTO sessions(id,title)VALUES('s2','other')")
        .unwrap();
    crush_message(
        &db,
        "u",
        "user",
        json!([{"type":"text","data":{"text":"x".repeat(2000)}}]),
        1000,
    );
    o.max_record_bytes = 1024;
    let summary = export(&o).unwrap();
    assert_eq!(summary.sessions, 2);
    assert_eq!(summary.gaps, 1);
    assert_eq!(export(&o).unwrap().unchanged, 2);
    db.execute("UPDATE messages SET parts='[]' WHERE id='u'", [])
        .unwrap();
    assert_eq!(export(&o).unwrap().gaps, 0);
}
#[test]
fn sqlite_relocation_preserves_native_record_ids() {
    let (_d, mut o, db) = database(Provider::Crush, CRUSH);
    crush_message(
        &db,
        "u",
        "user",
        json!([{"type":"text","data":{"text":"hello"}}]),
        1000,
    );
    export(&o).unwrap();
    let id = records(&o)[1]["id"].clone();
    drop(db);
    let moved = o.inputs[0].path.with_file_name("moved.db");
    std::fs::rename(&o.inputs[0].path, &moved).unwrap();
    o.inputs[0].path = moved;
    export(&o).unwrap();
    assert_eq!(records(&o)[1]["id"], id);
}

mod edges;
