#[allow(unused_imports)]
use super::*;

pub(crate) fn install_default_auggie_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_auggie_fixture(temp, query));
    copy_dir_all(&source, &temp.path().join(".augment").join("sessions"));
}

pub(crate) fn write_native_auggie_fixture(temp: &TempDir, query: &str) -> String {
    let root = temp.path().join("native-auggie/sessions");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("01K0AUGGIENATIVE0000000000.json"),
        serde_json::to_string_pretty(&json!({
            "sessionId": "01K0AUGGIENATIVE0000000000",
            "created": "2026-07-04T20:00:00.000Z",
            "modified": "2026-07-04T20:00:04.000Z",
            "workspaceId": "workspace-auggie-native",
            "workspaceRoot": "/workspace/auggie",
            "agentState": {
                "userGuidelines": "",
                "workspaceGuidelines": ""
            },
            "chatHistory": [
                {
                    "exchange": {
                        "request_message": query,
                        "response_text": "native Auggie import ok",
                        "request_id": "req-auggie-native-1"
                    },
                    "completed": true,
                    "sequenceId": 1,
                    "finishedAt": "2026-07-04T20:00:02.000Z",
                    "changedFiles": [],
                    "changedFilesSkipped": [],
                    "changedFilesSkippedCount": 0,
                    "isHistorySummary": false,
                    "historySummaryVersion": 0,
                    "source": "remote"
                },
                {
                    "exchange": {
                        "request_nodes": [{
                            "type": 0,
                            "text_node": {
                                "content": format!("{query} node")
                            }
                        }],
                        "response_nodes": [{
                            "type": 0,
                            "text_node": {
                                "content": "native Auggie node response"
                            }
                        }],
                        "request_id": "req-auggie-native-2"
                    },
                    "completed": true,
                    "sequenceId": 2,
                    "finishedAt": "2026-07-04T20:00:04.000Z",
                    "changedFiles": [],
                    "changedFilesSkipped": [],
                    "changedFilesSkippedCount": 0,
                    "isHistorySummary": false,
                    "historySummaryVersion": 0,
                    "source": "remote"
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    root.to_str().unwrap().to_owned()
}
