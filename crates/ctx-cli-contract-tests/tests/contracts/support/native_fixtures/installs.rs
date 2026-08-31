use rusqlite::{params, Connection};
use serde_json::json;
use std::{fs, path::PathBuf};
use tempfile::TempDir;

use crate::support::{copy_dir_all, provider_history_fixture};

use super::json_tree::{
    write_native_auggie_fixture, write_native_claude_fixture, write_native_continue_fixture,
    write_native_cursor_fixture, write_native_junie_fixture, write_native_mistral_vibe_fixture,
    write_native_mux_fixture, write_native_openclaw_fixture, write_native_openhands_fixture,
    write_native_qoder_fixture, write_native_rovodev_fixture, write_pi_session_jsonl,
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

pub(crate) fn install_default_kiro_fixture(temp: &TempDir, query: &str) -> PathBuf {
    let source = PathBuf::from(write_native_kiro_fixture(temp, query));
    let target = if cfg!(target_os = "linux") {
        temp.path().join(".local/share/kiro-cli/data.sqlite3")
    } else if cfg!(target_os = "macos") {
        temp.path()
            .join("Library/Application Support/kiro-cli/data.sqlite3")
    } else {
        // Kiro has no released automatic legacy default on Windows. Keep an
        // exact-path fixture for tests that explicitly exercise support.
        temp.path().join("exact/kiro-cli/data.sqlite3")
    };
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::copy(source, &target).unwrap();
    target
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
