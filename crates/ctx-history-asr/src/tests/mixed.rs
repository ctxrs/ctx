//! Authored database cases. Native writer qualification is documented separately.
use super::*;
use rusqlite::{params, Connection};
use std::fs;
fn openclaw(rows: &[Value]) -> (tempfile::TempDir, Options, Connection) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agents/main/agent/openclaw-agent.sqlite");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let db = Connection::open(&path).unwrap();
    db.execute_batch("PRAGMA user_version=37; CREATE TABLE session_windows(session_id TEXT PRIMARY KEY,session_key TEXT,previous_session_id TEXT,reason TEXT,started_at INTEGER,agent_harness_id TEXT,parent_session_key TEXT,spawned_by TEXT); INSERT INTO session_windows VALUES('window','lane',NULL,'initial',0,'openclaw',NULL,NULL); CREATE TABLE transcript_events(session_id TEXT,seq INTEGER,event_json TEXT,event_zstd BLOB,event_utf8_bytes INTEGER,PRIMARY KEY(session_id,seq));").unwrap();
    for (i, row) in rows.iter().enumerate() {
        db.execute(
            "INSERT INTO transcript_events VALUES('window',?1,?2,NULL,NULL)",
            params![i, row.to_string()],
        )
        .unwrap();
    }
    let options = Options::new(
        vec![Input {
            provider: Provider::OpenClaw,
            path,
        }],
        dir.path().join("bundle"),
    );
    (dir, options, db)
}
fn event(id: &str, parent: Value, message: Value) -> Value {
    json!({"type":"message","id":id,"parentId":parent,"timestamp":"2026-10-01T00:00:00.123456789Z","message":message})
}
fn assistant(id: &str, content: Value) -> Value {
    json!({"role":"assistant","provider":"openai","model":"requested","responseModel":"served","responseId":id,"content":content,"__openclaw":{"runId":"run"},"usage":{"input":1,"output":2,"cacheRead":0,"cacheWrite":0}})
}
#[test]
fn openclaw_current_schema_usage_secrets_and_ambiguous_calls() {
    let (_d, o, _db) = openclaw(&[
        json!({"type":"session","id":"window","cwd":"/synthetic"}),
        event("u", Value::Null, json!({"role":"user","content":"UI only"})),
        event(
            "a",
            json!("u"),
            assistant(
                "response",
                json!([{"type":"toolCall","id":"dup","name":"exec","arguments":{"integer":9007199254740993u64}}]),
            ),
        ),
        event(
            "b",
            json!("a"),
            assistant(
                "response2",
                json!([{"type":"toolCall","id":"dup","name":"exec","arguments":{}}]),
            ),
        ),
        event(
            "result",
            json!("b"),
            json!({"role":"toolResult","toolCallId":"dup","content":[{"type":"text","text":"out"}],"isError":false,"details":{"exitSignal":15,"exitCode":null}}),
        ),
        event(
            "secret",
            json!("result"),
            assistant(
                "response3",
                json!([{"type":"toolCall","id":"secret-call","name":"secrets","arguments":{"value":"SYNTHETIC_SECRET_VALUE"}}]),
            ),
        ),
        event(
            "secret-result",
            json!("secret"),
            json!({"role":"toolResult","toolCallId":"secret-call","content":"SYNTHETIC_SECRET_VALUE","details":{"value":"SYNTHETIC_SECRET_VALUE","status":"answered"}}),
        ),
    ]);
    export(&o).unwrap();
    let rs = records(&o);
    assert_eq!(rs[0]["producer"]["source"]["formatVersion"], "37");
    assert!(rs[0]["producer"]["instance"].is_string());
    let user = rs.iter().find(|r| r["messageId"] == "u").unwrap();
    assert_eq!(user["content"], json!([]));
    assert_eq!(user["displayContent"][0]["text"], "UI only");
    assert!(user.get("author").is_none());
    let call = rs
        .iter()
        .find(|r| r["type"] == "tool_call" && r["callId"] == "dup")
        .unwrap();
    assert!(call.get("input").is_none());
    assert!(call["inputText"]
        .as_str()
        .unwrap()
        .contains("9007199254740993"));
    let result = rs.iter().find(|r| r["linkage"] == "ambiguous").unwrap();
    assert!(result.get("callId").is_none());
    assert_eq!(result["raw"]["exitSignal"], 15);
    assert!(result["exec"].get("signal").is_none());
    assert!(!serde_json::to_string(&rs)
        .unwrap()
        .contains("SYNTHETIC_SECRET_VALUE"));
    let inf = rs
        .iter()
        .find(|r| r["inferenceId"] == "response" && r["type"] == "inference")
        .unwrap();
    assert_eq!(inf["model"]["id"], "served");
    assert_eq!(inf["usage"]["input"]["cacheRead"], 0);
}
#[test]
fn openclaw_capture_tail_rewrite_compression_and_noop() {
    let (_d, o, db) = openclaw(&[
        json!({"type":"session","id":"window"}),
        event(
            "u",
            Value::Null,
            json!({"role":"user","synthetic":true,"content":"before"}),
        ),
    ]);
    let bytes = std::fs::read(&o.inputs[0].path).unwrap();
    export(&o).unwrap();
    assert_eq!(std::fs::read(&o.inputs[0].path).unwrap(), bytes);
    let first = records(&o);
    let manifest = std::fs::read(o.out.join("bundle.json")).unwrap();
    assert_eq!(export(&o).unwrap().unchanged, 1);
    assert_eq!(std::fs::read(o.out.join("bundle.json")).unwrap(), manifest);
    let e = event(
        "a",
        json!("u"),
        assistant("response", json!([{"type":"text","text":"later"}])),
    );
    let raw = e.to_string();
    let zipped = zstd::stream::encode_all(raw.as_bytes(), 1).unwrap();
    db.execute(
        "INSERT INTO transcript_events VALUES('window',2,NULL,?1,?2)",
        params![zipped, raw.len()],
    )
    .unwrap();
    assert_eq!(export(&o).unwrap().snapshots, 1);
    let next = records(&o);
    assert_eq!(
        first.iter().find(|r| r["messageId"] == "u").unwrap()["id"],
        next.iter().find(|r| r["messageId"] == "u").unwrap()["id"]
    );
    assert!(
        next.iter().position(|r| r["messageId"] == "a").unwrap()
            < next
                .iter()
                .position(|r| r["assembly"]["mode"] == "snapshot")
                .unwrap()
    );
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(o.out.join("bundle.json")).unwrap()).unwrap();
    assert!(manifest["sessions"][0]["observedAt"].is_string());
    let mut rewrite = event(
        "u-edit",
        Value::Null,
        json!({"role":"user","synthetic":true,"content":"after"}),
    );
    rewrite["supersedesEntryId"] = json!("u");
    db.execute(
        "INSERT INTO transcript_events VALUES('window',3,?1,NULL,NULL)",
        [rewrite.to_string()],
    )
    .unwrap();
    export(&o).unwrap();
    let rs = records(&o);
    let chain: Vec<_> = rs.iter().filter(|r| r["messageId"] == "u").collect();
    assert_eq!(chain.len(), 2);
    assert_eq!(chain[1]["supersedes"], chain[0]["id"]);
    assert_eq!(chain[1]["assembly"]["mode"], "rewrite");
}
#[test]
fn openclaw_lane_facts_are_not_assigned_by_time_and_survive_repeat() {
    let (_d, o, db) = openclaw(&[]);
    db.execute_batch("CREATE TABLE conversation_deliveries(operation_id TEXT,source_session_key TEXT,status TEXT,queue_id TEXT); INSERT INTO conversation_deliveries VALUES('delivery','lane','sent',NULL);").unwrap();
    export(&o).unwrap();
    let path = o.out.join("bundle.json");
    let before = std::fs::read(&path).unwrap();
    let manifest: Value = serde_json::from_slice(&before).unwrap();
    assert_eq!(
        manifest["_meta"]["ai.openclaw/unassignedLaneFacts"][0]["record"]["operation_id"],
        "delivery"
    );
    assert!(!records(&o).iter().any(|r| r["type"] == "delivery"));
    export(&o).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
#[test]
fn nanoclaw_separate_stores_display_and_delivery() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v2.db");
    let db = Connection::open(&path).unwrap();
    db.execute_batch("CREATE TABLE sessions(id TEXT,agent_group_id TEXT,created_at TEXT); INSERT INTO sessions VALUES('session','group','2026-10-01T00:00:00Z');").unwrap();
    let tree = dir.path().join("v2-sessions/group/session");
    std::fs::create_dir_all(&tree).unwrap();
    let inbound = Connection::open(tree.join("inbound.db")).unwrap();
    inbound.execute_batch("CREATE TABLE messages_in(id TEXT,seq INTEGER,kind TEXT,timestamp TEXT,content TEXT); CREATE TABLE delivered(message_out_id TEXT,status TEXT,platform_message_id TEXT,delivered_at TEXT);").unwrap();
    inbound
        .execute(
            "INSERT INTO messages_in VALUES('in',2,'chat','2026-10-01T00:02:00Z',?1)",
            [json!({"text":"display only","senderId":"sender"}).to_string()],
        )
        .unwrap();
    let outbound = Connection::open(tree.join("outbound.db")).unwrap();
    outbound.execute_batch("CREATE TABLE messages_out(id TEXT,seq INTEGER,kind TEXT,timestamp TEXT,content TEXT); CREATE TABLE session_state(key TEXT,value TEXT,updated_at TEXT); INSERT INTO session_state VALUES('continuation:claude','native-session','2026-10-01T00:00:00Z');").unwrap();
    outbound
        .execute(
            "INSERT INTO messages_out VALUES('out',1,'chat','2026-10-01T00:01:00Z',?1)",
            [json!({"text":"sent"}).to_string()],
        )
        .unwrap();
    inbound
        .execute_batch(
            "INSERT INTO delivered VALUES('out','delivered','platform','2026-10-01T00:03:00Z')",
        )
        .unwrap();
    let o = Options::new(
        vec![Input {
            provider: Provider::NanoClaw,
            path,
        }],
        dir.path().join("bundle"),
    );
    export(&o).unwrap();
    let rs = records(&o);
    let msgs: Vec<_> = rs.iter().filter(|r| r["type"] == "message").collect();
    assert_eq!(msgs[0]["messageId"], "in");
    assert_eq!(msgs[1]["messageId"], "out");
    assert_eq!(msgs[0]["content"], json!([]));
    assert_eq!(msgs[0]["assembly"]["mode"], "snapshot");
    assert_eq!(
        rs[0]["lineage"]["executedBy"][0]["session"],
        "claude-code:native-session"
    );
    let delivery = rs.iter().find(|r| r["type"] == "delivery").unwrap();
    assert_eq!(delivery["message"], msgs[1]["id"]);
    assert_eq!(delivery["status"], "sent");
    assert_eq!(export(&o).unwrap().unchanged, 1);
}

#[test]
fn openclaw_snapshot_observation_and_projection_ids_survive_new_settings() {
    let (_d, o, db) = openclaw(&[]);
    db.execute_batch("CREATE TABLE session_nodes(session_key TEXT PRIMARY KEY,entry_json TEXT); INSERT INTO session_nodes VALUES('lane','{\"sessionId\":\"window\",\"label\":\"One\",\"credential\":\"PRIVATE_NODE_VALUE\"}');").unwrap();
    export(&o).unwrap();
    let rs = records(&o);
    let id = rs.iter().find(|r| r["title"] == "One").unwrap()["id"].clone();
    assert!(!serde_json::to_string(&rs)
        .unwrap()
        .contains("PRIVATE_NODE_VALUE"));
    db.execute("UPDATE session_nodes SET entry_json=?1",[json!({"sessionId":"window","label":"Two","permissionMode":"default","goal":{"id":"g","objective":"Test","status":"active","tokenBudget":100,"tokensUsed":0}}).to_string()]).unwrap();
    assert_eq!(export(&o).unwrap().snapshots, 1);
    let rs = records(&o);
    assert_eq!(rs.iter().find(|r| r["title"] == "Two").unwrap()["id"], id);
    assert!(rs[0]["observedAt"].is_string());
    let before = fs::read(o.out.join("bundle.json")).unwrap();
    assert_eq!(export(&o).unwrap().unchanged, 1);
    assert_eq!(fs::read(o.out.join("bundle.json")).unwrap(), before);
    assert!(rs
        .iter()
        .any(|r| r["type"] == "plan" && r["planId"] == "g" && r["assembly"]["mode"] == "snapshot"));
}
#[test]
fn openclaw_old_schemas_and_duplicate_identity_rejection() {
    for version in [17, 19] {
        let (_d, o, db) = openclaw(&[event(
            "a",
            Value::Null,
            json!({"role":"user","content":"old"}),
        )]);
        db.execute_batch(&format!("PRAGMA user_version={version}; ALTER TABLE transcript_events DROP COLUMN event_zstd; ALTER TABLE transcript_events DROP COLUMN event_utf8_bytes;")).unwrap();
        export(&o).unwrap();
        assert_eq!(
            records(&o)[0]["producer"]["source"]["formatVersion"],
            version.to_string()
        );
    }
    let (_d, o, _db) = openclaw(&[
        event("same", Value::Null, json!({"role":"user","content":"a"})),
        event("same", Value::Null, json!({"role":"user","content":"b"})),
    ]);
    assert!(export(&o).unwrap_err().to_string().contains("duplicate"));
}
#[test]
fn openclaw_wal_relocation_and_large_binary_capture() {
    let image = "A".repeat(100000);
    let (_d, mut o, db) = openclaw(&[event(
        "a",
        Value::Null,
        assistant(
            "response",
            json!([{"type":"image","mimeType":"image/png","data":image}]),
        ),
    )]);
    db.execute_batch("PRAGMA journal_mode=WAL; INSERT INTO transcript_events VALUES('window',2,'{\"type\":\"label\",\"id\":\"label\",\"targetId\":\"a\",\"label\":\"native WAL\"}',NULL,NULL);").unwrap();
    let before = fs::read(&o.inputs[0].path).unwrap();
    let wal = fs::read(o.inputs[0].path.with_extension("sqlite-wal")).unwrap();
    export(&o).unwrap();
    assert_eq!(fs::read(&o.inputs[0].path).unwrap(), before);
    assert_eq!(
        fs::read(o.inputs[0].path.with_extension("sqlite-wal")).unwrap(),
        wal
    );
    let rs = records(&o);
    let msg = rs.iter().find(|r| r["type"] == "message").unwrap();
    assert_eq!(msg["capture"]["content"]["state"], "external");
    assert_eq!(
        msg["capture"]["content"]["blob"]["mimeType"],
        "application/json"
    );
    assert!(rs.iter().any(|r| r["type"] == "ai.openclaw/label"));
    drop(db);
    let old = o.inputs[0].path.clone();
    let relocated = old.with_file_name("relocated.sqlite");
    fs::copy(old, &relocated).unwrap();
    o.inputs[0].path = relocated;
    assert_eq!(export(&o).unwrap().unchanged, 1);
}
#[test]
fn native_number_parser_never_rounds_large_integer_literals() {
    assert!(crate::native::parse_json(
        r#"{"ok":true,"no":false,"none":null,"a":18446744073709551615}"#
    )
    .is_some());
    assert!(crate::native::parse_json(r#"{"a":18446744073709551616}"#).is_none());
    assert!(crate::native::parse_json(r#"{"a":1,"a":2}"#).is_none());
}
#[test]
fn nanoclaw_missing_store_is_a_gap_and_secret_answer_is_redacted() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("v2.db");
    let db = Connection::open(&path).unwrap();
    db.execute_batch(
        "CREATE TABLE sessions(id TEXT,agent_group_id TEXT); INSERT INTO sessions VALUES('s','g');",
    )
    .unwrap();
    let o = Options::new(
        vec![Input {
            provider: Provider::NanoClaw,
            path,
        }],
        d.path().join("out"),
    );
    export(&o).unwrap();
    assert_eq!(records(&o).iter().filter(|r| r["type"] == "gap").count(), 2);
    let tree = d.path().join("v2-sessions/g/s");
    fs::create_dir_all(&tree).unwrap();
    for (file, table, content) in [
        (
            "inbound.db",
            "messages_in",
            json!({"questionId":"q","text":"SYNTHETIC_PRIVATE_ANSWER"}),
        ),
        (
            "outbound.db",
            "messages_out",
            json!({"type":"ask_question","questionId":"q","question":"Password","secret":true}),
        ),
    ] {
        let db = Connection::open(tree.join(file)).unwrap();
        db.execute_batch(&format!(
            "CREATE TABLE {table}(id TEXT PRIMARY KEY,seq INTEGER,kind TEXT,content TEXT);"
        ))
        .unwrap();
        db.execute(
            &format!("INSERT INTO {table} VALUES('id',1,'chat-sdk',?1)"),
            [content.to_string()],
        )
        .unwrap();
    }
    export(&o).unwrap();
    let rs = records(&o);
    assert!(!serde_json::to_string(&rs)
        .unwrap()
        .contains("SYNTHETIC_PRIVATE_ANSWER"));
    assert!(rs
        .iter()
        .any(|r| r["type"] == "input_request" && r["capture"]["answers"]["state"] == "redacted"));
}

#[test]
fn openclaw_malformed_positions_limits_and_compressed_runtime_are_explicit() {
    let (_d, mut o, db) = openclaw(&[]);
    db.execute_batch("INSERT INTO transcript_events VALUES('window',1,'{bad',NULL,NULL); CREATE TABLE trajectory_runtime_events(session_id TEXT,seq INTEGER,event_json TEXT,event_payload BLOB,event_codec TEXT,PRIMARY KEY(session_id,seq)); INSERT INTO trajectory_runtime_events VALUES('window',1,'',x'0001','br');").unwrap();
    let summary = export(&o).unwrap();
    assert_eq!(summary.gaps, 2);
    assert!(records(&o)[0]["producer"]["lossy"]
        .as_array()
        .unwrap()
        .contains(&json!("_ai.openclaw/compressed_runtime")));
    db.execute_batch("CREATE TABLE session_nodes(session_key TEXT,entry_json TEXT);")
        .unwrap();
    db.execute(
        "INSERT INTO session_nodes VALUES('lane',?1)",
        [json!({"sessionId":"window","label":"x".repeat(2048)}).to_string()],
    )
    .unwrap();
    o.max_record_bytes = 1024;
    assert!(export(&o)
        .unwrap_err()
        .to_string()
        .contains("max-record-bytes"));
}

#[test]
fn openclaw_state_discovery_ignores_export_trees_and_prefers_agent_database() {
    let (dir, mut o, _db) = openclaw(&[event(
        "e",
        Value::Null,
        json!({"role":"user","content":"native"}),
    )]);
    fs::create_dir_all(dir.path().join("exports")).unwrap();
    fs::write(
        dir.path().join("exports/export.jsonl"),
        "{\"type\":\"session\",\"id\":\"export-header\",\"sessionId\":\"not-native\"}\n",
    )
    .unwrap();
    fs::create_dir_all(dir.path().join("agents/main/sessions")).unwrap();
    fs::write(
        dir.path().join("agents/main/sessions/old.jsonl"),
        "{\"type\":\"session\",\"id\":\"old-shadow\"}\n",
    )
    .unwrap();
    o.inputs[0].path = dir.path().to_owned();
    assert_eq!(export(&o).unwrap().sessions, 1);
    assert_eq!(records(&o)[0]["sessionId"], "window");
}
