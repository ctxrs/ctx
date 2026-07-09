use rusqlite::{params, Connection};
use serde_json::json;
use std::{fs, path::PathBuf};
use tempfile::TempDir;

use crate::support::{copy_dir_all, provider_history_fixture};

use super::json_tree::{
    write_native_auggie_fixture, write_native_claude_fixture, write_native_continue_fixture,
    write_native_cursor_fixture, write_native_junie_fixture, write_native_mistral_vibe_fixture,
    write_native_mux_fixture, write_native_openclaw_fixture, write_native_openhands_fixture,
    write_native_qoder_fixture, write_native_rovodev_fixture, write_native_windsurf_fixture,
    write_pi_session_jsonl,
};
use super::sqlite::{
    write_lingma_sqlite_fixture, write_mimocode_sqlite_fixture, write_native_astrbot_fixture,
    write_native_forgecode_fixture, write_native_hermes_fixture, write_native_kilo_fixture,
    write_native_kiro_fixture, write_native_shelley_fixture,
};

pub(crate) fn install_default_claude_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_claude_fixture(temp, query));
    copy_dir_all(&source, &temp.path().join(".claude").join("projects"));
}

pub(crate) fn install_default_pi_fixture(temp: &TempDir, query: &str) {
    let root = temp.path().join(".pi/agent/sessions/--workspace--");
    fs::create_dir_all(&root).unwrap();
    write_pi_session_jsonl(
        &root.join("2026-06-24T12-00-00-000Z_pi-default-refresh.jsonl"),
        "pi-default-refresh",
        query,
    );
}

pub(crate) fn install_default_cursor_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_cursor_fixture(temp, query));
    copy_dir_all(&source, &temp.path().join(".cursor").join("projects"));
}

pub(crate) fn install_default_windsurf_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_windsurf_fixture(temp, query));
    copy_dir_all(&source, &temp.path().join(".windsurf").join("transcripts"));
}

pub(crate) fn install_default_qoder_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_qoder_fixture(temp, query));
    copy_dir_all(&source, &temp.path().join(".qoder").join("projects"));
}

pub(crate) fn install_default_openclaw_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_openclaw_fixture(temp, query));
    copy_dir_all(&source, &temp.path().join(".openclaw"));
}

pub(crate) fn install_default_hermes_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_hermes_fixture(temp, query));
    let target = temp.path().join(".hermes");
    fs::create_dir_all(&target).unwrap();
    fs::copy(source, target.join("state.db")).unwrap();
}

pub(crate) fn install_default_kilo_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_kilo_fixture(temp, query));
    let target = temp.path().join(".local/share/kilo");
    fs::create_dir_all(&target).unwrap();
    fs::copy(source, target.join("kilo.db")).unwrap();
}

pub(crate) fn install_default_mimocode_fixture(temp: &TempDir, query: &str) {
    let target = temp.path().join(".local/share/mimocode");
    fs::create_dir_all(&target).unwrap();
    write_mimocode_sqlite_fixture(&target.join("mimocode.db"), query, "mimocode-default");
}

pub(crate) fn install_default_kiro_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_kiro_fixture(temp, query));
    let target = temp.path().join(".local/share/kiro-cli");
    fs::create_dir_all(&target).unwrap();
    fs::copy(source, target.join("data.sqlite3")).unwrap();
}

pub(crate) fn install_default_astrbot_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_astrbot_fixture(temp, query));
    let target = temp.path().join(".astrbot/data");
    fs::create_dir_all(&target).unwrap();
    fs::copy(source, target.join("data_v4.db")).unwrap();
}

pub(crate) fn install_default_warp_fixture(temp: &TempDir) {
    let target = temp.path().join(".local/state/warp-terminal");
    fs::create_dir_all(&target).unwrap();
    fs::copy(
        provider_history_fixture("warp/v1/warp.sqlite"),
        target.join("warp.sqlite"),
    )
    .unwrap();
}

pub(crate) fn install_default_trae_cn_fixture(temp: &TempDir, query: &str) {
    let workspace = temp
        .path()
        .join("Library/Application Support/Trae CN/User/workspaceStorage/cn-workspace");
    fs::create_dir_all(&workspace).unwrap();
    fs::write(
        workspace.join("workspace.json"),
        r#"{"folder":"file:///workspace/trae-cn-default"}"#,
    )
    .unwrap();
    let conn = Connection::open(workspace.join("state.vscdb")).unwrap();
    conn.execute(
        "CREATE TABLE ItemTable ([key] TEXT PRIMARY KEY, value TEXT)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO ItemTable ([key], value) VALUES (?1, ?2)",
        params![
            "icube-ai-agent-storage-input-history",
            json!([
                {
                    "id": "input-1",
                    "inputText": query,
                    "createdAt": "2026-07-05T13:00:00Z"
                },
                {
                    "id": "input-2",
                    "text": format!("{query} follow-up"),
                    "createdAt": "2026-07-05T13:01:00Z"
                }
            ])
            .to_string()
        ],
    )
    .unwrap();
}

pub(crate) fn install_default_trae_fixture(temp: &TempDir, query: &str) {
    let workspace = temp
        .path()
        .join("Library/Application Support/Trae/User/workspaceStorage/standard-workspace");
    fs::create_dir_all(&workspace).unwrap();
    fs::write(
        workspace.join("workspace.json"),
        r#"{"folder":"file:///workspace/trae-standard-default"}"#,
    )
    .unwrap();
    let conn = Connection::open(workspace.join("state.vscdb")).unwrap();
    conn.execute(
        "CREATE TABLE ItemTable ([key] TEXT PRIMARY KEY, value TEXT)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO ItemTable ([key], value) VALUES (?1, ?2)",
        params![
            "memento/icube-ai-agent-storage",
            json!({
                "list": [
                    {
                        "id": "standard-session",
                        "title": "Standard Trae default discovery",
                        "createdAt": "2026-07-05T14:00:00Z",
                        "messages": [
                            {
                                "id": "standard-user",
                                "role": "user",
                                "content": query,
                                "createdAt": "2026-07-05T14:00:00Z"
                            },
                            {
                                "id": "standard-assistant",
                                "role": "assistant",
                                "content": format!("{query} assistant reply"),
                                "createdAt": "2026-07-05T14:01:00Z"
                            }
                        ]
                    }
                ]
            })
            .to_string()
        ],
    )
    .unwrap();
}

pub(crate) fn install_default_shelley_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_shelley_fixture(temp, query));
    let target = temp.path().join(".config/shelley");
    fs::create_dir_all(&target).unwrap();
    fs::copy(source, target.join("shelley.db")).unwrap();
}

pub(crate) fn install_default_continue_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_continue_fixture(temp, query));
    let target = temp.path().join(".continue").join("sessions");
    fs::create_dir_all(&target).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.is_file() {
            fs::copy(&path, target.join(path.file_name().unwrap())).unwrap();
        }
    }
}

pub(crate) fn install_default_forgecode_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_forgecode_fixture(temp, query));
    let target = temp.path().join(".forge");
    fs::create_dir_all(&target).unwrap();
    fs::copy(source, target.join(".forge.db")).unwrap();
}

pub(crate) fn install_default_mistral_vibe_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_mistral_vibe_fixture(temp, query));
    copy_dir_all(
        &source,
        &temp.path().join(".vibe").join("logs").join("session"),
    );
}

pub(crate) fn install_default_mux_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_mux_fixture(temp, query));
    copy_dir_all(&source, &temp.path().join(".mux").join("sessions"));
}

pub(crate) fn install_default_rovodev_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_rovodev_fixture(temp, query));
    copy_dir_all(&source, &temp.path().join(".rovodev").join("sessions"));
}

pub(crate) fn install_default_lingma_fixture(temp: &TempDir, query: &str) {
    let target = temp
        .path()
        .join(".lingma/vscode/sharedClientCache/cache/db/local.db");
    write_lingma_sqlite_fixture(&target, query);
}

pub(crate) fn install_default_auggie_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_auggie_fixture(temp, query));
    copy_dir_all(&source, &temp.path().join(".augment").join("sessions"));
}

pub(crate) fn install_default_junie_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_junie_fixture(temp, query));
    copy_dir_all(&source, &temp.path().join(".junie").join("sessions"));
}

pub(crate) fn install_default_openhands_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_openhands_fixture(temp, query));
    copy_dir_all(&source, &temp.path().join(".openhands"));
}
