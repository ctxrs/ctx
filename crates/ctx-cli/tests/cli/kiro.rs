#[allow(unused_imports)]
use super::*;

pub(crate) fn install_default_kiro_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_kiro_fixture(temp, query));
    let target = temp.path().join(".local/share/kiro-cli");
    fs::create_dir_all(&target).unwrap();
    fs::copy(source, target.join("data.sqlite3")).unwrap();
}

pub(crate) fn write_native_kiro_fixture(temp: &TempDir, query: &str) -> String {
    let path = temp.path().join("native-kiro.sqlite3");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table conversations (
            key text primary key,
            value text
        );
        create table conversations_v2 (
            key text not null,
            conversation_id text not null,
            value text not null,
            created_at integer not null,
            updated_at integer not null,
            primary key (key, conversation_id)
        );
        create index idx_conversations_v2_key_updated on conversations_v2(key, updated_at desc);
        create index idx_conversations_v2_updated_at on conversations_v2(updated_at desc);",
    )
    .unwrap();
    let value = json!({
        "conversation_id": "kiro-cli-native",
        "history": [
            {
                "user": {
                    "timestamp": "2026-06-25T20:10:00Z",
                    "content": {
                        "Prompt": {
                            "prompt": query,
                        },
                    },
                },
                "assistant": {
                    "timestamp": "2026-06-25T20:10:03Z",
                    "Response": {
                        "content": format!("Kiro CLI response for {query}"),
                    },
                },
            },
            {
                "assistant": {
                    "timestamp": "2026-06-25T20:10:05Z",
                    "ToolUse": {
                        "content": "Inspecting Kiro CLI fixture state.",
                        "tool_uses": [
                            {
                                "id": "toolu_kiro_cli_native_1",
                                "name": "grep",
                                "args": {
                                    "pattern": query,
                                    "path": "/workspace/kiro-cli-native",
                                },
                            },
                        ],
                    },
                },
            },
        ],
    });
    conn.execute(
        "insert into conversations_v2 (key, conversation_id, value, created_at, updated_at)
         values ('/workspace/kiro-cli-native', 'kiro-cli-native', ?1, 1782418200000, 1782418205000)",
        [value.to_string()],
    )
    .unwrap();
    path.to_str().unwrap().to_owned()
}
