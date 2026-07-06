use rusqlite::Connection;
use serde_json::{json, Value};
use std::{fs, io::Write, path::Path};

pub(crate) fn append_native_openclaw_event(path: &str, query: &str) {
    let transcript =
        Path::new(path).join("agents/personal-agent/sessions/openclaw-cli-native.jsonl");
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(transcript)
        .unwrap();
    writeln!(
        file,
        "{}",
        json!({
            "type": "message",
            "id": "openclaw-cli-native-incremental",
            "parentId": "openclaw-cli-native-assistant",
            "timestamp": "2026-06-24T12:00:03Z",
            "message": {"role": "user", "content": query}
        })
    )
    .unwrap();
}

pub(crate) fn append_native_hermes_event(path: &str, query: &str) {
    let conn = Connection::open(path).unwrap();
    conn.execute(
        "insert into messages (session_id, role, content, timestamp) values (?1, 'user', ?2, 1782259203.0)",
        ["hermes-cli-native", query],
    )
    .unwrap();
}

pub(crate) fn append_native_nanoclaw_event(path: &str, query: &str) {
    let conn = Connection::open(
        Path::new(path)
            .join("data/v2-sessions/ag-1/session-1")
            .join("inbound.db"),
    )
    .unwrap();
    conn.execute(
        "insert into messages_in values (
            'in-2', 1, 'chat', 1782259203000, 'done', 'message',
            'chat-1', 'telegram', 'thread-1', ?1, null, 0
        )",
        [json!({"text": query}).to_string()],
    )
    .unwrap();
}

pub(crate) fn append_native_astrbot_event(path: &str, query: &str) {
    let conn = Connection::open(path).unwrap();
    let content: String = conn
        .query_row(
            "select content from conversations where id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let mut content: Value = serde_json::from_str(&content).unwrap();
    content
        .as_array_mut()
        .unwrap()
        .push(json!({"role": "assistant", "content": query}));
    conn.execute(
        "update conversations set content = ?1, updated_at = 1782259203000 where id = 1",
        [content.to_string()],
    )
    .unwrap();
}

pub(crate) fn append_native_shelley_event(path: &str, query: &str) {
    let conn = Connection::open(path).unwrap();
    conn.execute(
        "insert into messages (
            message_id, conversation_id, sequence_id, type, user_data, created_at
        ) values (
            'shelley-cli-native-user-2', 'shelley-cli-native', 3, 'user', ?1,
            '2026-06-24 12:00:03'
        )",
        [json!({"Content": [{"Type": 2, "Text": query}]}).to_string()],
    )
    .unwrap();
}
