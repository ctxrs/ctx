//! Authored native-schema cases, not harness captures.
use super::*;
use rusqlite::{params, Connection};
fn database(provider: Provider) -> (tempfile::TempDir, Options, Connection) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("native.db");
    let c = Connection::open(&path).unwrap();
    c.execute_batch("PRAGMA journal_mode=WAL;
      CREATE TABLE session(id TEXT PRIMARY KEY,directory TEXT,title TEXT,version TEXT,time_created INTEGER,time_updated INTEGER,parent_id TEXT);
      CREATE TABLE message(id TEXT PRIMARY KEY,session_id TEXT,time_created INTEGER,time_updated INTEGER,data TEXT);
      CREATE TABLE part(id TEXT PRIMARY KEY,message_id TEXT,session_id TEXT,time_created INTEGER,time_updated INTEGER,data TEXT);
      CREATE TABLE session_message(id TEXT PRIMARY KEY,session_id TEXT,type TEXT,seq INTEGER,time_created INTEGER,time_updated INTEGER,data TEXT);
      INSERT INTO session VALUES('session','/workspace','title','test',1000,1000,NULL);").unwrap();
    let options = Options::new(vec![Input { provider, path }], dir.path().join("out"));
    (dir, options, c)
}
fn message(c: &Connection, id: &str, role: &str) {
    c.execute(
        "INSERT INTO message VALUES(?1,'session',1000,1000,?2)",
        params![id, json!({"role":role}).to_string()],
    )
    .unwrap();
}
fn part(c: &Connection, id: &str, msg: &str, v: Value) {
    c.execute(
        "INSERT INTO part VALUES(?1,?2,'session',1000,1000,?3)",
        params![id, msg, v.to_string()],
    )
    .unwrap();
}
fn path(o: &Options) -> std::path::PathBuf {
    let m: Value =
        serde_json::from_slice(&std::fs::read(o.out.join("bundle.json")).unwrap()).unwrap();
    o.out.join(m["sessions"][0]["path"].as_str().unwrap())
}
#[test]
fn sqlite_family_rewrites_without_native_clock_changes_keep_identity_and_prior_bytes() {
    for provider in [Provider::OpenCode, Provider::Kilo, Provider::MiMoCode] {
        let (_d, o, c) = database(provider);
        message(&c, "m", "assistant");
        part(
            &c,
            "p",
            "m",
            json!({"type":"text","text":"first ","synthetic":false}),
        );
        part(
            &c,
            "q",
            "m",
            json!({"type":"tool","callID":"call","tool":"bash","state":{"status":"running","input":{"command":"true"}}}),
        );
        let first = export(&o).unwrap();
        assert_eq!(first.sessions, 1);
        let old_path = path(&o);
        let old_bytes = std::fs::read(&old_path).unwrap();
        let before = records(&o);
        assert!(!before.iter().any(|r| r["type"] == "tool_result"));
        assert_eq!(export(&o).unwrap().unchanged, 1);
        c.execute(
            "UPDATE part SET data=?1 WHERE id='p'",
            [json!({"type":"text","text":"first second"}).to_string()],
        )
        .unwrap();
        c.execute("UPDATE part SET data=?1 WHERE id='q'",[json!({"type":"tool","callID":"call","tool":"bash","state":{"status":"completed","input":{"command":"true"},"output":"","metadata":{"exit":0}}}).to_string()]).unwrap();
        assert_eq!(export(&o).unwrap().snapshots, 1);
        let after = records(&o);
        let a = after.iter().find(|r| r["messageId"] == "m").unwrap();
        let b = before.iter().find(|r| r["messageId"] == "m").unwrap();
        assert_eq!(a["id"], b["id"]);
        assert_eq!(a["seq"], b["seq"]);
        assert_eq!(a["content"][0]["text"], "first second");
        assert_eq!(
            a["content"][0]["_meta"][format!("{}/row", provider.domain())]["id"],
            "p"
        );
        assert_eq!(a["assembly"]["mode"], "snapshot");
        assert!(!after.iter().any(|r| r.get("supersedes").is_some()));
        let result = after.iter().find(|r| r["type"] == "tool_result").unwrap();
        assert_eq!(result["exec"]["exitCode"], 0);
        assert_eq!(std::fs::read(old_path).unwrap(), old_bytes);
    }
}
#[test]
fn sqlite_relocation_and_append_do_not_change_existing_ids() {
    let (d, mut o, c) = database(Provider::OpenCode);
    message(&c, "u", "user");
    part(&c, "p", "u", json!({"type":"text","text":"hello"}));
    export(&o).unwrap();
    let before = records(&o);
    message(&c, "v", "assistant");
    part(&c, "q", "v", json!({"type":"text","text":"reply"}));
    export(&o).unwrap();
    let after = records(&o);
    assert_eq!(&after[..before.len()], before);
    c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
    drop(c);
    let relocated = d.path().join("relocated.db");
    std::fs::copy(&o.inputs[0].path, &relocated).unwrap();
    o.inputs[0].path = relocated;
    assert_eq!(export(&o).unwrap().unchanged, 1);
    assert_eq!(records(&o), after);
}
#[test]
fn sqlite_multiple_sessions_and_pinned_read_transaction() {
    let (d, o, c) = database(Provider::OpenCode);
    c.execute(
        "INSERT INTO session VALUES('two','/workspace','old','test',2000,2000,NULL)",
        [],
    )
    .unwrap();
    let stage = tempfile::tempdir_in(d.path()).unwrap();
    let mut headers = Vec::new();
    crate::sqlite::visit(
        &o.inputs[0],
        o.max_record_bytes,
        stage.path(),
        None,
        &mut |source| {
            headers.push(source.header.clone());
            if headers.len() == 1 {
                c.execute("UPDATE session SET title='new' WHERE id='two'", [])
                    .unwrap();
            }
            Ok(())
        },
        &mut 0,
    )
    .unwrap();
    assert_eq!(headers.len(), 2);
    assert_eq!(headers[1]["title"], "old");
    assert_eq!(export(&o).unwrap().sessions, 2);
}
#[test]
fn sqlite_row_limit_and_malformed_rows_emit_gaps() {
    let (_d, mut o, c) = database(Provider::OpenCode);
    o.max_record_bytes = 1024;
    message(&c, "m", "assistant");
    part(
        &c,
        "big",
        "m",
        json!({"type":"text","text":"x".repeat(8192)}),
    );
    c.execute(
        "INSERT INTO part VALUES('bad','m','session',1000,1000,'{broken')",
        [],
    )
    .unwrap();
    let result = export(&o).unwrap();
    assert_eq!(result.gaps, 2);
    let r = records(&o);
    assert!(r.iter().any(|r| r["reason"] == "conversion_loss"));
    assert!(r.iter().any(|r| r["reason"] == "unparseable"));
}
#[test]
fn sqlite_folded_message_spills_content_to_blob() {
    let (_d, mut o, c) = database(Provider::OpenCode);
    o.max_record_bytes = 1024;
    message(&c, "m", "assistant");
    for i in 0..12 {
        part(
            &c,
            &format!("p{i:02}"),
            "m",
            json!({"type":"text","text":"x".repeat(500)}),
        );
    }
    export(&o).unwrap();
    let r = records(&o);
    let m = r.iter().find(|r| r["messageId"] == "m").unwrap();
    assert_eq!(m["capture"]["content"]["state"], "external");
    let digest = m["capture"]["content"]["blob"]["blob"]
        .as_str()
        .unwrap()
        .strip_prefix("sha256:")
        .unwrap();
    let parts: Value =
        serde_json::from_slice(&std::fs::read(o.out.join("blobs/sha256").join(digest)).unwrap())
            .unwrap();
    assert_eq!(parts.as_array().unwrap().len(), 12);
}
#[test]
fn sequenced_messages_usage_tools_and_native_metadata() {
    let (_d, o, c) = database(Provider::Kilo);
    let body = json!({"content":[{"type":"reasoning","id":"reason","text":"thinking"},{"type":"text","id":"text","text":"answer"},{"type":"tool","id":"call","name":"grep","state":{"status":"error","input":{"q":"x"},"error":{"message":"failed"},"content":[]}}],"tokens":{"input":4,"output":3,"reasoning":5,"cache":{"read":2,"write":1}},"cost":0.0,"finish":"stop","model":{"id":"requested","providerID":"test"},"unknown":{"kept":true}});
    c.execute(
        "INSERT INTO session_message VALUES('a','session','assistant',0,1000,1000,?1)",
        [body.to_string()],
    )
    .unwrap();
    export(&o).unwrap();
    let r = records(&o);
    let inf = r.iter().find(|r| r["type"] == "inference").unwrap();
    assert_eq!(inf["usage"]["output"]["total"], 8);
    assert_eq!(inf["usage"]["output"]["reasoning"], 5);
    assert_eq!(inf["cost"]["amount"], 0.0);
    assert_eq!(inf["model"]["requested"], "requested");
    assert!(inf["model"].get("id").is_none());
    assert_eq!(r.iter().filter(|r| r["type"] == "tool_call").count(), 1);
    assert!(r
        .iter()
        .any(|r| r["type"] == "tool_result" && r["status"] == "failed"));
    assert!(r.iter().all(|r| r.get("supersedes").is_none()));
}
#[test]
fn duplicate_calls_within_one_native_message_are_ambiguous() {
    let (_d, o, c) = database(Provider::MiMoCode);
    let tool = json!({"type":"tool","id":"duplicate","name":"read","state":{"status":"completed","input":{},"content":[]}});
    c.execute(
        "INSERT INTO session_message VALUES('a','session','assistant',0,1000,1000,?1)",
        [json!({"content":[tool,tool]}).to_string()],
    )
    .unwrap();
    export(&o).unwrap();
    let r = records(&o);
    assert!(r
        .iter()
        .filter(|r| r["type"] == "tool_result")
        .all(|r| r["linkage"] == "ambiguous"));
}
fn kiro(legacy: bool) -> (tempfile::TempDir, Options, Connection) {
    let (d, o, c) = database(Provider::Kiro);
    if legacy {
        c.execute_batch("CREATE TABLE conversations(key TEXT PRIMARY KEY,value TEXT)")
            .unwrap();
    } else {
        c.execute_batch("CREATE TABLE conversations_v2(key TEXT,conversation_id TEXT,value TEXT,created_at INTEGER,updated_at INTEGER,PRIMARY KEY(key,conversation_id))").unwrap();
    }
    (d, o, c)
}
#[test]
fn kiro_directory_keys_require_stable_namespace_and_capture_scoped_ids() {
    let (_d, mut o, c) = kiro(true);
    c.execute("INSERT INTO conversations VALUES('/workspace',?1)",[json!({"conversation_id":"internal-uuid","history":[{"user":{"content":{"Prompt":{"prompt":"hello"}}},"assistant":{"Response":{"content":"reply"}}}]}).to_string()]).unwrap();
    assert!(export(&o).unwrap_err().to_string().contains("namespace"));
    o.namespace = Some("installation".into());
    export(&o).unwrap();
    let r = records(&o);
    assert_eq!(r[0]["sessionId"], "/workspace");
    assert_eq!(r[0]["namespace"], "installation");
    assert!(r.iter().skip(1).all(|r| r["idScope"] == "capture"));
    let old = path(&o);
    let bytes = std::fs::read(&old).unwrap();
    c.execute(
        "UPDATE conversations SET value=?1",
        [json!({"conversation_id":"new-uuid","history":[]}).to_string()],
    )
    .unwrap();
    assert_eq!(export(&o).unwrap().snapshots, 1);
    assert_eq!(std::fs::read(old).unwrap(), bytes);
    assert_eq!(records(&o)[0]["sessionId"], "/workspace");
}
#[test]
fn kiro_native_request_and_tool_results_keep_source_truth() {
    let (_d, o, c) = kiro(false);
    c.execute("INSERT INTO conversations_v2 VALUES('/workspace','uuid',?1,1000,2000)",[json!({"history":[{"assistant":{"ToolUse":{"message_id":"native-response","content":"","tool_uses":[{"id":"c","name":"read","args":{}}]}},"request_metadata":{"request_id":"request","model_id":"requested","request_start_timestamp_ms":1000,"stream_end_timestamp_ms":2000}},{"user":{"content":{"ToolUseResults":{"tool_use_results":[{"tool_use_id":"c","status":"Success","content":[{"Text":"exact\n"},{"Json":{"a":1}}]}]}}}}]}).to_string()]).unwrap();
    export(&o).unwrap();
    let r = records(&o);
    assert_eq!(r[0]["sessionId"], "uuid");
    assert!(r.iter().any(|r| r["messageId"] == "native-response"));
    let inf = r.iter().find(|r| r["type"] == "inference").unwrap();
    assert_eq!(inf["inferenceId"], "request");
    assert_eq!(inf["latency"]["durationMs"], 1000);
    assert!(inf.get("usage").is_none());
    let result = r.iter().find(|r| r["type"] == "tool_result").unwrap();
    assert_eq!(result["linkage"], "exact");
    assert_eq!(result["content"][0]["text"], "exact\n");
}

#[test]
fn sqlite_ignored_content_and_secret_answers_do_not_reappear_in_metadata() {
    let (_d, o, c) = database(Provider::OpenCode);
    message(&c, "u", "user");
    part(
        &c,
        "p",
        "u",
        json!({"type":"text","text":"hidden from model","ignored":true}),
    );
    part(
        &c,
        "q",
        "u",
        json!({"type":"text","text":"visible","questions":[{"id":"secret","secret":true}],"answers":{"secret":"authored-secret-marker"}}),
    );
    export(&o).unwrap();
    let r = records(&o);
    let msg = r.iter().find(|r| r["messageId"] == "u").unwrap();
    assert_eq!(msg["content"].as_array().unwrap().len(), 1);
    assert_eq!(msg["content"][0]["text"], "visible");
    assert!(!std::fs::read_to_string(path(&o))
        .unwrap()
        .contains("authored-secret-marker"));
    assert!(r.iter().any(|r| r["type"] == "ai.opencode/ignored_part"));
}

#[test]
fn kiro_oversized_row_retains_conversation_identity_and_emits_gap() {
    let (_d, mut o, c) = kiro(false);
    o.max_record_bytes = 1024;
    c.execute(
        "INSERT INTO conversations_v2 VALUES('/workspace','uuid',?1,1000,2000)",
        [json!({"history":[],"extra":"x".repeat(8192)}).to_string()],
    )
    .unwrap();
    let result = export(&o).unwrap();
    assert_eq!(result.sessions, 1);
    assert_eq!(result.gaps, 1);
    assert_eq!(result.unidentified_sources, 0);
}

#[test]
fn sqlite_todos_form_one_complete_plan_and_preserve_native_priority() {
    let (_d, o, c) = database(Provider::OpenCode);
    c.execute_batch("CREATE TABLE todo(session_id TEXT,position INTEGER,content TEXT,status TEXT,priority TEXT);
        INSERT INTO todo VALUES('session',1,'verify','pending','low');
        INSERT INTO todo VALUES('session',0,'implement','completed','high');").unwrap();
    export(&o).unwrap();
    let r = records(&o);
    let plans: Vec<_> = r.iter().filter(|r| r["type"] == "plan").collect();
    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0]["entries"].as_array().unwrap().len(), 2);
    assert_eq!(plans[0]["entries"][0]["content"], "implement");
    assert_eq!(plans[0]["entries"][1]["priority"], "low");
    assert_eq!(
        r.iter().filter(|r| r["type"] == "ai.opencode/todo").count(),
        2
    );
}

#[test]
fn sqlite_oversized_todo_aggregate_is_not_a_partial_plan() {
    let (_d, mut o, c) = database(Provider::OpenCode);
    o.max_record_bytes = 1024;
    c.execute_batch("CREATE TABLE todo(session_id TEXT,position INTEGER,content TEXT,status TEXT,priority TEXT)").unwrap();
    for i in 0..4 {
        c.execute(
            "INSERT INTO todo VALUES('session',?1,?2,'pending','low')",
            params![i, "x".repeat(500)],
        )
        .unwrap();
    }
    assert_eq!(export(&o).unwrap().gaps, 1);
    let r = records(&o);
    assert!(!r.iter().any(|r| r["type"] == "plan"));
    assert_eq!(
        r.iter().filter(|r| r["type"] == "ai.opencode/todo").count(),
        4
    );
}

#[test]
fn sqlite_scalar_event_data_is_a_gap_instead_of_a_panic() {
    let (_d, o, c) = database(Provider::OpenCode);
    c.execute(
        "INSERT INTO session_message VALUES('a','session','assistant',0,1000,1000,'[]')",
        [],
    )
    .unwrap();
    assert_eq!(export(&o).unwrap().gaps, 1);
}
