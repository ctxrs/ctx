//! Authored semantic cases. No fixture here is a captured provider transcript.
use crate::{export, Input, Options, Provider};
use rusqlite::Connection;
use serde_json::{json, Value};
fn fixture(provider: Provider, schema: &str) -> (tempfile::TempDir, Connection, Options) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("native.db");
    let db = Connection::open(&path).unwrap();
    db.execute_batch(schema).unwrap();
    let mut options = Options::new(vec![Input { provider, path }], dir.path().join("out"));
    if provider == Provider::Hermes {
        options.namespace = Some("synthetic-installation".into());
    }
    (dir, db, options)
}
fn records(o: &Options) -> Vec<Value> {
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(o.out.join("bundle.json")).unwrap()).unwrap();
    std::fs::read_to_string(
        o.out
            .join(manifest["sessions"][0]["path"].as_str().unwrap()),
    )
    .unwrap()
    .lines()
    .map(|l| serde_json::from_str(l).unwrap())
    .collect()
}
#[test]
fn hermes_relations_api_content_and_ambiguous_calls() {
    let (_dir,db,o)=fixture(Provider::Hermes,"CREATE TABLE sessions(id TEXT PRIMARY KEY,source TEXT,started_at REAL,parent_session_id TEXT,model_config TEXT,session_key TEXT,chat_id TEXT,chat_type TEXT,user_id TEXT); CREATE TABLE messages(id INTEGER PRIMARY KEY,session_id TEXT,role TEXT,content TEXT,api_content TEXT,tool_calls TEXT,tool_call_id TEXT,observed INTEGER,active INTEGER,timestamp REAL); INSERT INTO sessions VALUES('child','telegram',1.25,'parent','{\"_branched_from\":\"parent\"}','lane','chat','group','creator');");
    db.execute(
        "INSERT INTO messages VALUES(1,'child','user','display','wire',NULL,NULL,1,1,2.25)",
        [],
    )
    .unwrap();
    let call=json!([{"id":"duplicate","function":{"name":"read","arguments":"{\"n\":9007199254740993}"}}]).to_string();
    for id in [2, 3] {
        db.execute(
            "INSERT INTO messages VALUES(?1,'child','assistant','',NULL,?2,NULL,0,1,3.0)",
            rusqlite::params![id, call],
        )
        .unwrap();
    }
    db.execute(
        "INSERT INTO messages VALUES(4,'child','tool','result',NULL,NULL,'duplicate',0,1,4.0)",
        [],
    )
    .unwrap();
    export(&o).unwrap();
    let r = records(&o);
    assert!(r[0]["lineage"]["forkedFrom"]["session"]
        .as_str()
        .unwrap()
        .ends_with(":parent"));
    assert_eq!(r[0]["participants"][0]["ids"][0]["value"], "creator");
    let m = r.iter().find(|r| r["messageId"] == "1").unwrap();
    assert_eq!(m["content"][0]["text"], "wire");
    assert_eq!(m["displayContent"][0]["text"], "display");
    assert_eq!(m["addressed"], false);
    assert!(m.get("from").is_none());
    let c = r.iter().find(|r| r["type"] == "tool_call").unwrap();
    assert!(c.get("input").is_none());
    assert!(c["inputText"]
        .as_str()
        .unwrap()
        .contains("9007199254740993"));
    let result = r.iter().find(|r| r["type"] == "tool_result").unwrap();
    assert_eq!(result["linkage"], "ambiguous");
    assert!(result.get("callId").is_none());
    assert_eq!(export(&o).unwrap().unchanged, 1);
}
#[test]
fn astrbot_public_uuid_snapshot_and_context_usage() {
    let (_dir,db,o)=fixture(Provider::AstrBot,"CREATE TABLE conversations(inner_conversation_id INTEGER PRIMARY KEY,conversation_id TEXT UNIQUE,content TEXT,title TEXT,token_usage INTEGER); INSERT INTO conversations VALUES(42,'public-uuid','[]','Title',18);");
    db.execute("UPDATE conversations SET content=?1",[json!([{"role":"user","content":"question"},{"role":"_checkpoint","content":{"id":"checkpoint"}},{"role":"assistant","content":"answer"}]).to_string()]).unwrap();
    export(&o).unwrap();
    let before = records(&o);
    assert_eq!(before[0]["sessionId"], "public-uuid");
    assert!(before
        .iter()
        .filter(|r| r["type"] == "message")
        .all(|r| r.get("assembly").is_none()));
    assert!(before
        .iter()
        .filter(|r| r["type"] == "session_info")
        .all(|r| r["assembly"]["mode"] == "snapshot"));
    assert!(before.iter().any(|v| v["contextWindow"]["used"] == 18));
    assert!(!before
        .iter()
        .any(|v| v["type"] == "message" && v["role"] == "_checkpoint"));
    assert!(before
        .iter()
        .filter(|v| v["type"] == "message")
        .all(|v| v["idScope"] == "capture"));
    let manifest = std::fs::read(o.out.join("bundle.json")).unwrap();
    assert_eq!(export(&o).unwrap().unchanged, 1);
    assert_eq!(manifest, std::fs::read(o.out.join("bundle.json")).unwrap());
    db.execute(
        "UPDATE conversations SET content='[{\"role\":\"user\",\"content\":\"edited\"}]'",
        [],
    )
    .unwrap();
    assert_eq!(export(&o).unwrap().snapshots, 1);
    let after = records(&o);
    assert_eq!(before[1]["id"], after[1]["id"]);
    assert_eq!(after[1]["idScope"], "capture");
    assert_ne!(before[0]["observedAt"], after[0]["observedAt"]);
}
#[test]
fn shelley_go_content_types_results_and_usage() {
    let (_dir,db,o)=fixture(Provider::Shelley,"CREATE TABLE conversations(conversation_id TEXT PRIMARY KEY,slug TEXT,parent_conversation_id TEXT);CREATE TABLE messages(message_id TEXT PRIMARY KEY,conversation_id TEXT,sequence_id INTEGER,type TEXT,llm_data TEXT,user_data TEXT,usage_data TEXT,forked_from_message_id TEXT);INSERT INTO conversations VALUES('s','Example',NULL);");
    let body = json!({"Role":1,"Content":[{"Type":2,"Text":"answer"},{"Type":5,"ID":"call","ToolName":"read","ToolInput":{"path":"test.txt"}},{"Type":6,"ToolUseID":"call","ToolError":false,"ToolResult":[{"Type":2,"Text":"ok"}]}]});
    db.execute(
        "INSERT INTO messages VALUES('m','s',1,'agent',?1,NULL,?2,NULL)",
        rusqlite::params![
            body.to_string(),
            json!({"input_tokens":10,"cache_read_input_tokens":3,"output_tokens":2,"cost_usd":0.0})
                .to_string()
        ],
    )
    .unwrap();
    export(&o).unwrap();
    let r = records(&o);
    assert!(r
        .iter()
        .any(|v| v["type"] == "tool_call" && v["name"] == "read"));
    let result = r.iter().find(|v| v["type"] == "tool_result").unwrap();
    assert_eq!(result["status"], "completed");
    assert_eq!(result["content"][0]["text"], "ok");
    let u = r.iter().find(|v| v["type"] == "inference").unwrap();
    assert_eq!(u["usage"]["input"]["uncached"], 10);
    assert_eq!(u["cost"]["amount"], 0.0);
}
#[test]
fn zed_compressed_capture_stable_user_id_and_compaction() {
    let (_dir,db,o)=fixture(Provider::Zed,"CREATE TABLE threads(id TEXT PRIMARY KEY,summary TEXT,data_type TEXT,data BLOB);INSERT INTO threads VALUES('thread','Title','zstd',X'');");
    let mut data = json!({"version":"0.3.0","title":"Title","messages":[{"User":{"id":"user-id","content":[{"Text":"question"}]}},{"Agent":{"content":[{"Text":"answer"}],"tool_results":{}}},{"Compaction":{"Summary":"summary"}}],"cumulative_token_usage":{"input_tokens":8,"output_tokens":3},"subagent_context":{"parent_thread_id":"parent","depth":1}});
    let save = |v: &Value| {
        let bytes = zstd::encode_all(v.to_string().as_bytes(), 3).unwrap();
        db.execute("UPDATE threads SET data=?1", [bytes]).unwrap();
    };
    save(&data);
    export(&o).unwrap();
    let before = records(&o);
    assert_eq!(before[1]["messageId"], "user-id");
    assert!(before[1].get("idScope").is_none());
    assert!(before.iter().any(|v| v["type"] == "compaction"));
    data["messages"][1]["Agent"]["content"][0]["Text"] = json!("revised");
    save(&data);
    assert_eq!(export(&o).unwrap().snapshots, 1);
    let after = records(&o);
    assert_eq!(before[1]["id"], after[1]["id"]);
    assert_eq!(before[2]["id"], after[2]["id"]);
    assert_eq!(after[2]["idScope"], "capture");
}

#[test]
fn wal_changes_and_relocation_keep_native_identity() {
    let (dir,db,mut o)=fixture(Provider::AstrBot,"PRAGMA journal_mode=WAL;CREATE TABLE conversations(conversation_id TEXT PRIMARY KEY,content TEXT);INSERT INTO conversations VALUES('s','[{\"id\":\"m\",\"role\":\"user\",\"content\":\"before\"}]');PRAGMA wal_checkpoint(TRUNCATE);");
    export(&o).unwrap();
    let before = records(&o);
    let db_size = std::fs::metadata(&o.inputs[0].path).unwrap().len();
    db.execute("UPDATE conversations SET content='[{\"id\":\"m\",\"role\":\"user\",\"content\":\"after\"}]'",[]).unwrap();
    assert_eq!(std::fs::metadata(&o.inputs[0].path).unwrap().len(), db_size);
    assert_eq!(export(&o).unwrap().snapshots, 1);
    let after = records(&o);
    assert_eq!(before[1]["id"], after[1]["id"]);
    assert_eq!(after[1]["content"][0]["text"], "after");
    drop(db);
    let moved = dir.path().join("moved.db");
    std::fs::rename(&o.inputs[0].path, &moved).unwrap();
    o.inputs[0].path = moved;
    export(&o).unwrap();
    assert_eq!(records(&o)[1]["id"], after[1]["id"]);
}

#[test]
fn zed_native_calls_survive_positional_message_moves_and_duplicate_user_ids_do_not_merge() {
    let (_dir,db,o)=fixture(Provider::Zed,"CREATE TABLE threads(id TEXT PRIMARY KEY,data_type TEXT,data BLOB);INSERT INTO threads VALUES('s','json','');");
    let mut data = json!({"version":"0.3.0","messages":[{"User":{"id":"duplicate","content":[{"Text":"first"}]}},{"User":{"id":"duplicate","content":[{"Text":"second"}]}},{"Agent":{"content":[{"ToolUse":{"id":"c","name":"read","input":{"type":"json","value":{"path":"file"}}}}],"tool_results":{"c":{"content":[{"Text":"result"}],"is_error":false}}}}]});
    let save = |v: &Value| {
        db.execute("UPDATE threads SET data=?1", [v.to_string()])
            .unwrap();
    };
    save(&data);
    export(&o).unwrap();
    let before = records(&o);
    let ids: std::collections::BTreeSet<_> =
        before.iter().filter_map(|r| r["id"].as_str()).collect();
    assert_eq!(ids.len(), before.len());
    data["messages"]
        .as_array_mut()
        .unwrap()
        .insert(0, json!({"Agent":{"content":[{"Text":"inserted"}]}}));
    save(&data);
    export(&o).unwrap();
    let after = records(&o);
    for kind in ["tool_call", "tool_result"] {
        let b = before.iter().find(|r| r["type"] == kind).unwrap();
        let a = after.iter().find(|r| r["type"] == kind).unwrap();
        assert_eq!(a["id"], b["id"]);
        assert!(a.get("idScope").is_none());
    }
}

#[test]
fn astrbot_duplicate_ids_and_unassigned_lane_facts() {
    let (_dir,db,mut o)=fixture(Provider::AstrBot,"CREATE TABLE conversations(conversation_id TEXT PRIMARY KEY,content TEXT);INSERT INTO conversations VALUES('s','[]');CREATE TABLE platform_message_history(id INTEGER PRIMARY KEY,llm_checkpoint_id TEXT,content TEXT);INSERT INTO platform_message_history VALUES(1,'copied-checkpoint','lane-only');");
    db.execute("UPDATE conversations SET content=?1",[json!([{"id":"duplicate","role":"user","content":"one"},{"id":"duplicate","role":"user","content":"two"}]).to_string()]).unwrap();
    export(&o).unwrap();
    let r = records(&o);
    assert_ne!(r[1]["id"], r[2]["id"]);
    assert_eq!(r[1]["idScope"], "capture");
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(o.out.join("bundle.json")).unwrap()).unwrap();
    assert!(manifest["_meta"]["app.astrbot/unassigned"][0]["record"]
        .to_string()
        .contains("lane-only"));
    o.session = Some("s".into());
    export(&o).unwrap();
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(o.out.join("bundle.json")).unwrap()).unwrap();
    assert_eq!(
        manifest["_meta"]["app.astrbot/unassigned"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn malformed_oversized_and_unsafe_snapshot_values_are_retained_or_declared() {
    let (_dir,db,mut o)=fixture(Provider::AstrBot,"CREATE TABLE conversations(conversation_id TEXT PRIMARY KEY,content TEXT);INSERT INTO conversations VALUES('s','not-json');");
    export(&o).unwrap();
    assert!(records(&o)
        .iter()
        .any(|r| r["type"] == "gap" && r["reason"] == "unparseable"));
    db.execute(
        "UPDATE conversations SET content='[{\"role\":\"user\",\"extra\":9007199254740993}]'",
        [],
    )
    .unwrap();
    export(&o).unwrap();
    let r = records(&o);
    assert!(r.iter().any(|r| r.to_string().contains("9007199254740993")));
    assert!(!r.iter().any(|r| r.to_string().contains("9007199254740992")));
    db.execute("UPDATE conversations SET content=?1", ["x".repeat(4096)])
        .unwrap();
    o.max_record_bytes = 1024;
    export(&o).unwrap();
    assert!(records(&o)
        .iter()
        .any(|r| r["type"] == "gap" && r["reason"] == "conversion_loss"));
}

#[test]
fn binary_blobs_and_secret_answers_are_handled_before_native_metadata() {
    use base64::Engine;
    let (_dir,db,o)=fixture(Provider::AstrBot,"CREATE TABLE conversations(conversation_id TEXT PRIMARY KEY,content TEXT);INSERT INTO conversations VALUES('s','[]');");
    let bytes = vec![42; 70_000];
    let image = base64::engine::general_purpose::STANDARD.encode(&bytes);
    db.execute("UPDATE conversations SET content=?1",[json!([
        {"role":"assistant","content":"","tool_calls":[{"id":"q","function":{"name":"ask","arguments":"{\"questions\":[{\"id\":\"secret\",\"secret\":true}]}"}}]},
        {"role":"tool","tool_call_id":"q","content":"{\"answers\":{\"secret\":\"synthetic-secret-value\"},\"large\":18446744073709551617}"},
        {"role":"assistant","content":"{\"broken\":\"synthetic-secret-value\\"},
        {"role":"user","content":[{"type":"image","source":{"type":"base64","media_type":"image/png","data":image}}]}
    ]).to_string()]).unwrap();
    export(&o).unwrap();
    let r = records(&o);
    assert!(!r
        .iter()
        .any(|r| r.to_string().contains("synthetic-secret-value")));
    assert!(r
        .iter()
        .any(|r| r.to_string().contains("18446744073709551617")));
    let image = r.iter().find(|r| r["role"] == "user").unwrap()["content"][0].clone();
    assert!(image.get("data").is_none());
    let digest = image["blob"]["blob"]
        .as_str()
        .unwrap()
        .strip_prefix("sha256:")
        .unwrap();
    assert_eq!(
        std::fs::read(o.out.join("blobs/sha256").join(digest)).unwrap(),
        bytes
    );
}

#[test]
fn namespace_is_native_or_explicit_and_colliding_databases_are_rejected() {
    let (dir,_db,mut o)=fixture(Provider::Hermes,"CREATE TABLE sessions(id TEXT PRIMARY KEY);CREATE TABLE messages(id INTEGER PRIMARY KEY,session_id TEXT);INSERT INTO sessions VALUES('s');");
    o.namespace = None;
    assert!(export(&o).unwrap_err().to_string().contains("namespace"));
    o.namespace = Some("installation:percent%".into());
    export(&o).unwrap();
    assert_eq!(records(&o)[0]["namespace"], "installation:percent%");
    let copy = dir.path().join("copy.db");
    std::fs::copy(&o.inputs[0].path, &copy).unwrap();
    o.inputs.push(Input {
        provider: Provider::Hermes,
        path: copy,
    });
    assert!(export(&o)
        .unwrap_err()
        .to_string()
        .contains("multiple native databases"));
}

#[test]
fn hermes_occurrence_uids_disambiguate_provider_ids_and_inherited_markers_do_not_fabricate_forks() {
    let (_dir,db,o)=fixture(Provider::Hermes,"CREATE TABLE sessions(id TEXT PRIMARY KEY,parent_session_id TEXT,model_config TEXT);CREATE TABLE messages(id INTEGER PRIMARY KEY,session_id TEXT,role TEXT,content TEXT,tool_calls TEXT,tool_call_id TEXT,tool_call_uids TEXT,tool_call_uid TEXT,active INTEGER);INSERT INTO sessions VALUES('child','compressed-parent','{\"_branched_from\":\"original-parent\"}');");
    for (i, uid) in [(1, "occurrence-a"), (2, "occurrence-b")] {
        db.execute(
            "INSERT INTO messages VALUES(?1,'child','assistant','',?2,NULL,?3,NULL,1)",
            rusqlite::params![
                i,
                json!([{"id":"provider-repeat","function":{"name":"read","arguments":"{}"}}])
                    .to_string(),
                json!({"provider-repeat":uid}).to_string()
            ],
        )
        .unwrap();
    }
    db.execute("INSERT INTO messages VALUES(3,'child','tool','result',NULL,'provider-repeat',NULL,'occurrence-a',1)",[]).unwrap();
    export(&o).unwrap();
    let r = records(&o);
    assert!(r[0]["lineage"].get("forkedFrom").is_none());
    assert!(r[0]["lineage"]["relatedUnknown"]["session"]
        .as_str()
        .unwrap()
        .ends_with(":compressed-parent"));
    let result = r.iter().find(|r| r["type"] == "tool_result").unwrap();
    assert_eq!(result["linkage"], "exact");
    assert_eq!(result["callId"], "occurrence-a");
}

#[test]
fn shelley_generations_notices_and_copy_accounting_follow_native_fields() {
    let (_dir,db,mut o)=fixture(Provider::Shelley,"CREATE TABLE conversations(conversation_id TEXT PRIMARY KEY,current_generation INTEGER);CREATE TABLE messages(message_id TEXT PRIMARY KEY,conversation_id TEXT,sequence_id INTEGER,generation INTEGER,type TEXT,llm_data TEXT,user_data TEXT,usage_data TEXT,forked_from_message_id TEXT,excluded_from_context INTEGER);INSERT INTO conversations VALUES('parent',1),('child',2);");
    let body = json!({"Role":1,"Content":[{"Type":2,"Text":"answer"},{"Type":5,"ID":"call","ToolName":"read","ToolInput":{}}]});
    let usage = json!({"input_tokens":3,"output_tokens":1,"cost_usd":0.0});
    for (id, session, copy) in [
        ("original", "parent", None),
        ("copy", "child", Some("original")),
    ] {
        db.execute(
            "INSERT INTO messages VALUES(?1,?2,1,1,'agent',?3,NULL,?4,?5,0)",
            rusqlite::params![id, session, body.to_string(), usage.to_string(), copy],
        )
        .unwrap();
    }
    db.execute(
        "INSERT INTO messages VALUES('notice','child',2,2,'modelchange',?1,?2,?3,NULL,1)",
        rusqlite::params![
            json!({"Role":1,"Content":[{"Type":2,"Text":"model changed"}]}).to_string(),
            json!({"to":"example-model","reasoning_to":"low"}).to_string(),
            usage.to_string()
        ],
    )
    .unwrap();
    db.execute(
        "INSERT INTO messages VALUES('new','child',3,2,'user',?1,NULL,NULL,NULL,0)",
        [json!({"Role":0,"Content":[{"Type":2,"Text":"new generation"}]}).to_string()],
    )
    .unwrap();
    o.session = Some("child".into());
    export(&o).unwrap();
    let r = records(&o);
    assert_eq!(
        r[0]["lineage"]["forkedFrom"]["session"],
        "dev.exe.shelley:parent"
    );
    assert!(r
        .iter()
        .filter(|r| r["type"] == "tool_call" || r["type"] == "inference")
        .all(|r| r["copiedFrom"]["session"] == "dev.exe.shelley:parent"));
    assert_eq!(r.iter().filter(|r| r["type"] == "inference").count(), 1);
    let notice = r.iter().find(|r| r["messageId"] == "notice").unwrap();
    assert_eq!(notice["author"], "harness");
    assert_eq!(notice["audience"], "user");
    assert_eq!(notice["capture"]["content"]["state"], "absent");
    let fresh = r.iter().find(|r| r["messageId"] == "new").unwrap();
    assert_eq!(fresh["branchesFrom"], json!({"record":null}));
    let head = r
        .iter()
        .find(|r| r["type"] == "session_info" && r.get("heads").is_some())
        .unwrap();
    assert_eq!(head["heads"][0], fresh["id"]);
}

#[test]
fn hermes_receipt_log_precedes_mutable_snapshots() {
    let (_dir,_db,o)=fixture(Provider::Hermes,"CREATE TABLE sessions(id TEXT PRIMARY KEY);INSERT INTO sessions VALUES('s');CREATE TABLE messages(id INTEGER PRIMARY KEY,session_id TEXT);CREATE TABLE worker_executions(execution_id TEXT PRIMARY KEY,session_id TEXT,status TEXT);INSERT INTO worker_executions VALUES('e','s','done');CREATE TABLE worker_receipts(execution_id TEXT,sequence INTEGER,result_json TEXT);INSERT INTO worker_receipts VALUES('e',2,'second'),('e',1,'first');");
    export(&o).unwrap();
    let r = records(&o);
    let receipt: Vec<_> = r
        .iter()
        .filter(|r| r["type"] == "com.nousresearch.hermes/worker_receipts")
        .collect();
    assert_eq!(receipt[0]["native"]["record"]["sequence"], 1);
    assert!(receipt.iter().all(|r| r.get("assembly").is_none()));
    let state = r
        .iter()
        .find(|r| r["type"] == "com.nousresearch.hermes/worker_executions")
        .unwrap();
    assert!(receipt[1]["seq"].as_u64().unwrap() < state["seq"].as_u64().unwrap());
    assert_eq!(state["assembly"]["mode"], "snapshot");
}
