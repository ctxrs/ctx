#[allow(unused_imports)]
use super::*;

pub(crate) fn install_default_rovodev_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_rovodev_fixture(temp, query));
    copy_dir_all(&source, &temp.path().join(".rovodev").join("sessions"));
}

pub(crate) fn write_native_rovodev_fixture(temp: &TempDir, query: &str) -> String {
    let session = temp
        .path()
        .join("native-rovodev/sessions/rovodev-cli-native");
    fs::create_dir_all(&session).unwrap();
    fs::write(
        session.join("metadata.json"),
        json!({
            "session_id": "rovodev-cli-native",
            "title": "Rovo Dev CLI native",
            "workspace_path": "/workspace/rovodev",
            "created_at": "2026-07-04T18:20:00Z",
            "updated_at": "2026-07-04T18:20:02Z"
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        session.join("session_context.json"),
        json!({
            "message_history": [
                {
                    "id": "rovodev-cli-native-user",
                    "role": "user",
                    "created_at": "2026-07-04T18:20:00Z",
                    "parts": [{"kind": "text", "text": query}]
                },
                {
                    "id": "rovodev-cli-native-assistant",
                    "role": "assistant",
                    "created_at": "2026-07-04T18:20:01Z",
                    "parts": [
                        {"kind": "text", "text": "rovodev native import ok"},
                        {"kind": "tool_use", "name": "Write", "input": {"path": "src/rovodev_cli_native.txt", "content": "proof"}}
                    ]
                },
                {
                    "id": "rovodev-cli-native-tool",
                    "role": "tool",
                    "created_at": "2026-07-04T18:20:02Z",
                    "parts": [{"kind": "tool_result", "content": "wrote src/rovodev_cli_native.txt"}]
                }
            ]
        })
        .to_string(),
    )
    .unwrap();
    temp.path()
        .join("native-rovodev/sessions")
        .to_str()
        .unwrap()
        .to_owned()
}
