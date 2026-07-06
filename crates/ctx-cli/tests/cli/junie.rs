#[allow(unused_imports)]
use super::*;

pub(crate) fn install_default_junie_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_junie_fixture(temp, query));
    copy_dir_all(&source, &temp.path().join(".junie").join("sessions"));
}

pub(crate) fn write_native_junie_fixture(temp: &TempDir, query: &str) -> String {
    let sessions = temp.path().join("native-junie/sessions");
    let session_id = "session-260607-120000-native";
    let session = sessions.join(session_id);
    fs::create_dir_all(&session).unwrap();
    fs::write(
        sessions.join("index.jsonl"),
        format!(
            "{}\n",
            json!({
                "sessionId": session_id,
                "createdAt": 1783348800000i64,
                "updatedAt": 1783348920000i64,
                "taskName": "Junie native CLI fixture",
                "projectDir": "/workspace/junie-native"
            })
        ),
    )
    .unwrap();
    fs::write(
        session.join("events.jsonl"),
        format!(
            "{}\n{}\n",
            json!({
                "kind": "UserPromptEvent",
                "prompt": query
            }),
            json!({
                "kind": "SessionA2uxEvent",
                "timestampMs": 1783348920000i64,
                "event": {
                    "agentEvent": {
                        "kind": "ResultBlockUpdatedEvent",
                        "stepId": "result-1",
                        "result": format!("Junie answered {query}")
                    }
                }
            })
        ),
    )
    .unwrap();
    sessions.to_str().unwrap().to_owned()
}
