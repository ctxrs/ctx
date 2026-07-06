#[allow(unused_imports)]
use super::*;

pub(crate) fn install_default_mistral_vibe_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_mistral_vibe_fixture(temp, query));
    copy_dir_all(
        &source,
        &temp.path().join(".vibe").join("logs").join("session"),
    );
}

pub(crate) fn write_native_mistral_vibe_fixture(temp: &TempDir, query: &str) -> String {
    let session_dir = temp
        .path()
        .join("native-mistral-vibe/logs/session/session_20260704_160000_vibecli");
    fs::create_dir_all(&session_dir).unwrap();
    fs::write(
        session_dir.join("meta.json"),
        json!({
            "session_id": "mistral-vibe-cli-native",
            "parent_session_id": null,
            "start_time": "2026-07-04T16:00:00Z",
            "end_time": "2026-07-04T16:00:03Z",
            "git_commit": "2222222222222222222222222222222222222222",
            "git_branch": "main",
            "environment": {"working_directory": "/workspace/mistral-vibe"},
            "username": "fixture-user",
            "loops": [],
            "title": "Mistral Vibe CLI native",
            "title_source": "auto",
            "total_messages": 4,
            "stats": {"total_tokens": 64, "total_cost": 0.0},
            "agent_profile": {"name": "default", "overrides": {}}
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        session_dir.join("messages.jsonl"),
        format!(
            "{}\n{}\n{}\n{}\n",
            json!({
                "role": "user",
                "content": query,
                "message_id": "msg-mistral-vibe-user"
            }),
            json!({
                "role": "assistant",
                "content": "mistral vibe native import ok",
                "message_id": "msg-mistral-vibe-tool",
                "tool_calls": [{
                    "id": "call-mistral-vibe-cli",
                    "type": "function",
                    "function": {
                        "name": "write_file",
                        "arguments": "{\"path\":\"src/mistral_vibe_native.rs\",\"content\":\"proof\"}"
                    }
                }]
            }),
            json!({
                "role": "tool",
                "content": "wrote src/mistral_vibe_native.rs",
                "tool_call_id": "call-mistral-vibe-cli",
                "name": "write_file"
            }),
            json!({
                "role": "assistant",
                "content": "Mistral Vibe import finished",
                "message_id": "msg-mistral-vibe-final"
            })
        ),
    )
    .unwrap();
    temp.path()
        .join("native-mistral-vibe/logs/session")
        .to_str()
        .unwrap()
        .to_owned()
}
