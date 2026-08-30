use serde_json::json;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use tempfile::TempDir;

use crate::support::{copy_dir_all, provider_history_fixture, write_codex_message_fixture};

use super::{
    install_default_astrbot_fixture, install_default_auggie_fixture,
    install_default_claude_fixture, install_default_continue_fixture,
    install_default_cursor_fixture, install_default_forgecode_fixture,
    install_default_hermes_fixture, install_default_junie_fixture, install_default_kilo_fixture,
    install_default_kiro_fixture, install_default_lingma_fixture, install_default_mimocode_fixture,
    install_default_mistral_vibe_fixture, install_default_mux_fixture,
    install_default_openclaw_fixture, install_default_openhands_fixture,
    install_default_qoder_fixture, install_default_rovodev_fixture,
    install_default_shelley_fixture, install_default_warp_fixture,
    write_native_codebuddy_cli_jsonl_fixture, write_native_copilot_fixture,
    write_native_factory_droid_fixture, write_native_firebender_fixture,
    write_native_gemini_fixture, write_native_kimi_code_cli_wire_fixture,
    write_native_opencode_fixture, write_native_qwen_fixture, write_pi_session_jsonl,
    write_xopc_sqlite_fixture,
};

pub(crate) fn install_provider_default_fixture(
    temp: &TempDir,
    workspace: &Path,
    matrix_id: &str,
    user_text: &str,
    assistant_text: &str,
) {
    match matrix_id {
        "codex" => install_codex(temp, user_text, assistant_text),
        "deepseek_harness" => install_fixture_tree(
            "deepseek-harness/v0/raw/sessions",
            &temp.path().join(".dsh/sessions"),
        ),
        "grok_build" => install_fixture_tree(
            "grok-build/v1.0.3/sessions",
            &temp.path().join(".grok/sessions"),
        ),
        "pi" => install_pi(temp, user_text, assistant_text),
        "claude_code" => install_default_claude_fixture(temp, user_text),
        "open_code" => install_opencode(temp, user_text),
        "kilo" => install_kilo(temp, user_text),
        "mimocode" => install_mimocode(temp, user_text),
        "kiro_cli" => install_default_kiro_fixture(temp, user_text),
        "crush" => install_fixture_file("crush/v1/crush.db", &workspace.join(".crush/crush.db")),
        "goose" => install_fixture_file(
            "goose/v15/sessions.db",
            &temp.path().join(".local/share/goose/sessions/sessions.db"),
        ),
        "lingma" => install_default_lingma_fixture(temp, user_text),
        "qoder" => install_default_qoder_fixture(temp, user_text),
        "warp" => install_default_warp_fixture(temp),
        "codebuddy" => install_codebuddy(temp, user_text),
        "openclaw" => install_openclaw(temp, user_text),
        "hermes" => install_default_hermes_fixture(temp, user_text),
        "nanoclaw" => install_nanoclaw(temp, workspace, user_text),
        "astrbot" => install_default_astrbot_fixture(temp, user_text),
        "shelley" => install_shelley(temp, workspace, user_text),
        "continue" => install_default_continue_fixture(temp, user_text),
        "openhands" => install_openhands(temp, user_text, assistant_text),
        "antigravity_cli" => install_antigravity(temp),
        "gemini_cli" => install_gemini(temp, user_text, assistant_text),
        "tabnine" => install_fixture_tree(
            "tabnine-cli/.tabnine/agent",
            &temp.path().join(".tabnine/agent"),
        ),
        "cursor" => install_default_cursor_fixture(temp, user_text),
        "zed" => install_fixture_file(
            "zed/v1/threads.db",
            &temp.path().join(".local/share/zed/threads/threads.db"),
        ),
        "copilot_cli" => install_copilot(temp, user_text, assistant_text),
        "factory_ai_droid" => install_factory(temp, user_text, assistant_text),
        "qwen_code" => install_clean_qwen(temp, user_text),
        "kimi_code_cli" => install_kimi(temp, assistant_text),
        "auggie" => install_default_auggie_fixture(temp, user_text),
        "junie" => install_default_junie_fixture(temp, user_text),
        "firebender" => install_firebender(temp, workspace, user_text),
        "xopc" => write_xopc_sqlite_fixture(
            &temp.path().join(".xopc/xopc.db"),
            user_text,
            assistant_text,
        ),
        "forgecode" => install_default_forgecode_fixture(temp, user_text),
        "deepagents" => install_fixture_file(
            "deepagents/v1/sessions.db",
            &temp.path().join(".deepagents/.state/sessions.db"),
        ),
        "mistral_vibe" => install_default_mistral_vibe_fixture(temp, user_text),
        "mux" => install_default_mux_fixture(temp, user_text),
        "rovodev" => install_default_rovodev_fixture(temp, user_text),
        "cline" => install_fixture_tree("cline/data", &temp.path().join(".cline/data")),
        "roo_code" => install_fixture_tree(
            "roo/storage",
            &temp.path().join(".vscode-mock/global-storage"),
        ),
        "fx" => install_fx(temp, user_text, assistant_text),
        other => panic!("missing default fixture installer for matrix provider {other}"),
    }
}

fn install_codex(temp: &TempDir, user_text: &str, assistant_text: &str) {
    let root = temp.path().join(".codex/sessions/2026/08/18");
    let path = write_codex_message_fixture(&root, "provider-conformance-codex", user_text);
    append_json_line(
        &path,
        &json!({
            "timestamp": "2026-08-18T12:00:02Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "id": "provider-conformance-codex-assistant",
                "role": "assistant",
                "content": [{"type": "output_text", "text": assistant_text}]
            }
        }),
    );
}

fn install_pi(temp: &TempDir, user_text: &str, assistant_text: &str) {
    let root = temp
        .path()
        .join(".pi/agent/sessions/--provider-conformance--");
    fs::create_dir_all(&root).unwrap();
    let path = root.join("2026-08-18T12-00-00-000Z_provider-conformance.jsonl");
    write_pi_session_jsonl(&path, "provider-conformance-pi", user_text);
    append_json_line(
        &path,
        &json!({
            "type": "message",
            "id": "provider-conformance-pi-assistant",
            "timestamp": "2026-08-18T12:00:02.000Z",
            "message": {
                "role": "assistant",
                "content": [{"type": "text", "text": assistant_text}]
            }
        }),
    );
}

fn install_opencode(temp: &TempDir, user_text: &str) {
    let source = PathBuf::from(write_native_opencode_fixture(temp, user_text));
    let target = temp.path().join(".local/share/opencode/opencode.db");
    copy_file(&source, &target);
    insert_opencode_family_user_part(&target, "opencode-cli-native", user_text);
}

fn install_mimocode(temp: &TempDir, user_text: &str) {
    install_default_mimocode_fixture(temp, user_text);
    insert_opencode_family_user_part(
        &temp.path().join(".local/share/mimocode/mimocode.db"),
        "mimocode-default",
        user_text,
    );
}

fn install_kilo(temp: &TempDir, user_text: &str) {
    install_default_kilo_fixture(temp, user_text);
    let conn = rusqlite::Connection::open(temp.path().join(".local/share/kilo/kilo.db")).unwrap();
    conn.execute(
        "insert into session_message values (?1, ?2, 'assistant', 1782259201000, 1782259201000, ?3)",
        (
            "kilo-cli-native-assistant",
            "kilo-cli-native",
            json!({
                "time": {"created": 1782259201000_i64},
                "text": "Kilo assistant response"
            })
            .to_string(),
        ),
    )
    .unwrap();
}

fn insert_opencode_family_user_part(path: &Path, session_id: &str, user_text: &str) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute(
        "insert into part values (?1, ?2, ?3, 1782259200001, 1782259200001, ?4)",
        (
            format!("{session_id}-user-text"),
            format!("{session_id}-user"),
            session_id,
            json!({"type": "text", "text": user_text}).to_string(),
        ),
    )
    .unwrap();
}

fn install_codebuddy(temp: &TempDir, user_text: &str) {
    let source = PathBuf::from(write_native_codebuddy_cli_jsonl_fixture(temp, user_text));
    copy_dir_all(&source, &temp.path().join(".codebuddy"));
}

fn install_openclaw(temp: &TempDir, user_text: &str) {
    install_default_openclaw_fixture(temp, user_text);
    fs::write(
        temp.path().join(".openclaw/openclaw.json"),
        json!({"agents": {"list": [{"id": "personal-agent"}]}}).to_string(),
    )
    .unwrap();
}

fn install_nanoclaw(temp: &TempDir, workspace: &Path, user_text: &str) {
    let source = PathBuf::from(super::write_native_nanoclaw_fixture(temp, user_text));
    copy_dir_all(&source.join("data"), &workspace.join("data"));
}

fn install_shelley(temp: &TempDir, workspace: &Path, user_text: &str) {
    install_default_shelley_fixture(temp, user_text);
    copy_file(
        &temp.path().join(".config/shelley/shelley.db"),
        &workspace.join("shelley.db"),
    );
}

fn install_openhands(temp: &TempDir, user_text: &str, assistant_text: &str) {
    install_default_openhands_fixture(temp, user_text);
    let conversation = temp
        .path()
        .join(".openhands/local-user/v1_conversations/12345678123456781234567812345678");
    fs::write(
        conversation.join("dddddddddddddddddddddddddddddddd.json"),
        json!({
            "id": "dddddddddddddddddddddddddddddddd",
            "timestamp": "2026-06-24T12:00:03Z",
            "kind": "MessageEvent",
            "source": "agent",
            "llm_message": {
                "role": "assistant",
                "content": [{"type": "text", "text": assistant_text}]
            }
        })
        .to_string(),
    )
    .unwrap();
}

fn install_gemini(temp: &TempDir, user_text: &str, assistant_text: &str) {
    let source = PathBuf::from(write_native_gemini_fixture(temp, user_text));
    append_json_line(
        &source.join("tmp/project/chats/session-native.jsonl"),
        &json!({
            "id": "gemini-cli-native-assistant",
            "timestamp": "2026-06-24T12:00:02Z",
            "type": "gemini",
            "content": assistant_text
        }),
    );
    copy_dir_all(&source, &temp.path().join(".gemini"));
}

fn install_antigravity(temp: &TempDir) {
    copy_file(
        Path::new(&provider_history_fixture(
            "antigravity/v1/brain/agy-success/.system_generated/logs/transcript_full.jsonl",
        )),
        &temp.path().join(
            ".gemini/antigravity-cli/brain/agy-success/.system_generated/logs/transcript.jsonl",
        ),
    );
}

fn install_copilot(temp: &TempDir, user_text: &str, assistant_text: &str) {
    let source = PathBuf::from(write_native_copilot_fixture(temp, user_text));
    append_json_line(
        &source.join("copilot-cli-native/events.jsonl"),
        &json!({
            "id": "copilot-cli-native-assistant",
            "timestamp": "2026-06-24T12:00:02Z",
            "type": "assistant.message",
            "data": {"content": assistant_text}
        }),
    );
    copy_dir_all(&source, &temp.path().join(".copilot/session-state"));
}

fn install_factory(temp: &TempDir, user_text: &str, assistant_text: &str) {
    let source = PathBuf::from(write_native_factory_droid_fixture(temp, user_text));
    append_json_line(
        &source.join("project/droid-cli-native.jsonl"),
        &json!({
            "type": "message",
            "id": "droid-cli-native-assistant",
            "timestamp": "2026-06-24T12:00:02Z",
            "message": {
                "role": "assistant",
                "content": [{"type": "text", "text": assistant_text}]
            }
        }),
    );
    copy_dir_all(&source, &temp.path().join(".factory/sessions"));
}

fn install_clean_qwen(temp: &TempDir, user_text: &str) {
    let source = PathBuf::from(write_native_qwen_fixture(temp, user_text));
    let target = temp.path().join(".qwen/projects");
    copy_dir_all(&source, &target);
    let transcript = target.join("workspace-qwen/chats/qwen-cli-native.jsonl");
    let clean = fs::read_to_string(&transcript)
        .unwrap()
        .lines()
        .take(2)
        .map(|line| format!("{line}\n"))
        .collect::<String>();
    fs::write(transcript, clean).unwrap();
}

fn install_kimi(temp: &TempDir, assistant_text: &str) {
    let source = PathBuf::from(write_native_kimi_code_cli_wire_fixture(
        temp,
        assistant_text,
    ));
    copy_dir_all(&source, &temp.path().join(".kimi-code"));
}

fn install_firebender(temp: &TempDir, workspace: &Path, user_text: &str) {
    let source = PathBuf::from(write_native_firebender_fixture(temp, user_text));
    copy_dir_all(&source.join(".idea"), &workspace.join(".idea"));
}

fn install_fx(temp: &TempDir, user_text: &str, assistant_text: &str) {
    const SESSION_ID: &str = "1700000000001-1700000000000000001-0000000000000001";
    const LOG_GENERATION: &str = "b82f00a357b44301d54300d2856e934b";

    // Preserve the public v3 capture's immediate-child tree and event shapes;
    // only replace the conformance oracles and advance its byte watermark.
    let sessions = temp.path().join(".fx/sessions");
    install_fixture_tree("fx/v0.0.6/native-v3-tool-free/.fx/sessions", &sessions);
    let session = sessions.join(SESSION_ID);
    let events_path = session.join("events.jsonl");
    let mut events = Vec::new();
    for line in fs::read_to_string(&events_path).unwrap().lines() {
        let mut event: serde_json::Value = serde_json::from_str(line).unwrap();
        match event["kind"].as_str() {
            Some("recovery_checkpoint_set") => {
                *event.pointer_mut("/payload/checkpoint/user/text").unwrap() = json!(user_text);
                *event
                    .pointer_mut("/payload/checkpoint/assistant_source")
                    .unwrap() = json!(assistant_text);
            }
            Some("history_turn_committed") => {
                *event.pointer_mut("/payload/turn/user/text").unwrap() = json!(user_text);
                *event.pointer_mut("/payload/turn/assistant").unwrap() = json!(assistant_text);
            }
            _ => {}
        }
        serde_json::to_writer(&mut events, &event).unwrap();
        events.push(b'\n');
    }
    fs::write(&events_path, &events).unwrap();

    let commit_path = session.join(format!("commit.{LOG_GENERATION}.json"));
    let mut commit: serde_json::Value =
        serde_json::from_slice(&fs::read(&commit_path).unwrap()).unwrap();
    commit["through_event_log_bytes"] = json!(u64::try_from(events.len()).unwrap());
    fs::write(commit_path, serde_json::to_vec(&commit).unwrap()).unwrap();
}

fn install_fixture_tree(name: &str, target: &Path) {
    copy_dir_all(Path::new(&provider_history_fixture(name)), target);
}

fn install_fixture_file(name: &str, target: &Path) {
    copy_file(Path::new(&provider_history_fixture(name)), target);
}

fn copy_file(source: &Path, target: &Path) {
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::copy(source, target).unwrap();
}

fn append_json_line(path: &Path, value: &serde_json::Value) {
    let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
    writeln!(file, "{value}").unwrap();
}
