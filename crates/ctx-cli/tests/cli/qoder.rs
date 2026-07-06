#[allow(unused_imports)]
use super::*;

pub(crate) fn install_default_qoder_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_qoder_fixture(temp, query));
    copy_dir_all(&source, &temp.path().join(".qoder").join("projects"));
}

pub(crate) fn write_native_qoder_fixture(temp: &TempDir, query: &str) -> String {
    let root = temp
        .path()
        .join("native-qoder/projects/sanitized-workspace/transcript");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("qoder-cli-native.jsonl"),
        format!(
            "{}\n{}\n{}\n{}\n{}\n",
            json!({
                "type": "session_meta",
                "sessionId": "qoder-cli-native",
                "uuid": "qoder-cli-meta",
                "timestamp": "2026-07-01T12:00:00Z",
                "cwd": "/workspace/qoder-cli",
                "data": {
                    "meta_type": "session_info",
                    "content": {"mode": "agent", "session_type": "assistant"}
                }
            }),
            json!({
                "type": "user",
                "sessionId": "qoder-cli-native",
                "uuid": "qoder-cli-user",
                "timestamp": "2026-07-01T12:00:01Z",
                "cwd": "/workspace/qoder-cli",
                "message": {"role": "user", "content": query}
            }),
            json!({
                "type": "assistant",
                "sessionId": "qoder-cli-native",
                "uuid": "qoder-cli-assistant",
                "timestamp": "2026-07-01T12:00:02Z",
                "cwd": "/workspace/qoder-cli",
                "message": {
                    "role": "assistant",
                    "content": [{"type": "text", "text": "qoder native import ok"}]
                }
            }),
            json!({
                "type": "assistant",
                "sessionId": "qoder-cli-native",
                "uuid": "qoder-cli-tool",
                "timestamp": "2026-07-01T12:00:03Z",
                "cwd": "/workspace/qoder-cli",
                "message": {
                    "role": "assistant",
                    "content": [{
                        "type": "tool_use",
                        "id": "call-qoder-cli-read",
                        "name": "read_file",
                        "input": {"file_path": "src/qoder_cli_native.py"}
                    }]
                }
            }),
            json!({
                "type": "user",
                "sessionId": "qoder-cli-native",
                "uuid": "qoder-cli-tool-result",
                "timestamp": "2026-07-01T12:00:04Z",
                "cwd": "/workspace/qoder-cli",
                "message": {
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": "call-qoder-cli-read",
                        "content": "native qoder fixture result",
                        "is_error": false
                    }]
                },
                "toolUseResult": "native qoder fixture result"
            })
        ),
    )
    .unwrap();
    temp.path()
        .join("native-qoder/projects")
        .to_str()
        .unwrap()
        .to_owned()
}
