#[allow(unused_imports)]
use super::*;

pub(crate) fn write_native_qwen_fixture(temp: &TempDir, query: &str) -> String {
    let chats = temp
        .path()
        .join("native-qwen/.qwen/projects/workspace-qwen/chats");
    fs::create_dir_all(&chats).unwrap();
    fs::write(
        chats.join("qwen-cli-native.jsonl"),
        format!(
            "{}\n{}\n{}\n",
            json!({
                "uuid": "qwen-cli-native-user",
                "parentUuid": null,
                "sessionId": "qwen-cli-native",
                "timestamp": "2026-07-04T12:00:00Z",
                "type": "user",
                "cwd": "/workspace/qwen",
                "version": "test",
                "gitBranch": "main",
                "message": {"role": "user", "content": [{"type": "text", "text": query}]},
                "model": "qwen3-coder"
            }),
            json!({
                "uuid": "qwen-cli-native-assistant",
                "parentUuid": "qwen-cli-native-user",
                "sessionId": "qwen-cli-native",
                "timestamp": "2026-07-04T12:00:01Z",
                "type": "assistant",
                "cwd": "/workspace/qwen",
                "version": "test",
                "gitBranch": "main",
                "message": {
                    "role": "assistant",
                    "content": [
                        {"type": "text", "text": "native Qwen import ok"},
                        {"type": "tool_use", "id": "tool-1", "name": "Write", "input": {"path": "src/qwen_cli_native.txt", "content": "proof"}}
                    ]
                },
                "usageMetadata": {"inputTokens": 5, "outputTokens": 7},
                "model": "qwen3-coder"
            }),
            json!({
                "uuid": "qwen-cli-native-tool",
                "parentUuid": "qwen-cli-native-assistant",
                "sessionId": "qwen-cli-native",
                "timestamp": "2026-07-04T12:00:02Z",
                "type": "tool_result",
                "cwd": "/workspace/qwen",
                "version": "test",
                "gitBranch": "main",
                "message": {"role": "tool", "content": [{"type": "tool_result", "tool_use_id": "tool-1", "content": "wrote src/qwen_cli_native.txt"}]},
                "toolCallResult": {"tool": "Write", "path": "src/qwen_cli_native.txt", "output": "ok"},
                "model": "qwen3-coder"
            })
        ),
    )
    .unwrap();
    temp.path()
        .join("native-qwen/.qwen/projects")
        .to_str()
        .unwrap()
        .to_owned()
}
