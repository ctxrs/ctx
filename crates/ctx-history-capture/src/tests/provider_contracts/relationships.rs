use crate::tests::support::fixtures::sqlite::write_opencode_smoke_db;
use crate::tests::support::paths::{provider_history_fixture, tempdir};
use crate::tests::support::provider_state::stored_provider_session_id;
use crate::{
    import_crush_sqlite, import_hermes_sqlite, import_kilo_sqlite, import_mimocode_sqlite,
    import_mistral_vibe_history, import_rovodev_history, import_warp_sqlite,
    CrushSqliteImportOptions, HermesSqliteImportOptions, KiloSqliteImportOptions,
    MiMoCodeSqliteImportOptions, MistralVibeImportOptions, ProviderImportSummary,
    RovoDevImportOptions, WarpSqliteImportOptions,
};
use ctx_history_core::CaptureProvider;
use ctx_history_store::Store;
use rusqlite::Connection;
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

#[test]
fn native_parent_child_edges_import_for_claimed_provider_shapes() {
    let temp = tempdir();

    let kilo = write_opencode_smoke_db(&temp, false);
    assert_imports_parent_child_edge(
        "Kilo",
        CaptureProvider::Kilo,
        "opencode-root",
        "opencode-child",
        |store| {
            import_kilo_sqlite(
                &kilo,
                store,
                KiloSqliteImportOptions {
                    source_path: Some(kilo.clone()),
                    ..KiloSqliteImportOptions::default()
                },
            )
            .unwrap()
        },
    );

    let mimocode = temp.path().join("mimocode-edge.db");
    fs::copy(&kilo, &mimocode).unwrap();
    assert_imports_parent_child_edge(
        "MiMo Code",
        CaptureProvider::MiMoCode,
        "opencode-root",
        "opencode-child",
        |store| {
            import_mimocode_sqlite(
                &mimocode,
                store,
                MiMoCodeSqliteImportOptions {
                    source_path: Some(mimocode.clone()),
                    ..MiMoCodeSqliteImportOptions::default()
                },
            )
            .unwrap()
        },
    );

    let crush = write_crush_edge_db(&temp);
    assert_imports_parent_child_edge(
        "Crush",
        CaptureProvider::Crush,
        "crush-edge-root",
        "crush-edge-child",
        |store| {
            import_crush_sqlite(
                &crush,
                store,
                CrushSqliteImportOptions {
                    source_path: Some(crush.clone()),
                    ..CrushSqliteImportOptions::default()
                },
            )
            .unwrap()
        },
    );

    let hermes = write_hermes_edge_db(&temp);
    assert_imports_parent_child_edge(
        "Hermes",
        CaptureProvider::Hermes,
        "hermes-edge-root",
        "hermes-edge-child",
        |store| {
            import_hermes_sqlite(
                &hermes,
                store,
                HermesSqliteImportOptions {
                    source_path: Some(hermes.clone()),
                    ..HermesSqliteImportOptions::default()
                },
            )
            .unwrap()
        },
    );

    let warp = write_warp_edge_db(&temp);
    assert_imports_parent_child_edge(
        "Warp",
        CaptureProvider::Warp,
        "warp-conversation-1",
        "warp-child-conversation",
        |store| {
            import_warp_sqlite(
                &warp,
                store,
                WarpSqliteImportOptions {
                    source_path: Some(warp.clone()),
                    ..WarpSqliteImportOptions::default()
                },
            )
            .unwrap()
        },
    );

    let mistral = write_mistral_vibe_edge_fixture(&temp);
    assert_imports_parent_child_edge(
        "Mistral Vibe",
        CaptureProvider::MistralVibe,
        "mistral-edge-root",
        "mistral-edge-child",
        |store| {
            import_mistral_vibe_history(
                &mistral,
                store,
                MistralVibeImportOptions {
                    source_path: Some(mistral.clone()),
                    ..MistralVibeImportOptions::default()
                },
            )
            .unwrap()
        },
    );

    let rovodev = write_rovodev_edge_fixture(&temp);
    assert_imports_parent_child_edge(
        "Rovo Dev",
        CaptureProvider::RovoDev,
        "rovodev-edge-root",
        "rovodev-edge-child",
        |store| {
            import_rovodev_history(
                &rovodev,
                store,
                RovoDevImportOptions {
                    source_path: Some(rovodev.clone()),
                    ..RovoDevImportOptions::default()
                },
            )
            .unwrap()
        },
    );
}

fn assert_imports_parent_child_edge(
    label: &str,
    provider: CaptureProvider,
    parent_external_id: &str,
    child_external_id: &str,
    run_import: impl FnOnce(&mut Store) -> ProviderImportSummary,
) {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = run_import(&mut store);
    assert_eq!(summary.failed, 0, "{label}: {:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 2, "{label}: {summary:?}");
    assert_eq!(summary.imported_edges, 1, "{label}: {summary:?}");
    let parent_id = stored_provider_session_id(&store, provider, parent_external_id);
    let child_id = stored_provider_session_id(&store, provider, child_external_id);
    assert_eq!(
        store.get_session(child_id).unwrap().parent_session_id,
        Some(parent_id),
        "{label}: child session did not point at parent"
    );
}

fn write_crush_edge_db(temp: &TempDir) -> PathBuf {
    let path = temp.path().join("crush-edge.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table sessions (
            id text primary key,
            parent_session_id text,
            title text,
            prompt_tokens integer,
            completion_tokens integer,
            cost real,
            created_at integer not null,
            updated_at integer not null,
            summary_message_id text
        );
        create table messages (
            id text primary key,
            session_id text not null,
            role text not null,
            parts text not null default '[]',
            created_at integer not null,
            updated_at integer not null,
            provider text,
            model text,
            is_summary_message integer not null default 0
        );
        create table files (
            id text primary key,
            session_id text not null,
            path text not null,
            version text,
            created_at integer not null,
            updated_at integer not null
        );
        create table read_files (
            session_id text not null,
            path text not null,
            read_at integer not null
        );",
    )
    .unwrap();
    conn.execute(
        "insert into sessions values (?1, null, 'root', 1, 1, 0.0, 1782259200000, 1782259201000, null)",
        ["crush-edge-root"],
    )
    .unwrap();
    conn.execute(
        "insert into sessions values (?1, ?2, 'child', 1, 1, 0.0, 1782259202000, 1782259203000, null)",
        ["crush-edge-child", "crush-edge-root"],
    )
    .unwrap();
    conn.execute(
        "insert into messages values (?1, ?2, 'user', ?3, 1782259200000, 1782259200000, null, null, 0)",
        rusqlite::params![
            "crush-edge-root-msg",
            "crush-edge-root",
            json!([{"type": "text", "text": "crush edge root oracle"}]).to_string(),
        ],
    )
    .unwrap();
    conn.execute(
        "insert into messages values (?1, ?2, 'assistant', ?3, 1782259202000, 1782259202000, null, null, 0)",
        rusqlite::params![
            "crush-edge-child-msg",
            "crush-edge-child",
            json!([{"type": "text", "text": "crush edge child oracle"}]).to_string(),
        ],
    )
    .unwrap();
    path
}

fn write_hermes_edge_db(temp: &TempDir) -> PathBuf {
    let path = temp.path().join("hermes-edge.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table sessions (
            id text primary key,
            source text not null,
            parent_session_id text,
            started_at real not null,
            cwd text
        );
        create table messages (
            id integer primary key autoincrement,
            session_id text not null,
            role text not null,
            content text,
            timestamp real not null,
            active integer not null default 1,
            compacted integer not null default 0
        );",
    )
    .unwrap();
    conn.execute(
        "insert into sessions values (?1, 'acp', null, 1782259200.0, '/workspace/hermes')",
        ["hermes-edge-root"],
    )
    .unwrap();
    conn.execute(
        "insert into sessions values (?1, 'acp', ?2, 1782259202.0, '/workspace/hermes')",
        ["hermes-edge-child", "hermes-edge-root"],
    )
    .unwrap();
    conn.execute(
        "insert into messages (session_id, role, content, timestamp) values (?1, 'user', 'hermes edge root oracle', 1782259201.0)",
        ["hermes-edge-root"],
    )
    .unwrap();
    conn.execute(
        "insert into messages (session_id, role, content, timestamp) values (?1, 'assistant', 'hermes edge child oracle', 1782259203.0)",
        ["hermes-edge-child"],
    )
    .unwrap();
    path
}

fn write_warp_edge_db(temp: &TempDir) -> PathBuf {
    let fixture = provider_history_fixture("warp/v1/warp.sqlite");
    let path = temp.path().join("warp-edge.sqlite");
    fs::copy(&fixture, &path).unwrap();
    let conn = Connection::open(&path).unwrap();
    let task: Vec<u8> = conn
        .query_row(
            "select task from agent_tasks where conversation_id = 'warp-conversation-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    conn.execute(
        "insert into agent_conversations (conversation_id, conversation_data, last_modified_at)
         values (?1, ?2, '2026-06-24 12:00:05')",
        [
            "warp-child-conversation",
            r#"{"agent_name":"Warp child","parent_conversation_id":"warp-conversation-1"}"#,
        ],
    )
    .unwrap();
    conn.execute(
        "insert into agent_tasks (conversation_id, task_id, task, last_modified_at)
         values (?1, ?2, ?3, '2026-06-24 12:00:06')",
        rusqlite::params!["warp-child-conversation", "warp-child-task", task],
    )
    .unwrap();
    path
}

fn write_mistral_vibe_edge_fixture(temp: &TempDir) -> PathBuf {
    let root = temp.path().join("mistral-edge/logs/session");
    write_mistral_vibe_session(&root, "root", "mistral-edge-root", None);
    write_mistral_vibe_session(
        &root,
        "child",
        "mistral-edge-child",
        Some("mistral-edge-root"),
    );
    root
}

fn write_mistral_vibe_session(
    root: &Path,
    dir_name: &str,
    session_id: &str,
    parent_session_id: Option<&str>,
) {
    let session = root.join(dir_name);
    fs::create_dir_all(&session).unwrap();
    fs::write(
        session.join("meta.json"),
        json!({
            "session_id": session_id,
            "parent_session_id": parent_session_id,
            "start_time": "2026-07-04T19:05:00Z",
            "environment": {"working_directory": "/workspace/mistral-edge"}
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        session.join("messages.jsonl"),
        format!(
            "{}\n",
            json!({
                "role": "user",
                "content": format!("{session_id} oracle"),
                "message_id": format!("{session_id}-msg")
            })
        ),
    )
    .unwrap();
}

fn write_rovodev_edge_fixture(temp: &TempDir) -> PathBuf {
    let root = temp.path().join("rovodev-edge/sessions");
    write_rovodev_session(&root, "rovodev-edge-root", None);
    write_rovodev_session(&root, "rovodev-edge-child", Some("rovodev-edge-root"));
    root
}

fn write_rovodev_session(root: &Path, session_id: &str, parent_session_id: Option<&str>) {
    let session = root.join(session_id);
    fs::create_dir_all(&session).unwrap();
    fs::write(
        session.join("metadata.json"),
        json!({
            "session_id": session_id,
            "parent_session_id": parent_session_id,
            "workspace_path": "/workspace/rovodev-edge",
            "created_at": "2026-07-04T18:20:00Z"
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        session.join("session_context.json"),
        json!({
            "message_history": [{
                "id": format!("{session_id}-msg"),
                "role": "user",
                "created_at": "2026-07-04T18:20:00Z",
                "parts": [{"kind": "text", "text": format!("{session_id} oracle")}]
            }]
        })
        .to_string(),
    )
    .unwrap();
}
