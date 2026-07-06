#[allow(unused_imports)]
use super::*;

pub(crate) fn provider_history_fixture(name: &str) -> String {
    materialized_fixture("provider-history", name)
}

pub(crate) fn redaction_fixture(name: &str) -> String {
    materialized_fixture("redaction", name)
}

pub(crate) fn write_sqlite_fixture_from_sql(sql_fixture: &str, db_path: &Path) {
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let sql = fs::read_to_string(provider_history_fixture(sql_fixture)).unwrap();
    let conn = Connection::open(db_path).unwrap();
    conn.execute_batch(&sql).unwrap();
}

pub(crate) fn install_default_continue_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_continue_fixture(temp, query));
    let target = temp.path().join(".continue").join("sessions");
    fs::create_dir_all(&target).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.is_file() {
            fs::copy(&path, target.join(path.file_name().unwrap())).unwrap();
        }
    }
}

pub(crate) fn write_native_continue_fixture(temp: &TempDir, query: &str) -> String {
    let root = temp.path().join("native-continue/sessions");
    fs::create_dir_all(&root).unwrap();
    let session_id = "continue-cli-native";
    fs::write(
        root.join("sessions.json"),
        serde_json::to_string_pretty(&json!([
            {
                "sessionId": session_id,
                "title": "native continue",
                "dateCreated": "2026-06-24T12:00:00Z",
                "workspaceDirectory": "/workspace",
                "messageCount": 1
            }
        ]))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        root.join(format!("{session_id}.json")),
        serde_json::to_string_pretty(&json!({
            "sessionId": session_id,
            "title": "native continue",
            "workspaceDirectory": "/workspace",
            "history": [
                {
                    "id": "continue-cli-native-user",
                    "timestamp": "2026-06-24T12:00:01Z",
                    "message": {
                        "role": "user",
                        "content": query
                    },
                    "contextItems": [
                        {
                            "name": "fixture.rs",
                            "content": "Continue context item marker"
                        }
                    ],
                    "editorState": query
                },
                {
                    "id": "continue-cli-native-assistant",
                    "timestamp": "2026-06-24T12:00:02Z",
                    "message": {
                        "role": "assistant",
                        "content": "native Continue import ok"
                    },
                    "toolCallStates": [
                        {
                            "toolCallId": "tool-continue-read",
                            "toolCall": {
                                "id": "tool-continue-read",
                                "type": "function",
                                "function": {
                                    "name": "readFile",
                                    "arguments": "{\"filepath\":\"fixture.rs\"}"
                                }
                            },
                            "status": "done",
                            "output": [
                                {
                                    "name": "Result",
                                    "description": "",
                                    "content": "Continue tool output marker"
                                }
                            ]
                        }
                    ]
                }
            ],
            "usage": {
                "totalCost": 0,
                "promptTokens": 12,
                "completionTokens": 8
            }
        }))
        .unwrap(),
    )
    .unwrap();
    root.to_str().unwrap().to_owned()
}
