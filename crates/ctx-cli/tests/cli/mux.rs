#[allow(unused_imports)]
use super::*;

pub(crate) fn install_default_mux_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_mux_fixture(temp, query));
    copy_dir_all(&source, &temp.path().join(".mux").join("sessions"));
}

pub(crate) fn write_native_mux_fixture(temp: &TempDir, query: &str) -> String {
    let root = temp.path().join("native-mux/sessions");
    let session_dir = root.join("mux-cli-native");
    let child_dir = session_dir
        .join("subagent-transcripts")
        .join("mux-cli-child");
    fs::create_dir_all(&child_dir).unwrap();
    fs::write(
        session_dir.join("metadata.json"),
        json!({
            "workspaceId": "mux-cli-native",
            "projectPath": "/workspace/mux",
            "model": "gpt-5-test"
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        session_dir.join("chat.jsonl"),
        format!(
            "{}\n{}\n{}\n",
            json!({
                "id": "msg-mux-cli-user",
                "role": "user",
                "parts": [{"type": "text", "text": query, "timestamp": 1783180800000_i64}],
                "createdAt": "2026-07-04T16:00:00.000Z",
                "metadata": {"historySequence": 0, "timestamp": 1783180800000_i64, "model": "gpt-5-test"},
                "workspaceId": "mux-cli-native"
            }),
            json!({
                "id": "msg-mux-cli-tool-call",
                "role": "assistant",
                "parts": [
                    {"type": "text", "text": "mux cli native import ok", "timestamp": 1783180801000_i64},
                    {
                        "type": "dynamic-tool",
                        "toolCallId": "call-mux-cli",
                        "toolName": "file_write",
                        "input": {"path": "src/mux_native.rs", "content": "proof"},
                        "state": "input-available",
                        "timestamp": 1783180801000_i64
                    }
                ],
                "createdAt": "2026-07-04T16:00:01.000Z",
                "metadata": {"historySequence": 1, "timestamp": 1783180801000_i64, "model": "gpt-5-test"},
                "workspaceId": "mux-cli-native"
            }),
            json!({
                "id": "msg-mux-cli-tool-output",
                "role": "assistant",
                "parts": [{
                    "type": "dynamic-tool",
                    "toolCallId": "call-mux-cli",
                    "toolName": "file_write",
                    "input": {"path": "src/mux_native.rs", "content": "proof"},
                    "state": "output-available",
                    "output": {"path": "src/mux_native.rs", "ok": true},
                    "timestamp": 1783180802000_i64
                }],
                "createdAt": "2026-07-04T16:00:02.000Z",
                "metadata": {"historySequence": 2, "timestamp": 1783180802000_i64, "model": "gpt-5-test"},
                "workspaceId": "mux-cli-native"
            })
        ),
    )
    .unwrap();
    fs::write(
        session_dir.join("partial.json"),
        json!({
            "id": "msg-mux-cli-partial",
            "role": "assistant",
            "parts": [{"type": "text", "text": "mux cli partial searchable", "timestamp": 1783180803000_i64}],
            "createdAt": "2026-07-04T16:00:03.000Z",
            "metadata": {"historySequence": 3, "timestamp": 1783180803000_i64, "model": "gpt-5-test", "partial": true},
            "workspaceId": "mux-cli-native"
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        child_dir.join("metadata.json"),
        json!({
            "childTaskId": "mux-cli-child",
            "parentWorkspaceId": "mux-cli-native",
            "projectPath": "/workspace/mux",
            "model": "gpt-5-test"
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        child_dir.join("chat.jsonl"),
        format!(
            "{}\n{}\n",
            json!({
                "id": "msg-mux-cli-child-user",
                "role": "user",
                "parts": [{"type": "text", "text": "mux child prompt", "timestamp": 1783180804000_i64}],
                "createdAt": "2026-07-04T16:00:04.000Z",
                "metadata": {"historySequence": 0, "timestamp": 1783180804000_i64, "model": "gpt-5-test"},
                "workspaceId": "mux-cli-child"
            }),
            json!({
                "id": "msg-mux-cli-child-assistant",
                "role": "assistant",
                "parts": [{"type": "text", "text": "mux child finished", "timestamp": 1783180805000_i64}],
                "createdAt": "2026-07-04T16:00:05.000Z",
                "metadata": {"historySequence": 1, "timestamp": 1783180805000_i64, "model": "gpt-5-test"},
                "workspaceId": "mux-cli-child"
            })
        ),
    )
    .unwrap();
    root.to_str().unwrap().to_owned()
}
