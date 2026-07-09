use super::support::*;

#[test]

fn native_opencode_imports_read_only_sqlite() {
    let temp = tempdir();
    let fixture = write_opencode_smoke_db(&temp, false);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_opencode_sqlite(
        &fixture,
        &mut store,
        OpenCodeSqliteImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-06-24T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            allow_partial_failures: true,
            ..OpenCodeSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0);
    assert_eq!(summary.imported_sessions, 2);
    assert_eq!(summary.imported_events, 3);
    assert_eq!(summary.imported_edges, 1);
    let parent_id = stored_provider_session_id(&store, CaptureProvider::OpenCode, "opencode-root");
    let child_id = stored_provider_session_id(&store, CaptureProvider::OpenCode, "opencode-child");
    assert_eq!(
        store.get_session(child_id).unwrap().parent_session_id,
        Some(parent_id)
    );
    let events = store.events_for_session(parent_id).unwrap();
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall));
    assert_eq!(
        events[0].sync.metadata["source_format"].as_str(),
        Some(OPENCODE_SQLITE_SOURCE_FORMAT)
    );
}
#[test]
fn native_kilo_imports_opencode_derived_sqlite_fixture_idempotently() {
    let temp = tempdir();
    let fixture = provider_history_fixture("kilo/kilo.db");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_kilo_sqlite(
        &fixture,
        &mut store,
        KiloSqliteImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-07-04T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            allow_partial_failures: true,
            ..KiloSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 2);

    let session_id = stored_provider_session_id(&store, CaptureProvider::Kilo, "kilo-root");
    let session = store.get_session(session_id).unwrap();
    assert_eq!(session.provider, CaptureProvider::Kilo);
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(
        events[0].sync.metadata["source_format"].as_str(),
        Some(KILO_SQLITE_SOURCE_FORMAT)
    );
    assert_eq!(
        events[0].payload["body"]["session_message_seq"].as_i64(),
        Some(1)
    );
    assert_eq!(
        events[1].payload["body"]["session_message_seq"].as_i64(),
        Some(2)
    );

    let second = import_kilo_sqlite(
        &fixture,
        &mut store,
        KiloSqliteImportOptions {
            source_path: Some(fixture.clone()),
            allow_partial_failures: true,
            ..KiloSqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 1);
    assert_eq!(second.skipped_events, 2);
}
#[test]
fn native_warp_imports_sqlite_fixture_idempotently() {
    let temp = tempdir();
    let fixture = provider_history_fixture("warp/v1/warp.sqlite");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_warp_sqlite(
        &fixture,
        &mut store,
        WarpSqliteImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-07-05T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            allow_partial_failures: true,
            ..WarpSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 4);

    let session_id =
        stored_provider_session_id(&store, CaptureProvider::Warp, "warp-conversation-1");
    let session = store.get_session(session_id).unwrap();
    assert_eq!(session.provider, CaptureProvider::Warp);
    let rendered_session = serde_json::to_string(&session.sync.metadata).unwrap();
    assert!(rendered_session.contains("Sanitized Warp Agent"));
    assert!(rendered_session.contains("server_conversation_token_present"));
    assert!(!rendered_session.contains("warp-server-token-fixture"));

    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 4);
    assert_eq!(events[0].role, Some(EventRole::User));
    assert_eq!(events[1].role, Some(EventRole::Assistant));
    assert_eq!(events[2].event_type, EventType::ToolCall);
    assert_eq!(events[3].event_type, EventType::ToolOutput);
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("warp sqlite oracle prompt"));
    assert!(rendered.contains("Warp sqlite oracle answer"));
    assert!(rendered.contains("warp_sqlite"));
    assert!(store
        .search_event_hits("Warp sqlite oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Warp)));

    let second = import_warp_sqlite(
        &fixture,
        &mut store,
        WarpSqliteImportOptions {
            source_path: Some(fixture.clone()),
            allow_partial_failures: true,
            ..WarpSqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 1);
    assert_eq!(second.skipped_events, 4);
}

#[test]
fn native_warp_import_reads_committed_wal_content() {
    let temp = tempdir();
    let fixture = provider_history_fixture("warp/v1/warp.sqlite");
    let live_db = temp.path().join("warp-live.sqlite");
    fs::copy(&fixture, &live_db).unwrap();
    let writer = Connection::open(&live_db).unwrap();
    writer.pragma_update(None, "journal_mode", "WAL").unwrap();
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    let conversation_data = json!({
        "agent_name": "Warp WAL Agent",
        "server_conversation_token": "warp-server-token-preserved"
    })
    .to_string();
    writer
        .execute(
            "update agent_conversations set conversation_data = ?1 where conversation_id = ?2",
            rusqlite::params![conversation_data, "warp-conversation-1"],
        )
        .unwrap();
    let before_import = sqlite_file_snapshot(&live_db);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_warp_sqlite(
        &live_db,
        &mut store,
        WarpSqliteImportOptions {
            source_path: Some(live_db.clone()),
            allow_partial_failures: true,
            ..WarpSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 4);
    let session_id =
        stored_provider_session_id(&store, CaptureProvider::Warp, "warp-conversation-1");
    let session = store.get_session(session_id).unwrap();
    let rendered_session = serde_json::to_string(&session.sync.metadata).unwrap();
    assert!(rendered_session.contains("Warp WAL Agent"));
    assert!(rendered_session.contains("server_conversation_token_present"));
    assert!(!rendered_session.contains("warp-server-token-preserved"));
    assert_eq!(sqlite_file_snapshot(&live_db), before_import);
    drop(writer);
}

#[test]
fn native_warp_rejects_changed_schema_before_querying() {
    let temp = tempdir();
    let db = temp.path().join("warp-missing-task.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE agent_conversations (
                id INTEGER PRIMARY KEY,
                conversation_id TEXT NOT NULL,
                conversation_data TEXT NOT NULL,
                last_modified_at TEXT NOT NULL
            );
            CREATE TABLE agent_tasks (
                id INTEGER PRIMARY KEY,
                conversation_id TEXT NOT NULL,
                task_id TEXT NOT NULL,
                last_modified_at TEXT NOT NULL
            );",
    )
    .unwrap();
    drop(conn);

    let err = import_warp_sqlite(
        &db,
        &mut Store::open(temp.path().join("work.sqlite")).unwrap(),
        WarpSqliteImportOptions::default(),
    )
    .unwrap_err();

    assert!(err
        .to_string()
        .contains("Warp agent_tasks table missing required column(s): task"));
}

#[test]
fn native_hermes_rejects_out_of_range_message_timestamp() {
    let temp = tempdir();
    let fixture = write_hermes_smoke_db(&temp);
    let conn = Connection::open(&fixture).unwrap();
    conn.execute(
        "update messages set timestamp = ?1 where content = 'bad timestamp'",
        [1.0e300_f64],
    )
    .unwrap();
    drop(conn);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_hermes_sqlite(
        &fixture,
        &mut store,
        HermesSqliteImportOptions {
            allow_partial_failures: true,
            ..HermesSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1);
    assert!(summary.failures[0]
        .error
        .contains("Hermes message timestamp"));
    assert_eq!(summary.imported_events, 1);
}

#[test]
fn native_sqlite_imports_do_not_mutate_provider_databases() {
    let temp = tempdir();

    let opencode = write_opencode_smoke_db(&temp, false);
    assert_sqlite_clean_import_preserves_file("OpenCode", &opencode, |store| {
        import_opencode_sqlite(
            &opencode,
            store,
            OpenCodeSqliteImportOptions {
                allow_partial_failures: true,
                ..OpenCodeSqliteImportOptions::default()
            },
        )
        .unwrap()
    });

    let mimocode = write_opencode_message_part_db(
        &temp,
        "mimocode-read-only.db",
        "mimocode-read-only-root",
        "mimocode read-only oracle",
    );
    assert_sqlite_clean_import_preserves_file("MiMo Code", &mimocode, |store| {
        import_mimocode_sqlite(
            &mimocode,
            store,
            MiMoCodeSqliteImportOptions {
                allow_partial_failures: true,
                ..MiMoCodeSqliteImportOptions::default()
            },
        )
        .unwrap()
    });

    let kilo = provider_history_fixture("kilo/kilo.db");
    assert_sqlite_clean_import_preserves_file("Kilo", &kilo, |store| {
        import_kilo_sqlite(
            &kilo,
            store,
            KiloSqliteImportOptions {
                allow_partial_failures: true,
                ..KiloSqliteImportOptions::default()
            },
        )
        .unwrap()
    });

    let kiro = provider_history_fixture("kiro-cli/v2/data.sqlite3");
    assert_sqlite_clean_import_preserves_file("Kiro CLI", &kiro, |store| {
        import_kiro_sqlite(
            &kiro,
            store,
            KiroSqliteImportOptions {
                allow_partial_failures: true,
                ..KiroSqliteImportOptions::default()
            },
        )
        .unwrap()
    });

    let crush = provider_history_fixture("crush/v1/crush.db");
    assert_sqlite_clean_import_preserves_file("Crush", &crush, |store| {
        import_crush_sqlite(
            &crush,
            store,
            CrushSqliteImportOptions {
                allow_partial_failures: true,
                ..CrushSqliteImportOptions::default()
            },
        )
        .unwrap()
    });

    let goose = provider_history_fixture("goose/v14/sessions.db");
    assert_sqlite_clean_import_preserves_file("Goose", &goose, |store| {
        import_goose_sessions_sqlite(
            &goose,
            store,
            GooseSessionsSqliteImportOptions {
                allow_partial_failures: true,
                ..GooseSessionsSqliteImportOptions::default()
            },
        )
        .unwrap()
    });

    let lingma = provider_history_fixture("lingma/v1/local.db");
    assert_sqlite_clean_import_preserves_file("Lingma", &lingma, |store| {
        import_lingma_sqlite(
            &lingma,
            store,
            LingmaSqliteImportOptions {
                allow_partial_failures: true,
                ..LingmaSqliteImportOptions::default()
            },
        )
        .unwrap()
    });

    let warp = provider_history_fixture("warp/v1/warp.sqlite");
    assert_sqlite_clean_import_preserves_file("Warp", &warp, |store| {
        import_warp_sqlite(
            &warp,
            store,
            WarpSqliteImportOptions {
                allow_partial_failures: true,
                ..WarpSqliteImportOptions::default()
            },
        )
        .unwrap()
    });

    let zed = provider_history_fixture("zed/v1/threads.db");
    assert_sqlite_clean_import_preserves_file("Zed", &zed, |store| {
        import_zed_threads_sqlite(
            &zed,
            store,
            ZedThreadsSqliteImportOptions {
                allow_partial_failures: true,
                ..ZedThreadsSqliteImportOptions::default()
            },
        )
        .unwrap()
    });

    let hermes = write_hermes_smoke_db(&temp);
    assert_sqlite_clean_import_preserves_file("Hermes", &hermes, |store| {
        import_hermes_sqlite(
            &hermes,
            store,
            HermesSqliteImportOptions {
                allow_partial_failures: true,
                ..HermesSqliteImportOptions::default()
            },
        )
        .unwrap()
    });

    let astrbot = provider_history_fixture("astrbot/v1/data/data_v4.db");
    assert_sqlite_clean_import_preserves_file("AstrBot", &astrbot, |store| {
        import_astrbot_sqlite(
            &astrbot,
            store,
            AstrBotSqliteImportOptions {
                allow_partial_failures: true,
                ..AstrBotSqliteImportOptions::default()
            },
        )
        .unwrap()
    });

    let shelley = write_shelley_smoke_db(&temp);
    assert_sqlite_clean_import_preserves_file("Shelley", &shelley, |store| {
        import_shelley_sqlite(
            &shelley,
            store,
            ShelleySqliteImportOptions {
                allow_partial_failures: true,
                ..ShelleySqliteImportOptions::default()
            },
        )
        .unwrap()
    });

    let firebender_root = provider_history_fixture("firebender/v1");
    let firebender_db = firebender_root
        .join(".idea")
        .join("firebender")
        .join("chat_history.db");
    assert_sqlite_clean_import_preserves_file("Firebender", &firebender_db, |store| {
        import_firebender_sqlite(
            &firebender_root,
            store,
            FirebenderSqliteImportOptions {
                allow_partial_failures: true,
                ..FirebenderSqliteImportOptions::default()
            },
        )
        .unwrap()
    });

    let forgecode = provider_history_fixture("forgecode/v1/forge.db");
    assert_sqlite_clean_import_preserves_file("ForgeCode", &forgecode, |store| {
        import_forgecode_sqlite(
            &forgecode,
            store,
            ForgeCodeSqliteImportOptions {
                allow_partial_failures: true,
                ..ForgeCodeSqliteImportOptions::default()
            },
        )
        .unwrap()
    });

    let deepagents = provider_history_fixture("deepagents/v1/sessions.db");
    assert_sqlite_clean_import_preserves_file("DeepAgents", &deepagents, |store| {
        import_deepagents_sqlite(
            &deepagents,
            store,
            DeepAgentsSqliteImportOptions {
                allow_partial_failures: true,
                ..DeepAgentsSqliteImportOptions::default()
            },
        )
        .unwrap()
    });
}

#[test]
fn native_sqlite_successful_tool_outputs_are_metadata_only_and_not_searchable() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let goose = provider_history_fixture("goose/v14/sessions.db");
    let goose_summary = import_goose_sessions_sqlite(
        &goose,
        &mut store,
        GooseSessionsSqliteImportOptions {
            source_path: Some(goose.clone()),
            allow_partial_failures: true,
            ..GooseSessionsSqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(goose_summary.failed, 0, "{:?}", goose_summary.failures);
    assert_successful_output_metadata_only(
        &store,
        CaptureProvider::Goose,
        "goose-root",
        EventType::ToolOutput,
        "goose tool output oracle",
    );
    assert_search_hit_cites_source(
        &store,
        CaptureProvider::Goose,
        GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
        "goose sqlite search oracle request",
    );

    let forgecode = provider_history_fixture("forgecode/v1/forge.db");
    let forge_summary = import_forgecode_sqlite(
        &forgecode,
        &mut store,
        ForgeCodeSqliteImportOptions {
            source_path: Some(forgecode.clone()),
            allow_partial_failures: true,
            ..ForgeCodeSqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(forge_summary.failed, 0, "{:?}", forge_summary.failures);
    assert_successful_output_metadata_only(
        &store,
        CaptureProvider::ForgeCode,
        "forge-root",
        EventType::ToolOutput,
        "wrote src/forge_oracle.rs",
    );
    assert!(store.search_event_hits("wrote", 10).unwrap().is_empty());
    assert_search_hit_cites_source(
        &store,
        CaptureProvider::ForgeCode,
        FORGECODE_SQLITE_SOURCE_FORMAT,
        "forgecode oracle prompt",
    );
}

fn assert_sqlite_clean_import_preserves_file(
    label: &str,
    source_file: &Path,
    run_import: impl FnOnce(&mut Store) -> ProviderImportSummary,
) {
    let summary = assert_sqlite_source_file_unchanged(source_file, run_import);
    assert_eq!(summary.failed, 0, "{label}: {:?}", summary.failures);
    assert!(
        summary.imported_sessions > 0,
        "{label}: expected imported sessions, got {summary:?}"
    );
    assert!(
        summary.imported_events > 0,
        "{label}: expected imported events, got {summary:?}"
    );
}

fn assert_successful_output_metadata_only(
    store: &Store,
    provider: CaptureProvider,
    provider_session_id: &str,
    event_type: EventType,
    forbidden_output: &str,
) {
    let session_id = stored_provider_session_id(store, provider, provider_session_id);
    let events = store.events_for_session(session_id).unwrap();
    let event = events
        .iter()
        .find(|event| event.event_type == event_type)
        .unwrap_or_else(|| panic!("missing {provider:?} {event_type:?} event"));
    assert_eq!(event.payload["body"]["text"].as_str(), Some(""));
    assert_eq!(
        event.payload["body"]["content_retention"].as_str(),
        Some("metadata_only")
    );
    let rendered = serde_json::to_string(event).unwrap();
    assert!(
        !rendered.contains(forbidden_output),
        "{provider:?} leaked raw output into sanitized event payload"
    );
    assert!(
        store
            .search_event_hits(forbidden_output, 10)
            .unwrap()
            .is_empty(),
        "{provider:?} raw output should not be indexed"
    );
}

fn assert_search_hit_cites_source(
    store: &Store,
    provider: CaptureProvider,
    source_format: &str,
    query: &str,
) {
    let hits = store.search_event_hits(query, 10).unwrap();
    let hit = hits
        .iter()
        .find(|hit| hit.provider == Some(provider))
        .unwrap_or_else(|| panic!("missing {provider:?} search hit for {query:?}"));
    assert_eq!(hit.source_format.as_deref(), Some(source_format));
    assert!(hit.raw_source_path.is_some());
    assert!(hit.cursor.is_some());
}

#[cfg(unix)]
#[test]
fn native_opencode_normalizer_rejects_symlinked_sqlite() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let fixture = write_opencode_smoke_db(&temp, false);
    let link = temp.path().join("linked-opencode.db");
    symlink(&fixture, &link).unwrap();

    let err = normalize_opencode_sqlite(
        &link,
        &ProviderAdapterContext::default(),
        &OPENCODE_SQLITE_DIALECT,
    )
    .unwrap_err();
    assert!(matches!(
        err,
        CaptureError::InvalidProviderTranscriptPath { path, reason }
            if path.ends_with("linked-opencode.db")
                && reason == "symlinked provider transcript files are rejected"
    ));
}

#[test]
fn native_opencode_synthesizes_session_message_seq_when_missing() {
    let temp = tempdir();
    let fixture = write_opencode_session_message_without_seq_db(&temp);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_opencode_sqlite(
        &fixture,
        &mut store,
        OpenCodeSqliteImportOptions {
            allow_partial_failures: true,
            ..OpenCodeSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 2);

    let session_id =
        stored_provider_session_id(&store, CaptureProvider::OpenCode, "opencode-no-seq");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(
        events[0].payload["body"]["session_message_seq"].as_i64(),
        Some(1)
    );
    assert_eq!(
        events[1].payload["body"]["session_message_seq"].as_i64(),
        Some(2)
    );
    assert_ne!(events[0].id, events[1].id);
}

#[test]
fn native_opencode_rejects_negative_session_message_seq() {
    let temp = tempdir();
    let fixture = write_opencode_smoke_db(&temp, false);
    let conn = Connection::open(&fixture).unwrap();
    conn.execute(
        "update session_message set seq = -1 where id = 'msg-user'",
        [],
    )
    .unwrap();
    drop(conn);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_opencode_sqlite(
        &fixture,
        &mut store,
        OpenCodeSqliteImportOptions {
            allow_partial_failures: true,
            ..OpenCodeSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1);
    assert!(summary.failures[0]
        .error
        .contains("OpenCode session_message seq must be nonnegative"));
    assert_eq!(summary.imported_events, 1);
    let events = store
        .list_sessions()
        .unwrap()
        .into_iter()
        .flat_map(|session| store.events_for_session(session.id).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(events.len(), summary.imported_events);
    assert!(events.iter().all(|event| {
        event.payload["body"]["session_message_seq"]
            .as_i64()
            .is_some_and(|seq| seq >= 0)
    }));
}

#[test]
fn native_opencode_rejects_out_of_range_message_timestamp() {
    let temp = tempdir();
    let fixture = write_opencode_smoke_db(&temp, false);
    let conn = Connection::open(&fixture).unwrap();
    let data_without_payload_time = json!({"text": "bad timestamp fallback"}).to_string();
    conn.execute(
        "update session_message set time_created = ?1, data = ?2 where id = 'msg-user'",
        rusqlite::params![i64::MAX, data_without_payload_time],
    )
    .unwrap();
    drop(conn);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_opencode_sqlite(
        &fixture,
        &mut store,
        OpenCodeSqliteImportOptions {
            allow_partial_failures: true,
            ..OpenCodeSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1);
    assert!(summary.failures[0]
        .error
        .contains("OpenCode session_message time_created"));
    assert_eq!(summary.imported_events, 1);
}

fn oversized_opencode_text_payload() -> String {
    format!(
        "{{\"time\":{{\"created\":1782259200000}},\"text\":\"{}\"}}",
        "x".repeat(MAX_PROVIDER_SQLITE_VALUE_BYTES + 1)
    )
}

#[test]
fn native_opencode_skips_oversized_sqlite_text_value_and_imports_other_rows() {
    let temp = tempdir();
    let fixture = write_opencode_smoke_db(&temp, false);
    let conn = Connection::open(&fixture).unwrap();
    let oversized_data = oversized_opencode_text_payload();
    conn.execute(
        "update session_message set data = ?1 where id = 'msg-user'",
        [&oversized_data],
    )
    .unwrap();
    let other_conversational: i64 = conn
        .query_row(
            "select count(*) from session_message where id != 'msg-user'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        other_conversational > 0,
        "test fixture must contain at least one non-oversized conversational row"
    );
    drop(conn);

    let summary = import_opencode_sqlite(
        &fixture,
        &mut Store::open(temp.path().join("work.sqlite")).unwrap(),
        OpenCodeSqliteImportOptions::default(),
    )
    .expect("oversized rows should be skipped, not abort the whole import");

    assert_eq!(
        summary.failed, 0,
        "oversized rows must not be counted as failures, got failures: {:?}",
        summary.failures
    );
    assert_eq!(summary.skipped, 3, "unexpected summary: {summary:?}");
    assert_eq!(summary.skipped_events, 2, "unexpected summary: {summary:?}");
    assert_eq!(
        summary.skipped_sessions, 1,
        "unexpected summary: {summary:?}"
    );
    assert!(
        summary.imported_events >= 1,
        "non-oversized rows should still import, got summary: {summary:?}"
    );
}

#[test]
fn native_opencode_skips_all_oversized_sqlite_text_values_without_failure() {
    let temp = tempdir();
    let fixture = write_opencode_smoke_db(&temp, false);
    let conn = Connection::open(&fixture).unwrap();
    conn.execute("delete from session_message where id != 'msg-user'", [])
        .unwrap();
    let oversized_data = oversized_opencode_text_payload();
    conn.execute(
        "update session_message set data = ?1 where id = 'msg-user'",
        [&oversized_data],
    )
    .unwrap();
    drop(conn);

    let summary = import_opencode_sqlite(
        &fixture,
        &mut Store::open(temp.path().join("work.sqlite")).unwrap(),
        OpenCodeSqliteImportOptions::default(),
    )
    .expect("oversized rows should be skipped without fabricating import failures");

    assert_eq!(
        summary.failed, 0,
        "unexpected failures: {:?}",
        summary.failures
    );
    assert_eq!(summary.skipped, 1, "unexpected summary: {summary:?}");
    assert_eq!(summary.skipped_events, 1, "unexpected summary: {summary:?}");
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
}

#[test]
fn native_opencode_skips_oversized_legacy_message_value_without_failure() {
    let temp = tempdir();
    let fixture = write_opencode_current_schema_db(&temp, true);
    let conn = Connection::open(&fixture).unwrap();
    let oversized_data = oversized_opencode_text_payload();
    conn.execute("update message set data = ?1", [&oversized_data])
        .unwrap();
    drop(conn);

    let summary = import_opencode_sqlite(
        &fixture,
        &mut Store::open(temp.path().join("work.sqlite")).unwrap(),
        OpenCodeSqliteImportOptions::default(),
    )
    .expect("oversized legacy message rows should be skipped without import failure");

    assert_eq!(summary.failed, 0, "{summary:?}");
    assert_eq!(summary.skipped, 1, "{summary:?}");
    assert_eq!(summary.skipped_events, 1, "{summary:?}");
    assert_eq!(summary.imported_events, 0, "{summary:?}");
}

#[test]
fn native_opencode_imports_message_part_text_and_metadata() {
    let temp = tempdir();
    let fixture = write_opencode_message_part_db(
        &temp,
        "opencode-message-part.db",
        "opencode-part-root",
        "opencode message part oracle",
    );
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_opencode_sqlite(
        &fixture,
        &mut store,
        OpenCodeSqliteImportOptions {
            allow_partial_failures: true,
            ..OpenCodeSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 2);
    assert_message_part_import(
        &store,
        CaptureProvider::OpenCode,
        OPENCODE_SQLITE_SOURCE_FORMAT,
        "opencode-part-root",
        "opencode message part oracle",
    );
}

#[test]
fn native_kilo_imports_message_part_text_and_metadata() {
    let temp = tempdir();
    let fixture = write_opencode_message_part_db(
        &temp,
        "kilo-message-part.db",
        "kilo-part-root",
        "kilo message part oracle",
    );
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_kilo_sqlite(
        &fixture,
        &mut store,
        KiloSqliteImportOptions {
            allow_partial_failures: true,
            ..KiloSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 2);
    assert_message_part_import(
        &store,
        CaptureProvider::Kilo,
        KILO_SQLITE_SOURCE_FORMAT,
        "kilo-part-root",
        "kilo message part oracle",
    );
}

#[test]
fn native_mimocode_imports_message_part_text_and_metadata_idempotently() {
    let temp = tempdir();
    let fixture = write_opencode_message_part_db(
        &temp,
        "mimocode-message-part.db",
        "mimocode-part-root",
        "mimocode message part oracle",
    );
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_mimocode_sqlite(
        &fixture,
        &mut store,
        MiMoCodeSqliteImportOptions {
            allow_partial_failures: true,
            source_path: Some(fixture.clone()),
            ..MiMoCodeSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 2);
    assert_message_part_import(
        &store,
        CaptureProvider::MiMoCode,
        MIMOCODE_SQLITE_SOURCE_FORMAT,
        "mimocode-part-root",
        "mimocode message part oracle",
    );

    let second = import_mimocode_sqlite(
        &fixture,
        &mut store,
        MiMoCodeSqliteImportOptions {
            allow_partial_failures: true,
            source_path: Some(fixture.clone()),
            ..MiMoCodeSqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 1);
    assert_eq!(second.skipped_events, 2);
}

#[test]
fn native_opencode_message_part_invalid_json_reports_failure() {
    let temp = tempdir();
    let fixture = write_opencode_message_part_db(
        &temp,
        "opencode-message-part-invalid-json.db",
        "opencode-invalid-part-root",
        "opencode invalid part oracle",
    );
    let conn = Connection::open(&fixture).unwrap();
    conn.execute(
        "update part set data = '{invalid json' where id = 'part-text'",
        [],
    )
    .unwrap();
    drop(conn);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary =
        import_opencode_sqlite(&fixture, &mut store, OpenCodeSqliteImportOptions::default())
            .unwrap();

    assert_eq!(summary.failed, 1);
    assert!(summary.failures[0]
        .error
        .contains("invalid JSON in session_message"));
}

fn assert_message_part_import(
    store: &Store,
    provider: CaptureProvider,
    source_format: &str,
    provider_session_id: &str,
    oracle_text: &str,
) {
    let session_id = stored_provider_session_id(store, provider, provider_session_id);
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 2);
    let event = events
        .iter()
        .find(|event| event.event_type == EventType::Message)
        .expect("message part event imported");
    assert_eq!(event.event_type, EventType::Message);
    assert_eq!(event.payload["body"]["text"].as_str(), Some(oracle_text));
    assert_eq!(
        event.payload["body"]["message_id"].as_str(),
        Some("part-message")
    );
    assert_eq!(event.payload["body"]["part_id"].as_str(), Some("part-text"));
    assert_eq!(
        event.sync.metadata["source_format"].as_str(),
        Some(source_format)
    );
    let rendered = serde_json::to_string(event).unwrap();
    assert!(rendered.contains("message:part-message:part:part-text"));
    assert!(!rendered.contains("session_message:"));
    assert!(!rendered.contains("part-tool"));
    assert!(!rendered.contains("write_file"));
    assert!(!rendered.contains("outputPath"));
    assert!(!rendered.contains("part-patch"));
    assert!(!rendered.contains("opencode_part_from_files"));
    assert!(!rendered.contains("*** Begin Patch"));
    assert!(!rendered.contains("raw-opencode-patch-needle"));

    let tool_output = events
        .iter()
        .find(|event| event.event_type == EventType::ToolOutput)
        .expect("tool part metadata-only output event imported");
    assert_eq!(tool_output.payload["body"]["text"].as_str(), Some(""));
    assert_eq!(
        tool_output.payload["body"]["content_retention"].as_str(),
        Some("metadata_only")
    );
    let rendered_tool = serde_json::to_string(tool_output).unwrap();
    assert!(rendered_tool.contains("write_file"));
    assert!(rendered_tool.contains("completed"));
    assert!(!rendered_tool.contains("outputPath"));
    assert!(!rendered_tool.contains("tool_arg_should_not_touch"));

    assert!(store
        .search_event_hits(oracle_text, 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(provider)));
    assert!(store
        .search_event_hits("Begin Patch", 10)
        .unwrap()
        .is_empty());
    assert!(store
        .search_event_hits("raw-opencode-patch-needle", 10)
        .unwrap()
        .is_empty());
    assert!(store
        .search_event_hits("tool_arg_should_not_touch", 10)
        .unwrap()
        .is_empty());
    assert!(store
        .search_event_hits("opencode_part_from_files", 10)
        .unwrap()
        .is_empty());

    let archive = store.export_archive().unwrap();
    assert!(archive
        .files_touched
        .iter()
        .any(|file| file.path == "src/opencode_part.txt"));
    assert!(archive
        .files_touched
        .iter()
        .any(|file| file.path == "src/opencode_part_from_files.txt"));
    assert!(!archive
        .files_touched
        .iter()
        .any(|file| file.path == "src/tool_arg_should_not_touch.txt"));
}

#[test]
fn native_opencode_reports_malformed_and_corrupt_db() {
    let temp = tempdir();
    let malformed = write_opencode_smoke_db(&temp, true);
    let corrupt = temp.path().join("corrupt-opencode.db");
    fs::write(&corrupt, b"not sqlite").unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_opencode_sqlite(
        &malformed,
        &mut store,
        OpenCodeSqliteImportOptions {
            allow_partial_failures: true,
            ..OpenCodeSqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(summary.failed, 1);
    assert!(summary.failures[0].error.contains("invalid JSON"));

    let err = import_opencode_sqlite(&corrupt, &mut store, OpenCodeSqliteImportOptions::default())
        .unwrap_err();
    assert!(err.to_string().contains("not a database"));
}

#[test]
fn native_opencode_rejects_empty_current_schema_without_model_column() {
    let temp = tempdir();
    let fixture = write_opencode_current_schema_db(&temp, false);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_opencode_sqlite(
        &fixture,
        &mut store,
        OpenCodeSqliteImportOptions {
            allow_partial_failures: true,
            ..OpenCodeSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1);
    assert!(summary.failures[0]
        .error
        .contains("no real conversational message rows"));
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn native_opencode_imports_legacy_message_table_when_session_message_is_absent() {
    let temp = tempdir();
    let fixture = write_opencode_current_schema_db(&temp, true);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_opencode_sqlite(
        &fixture,
        &mut store,
        OpenCodeSqliteImportOptions {
            allow_partial_failures: true,
            ..OpenCodeSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 1);

    let session_id = stored_provider_session_id(&store, CaptureProvider::OpenCode, "current-root");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].sync.metadata["source_format"].as_str(),
        Some(OPENCODE_SQLITE_SOURCE_FORMAT)
    );
    assert!(events[0].payload.to_string().contains("legacy hello"));
}

#[test]
fn native_opencode_falls_back_when_session_message_is_metadata_only() {
    let temp = tempdir();
    let fixture = write_opencode_session_message_metadata_with_legacy_message_db(&temp);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_opencode_sqlite(
        &fixture,
        &mut store,
        OpenCodeSqliteImportOptions {
            allow_partial_failures: true,
            ..OpenCodeSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 1);
    let session_id = store
        .session_by_external_session(CaptureProvider::OpenCode, "strict-root")
        .unwrap()
        .unwrap()
        .id;
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 1);
    assert!(events[0]
        .payload
        .to_string()
        .contains("legacy fallback prompt"));
    let session = store.get_session(session_id).unwrap();
    assert_eq!(
        session.sync.metadata["metadata"]["legacy_projection"]["selected_message_table"].as_str(),
        Some("message")
    );
}

#[test]
fn native_opencode_rejects_malformed_authoritative_rows_without_legacy_fallback() {
    let temp = tempdir();
    let fixture = write_opencode_session_message_malformed_with_legacy_message_db(&temp);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_opencode_sqlite(
        &fixture,
        &mut store,
        OpenCodeSqliteImportOptions {
            allow_partial_failures: true,
            ..OpenCodeSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert!(summary.failures[0].error.contains("invalid JSON"));
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn native_opencode_rejects_malformed_metadata_authoritative_rows_without_legacy_fallback() {
    let temp = tempdir();
    let fixture = write_opencode_session_message_metadata_bad_seq_with_legacy_message_db(&temp);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_opencode_sqlite(
        &fixture,
        &mut store,
        OpenCodeSqliteImportOptions {
            allow_partial_failures: true,
            ..OpenCodeSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{summary:?}");
    assert!(summary.failures[0]
        .error
        .contains("OpenCode session_message seq must be nonnegative"));
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn native_opencode_rejects_tool_only_sqlite_rows() {
    let temp = tempdir();
    let fixture = write_opencode_tool_only_db(&temp, "opencode-tool-only.db");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_opencode_sqlite(
        &fixture,
        &mut store,
        OpenCodeSqliteImportOptions {
            allow_partial_failures: true,
            ..OpenCodeSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1);
    assert!(summary.failures[0]
        .error
        .contains("no real conversational message rows"));
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn native_opencode_falls_back_when_session_entry_is_metadata_only() {
    let temp = tempdir();
    let fixture = write_opencode_session_entry_metadata_with_legacy_message_db(&temp);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_opencode_sqlite(
        &fixture,
        &mut store,
        OpenCodeSqliteImportOptions {
            allow_partial_failures: true,
            ..OpenCodeSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 1);
    let session_id = store
        .session_by_external_session(CaptureProvider::OpenCode, "strict-root")
        .unwrap()
        .unwrap()
        .id;
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 1);
    assert!(events[0]
        .payload
        .to_string()
        .contains("legacy fallback prompt"));
    let session = store.get_session(session_id).unwrap();
    assert_eq!(
        session.sync.metadata["metadata"]["legacy_projection"]["selected_message_table"].as_str(),
        Some("message")
    );
}

#[test]
fn native_kilo_rejects_metadata_only_sqlite_rows() {
    let temp = tempdir();
    let fixture = write_opencode_all_metadata_db(&temp, "kilo-all-metadata.db");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_kilo_sqlite(
        &fixture,
        &mut store,
        KiloSqliteImportOptions {
            allow_partial_failures: true,
            ..KiloSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1);
    assert!(summary.failures[0]
        .error
        .contains("no real conversational message rows"));
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn native_kilo_rejects_tool_only_sqlite_rows() {
    let temp = tempdir();
    let fixture = write_opencode_tool_only_db(&temp, "kilo-tool-only.db");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_kilo_sqlite(
        &fixture,
        &mut store,
        KiloSqliteImportOptions {
            allow_partial_failures: true,
            ..KiloSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1);
    assert!(summary.failures[0]
        .error
        .contains("no real conversational message rows"));
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn native_opencode_rejects_changed_message_schema_before_querying() {
    let temp = tempdir();
    let fixture = write_opencode_future_incomplete_schema_db(&temp);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let err = import_opencode_sqlite(&fixture, &mut store, OpenCodeSqliteImportOptions::default())
        .unwrap_err();

    assert!(err
        .to_string()
        .contains("OpenCode SQLite message table missing required column(s): data"));
}

#[test]
fn openclaw_import_ignores_oversized_session_index_sidecar() {
    let temp = tempdir();
    let root = temp.path().join("openclaw");
    let sessions = root.join("agents/personal-agent/sessions");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join("sessions.json"),
        vec![b'x'; MAX_OPENCLAW_SESSION_INDEX_BYTES + 1],
    )
    .unwrap();
    fs::write(
        sessions.join("openclaw-oversized-index.jsonl"),
        format!(
            "{}\n{}\n",
            json!({
                "type": "session",
                "id": "openclaw-oversized-index",
                "timestamp": "2026-06-24T12:00:00Z",
                "cwd": "/workspace"
            }),
            json!({
                "type": "message",
                "id": "openclaw-oversized-index-user",
                "timestamp": "2026-06-24T12:00:01Z",
                "message": {"role": "user", "content": "oversized sidecar should not block import"}
            })
        ),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_openclaw_history(
        &root,
        &mut store,
        OpenClawImportOptions {
            allow_partial_failures: true,
            ..OpenClawImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 1);
    let session_id = stored_provider_session_id(
        &store,
        CaptureProvider::OpenClaw,
        "personal-agent/openclaw-oversized-index",
    );
    let session = store.get_session(session_id).unwrap();
    assert_eq!(
        session.external_session_id.as_deref(),
        Some("personal-agent/openclaw-oversized-index")
    );
}

#[test]
fn native_shelley_imports_sessions_messages_metadata_and_citations() {
    let temp = tempdir();
    let fixture = write_shelley_smoke_db(&temp);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_shelley_sqlite(
        &fixture,
        &mut store,
        ShelleySqliteImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-06-24T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            allow_partial_failures: true,
            ..ShelleySqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 2);
    assert_eq!(summary.imported_events, 4);
    assert_eq!(summary.imported_edges, 1);

    let parent_id = stored_provider_session_id(&store, CaptureProvider::Shelley, "shelley-root");
    let child_id = stored_provider_session_id(&store, CaptureProvider::Shelley, "shelley-child");
    assert_eq!(
        store.get_session(child_id).unwrap().parent_session_id,
        Some(parent_id)
    );
    assert!(store
        .get_session(parent_id)
        .unwrap()
        .sync
        .metadata
        .to_string()
        .contains("queued oracle"));

    let source = store
        .capture_source_by_external_session(CaptureProvider::Shelley, "shelley-root")
        .unwrap()
        .unwrap();
    assert_eq!(
        source.descriptor.raw_source_path.as_deref(),
        fixture.to_str()
    );
    assert_eq!(source.descriptor.provider, CaptureProvider::Shelley);

    let events = store.events_for_session(parent_id).unwrap();
    assert_eq!(events.len(), 3);
    let agent_event = events
        .iter()
        .find(|event| event.sync.metadata["metadata"]["message_id"].as_str() == Some("msg-agent"))
        .expect("Shelley agent event imported");
    let tool_result_event = events
        .iter()
        .find(|event| {
            event.sync.metadata["metadata"]["message_id"].as_str() == Some("msg-tool-result")
        })
        .expect("Shelley tool-result event imported");
    assert_eq!(agent_event.event_type, EventType::ToolCall);
    assert_eq!(tool_result_event.event_type, EventType::ToolOutput);
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("shelley search oracle"));
    assert!(rendered.contains("thinking through the search"));
    assert!(rendered.contains("tool call: bash"));
    assert!(!rendered.contains("tool output oracle"));
    assert!(rendered.contains("claude-opus-4-7"));
    assert!(rendered.contains("https://api.anthropic.com/v1/messages"));
    let user_event = events
        .iter()
        .find(|event| event.sync.metadata["metadata"]["message_id"].as_str() == Some("msg-user"))
        .expect("Shelley user event imported");
    assert!(user_event
        .sync
        .metadata
        .to_string()
        .contains("conversation:shelley-root:sequence:1:message:msg-user"));

    let source_path = fixture.display().to_string();
    let cursor = store
        .get_sync_cursor(
            None,
            "test-machine",
            &provider_source_cursor_stream(
                CaptureProvider::Shelley,
                SHELLEY_SQLITE_SOURCE_FORMAT,
                Some(&source_path),
            ),
        )
        .unwrap()
        .unwrap();
    assert!(cursor
        .cursor
        .contains("conversation:shelley-root:sequence:3:message:msg-tool-result"));
}

#[test]
fn native_shelley_reimport_is_idempotent() {
    let temp = tempdir();
    let fixture = write_shelley_smoke_db(&temp);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_shelley_sqlite(
        &fixture,
        &mut store,
        ShelleySqliteImportOptions {
            allow_partial_failures: true,
            ..ShelleySqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(first.imported_events, 4);

    let second = import_shelley_sqlite(
        &fixture,
        &mut store,
        ShelleySqliteImportOptions {
            allow_partial_failures: true,
            ..ShelleySqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.imported_edges, 0);
    assert_eq!(second.skipped_sessions, 3);
    assert_eq!(second.skipped_events, 4);
    assert_eq!(second.skipped_edges, 1);
}

#[test]
fn native_shelley_handles_duplicate_sequences_and_nonchat_rows() {
    let temp = tempdir();
    let fixture = write_shelley_adversarial_db(&temp);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_shelley_sqlite(
        &fixture,
        &mut store,
        ShelleySqliteImportOptions {
            allow_partial_failures: true,
            ..ShelleySqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 5);

    let session_id =
        stored_provider_session_id(&store, CaptureProvider::Shelley, "shelley-adversarial");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 5);
    assert_eq!(
        events
            .iter()
            .map(|event| event.id)
            .collect::<BTreeSet<_>>()
            .len(),
        5
    );
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("duplicate sequence first"));
    assert!(rendered.contains("duplicate sequence second"));
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::VcsChange));
    assert!(events
        .iter()
        .any(|event| event.sync.metadata["metadata"]["message_type"].as_str() == Some("warning")));

    let large = events
        .iter()
        .find(|event| event.sync.metadata["metadata"]["message_id"].as_str() == Some("msg-large"))
        .expect("large Shelley event imported");
    assert_eq!(large.payload["body"]["truncated"].as_bool(), Some(true));
    assert!(
        large.payload["body"]["text"]
            .as_str()
            .unwrap()
            .chars()
            .count()
            <= PROVIDER_MAX_TEXT_CHARS
    );
}

#[test]
fn native_shelley_text_extraction_is_not_duplicate_or_unbounded() {
    let text = shelley_value_text(&json!({
        "Content": [
            {"Type": 2, "Text": "once"}
        ]
    }))
    .unwrap();
    assert_eq!(text, "once");

    let huge = "x".repeat(PROVIDER_MAX_TEXT_CHARS + 200);
    let text = shelley_value_text(&json!({
        "Content": [
            {"Type": 2, "Text": huge},
            {"Type": 2, "Text": "after cap"}
        ]
    }))
    .unwrap();
    assert_eq!(text.chars().count(), PROVIDER_MAX_TEXT_CHARS + 1);
    assert!(!text.contains("after cap"));
}

#[test]
fn native_shelley_event_index_uses_stable_message_identity() {
    let message = ShelleyMessageRow {
        rowid: 1,
        message_id: "msg-stable".to_owned(),
        conversation_id: "conv-stable".to_owned(),
        sequence_id: 42,
        entry_type: "user".to_owned(),
        llm_data: None,
        user_data: None,
        usage_data: None,
        created_at: None,
        display_data: None,
        excluded_from_context: false,
        generation: None,
        llm_api_url: None,
        model_name: None,
        forked_from_message_id: None,
    };
    let mut moved_row = message.clone();
    moved_row.rowid = 999;
    let mut duplicate_sequence = message.clone();
    duplicate_sequence.message_id = "msg-stable-other".to_owned();

    assert_eq!(
        shelley_event_index(&message),
        shelley_event_index(&moved_row)
    );
    assert_ne!(
        shelley_event_index(&message),
        shelley_event_index(&duplicate_sequence)
    );
}

#[test]
fn native_shelley_reports_malformed_and_corrupt_db() {
    let temp = tempdir();
    let malformed = write_shelley_malformed_db(&temp);
    let corrupt = temp.path().join("corrupt-shelley.db");
    fs::write(&corrupt, b"not sqlite").unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let err = import_shelley_sqlite(
        &malformed,
        &mut store,
        ShelleySqliteImportOptions::default(),
    )
    .unwrap_err();
    assert!(err
        .to_string()
        .contains("Shelley messages table missing required column(s): type"));

    let err = import_shelley_sqlite(&corrupt, &mut store, ShelleySqliteImportOptions::default())
        .unwrap_err();
    assert!(err.to_string().contains("not a database"));
}

#[test]
fn provider_sources_discovers_shelley_default_db() {
    let temp = tempdir();
    let db = temp.path().join(".config/shelley/shelley.db");
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    fs::write(&db, b"not inspected by source probe").unwrap();

    let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Shelley);
    let source = sources
        .iter()
        .find(|source| source.source_format == SHELLEY_SQLITE_SOURCE_FORMAT)
        .unwrap_or_else(|| panic!("missing Shelley source in {sources:#?}"));
    assert_eq!(source.provider, CaptureProvider::Shelley);
    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.import_support, ProviderImportSupport::Native);
    assert_eq!(source.path, db);
}
