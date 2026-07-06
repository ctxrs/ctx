#[allow(unused_imports)]
use super::*;

pub(crate) fn write_native_codebuddy_fixture(temp: &TempDir, query: &str) -> String {
    let root = temp.path().join("native-codebuddy/CodeBuddyExtension");
    let project = root.join("Data/VSCode/default/history/11112222333344445555666677778888");
    let session = project.join("session-cli");
    let messages = session.join("messages");
    fs::create_dir_all(&messages).unwrap();
    fs::write(
        project.join("index.json"),
        json!({
            "conversations": [{
                "id": "session-cli",
                "type": "chat",
                "name": "CodeBuddy CLI fixture",
                "createdAt": "2026-07-04T14:00:00Z",
                "lastMessageAt": "2026-07-04T14:00:02Z"
            }],
            "current": "session-cli"
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        session.join("index.json"),
        json!({
            "messages": [
                {"id": "msg-user", "role": "user", "type": "message"},
                {"id": "msg-assistant", "role": "assistant", "type": "message"}
            ]
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        messages.join("msg-user.json"),
        json!({
            "id": "msg-user",
            "role": "user",
            "message": json!({
                "content": [{"type": "text", "text": query}],
                "createdAt": "2026-07-04T14:00:01Z"
            }).to_string()
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        messages.join("msg-assistant.json"),
        json!({
            "id": "msg-assistant",
            "role": "assistant",
            "message": json!({
                "content": "CodeBuddy CLI native import ok",
                "createdAt": "2026-07-04T14:00:02Z"
            }).to_string()
        })
        .to_string(),
    )
    .unwrap();
    root.to_str().unwrap().to_owned()
}
