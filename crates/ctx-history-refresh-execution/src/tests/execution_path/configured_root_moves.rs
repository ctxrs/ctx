use super::*;

#[path = "configured_root_moves/continuity.rs"]
mod continuity;
#[path = "configured_root_moves/removal.rs"]
mod removal;
#[path = "configured_root_moves/replacement.rs"]
mod replacement;

fn write_codex_rollout(path: &Path, session_id: &str, marker: &str) {
    fs::create_dir_all(path).unwrap();
    fs::write(
        path.join("rollout.jsonl"),
        format!(
            "{}\n{}\n",
            json!({
                "timestamp": "2026-08-24T00:00:00Z",
                "type": "session_meta",
                "payload": {
                    "id": session_id,
                    "timestamp": "2026-08-24T00:00:00Z",
                    "cwd": "/repo/atomic-root-replacement",
                    "originator": "codex_cli_rs",
                    "cli_version": "1.0.0",
                    "source": "cli",
                    "model_provider": "openai"
                }
            }),
            json!({
                "timestamp": "2026-08-24T00:00:01Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": marker}]
                }
            })
        ),
    )
    .unwrap();
}

fn write_claude_message(path: &Path, session_id: &str, marker: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        path,
        format!(
            "{}\n",
            json!({
                "type": "user",
                "uuid": marker,
                "sessionId": session_id,
                "message": {"role": "user", "content": marker}
            })
        ),
    )
    .unwrap();
}

fn write_codex_history(path: &Path, session_id: &str, marker: &str) {
    fs::write(
        path,
        format!(
            "{}\n",
            json!({"session_id": session_id, "ts": 1_778_000_000_i64, "text": marker})
        ),
    )
    .unwrap();
}

fn write_openhands_legacy_message(root: &Path, marker: &str) {
    let path = root.join("v1_conversations/legacy-conversation/legacy-event.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        path,
        serde_json::to_vec(&json!({
            "id": "legacy-event",
            "source": "user",
            "message": marker,
            "timestamp": "2026-08-25T12:00:00Z"
        }))
        .unwrap(),
    )
    .unwrap();
}

fn write_openhands_current_message(root: &Path, marker: &str) {
    let path = root.join("current-conversation/events/event-00001-current-event.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        path,
        serde_json::to_vec(&json!({
            "id": "current-event",
            "source": "user",
            "message": marker,
            "timestamp": "2026-08-25T12:00:00Z"
        }))
        .unwrap(),
    )
    .unwrap();
}

fn write_qoder_message(path: &Path, session_id: &str, marker: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        path,
        format!(
            "{}\n{}\n",
            json!({
                "type": "session_meta",
                "sessionId": session_id,
                "uuid": format!("{session_id}-meta"),
                "timestamp": "2026-07-01T12:00:00Z",
                "cwd": "/workspace/qoder-cli",
                "data": {
                    "meta_type": "session_info",
                    "content": {"mode": "agent", "session_type": "assistant"}
                }
            }),
            json!({
                "type": "user",
                "sessionId": session_id,
                "uuid": format!("{session_id}-user"),
                "timestamp": "2026-07-01T12:00:01Z",
                "cwd": "/workspace/qoder-cli",
                "message": {"role": "user", "content": marker}
            })
        ),
    )
    .unwrap();
}
