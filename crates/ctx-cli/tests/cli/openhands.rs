#[allow(unused_imports)]
use super::*;

pub(crate) fn install_default_openhands_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_openhands_fixture(temp, query));
    copy_dir_all(&source, &temp.path().join(".openhands"));
}

pub(crate) fn write_native_openhands_fixture(temp: &TempDir, query: &str) -> String {
    let conversation = temp
        .path()
        .join("native-openhands/local-user/v1_conversations/12345678123456781234567812345678");
    fs::create_dir_all(&conversation).unwrap();
    fs::write(
        conversation.join("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.json"),
        serde_json::to_string_pretty(&json!({
            "id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "timestamp": "2026-06-24T12:00:00Z",
            "source": "user",
            "llm_message": {
                "role": "user",
                "content": [{"type": "text", "text": query}]
            },
            "activated_microagents": [],
            "extended_content": []
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        conversation.join("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.json"),
        serde_json::to_string_pretty(&json!({
            "id": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "timestamp": "2026-06-24T12:00:01Z",
            "source": "agent",
            "action": {
                "kind": "FileEditorAction",
                "command": "str_replace",
                "path": "openhands-cli-native-oracle.txt",
                "file_text": null,
                "old_str": "old",
                "new_str": "new",
                "insert_line": null,
                "view_range": null
            },
            "tool_name": "FileEditor",
            "tool_call_id": "call-openhands-file",
            "tool_call": {
                "id": "call-openhands-file",
                "type": "function",
                "function": {
                    "name": "FileEditor",
                    "arguments": "{\"command\":\"str_replace\"}"
                }
            },
            "llm_response_id": "response-openhands-file",
            "security_risk": "LOW",
            "thought": []
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        conversation.join("cccccccccccccccccccccccccccccccc.json"),
        serde_json::to_string_pretty(&json!({
            "id": "cccccccccccccccccccccccccccccccc",
            "timestamp": "2026-06-24T12:00:02Z",
            "source": "environment",
            "observation": {
                "kind": "FileEditorObservation",
                "command": "str_replace",
                "output": "Edited openhands-cli-native-oracle.txt",
                "path": "openhands-cli-native-oracle.txt",
                "prev_exist": true,
                "old_content": "old",
                "new_content": "new",
                "error": null
            },
            "tool_name": "FileEditor",
            "tool_call_id": "call-openhands-file",
            "action_id": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        }))
        .unwrap(),
    )
    .unwrap();
    temp.path()
        .join("native-openhands")
        .to_str()
        .unwrap()
        .to_owned()
}
