//! Authored SQLite semantic cases, plus the repository's separately documented
//! native/sanitized fixtures. Authored data is not a current-writer capture.
use super::*;
use rusqlite::{params, Connection};
use std::path::Path;

fn database(provider: Provider, schema: &str) -> (tempfile::TempDir, Options, Connection) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.db");
    let db = Connection::open(&path).unwrap();
    db.execute_batch(schema).unwrap();
    let options = Options::new(vec![Input { provider, path }], dir.path().join("out"));
    (dir, options, db)
}
const FIRE:&str="CREATE TABLE chat_sessions(id TEXT PRIMARY KEY,name TEXT,created_at INTEGER,updated_at INTEGER,messages_json TEXT,metadata_json TEXT);";
fn fire(db: &Connection, messages: &Value) {
    db.execute(
        "INSERT OR REPLACE INTO chat_sessions VALUES('s','title',1000,2000,?1,'{}')",
        [messages.to_string()],
    )
    .unwrap();
}

#[test]
fn snapshot_order_noop_append_rewrite_and_relocation() {
    let (dir, mut options, db) = database(Provider::Firebender, FIRE);
    fire(
        &db,
        &json!([{"id":"a","role":"user","content":"  first\r\n"},{"id":"c","role":"assistant","content":"last"}]),
    );
    export(&options).unwrap();
    let before = records(&options);
    assert_eq!(before[1]["content"][0]["text"], "  first\r\n");
    assert!(before[1].get("ts").is_none());
    assert_eq!(export(&options).unwrap().unchanged, 1);
    assert_eq!(records(&options), before);
    fire(
        &db,
        &json!([{"id":"a","role":"user","content":"  first\r\n"},{"id":"b","role":"user","content":"inserted"},{"id":"c","role":"assistant","content":"last"}]),
    );
    assert_eq!(export(&options).unwrap().snapshots, 1);
    let after = records(&options);
    let ids: Vec<_> = after
        .iter()
        .filter_map(|r| r["messageId"].as_str())
        .collect();
    assert_eq!(ids, ["a", "b", "c"]);
    assert_eq!(before[1]["id"], after[1]["id"]);
    assert_eq!(before[2]["id"], after[3]["id"]);
    assert_ne!(before[0]["observedAt"], after[0]["observedAt"]);
    let moved = dir.path().join("moved.db");
    std::fs::copy(&options.inputs[0].path, &moved).unwrap();
    options.inputs[0].path = moved;
    export(&options).unwrap();
    assert_eq!(records(&options), after);
}

#[test]
fn duplicate_calls_are_ambiguous_and_large_integers_stay_exact() {
    let (_dir, options, db) = database(Provider::Firebender, FIRE);
    fire(
        &db,
        &json!([
            {"role":"assistant","tool_calls":[{"id":"same","function":{"name":"read","arguments":"{\"n\":9007199254740993}"}}]},
            {"role":"assistant","tool_calls":[{"id":"same","function":{"name":"read","arguments":"{}"}}]},
            {"role":"tool","tool_call_id":"same","content":"result","native_number":9007199254740993u64}
        ]),
    );
    export(&options).unwrap();
    let r = records(&options);
    let call = r.iter().find(|r| r["type"] == "tool_call").unwrap();
    assert_eq!(call["inputText"], "{\"n\":9007199254740993}");
    assert!(call.get("input").is_none());
    let result = r.iter().find(|r| r["type"] == "tool_result").unwrap();
    assert_eq!(result["linkage"], "ambiguous");
    assert!(result.get("callId").is_none());
    assert_eq!(result["status"], "unknown");
    let blob = result["native"]["blob"]["blob"]
        .as_str()
        .unwrap()
        .strip_prefix("sha256:")
        .unwrap();
    assert!(
        std::fs::read_to_string(options.out.join("blobs/sha256").join(blob))
            .unwrap()
            .contains("9007199254740993")
    );
}

#[test]
fn multi_session_database_and_firebender_children() {
    let (_dir, options, db) = database(Provider::Firebender, FIRE);
    fire(&db, &json!([]));
    db.execute_batch("INSERT INTO chat_sessions VALUES('s2','second',1,2,'[]','{}'); CREATE TABLE subagent_conversations(session_id TEXT,tool_call_id TEXT,messages_json TEXT,created_at INTEGER,updated_at INTEGER,subagent_type TEXT,description TEXT,PRIMARY KEY(session_id,tool_call_id)); INSERT INTO subagent_conversations VALUES('s','call-child','[]',1,2,'Explore','description');").unwrap();
    let result = export(&options).unwrap();
    assert_eq!(result.sessions, 2);
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(options.out.join("bundle.json")).unwrap()).unwrap();
    assert!(manifest["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .all(|s| matches!(s["sessionId"].as_str(), Some("s" | "s2"))));
    let stream = manifest["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["sessionId"] == "s")
        .unwrap();
    let text = std::fs::read_to_string(options.out.join(stream["path"].as_str().unwrap())).unwrap();
    assert!(text.contains("com.firebender/subagent_conversation"));
    assert!(text.contains("call-child"));
}

#[test]
fn bounded_row_becomes_gap_and_secret_answers_are_not_retained() {
    let (_dir, mut options, db) = database(Provider::Firebender, FIRE);
    fire(&db, &json!([{"role":"user","content":"x".repeat(4096)}]));
    options.max_record_bytes = 1024;
    assert_eq!(export(&options).unwrap().gaps, 1);
    options.max_record_bytes = 16384;
    fire(
        &db,
        &json!([
            {"role":"assistant","tool_calls":[{"id":"q","function":{"name":"question","arguments":"{\"questions\":[{\"id\":\"private\",\"secret\":true}]}"}}]},
            {"role":"tool","tool_call_id":"q","content":{"answers":{"private":"synthetic-secret-answer"}}}
        ]),
    );
    export(&options).unwrap();
    assert!(!serde_json::to_string(&records(&options))
        .unwrap()
        .contains("synthetic-secret-answer"));
}

const DEVIN:&str="CREATE TABLE sessions(id TEXT PRIMARY KEY,working_directory TEXT,created_at INTEGER,main_chain_id INTEGER,title TEXT,model TEXT,agent_mode TEXT); CREATE TABLE message_nodes(session_id TEXT,node_id INTEGER,parent_node_id INTEGER,chat_message TEXT,created_at INTEGER,metadata TEXT,UNIQUE(session_id,node_id));";
fn node(db: &Connection, id: i64, parent: Option<i64>, m: Value, meta: Value) {
    db.execute(
        "INSERT INTO message_nodes VALUES('s',?1,?2,?3,1000,?4)",
        params![id, parent, m.to_string(), meta.to_string()],
    )
    .unwrap();
}
#[test]
fn devin_mechanical_parallel_calls_real_branch_and_primary_head() {
    let (_dir, options, db) = database(Provider::Devin, DEVIN);
    db.execute_batch(
        "INSERT INTO sessions VALUES('s','/synthetic',1000,3,'title','requested','normal')",
    )
    .unwrap();
    node(
        &db,
        1,
        None,
        json!({"role":"user","message_id":"u","content":"go"}),
        json!({}),
    );
    node(
        &db,
        2,
        Some(1),
        json!({"role":"assistant","message_id":"a","content":"working","metadata":{"request_id":"response"},"tool_calls":[{"id":"t1","name":"one","arguments":{}},{"id":"t2","name":"two","arguments":{}}]}),
        json!({}),
    );
    node(
        &db,
        3,
        Some(2),
        json!({"role":"tool","tool_call_id":"t1","content":"one"}),
        json!({}),
    );
    node(
        &db,
        4,
        Some(2),
        json!({"role":"tool","tool_call_id":"t2","content":"two"}),
        json!({}),
    );
    node(
        &db,
        5,
        Some(1),
        json!({"role":"user","message_id":"edit","content":"change"}),
        json!({}),
    );
    export(&options).unwrap();
    let r = records(&options);
    let results: Vec<_> = r.iter().filter(|r| r["type"] == "tool_result").collect();
    assert!(results[1].get("branchesFrom").is_none());
    let edited = r.iter().find(|r| r["messageId"] == "edit").unwrap();
    assert_eq!(
        edited["branchesFrom"]["record"],
        r.iter().find(|r| r["messageId"] == "u").unwrap()["id"]
    );
    assert_eq!(r.last().unwrap()["heads"][0], results[0]["id"]);
}

#[test]
fn deepagents_messages_only_and_messagepack_extensions() {
    let (_dir,options,db)=database(Provider::DeepAgents,"CREATE TABLE checkpoints(thread_id TEXT,checkpoint_ns TEXT,checkpoint_id TEXT,parent_checkpoint_id TEXT,type TEXT,checkpoint BLOB,metadata BLOB); CREATE TABLE writes(thread_id TEXT,checkpoint_ns TEXT,checkpoint_id TEXT,task_id TEXT,idx INTEGER,channel TEXT,type TEXT,value BLOB);");
    fn pack(v: rmpv::Value) -> Vec<u8> {
        let mut out = Vec::new();
        rmpv::encode::write_value(&mut out, &v).unwrap();
        out
    }
    let fields = rmpv::Value::Map(vec![
        ("type".into(), "human".into()),
        ("id".into(), "m".into()),
        ("content".into(), "  exact\n".into()),
    ]);
    let ext = rmpv::Value::Ext(
        5,
        pack(rmpv::Value::Array(vec![
            "langchain_core.messages.human".into(),
            "HumanMessage".into(),
            fields.clone(),
            "model_validate".into(),
        ])),
    );
    for (i, channel) in [(0, "messages"), (1, "agent")] {
        db.execute(
            "INSERT INTO writes VALUES('thread','','checkpoint','task',?1,?2,'msgpack',?3)",
            params![i, channel, pack(rmpv::Value::Array(vec![ext.clone()]))],
        )
        .unwrap();
    }
    db.execute(
        "INSERT INTO checkpoints VALUES('thread','','checkpoint',NULL,'msgpack',?1,?2)",
        params![
            pack(rmpv::Value::Map(vec![(
                "messages".into(),
                rmpv::Value::Array(vec![fields])
            )])),
            b"{}".as_slice()
        ],
    )
    .unwrap();
    db.execute_batch("INSERT INTO writes VALUES('thread','','checkpoint','task',2,'branch:to:model','null',X'');").unwrap();
    export(&options).unwrap();
    let r = records(&options);
    assert!(!r.iter().any(|r| r["type"] == "gap"));
    assert!(r
        .iter()
        .any(|r| r["type"] == "com.langchain.deepagents/write"
            && r["native"]["record"]["type"] == "null"));
    assert_eq!(r.iter().filter(|r| r["type"] == "message").count(), 1);
    assert_eq!(
        r.iter().find(|r| r["type"] == "message").unwrap()["content"][0]["text"],
        "  exact\n"
    );
    assert_eq!(export(&options).unwrap().unchanged, 1);
}

#[test]
fn retained_provider_fixtures_convert_without_source_writes() {
    for (provider, relative, sessions) in [
        (
            Provider::Firebender,
            "firebender/v1/.idea/firebender/chat_history.db",
            1,
        ),
        (Provider::ForgeCode, "forgecode/v1/forge.db", 1),
        (Provider::DeepAgents, "deepagents/v1/sessions.db", 1),
        (Provider::Devin, "devin/v17/sessions.db", 3),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.db");
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/provider-history")
                .join(relative),
            &path,
        )
        .unwrap();
        let options = Options::new(
            vec![Input {
                provider,
                path: path.clone(),
            }],
            dir.path().join("out"),
        );
        let before = std::fs::read(&path).unwrap();
        let result = export(&options).unwrap();
        assert_eq!(result.sessions, sessions);
        assert_eq!(result.gaps, 0);
        assert_eq!(export(&options).unwrap().unchanged, sessions);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!dir.path().join("source.db-wal").exists());
        assert!(!dir.path().join("source.db-shm").exists());
    }
}

#[test]
fn devin_compaction_keeps_native_prefix_and_missing_head_is_audit_only() {
    let (_dir, options, db) = database(Provider::Devin, DEVIN);
    db.execute_batch("INSERT INTO sessions VALUES('s','/synthetic',1,4,'title','model','normal')")
        .unwrap();
    node(
        &db,
        1,
        None,
        json!({"role":"user","message_id":"u","content":"old"}),
        json!({}),
    );
    node(
        &db,
        2,
        Some(1),
        json!({"role":"assistant","message_id":"a","content":"old answer"}),
        json!({}),
    );
    node(
        &db,
        3,
        None,
        json!({"role":"system","message_id":"p","content":"new prefix"}),
        json!({"is_system_prefix":true}),
    );
    node(
        &db,
        4,
        Some(3),
        json!({"role":"system","message_id":"c","content":"summary"}),
        json!({"summarized_from":2}),
    );
    export(&options).unwrap();
    let r = records(&options);
    let prefix = r.iter().find(|r| r["messageId"] == "p").unwrap();
    assert_eq!(prefix["branchesFrom"], json!({"record":null}));
    let c = r.iter().find(|r| r["type"] == "compaction").unwrap();
    assert!(c.get("branchesFrom").is_none());
    assert_eq!(c["replaces"], json!({"state":"unknown"}));
    for value in ["NULL", "999"] {
        db.execute_batch(&format!("UPDATE sessions SET main_chain_id={value}"))
            .unwrap();
        export(&options).unwrap();
        let r = records(&options);
        assert!(!r.iter().any(crate::records::context));
        assert!(!r.iter().any(|r| r.get("heads").is_some()));
    }
}

#[test]
fn devin_tool_snapshot_keeps_branch_structure_and_numeric_signal() {
    let (_dir, options, db) = database(Provider::Devin, DEVIN);
    db.execute_batch("INSERT INTO sessions VALUES('s','/synthetic',1,4,'title','model','normal'); CREATE TABLE tool_call_state(session_id TEXT,tool_call_id TEXT,tool_call_json TEXT,tool_call_update_json TEXT)").unwrap();
    node(
        &db,
        1,
        None,
        json!({"role":"assistant","tool_calls":[{"id":"c","function":{"name":"shell","arguments":"{}"}}]}),
        json!({}),
    );
    node(
        &db,
        2,
        Some(1),
        json!({"role":"user","content":"different branch"}),
        json!({}),
    );
    node(
        &db,
        3,
        Some(2),
        json!({"role":"assistant","content":"answer"}),
        json!({}),
    );
    node(
        &db,
        4,
        Some(1),
        json!({"role":"tool","tool_call_id":"c","content":"result"}),
        json!({}),
    );
    db.execute(
        "INSERT INTO tool_call_state VALUES('s','c',NULL,?1)",
        [
            json!({"status":"completed","_meta":{"terminal_exit":{"exit_code":0,"signal":9}}})
                .to_string(),
        ],
    )
    .unwrap();
    export(&options).unwrap();
    let r = records(&options);
    let results: Vec<_> = r.iter().filter(|r| r["type"] == "tool_result").collect();
    assert_eq!(results.len(), 2);
    assert!(results[0].get("branchesFrom").is_some());
    assert_eq!(results[0]["branchesFrom"], results[1]["branchesFrom"]);
    assert_eq!(results[1]["supersedes"], results[0]["id"]);
    assert!(results[1]["exec"].get("signal").is_none());
    assert_eq!(results[1]["raw"]["terminal_exit"]["signal"], 9);
}

#[test]
fn deepagents_revisions_removals_and_namespaces() {
    let (_dir, options, db) = database(Provider::DeepAgents,"CREATE TABLE checkpoints(thread_id TEXT,checkpoint_ns TEXT,checkpoint_id TEXT); CREATE TABLE writes(thread_id TEXT,checkpoint_ns TEXT,checkpoint_id TEXT,task_id TEXT,idx INTEGER,channel TEXT,type TEXT,value BLOB)");
    let write = |i: i32, ns: &str, m: Value| {
        fn packed(v: &Value) -> rmpv::Value {
            match v {
                Value::Null => rmpv::Value::Nil,
                Value::String(s) => s.as_str().into(),
                Value::Array(a) => rmpv::Value::Array(a.iter().map(packed).collect()),
                Value::Object(o) => rmpv::Value::Map(
                    o.iter()
                        .map(|(k, v)| (k.as_str().into(), packed(v)))
                        .collect(),
                ),
                _ => panic!("authored helper only supports strings and containers"),
            }
        }
        let mut bytes = Vec::new();
        rmpv::encode::write_value(&mut bytes, &packed(&m)).unwrap();
        db.execute(
            "INSERT INTO writes VALUES('thread',?1,'checkpoint','task',?2,'messages','msgpack',?3)",
            params![ns, i, bytes],
        )
        .unwrap();
    };
    for (i, text) in [(0, "first"), (1, "revised")] {
        write(
            i,
            "",
            json!({"type":"ai","id":"a","content":text,"tool_calls":[{"id":"c","name":"read","args":{}}]}),
        );
    }
    write(
        2,
        "",
        json!({"type":"tool","id":"result","tool_call_id":"c","content":"output"}),
    );
    write(3, "", json!({"type":"remove","id":"a"}));
    write(
        4,
        "child:uuid",
        json!({"type":"human","id":"lane","content":"lane-only"}),
    );
    assert_eq!(export(&options).unwrap().sessions, 1);
    let r = records(&options);
    let messages: Vec<_> = r.iter().filter(|r| r["type"] == "message").collect();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1]["supersedes"], messages[0]["id"]);
    let calls: Vec<_> = r.iter().filter(|r| r["type"] == "tool_call").collect();
    assert_eq!(calls[1]["supersedes"], calls[0]["id"]);
    assert_eq!(
        r.iter().find(|r| r["type"] == "tool_result").unwrap()["linkage"],
        "exact"
    );
    let prune = r.iter().find(|r| r["type"] == "compaction").unwrap();
    assert_eq!(prune["kind"], "prune");
    assert_eq!(prune["replaces"]["ids"].as_array().unwrap().len(), 2);
    assert!(r
        .iter()
        .any(|r| r["type"] == "com.langchain.deepagents/write"
            && r.to_string().contains("lane-only")));
    write(
        5,
        "",
        json!({"type":"ai","id":"duplicates","tool_calls":[{"id":"duplicate","name":"read","args":{}},{"id":"duplicate","name":"write","args":{}}]}),
    );
    write(
        6,
        "",
        json!({"type":"tool","id":"duplicate-result","tool_call_id":"duplicate","content":"ambiguous"}),
    );
    export(&options).unwrap();
    let r = records(&options);
    let calls: Vec<_> = r
        .iter()
        .filter(|r| r["type"] == "tool_call" && r["callId"] == "duplicate")
        .collect();
    assert_eq!(calls.len(), 2);
    assert!(calls.iter().all(|r| r.get("supersedes").is_none()));
    assert!(r
        .iter()
        .any(|r| r["type"] == "tool_result" && r["linkage"] == "ambiguous"));
}

#[test]
fn application_state_is_unowned_and_unsafe_json_is_not_rounded() {
    let (_dir, options, db) = database(Provider::Devin, DEVIN);
    db.execute_batch("INSERT INTO sessions VALUES('s','/synthetic',1,NULL,'title','model','normal'); CREATE TABLE app_state(key TEXT PRIMARY KEY,value TEXT); INSERT INTO app_state VALUES('selected','{\"n\":9007199254740993}')").unwrap();
    export(&options).unwrap();
    let manifest = std::fs::read_to_string(options.out.join("bundle.json")).unwrap();
    assert!(manifest.contains("9007199254740993"));
    assert!(!serde_json::to_string(&records(&options))
        .unwrap()
        .contains("selected"));
    assert!(crate::batch8::sqlite::exact_json("{\"n\":184467440737095516160}").is_none());
    assert!(crate::batch8::sqlite::exact_json("{\"n\":18446744073709551615}").is_some());
    db.execute_batch("DELETE FROM sessions").unwrap();
    assert_eq!(export(&options).unwrap().sessions, 0);
    assert!(std::fs::read_to_string(options.out.join("bundle.json"))
        .unwrap()
        .contains("9007199254740993"));
}

#[test]
fn forgecode_native_usage_reasoning_and_tool_output() {
    let (_dir, options, db) = database(
        Provider::ForgeCode,
        "CREATE TABLE conversations(conversation_id TEXT PRIMARY KEY,context TEXT,metrics TEXT)",
    );
    let context = json!({"messages":[
        {"text":{"role":"Assistant","content":"answer","model":"served","tool_calls":[{"call_id":"c","name":"read","arguments":{"path":"file"}}],"reasoning_details":[{"type_of":"reasoning.summary","text":"brief"}]},"usage":{"prompt_tokens":{"Actual":10},"completion_tokens":{"Actual":4},"total_tokens":{"Actual":14},"cached_tokens":{"Actual":3},"cost":0.01}},
        {"tool":{"call_id":"c","output":{"is_error":false,"values":[{"text":"result"},{"ai":{"value":"child result","conversation_id":"native-child"}}]}}}
    ]});
    db.execute(
        "INSERT INTO conversations VALUES('s',?1,'{}')",
        [context.to_string()],
    )
    .unwrap();
    export(&options).unwrap();
    let r = records(&options);
    let inference = r.iter().find(|r| r["type"] == "inference").unwrap();
    assert_eq!(
        inference["usage"]["input"],
        json!({"total":10,"cacheRead":3,"includesCache":null})
    );
    assert_eq!(inference["cost"], json!({"amount":0.01,"currency":"USD"}));
    let message = r.iter().find(|r| r["type"] == "message").unwrap();
    assert_eq!(message["inferenceId"], inference["inferenceId"]);
    assert_eq!(message["content"][0]["visibility"], "summary");
    assert!(message.get("capture").is_none());
    let call = r.iter().find(|r| r["type"] == "tool_call").unwrap();
    assert_eq!(call["inferenceId"], inference["inferenceId"]);
    let result = r.iter().find(|r| r["type"] == "tool_result").unwrap();
    assert_eq!(result["status"], "completed");
    assert_eq!(result["content"][1]["text"], "child result");
    assert!(result.to_string().contains("native-child"));
}

#[test]
fn native_call_record_id_survives_message_growth_and_call_insertion() {
    let (_dir, options, db) = database(Provider::Firebender, FIRE);
    let call = json!({"id":"c","function":{"name":"read","arguments":"{}"}});
    fire(
        &db,
        &json!([{"id":"m","role":"assistant","tool_calls":[call]}]),
    );
    export(&options).unwrap();
    let before = records(&options)
        .into_iter()
        .find(|r| r["callId"] == "c")
        .unwrap();
    fire(
        &db,
        &json!([{"id":"m","role":"assistant","content":"later content","tool_calls":[{"id":"earlier","function":{"name":"other","arguments":"{}"}},call]}]),
    );
    export(&options).unwrap();
    let after = records(&options)
        .into_iter()
        .find(|r| r["callId"] == "c")
        .unwrap();
    assert_eq!(before["id"], after["id"]);
    assert_eq!(after["idScope"], Value::Null);
}
