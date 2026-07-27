use ctx_history_core::new_id;
use rusqlite::{params, Connection};

use super::fixtures::tempdir;
use crate::schema::ddl::CREATE_TABLES_SQL;
use crate::schema::indexes::INDEXES_SQL;
use crate::schema::provider_checks::{
    rebuild_capture_sources_provider_check, rebuild_catalog_sessions_provider_check,
};
use crate::{Store, SCHEMA_VERSION};

fn assert_provider_migration_accepts(
    legacy_version: i64,
    provider: &str,
    source_format: &str,
    source_root: &str,
    source_path: &str,
) {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    {
        let conn = Connection::open(&path).unwrap();
        let legacy_sql = CREATE_TABLES_SQL.replace(&format!(", '{provider}'"), "");
        conn.execute_batch(&legacy_sql).unwrap();
        conn.execute_batch(INDEXES_SQL).unwrap();
        conn.execute_batch(&format!("PRAGMA user_version = {legacy_version};"))
            .unwrap();
    }

    let store = Store::open(&path).unwrap();
    let version: i64 = store
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);

    store
        .conn
        .execute(
            r#"
            INSERT INTO capture_sources
            (id, kind, provider, machine_id, started_at_ms, fidelity)
            VALUES (?1, 'provider_import', ?2, 'test-machine', 0, 'imported')
            "#,
            params![new_id().to_string(), provider],
        )
        .unwrap();
    store
        .conn
        .execute(
            r#"
            INSERT INTO catalog_sessions
            (source_path, provider, source_format, source_root, agent_type, file_size_bytes, file_modified_at_ms, cataloged_at_ms)
            VALUES (?1, ?2, ?3, ?4, 'primary', 1, 0, 0)
            "#,
            params![source_path, provider, source_format, source_root],
        )
        .unwrap();
    store
        .conn
        .execute(
            r#"
            INSERT INTO source_import_files
            (provider, source_format, source_root, source_path, file_size_bytes, file_modified_at_ms, observed_at_ms)
            VALUES (?1, ?2, ?3, ?4, 1, 0, 0)
            "#,
            params![provider, source_format, source_root, source_path],
        )
        .unwrap();
}

#[test]
fn provider_check_constraints_accept_supported_providers() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    rebuild_capture_sources_provider_check(&store.conn).unwrap();
    rebuild_catalog_sessions_provider_check(&store.conn).unwrap();

    let schema = store.schema().unwrap();
    let providers = [
        ("codex", "codex_rollout_jsonl"),
        ("claude", "claude_projects_jsonl"),
        ("pi", "pi_sessions_jsonl"),
        ("opencode", "opencode_sqlite"),
        ("kilo", "kilo_sqlite"),
        ("mimocode", "mimocode_sqlite"),
        ("kiro_cli", "kiro_cli_sqlite"),
        ("crush", "crush_sqlite"),
        ("goose", "goose_sessions_sqlite"),
        ("antigravity", "antigravity_history"),
        ("gemini", "gemini_history"),
        ("tabnine", "tabnine_history"),
        ("cursor", "cursor_sqlite"),
        ("windsurf", "windsurf_cascade_hook_transcript_jsonl"),
        ("zed", "zed_threads_sqlite"),
        ("copilot_cli", "copilot_cli_session_events_jsonl"),
        ("factory_ai_droid", "factory_ai_droid_sessions_jsonl"),
        ("qwen_code", "qwen_code_chat_jsonl"),
        ("kimi_code_cli", "kimi_code_cli_wire_jsonl"),
        ("forgecode", "forgecode_sqlite"),
        ("deepagents", "deepagents_sessions_sqlite"),
        ("mistral_vibe", "mistral_vibe_session_jsonl"),
        ("mux", "mux_session_jsonl"),
        ("rovodev", "rovodev_session_json"),
        ("openclaw", "openclaw_session_jsonl_tree"),
        ("hermes", "hermes_state_sqlite"),
        ("nanoclaw", "nanoclaw_project"),
        ("astrbot", "astrbot_data_v4_sqlite"),
        ("shelley", "shelley_sqlite"),
        ("continue", "continue_cli_sessions_json"),
        ("openhands", "openhands_file_events"),
        ("cline", "cline_task_directory_json"),
        ("roo_code", "cline_task_directory_json"),
        ("lingma", "lingma_sqlite"),
        ("qoder", "qoder_transcript_jsonl_tree"),
        ("warp", "warp_sqlite"),
        ("codebuddy", "codebuddy_history_json"),
        ("auggie", "auggie_session_json"),
        ("firebender", "firebender_chat_history_sqlite"),
        ("junie", "junie_session_events_jsonl_tree"),
        ("trae", "trae_state_vscdb"),
        ("shell", "shell_history"),
        ("git", "git_history"),
        ("jj", "jj_history"),
        ("gh", "gh_history"),
        ("custom", "ctx_history_jsonl_v1"),
        ("unknown", "unknown"),
    ];
    for (provider, source_format) in providers {
        assert!(
            schema.contains(provider),
            "schema provider checks should include {provider}"
        );
        store
            .conn
            .execute(
                r#"
                INSERT INTO capture_sources
                (id, kind, provider, machine_id, started_at_ms, fidelity)
                VALUES (?1, 'provider_import', ?2, 'test-machine', 0, 'partial')
                "#,
                params![new_id().to_string(), provider],
            )
            .unwrap();
        store
            .conn
            .execute(
                r#"
                INSERT INTO catalog_sessions
                (source_path, provider, source_format, source_root, agent_type, file_size_bytes, file_modified_at_ms, cataloged_at_ms)
                VALUES (?1, ?2, ?3, '/tmp/provider', 'primary', 1, 0, 0)
                "#,
                params![format!("/tmp/provider/{provider}.jsonl"), provider, source_format],
            )
            .unwrap();
    }

    let source_count: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM capture_sources", [], |row| row.get(0))
        .unwrap();
    let catalog_count: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM catalog_sessions", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(source_count, providers.len() as i64);
    assert_eq!(catalog_count, providers.len() as i64);
}

#[test]
fn schema_v17_adds_jsonl_provider_checks() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    {
        let conn = Connection::open(&path).unwrap();
        let legacy_sql = CREATE_TABLES_SQL.replace(
            ", 'qwen_code', 'kimi_code_cli', 'forgecode', 'deepagents', 'mistral_vibe'",
            "",
        );
        conn.execute_batch(&legacy_sql).unwrap();
        conn.execute_batch(INDEXES_SQL).unwrap();
        conn.execute_batch("PRAGMA user_version = 16;").unwrap();
    }

    let store = Store::open(&path).unwrap();
    let version: i64 = store
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);

    for (provider, source_format) in [
        ("qwen_code", "qwen_code_chat_jsonl"),
        ("kimi_code_cli", "kimi_code_cli_wire_jsonl"),
    ] {
        store
            .conn
            .execute(
                r#"
                INSERT INTO capture_sources
                (id, kind, provider, machine_id, started_at_ms, fidelity)
                VALUES (?1, 'provider_import', ?2, 'test-machine', 0, 'imported')
                "#,
                params![new_id().to_string(), provider],
            )
            .unwrap();
        store
            .conn
            .execute(
                r#"
                INSERT INTO catalog_sessions
                (source_path, provider, source_format, source_root, agent_type, file_size_bytes, file_modified_at_ms, cataloged_at_ms)
                VALUES (?1, ?2, ?3, '/tmp/provider', 'primary', 1, 0, 0)
                "#,
                params![format!("/tmp/provider/{provider}.jsonl"), provider, source_format],
            )
            .unwrap();
        store
            .conn
            .execute(
                r#"
                INSERT INTO source_import_files
                (provider, source_format, source_root, source_path, file_size_bytes, file_modified_at_ms, observed_at_ms)
                VALUES (?1, ?2, '/tmp/provider', ?3, 1, 0, 0)
                "#,
                params![
                    provider,
                    source_format,
                    format!("/tmp/provider/{provider}.jsonl")
                ],
            )
            .unwrap();
    }
}

#[test]
fn schema_v18_adds_codebuddy_provider_checks() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    {
        let conn = Connection::open(&path).unwrap();
        let legacy_sql = CREATE_TABLES_SQL.replace(", 'codebuddy'", "");
        conn.execute_batch(&legacy_sql).unwrap();
        conn.execute_batch(INDEXES_SQL).unwrap();
        conn.execute_batch("PRAGMA user_version = 17;").unwrap();
    }

    let store = Store::open(&path).unwrap();
    let version: i64 = store
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);

    let provider = "codebuddy";
    let source_format = "codebuddy_history_json";
    store
        .conn
        .execute(
            r#"
            INSERT INTO capture_sources
            (id, kind, provider, machine_id, started_at_ms, fidelity)
            VALUES (?1, 'provider_import', ?2, 'test-machine', 0, 'imported')
            "#,
            params![new_id().to_string(), provider],
        )
        .unwrap();
    store
        .conn
        .execute(
            r#"
            INSERT INTO catalog_sessions
            (source_path, provider, source_format, source_root, agent_type, file_size_bytes, file_modified_at_ms, cataloged_at_ms)
            VALUES (?1, ?2, ?3, '/tmp/provider', 'primary', 1, 0, 0)
            "#,
            params![
                format!("/tmp/provider/{provider}/session/index.json"),
                provider,
                source_format
            ],
        )
        .unwrap();
    store
        .conn
        .execute(
            r#"
            INSERT INTO source_import_files
            (provider, source_format, source_root, source_path, file_size_bytes, file_modified_at_ms, observed_at_ms)
            VALUES (?1, ?2, '/tmp/provider', ?3, 1, 0, 0)
            "#,
            params![
                provider,
                source_format,
                format!("/tmp/provider/{provider}/session/index.json")
            ],
        )
        .unwrap();
}

#[test]
fn schema_v19_adds_zed_provider_checks() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    {
        let conn = Connection::open(&path).unwrap();
        let legacy_sql = CREATE_TABLES_SQL.replace(", 'zed'", "");
        conn.execute_batch(&legacy_sql).unwrap();
        conn.execute_batch(INDEXES_SQL).unwrap();
        conn.execute_batch("PRAGMA user_version = 18;").unwrap();
    }

    let store = Store::open(&path).unwrap();
    let version: i64 = store
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);

    store
        .conn
        .execute(
            r#"
            INSERT INTO capture_sources
            (id, kind, provider, machine_id, started_at_ms, fidelity)
            VALUES (?1, 'provider_import', 'zed', 'test-machine', 0, 'imported')
            "#,
            params![new_id().to_string()],
        )
        .unwrap();
    store
        .conn
        .execute(
            r#"
            INSERT INTO catalog_sessions
            (source_path, provider, source_format, source_root, agent_type, file_size_bytes, file_modified_at_ms, cataloged_at_ms)
            VALUES ('/tmp/zed/threads.db', 'zed', 'zed_threads_sqlite', '/tmp/zed/threads.db', 'primary', 1, 0, 0)
            "#,
            [],
        )
        .unwrap();
    store
        .conn
        .execute(
            r#"
            INSERT INTO source_import_files
            (provider, source_format, source_root, source_path, file_size_bytes, file_modified_at_ms, observed_at_ms)
            VALUES ('zed', 'zed_threads_sqlite', '/tmp/zed/threads.db', '/tmp/zed/threads.db', 1, 0, 0)
            "#,
            [],
        )
        .unwrap();
}

#[test]
fn schema_v20_adds_kiro_cli_provider_checks() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    {
        let conn = Connection::open(&path).unwrap();
        let legacy_sql = CREATE_TABLES_SQL.replace(", 'kiro_cli'", "");
        conn.execute_batch(&legacy_sql).unwrap();
        conn.execute_batch(INDEXES_SQL).unwrap();
        conn.execute_batch("PRAGMA user_version = 19;").unwrap();
    }

    let store = Store::open(&path).unwrap();
    let version: i64 = store
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);

    store
        .conn
        .execute(
            r#"
            INSERT INTO capture_sources
            (id, kind, provider, machine_id, started_at_ms, fidelity)
            VALUES (?1, 'provider_import', 'kiro_cli', 'test-machine', 0, 'imported')
            "#,
            params![new_id().to_string()],
        )
        .unwrap();
    store
        .conn
        .execute(
            r#"
            INSERT INTO catalog_sessions
            (source_path, provider, source_format, source_root, agent_type, file_size_bytes, file_modified_at_ms, cataloged_at_ms)
            VALUES ('/tmp/kiro/data.sqlite3', 'kiro_cli', 'kiro_cli_sqlite', '/tmp/kiro', 'primary', 1, 0, 0)
            "#,
            [],
        )
        .unwrap();
    store
        .conn
        .execute(
            r#"
            INSERT INTO source_import_files
            (provider, source_format, source_root, source_path, file_size_bytes, file_modified_at_ms, observed_at_ms)
            VALUES ('kiro_cli', 'kiro_cli_sqlite', '/tmp/kiro', '/tmp/kiro/data.sqlite3', 1, 0, 0)
            "#,
            [],
        )
        .unwrap();
}

#[test]
fn schema_v22_adds_forgecode_provider_checks() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    {
        let conn = Connection::open(&path).unwrap();
        let legacy_sql = CREATE_TABLES_SQL.replace(", 'forgecode'", "");
        conn.execute_batch(&legacy_sql).unwrap();
        conn.execute_batch(INDEXES_SQL).unwrap();
        conn.execute_batch("PRAGMA user_version = 21;").unwrap();
    }

    let store = Store::open(&path).unwrap();
    let version: i64 = store
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);

    store
        .conn
        .execute(
            r#"
            INSERT INTO capture_sources
            (id, kind, provider, machine_id, started_at_ms, fidelity)
            VALUES (?1, 'provider_import', 'forgecode', 'test-machine', 0, 'imported')
            "#,
            params![new_id().to_string()],
        )
        .unwrap();
    store
        .conn
        .execute(
            r#"
            INSERT INTO catalog_sessions
            (source_path, provider, source_format, source_root, agent_type, file_size_bytes, file_modified_at_ms, cataloged_at_ms)
            VALUES ('/tmp/forge/.forge.db', 'forgecode', 'forgecode_sqlite', '/tmp/forge', 'primary', 1, 0, 0)
            "#,
            [],
        )
        .unwrap();
    store
        .conn
        .execute(
            r#"
            INSERT INTO source_import_files
            (provider, source_format, source_root, source_path, file_size_bytes, file_modified_at_ms, observed_at_ms)
            VALUES ('forgecode', 'forgecode_sqlite', '/tmp/forge', '/tmp/forge/.forge.db', 1, 0, 0)
            "#,
            [],
        )
        .unwrap();
}

#[test]
fn schema_v23_adds_mistral_vibe_provider_checks() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    {
        let conn = Connection::open(&path).unwrap();
        let legacy_sql = CREATE_TABLES_SQL.replace(", 'mistral_vibe'", "");
        conn.execute_batch(&legacy_sql).unwrap();
        conn.execute_batch(INDEXES_SQL).unwrap();
        conn.execute_batch("PRAGMA user_version = 22;").unwrap();
    }

    let store = Store::open(&path).unwrap();
    let version: i64 = store
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);

    store
        .conn
        .execute(
            r#"
            INSERT INTO capture_sources
            (id, kind, provider, machine_id, started_at_ms, fidelity)
            VALUES (?1, 'provider_import', 'mistral_vibe', 'test-machine', 0, 'imported')
            "#,
            params![new_id().to_string()],
        )
        .unwrap();
    store
        .conn
        .execute(
            r#"
            INSERT INTO catalog_sessions
            (source_path, provider, source_format, source_root, agent_type, file_size_bytes, file_modified_at_ms, cataloged_at_ms)
            VALUES ('/tmp/vibe/messages.jsonl', 'mistral_vibe', 'mistral_vibe_session_jsonl', '/tmp/vibe', 'primary', 1, 0, 0)
            "#,
            [],
        )
        .unwrap();
    store
        .conn
        .execute(
            r#"
            INSERT INTO source_import_files
            (provider, source_format, source_root, source_path, file_size_bytes, file_modified_at_ms, observed_at_ms)
            VALUES ('mistral_vibe', 'mistral_vibe_session_jsonl', '/tmp/vibe', '/tmp/vibe/messages.jsonl', 1, 0, 0)
            "#,
            [],
        )
        .unwrap();
}

#[test]
fn schema_v24_adds_deepagents_mux_and_lingma_provider_checks() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    {
        let conn = Connection::open(&path).unwrap();
        let legacy_sql = CREATE_TABLES_SQL
            .replace(", 'deepagents'", "")
            .replace(", 'mux'", "")
            .replace(", 'lingma'", "");
        conn.execute_batch(&legacy_sql).unwrap();
        conn.execute_batch(INDEXES_SQL).unwrap();
        conn.execute_batch("PRAGMA user_version = 23;").unwrap();
    }

    let store = Store::open(&path).unwrap();
    let version: i64 = store
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);

    for provider in ["deepagents", "mux", "lingma"] {
        store
            .conn
            .execute(
                r#"
                INSERT INTO capture_sources
                (id, kind, provider, machine_id, started_at_ms, fidelity)
                VALUES (?1, 'provider_import', ?2, 'test-machine', 0, 'imported')
                "#,
                params![new_id().to_string(), provider],
            )
            .unwrap();
    }

    for (source_path, provider, source_format, source_root) in [
        (
            "/tmp/deepagents/sessions.db",
            "deepagents",
            "deepagents_sessions_sqlite",
            "/tmp/deepagents",
        ),
        (
            "/tmp/mux/chat.jsonl",
            "mux",
            "mux_session_jsonl",
            "/tmp/mux",
        ),
        (
            "/tmp/lingma/local.db",
            "lingma",
            "lingma_sqlite",
            "/tmp/lingma/local.db",
        ),
    ] {
        store
            .conn
            .execute(
                r#"
                INSERT INTO catalog_sessions
                (source_path, provider, source_format, source_root, agent_type, file_size_bytes, file_modified_at_ms, cataloged_at_ms)
                VALUES (?1, ?2, ?3, ?4, 'primary', 1, 0, 0)
                "#,
                params![source_path, provider, source_format, source_root],
            )
            .unwrap();
    }

    for (provider, source_format, source_root, source_path) in [
        (
            "deepagents",
            "deepagents_sessions_sqlite",
            "/tmp/deepagents",
            "/tmp/deepagents/sessions.db",
        ),
        (
            "mux",
            "mux_session_jsonl",
            "/tmp/mux",
            "/tmp/mux/chat.jsonl",
        ),
        (
            "lingma",
            "lingma_sqlite",
            "/tmp/lingma/local.db",
            "/tmp/lingma/local.db",
        ),
    ] {
        store
            .conn
            .execute(
                r#"
                INSERT INTO source_import_files
                (provider, source_format, source_root, source_path, file_size_bytes, file_modified_at_ms, observed_at_ms)
                VALUES (?1, ?2, ?3, ?4, 1, 0, 0)
                "#,
                params![provider, source_format, source_root, source_path],
            )
            .unwrap();
    }
}

#[test]
fn schema_v25_adds_rovodev_provider_checks() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    {
        let conn = Connection::open(&path).unwrap();
        let legacy_sql = CREATE_TABLES_SQL.replace(", 'rovodev'", "");
        conn.execute_batch(&legacy_sql).unwrap();
        conn.execute_batch(INDEXES_SQL).unwrap();
        conn.execute_batch("PRAGMA user_version = 24;").unwrap();
    }

    let store = Store::open(&path).unwrap();
    let version: i64 = store
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);

    store
        .conn
        .execute(
            r#"
            INSERT INTO capture_sources
            (id, kind, provider, machine_id, started_at_ms, fidelity)
            VALUES (?1, 'provider_import', 'rovodev', 'test-machine', 0, 'imported')
            "#,
            params![new_id().to_string()],
        )
        .unwrap();

    let source_path = "/tmp/rovodev/sessions/session/session_context.json";
    let provider = "rovodev";
    let source_format = "rovodev_session_json";
    let source_root = "/tmp/rovodev/sessions";
    store
        .conn
        .execute(
            r#"
            INSERT INTO catalog_sessions
            (source_path, provider, source_format, source_root, agent_type, file_size_bytes, file_modified_at_ms, cataloged_at_ms)
            VALUES (?1, ?2, ?3, ?4, 'primary', 1, 0, 0)
            "#,
            params![source_path, provider, source_format, source_root],
        )
        .unwrap();
    store
        .conn
        .execute(
            r#"
            INSERT INTO source_import_files
            (provider, source_format, source_root, source_path, file_size_bytes, file_modified_at_ms, observed_at_ms)
            VALUES (?1, ?2, ?3, ?4, 1, 0, 0)
            "#,
            params![provider, source_format, source_root, source_path],
        )
        .unwrap();
}
#[test]
fn schema_v27_adds_windsurf_provider_checks() {
    assert_provider_migration_accepts(
        26,
        "windsurf",
        "windsurf_cascade_hook_transcript_jsonl",
        "/tmp/windsurf/transcripts",
        "/tmp/windsurf/transcripts/trajectory.jsonl",
    );
}
#[test]
fn schema_v30_adds_auggie_provider_checks() {
    assert_provider_migration_accepts(
        29,
        "auggie",
        "auggie_session_json",
        "/tmp/augment/sessions",
        "/tmp/augment/sessions/session.json",
    );
}

#[test]
fn schema_v31_adds_firebender_provider_checks() {
    assert_provider_migration_accepts(
        30,
        "firebender",
        "firebender_chat_history_sqlite",
        "/tmp/project/.idea/firebender/chat_history.db",
        "/tmp/project/.idea/firebender/chat_history.db",
    );
}
#[test]
fn schema_v35_adds_trae_provider_checks() {
    assert_provider_migration_accepts(
        34,
        "trae",
        "trae_state_vscdb",
        "/tmp/Trae/User/workspaceStorage",
        "/tmp/Trae/User/workspaceStorage/workspace/state.vscdb",
    );
}

#[test]
fn schema_v36_adds_warp_provider_checks() {
    assert_provider_migration_accepts(
        35,
        "warp",
        "warp_sqlite",
        "/tmp/warp-terminal",
        "/tmp/warp-terminal/warp.sqlite",
    );
}

#[test]
fn schema_v37_adds_qoder_provider_checks() {
    assert_provider_migration_accepts(
        36,
        "qoder",
        "qoder_transcript_jsonl_tree",
        "/tmp/qoder/projects",
        "/tmp/qoder/projects/workspace/transcript/session.jsonl",
    );
}
#[test]
fn schema_v40_adds_junie_provider_checks() {
    assert_provider_migration_accepts(
        39,
        "junie",
        "junie_session_events_jsonl_tree",
        "/tmp/junie/sessions",
        "/tmp/junie/sessions/session-260607-100000-acme/events.jsonl",
    );
}

#[test]
fn schema_v46_adds_mimocode_provider_checks() {
    assert_provider_migration_accepts(
        45,
        "mimocode",
        "mimocode_sqlite",
        "/tmp/mimocode",
        "/tmp/mimocode/mimocode.db",
    );
}
