//! Authored protobuf envelopes for native merge/identity boundary regressions.
use super::*;
use rusqlite::params;
fn varint(mut n: u64) -> Vec<u8> {
    let mut b = Vec::new();
    while n >= 128 {
        b.push((n as u8) | 128);
        n >>= 7;
    }
    b.push(n as u8);
    b
}
fn field(n: u32, b: &[u8]) -> Vec<u8> {
    let mut out = varint((n as u64) << 3 | 2);
    out.extend(varint(b.len() as u64));
    out.extend(b);
    out
}
fn text(n: u32, s: &str) -> Vec<u8> {
    field(n, s.as_bytes())
}
fn message(id: &str, n: u32, b: &[u8]) -> Vec<u8> {
    [text(1, id), field(n, b)].concat()
}
fn task(messages: &[Vec<u8>]) -> Vec<u8> {
    let mut b = text(1, "t");
    for m in messages {
        b.extend(field(5, m));
    }
    b
}
fn setup(messages: &[Vec<u8>]) -> (tempfile::TempDir, Options, rusqlite::Connection) {
    let (d,o,db)=super::sqlite::database(Provider::Warp,"CREATE TABLE agent_conversations(id INTEGER PRIMARY KEY,conversation_id TEXT,conversation_data TEXT,last_modified_at TEXT);CREATE TABLE agent_tasks(id INTEGER PRIMARY KEY,conversation_id TEXT,task_id TEXT,task BLOB,last_modified_at TEXT);INSERT INTO agent_conversations VALUES(1,'c','{\"server_conversation_token\":\"AUTHORED-CLOUD-TOKEN\",\"agent_name\":\"Example\"}','2026-01-01 00:00:00');");
    db.execute(
        "INSERT INTO agent_tasks VALUES(1,'c','t',?1,'2026-01-01 00:00:00')",
        [task(messages)],
    )
    .unwrap();
    (d, o, db)
}
#[test]
fn warp_mcp_identity_binary_evidence_and_remote_cache() {
    let mcp = [
        text(1, "read"),
        field(2, &[]),
        text(3, "11111111-1111-4111-8111-111111111111"),
    ]
    .concat();
    let call = [text(1, "call"), field(12, &mcp)].concat();
    let success = field(1, &field(1, &text(1, "result")));
    let result = [text(1, "call"), field(16, &field(1, &success))].concat();
    let (_d, o, _db) = setup(&[
        message("u", 2, &text(1, "prompt")),
        message("call-message", 4, &call),
        message("result-message", 5, &result),
        message("a", 3, &text(1, "answer")),
    ]);
    export(&o).unwrap();
    let rows = records(&o);
    assert_eq!(rows[0]["producer"]["method"], "decoded");
    assert_eq!(rows[0]["authority"], "remote_cache");
    let call = rows.iter().find(|v| v["type"] == "tool_call").unwrap();
    assert_eq!(call["provider"]["identity"], "exact");
    assert_eq!(call["provider"]["tool"], "read");
    assert_eq!(call["input"], json!({}));
    let result = rows.iter().find(|v| v["type"] == "tool_result").unwrap();
    assert_eq!(result["linkage"], "exact");
    assert_eq!(result["raw"]["content"][0]["text"], "result");
    assert_eq!(result["capture"]["content"]["state"], "absent");
    assert!(result.get("ts").is_none());
    assert!(!rows
        .iter()
        .any(|v| v.to_string().contains("AUTHORED-CLOUD-TOKEN")));
    assert_eq!(export(&o).unwrap().unchanged, 1);
}
#[test]
fn warp_duplicate_call_ids_are_unpaired_and_message_ids_survive_rewrites() {
    let call = [text(1, "duplicate"), field(2, &text(1, "pwd"))].concat();
    let result = [text(1, "duplicate"), field(14, &[])].concat();
    let original = message("a", 3, &text(1, "first"));
    let (_d, o, db) = setup(&[
        original.clone(),
        message("c1", 4, &call),
        message("c2", 4, &call),
        message("r", 5, &result),
    ]);
    export(&o).unwrap();
    let rows = records(&o);
    let id = rows.iter().find(|v| v["messageId"] == "a").unwrap()["id"].clone();
    let result = rows.iter().find(|v| v["type"] == "tool_result").unwrap();
    assert_eq!(result["linkage"], "ambiguous");
    assert!(result.get("callId").is_none());
    assert_eq!(result["status"], "cancelled");
    db.execute(
        "UPDATE agent_tasks SET task=?1 WHERE task_id='t'",
        [task(&[
            message("new", 2, &text(1, "inserted")),
            message("a", 3, &text(1, "changed")),
        ])],
    )
    .unwrap();
    export(&o).unwrap();
    let rows = records(&o);
    assert_eq!(
        rows.iter().find(|v| v["messageId"] == "a").unwrap()["id"],
        id
    );
    assert!(!rows.iter().any(|v| v["type"] == "tool_call"));
}
#[test]
fn warp_oneof_last_arm_and_embedded_merge_and_malformed_neighbor() {
    let mut merged = text(1, "m");
    merged.extend(field(2, &text(1, "obsolete")));
    merged.extend(field(3, &text(1, "answer")));
    merged.extend(field(3, &[]));
    merged.extend(field(14, &[varint(8), varint(253_402_300_800)].concat()));
    let (_d, o, db) = setup(&[merged]);
    db.execute(
        "INSERT INTO agent_tasks VALUES(2,'c','bad',?1,'2026-01-01 00:00:00')",
        params![vec![255u8]],
    )
    .unwrap();
    export(&o).unwrap();
    let rows = records(&o);
    let m = rows.iter().find(|v| v["messageId"] == "m").unwrap();
    assert_eq!(m["role"], "assistant");
    assert!(m.get("ts").is_none());
    assert_eq!(m["displayContent"][0]["text"], "answer");
    assert!(rows.iter().any(|v| v["type"] == "gap"));
}

#[test]
fn warp_struct_embedded_values_merge_and_oneof_switch_clears_prior_value() {
    let entry = |key: &str, value: Vec<u8>| field(1, &[text(1, key), field(2, &value)].concat());
    let a = entry("a", text(3, "A"));
    let b = entry("b", text(3, "B"));
    let merged = [field(5, &a), field(5, &b)].concat();
    let switched = [field(5, &a), text(3, "clear"), field(5, &b)].concat();
    let args = [entry("merged", merged), entry("switched", switched)].concat();
    let call = [
        text(1, "c"),
        field(12, &[text(1, "read"), field(2, &args)].concat()),
    ]
    .concat();
    let (_d, o, _db) = setup(&[message("m", 4, &call)]);
    export(&o).unwrap();
    let rows = records(&o);
    assert_eq!(
        rows.iter().find(|v| v["type"] == "tool_call").unwrap()["input"],
        json!({"merged":{"a":"A","b":"B"},"switched":{"b":"B"}})
    );
}
