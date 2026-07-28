use super::*;

#[test]
fn large_source_splits_exact_journal_envelopes_and_replays_as_noop() {
    const EVENT_COUNT: usize = 512;
    const BODY_BYTES: usize = 32 * 1024;

    let temp = tempfile::TempDir::new().unwrap();
    let source_root = temp.path().join("sessions");
    std::fs::create_dir(&source_root).unwrap();
    let source =
        source_root.join("rollout-2026-01-01T00-00-00-00000000-0000-0000-0000-000000000001.jsonl");
    let mut contents = serde_json::to_string(&serde_json::json!({
        "timestamp": "2026-01-01T00:00:00Z",
        "type": "session_meta",
        "payload": {
            "id": "00000000-0000-0000-0000-000000000001",
            "timestamp": "2026-01-01T00:00:00Z",
            "cwd": "/workspace",
            "source": "cli"
        }
    }))
    .unwrap();
    contents.push('\n');
    for index in 0..EVENT_COUNT {
        contents.push_str(
            &serde_json::to_string(&serde_json::json!({
                "timestamp": "2026-01-01T00:00:01Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "content": [{
                        "type": "input_text",
                        "text": format!("journal-boundary-{index}-{}", "x".repeat(BODY_BYTES))
                    }]
                }
            }))
            .unwrap(),
        );
        contents.push('\n');
    }
    std::fs::write(&source, contents).unwrap();

    let database = temp.path().join("work.sqlite");
    let mut store = Store::open(&database).unwrap();
    let options = CodexSessionImportOptions {
        machine_id: "journal-boundary-machine".to_owned(),
        source_path: Some(source_root),
        imported_at: "2026-01-02T00:00:00Z".parse().unwrap(),
        ..CodexSessionImportOptions::default()
    };
    let imported =
        import_codex_native_session_files(vec![source.clone()], &mut store, options.clone())
            .unwrap();
    assert_eq!(imported.imported_sessions, 1);
    assert_eq!(imported.imported_events, EVENT_COUNT);
    assert_eq!(imported.failed, 0);
    let observer = rusqlite::Connection::open(&database).unwrap();
    let before_data_version = observer
        .query_row("PRAGMA data_version", [], |row| row.get::<_, i64>(0))
        .unwrap();
    let before_database = std::fs::read(&database).unwrap();
    let wal_path = database.with_extension("sqlite-wal");
    let before_wal = std::fs::read(&wal_path).ok();

    let replay = import_codex_native_session_files(vec![source], &mut store, options).unwrap();
    assert_eq!(replay.imported_sessions, 0);
    assert_eq!(replay.imported_events, 0);
    assert_eq!(replay.skipped_sessions, 1);
    assert_eq!(
        observer
            .query_row("PRAGMA data_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        before_data_version
    );
    assert_eq!(std::fs::read(&database).unwrap(), before_database);
    assert_eq!(std::fs::read(&wal_path).ok(), before_wal);

    let connection = rusqlite::Connection::open(database).unwrap();
    let (events, distinct_events) = connection
        .query_row(
            "SELECT COUNT(*), COUNT(DISTINCT id) FROM events",
            [],
            |row| Ok((row.get::<_, usize>(0)?, row.get::<_, usize>(1)?)),
        )
        .unwrap();
    assert_eq!(events, EVENT_COUNT);
    assert_eq!(distinct_events, EVENT_COUNT);
}

#[cfg(unix)]
#[test]
fn exact_repeat_does_not_open_or_parse_unchanged_source_and_append_still_resumes() {
    let temp = tempfile::TempDir::new().unwrap();
    let source_root = temp.path().join("sessions");
    std::fs::create_dir(&source_root).unwrap();
    let source = source_root.join("rollout-noop-open-regression.jsonl");
    let session = serde_json::to_string(&serde_json::json!({
        "timestamp": "2026-01-01T00:00:00Z",
        "type": "session_meta",
        "payload": {
            "id": "00000000-0000-0000-0000-000000000042",
            "timestamp": "2026-01-01T00:00:00Z",
            "cwd": "/workspace",
            "source": "cli"
        }
    }))
    .unwrap();
    let message = |text: &str| {
        serde_json::to_string(&serde_json::json!({
            "timestamp": "2026-01-01T00:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": text}]
            }
        }))
        .unwrap()
    };
    std::fs::write(&source, format!("{session}\n{}\n", message("initial"))).unwrap();

    let database = temp.path().join("work.sqlite");
    let mut store = Store::open(&database).unwrap();
    let options = CodexSessionImportOptions {
        machine_id: "noop-open-machine".to_owned(),
        source_path: Some(source_root.clone()),
        imported_at: "2026-01-02T00:00:00Z".parse().unwrap(),
        ..CodexSessionImportOptions::default()
    };
    let initial =
        import_codex_native_session_root(&source_root, &mut store, options.clone()).unwrap();
    assert_eq!(initial.imported_events, 1);

    let observer = rusqlite::Connection::open(&database).unwrap();
    let before_data_version = observer
        .query_row("PRAGMA data_version", [], |row| row.get::<_, i64>(0))
        .unwrap();
    let before_database = std::fs::read(&database).unwrap();
    let wal_path = database.with_extension("sqlite-wal");
    let before_wal = std::fs::read(&wal_path).ok();
    let content_open_guard = crate::provider_sources::forbid_ordinary_file_content_open(&source);
    let replay =
        import_codex_native_session_root(&source_root, &mut store, options.clone()).unwrap();
    drop(content_open_guard);

    assert_eq!(replay.imported_sessions, 0);
    assert_eq!(replay.imported_events, 0);
    assert_eq!(replay.skipped_sessions, 1, "{replay:?}");
    assert_eq!(
        observer
            .query_row("PRAGMA data_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        before_data_version
    );
    assert_eq!(std::fs::read(&database).unwrap(), before_database);
    assert_eq!(std::fs::read(&wal_path).ok(), before_wal);

    use std::io::Write;
    let mut source_file = std::fs::OpenOptions::new()
        .append(true)
        .open(&source)
        .unwrap();
    writeln!(source_file, "{}", message("appended")).unwrap();
    drop(source_file);
    let appended = import_codex_native_session_root(&source_root, &mut store, options).unwrap();
    assert_eq!(appended.imported_events, 1);
    assert_eq!(appended.failed, 0);
}

#[test]
fn over_2043_event_rewrite_splits_staging_and_clears_rejections_then_noops() {
    const EVENT_COUNT: usize = 2_050;
    const SESSION_ID: &str = "00000000-0000-0000-0000-000000002050";

    let temp = tempfile::TempDir::new().unwrap();
    let source_root = temp.path().join("sessions");
    std::fs::create_dir(&source_root).unwrap();
    let source = source_root.join("rollout-rewrite-capacity.jsonl");
    let fixture = |prefix: &str, malformed: bool| {
        let mut contents = serde_json::to_string(&serde_json::json!({
            "timestamp": "2026-01-01T00:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": SESSION_ID,
                "timestamp": "2026-01-01T00:00:00Z",
                "cwd": "/workspace",
                "source": "cli"
            }
        }))
        .unwrap();
        contents.push('\n');
        if malformed {
            contents.push_str("{not-json}\n");
        }
        for index in 0..EVENT_COUNT {
            contents.push_str(
                &serde_json::to_string(&serde_json::json!({
                    "timestamp": "2026-01-01T00:00:01Z",
                    "type": "response_item",
                    "payload": {
                        "type": "message",
                        "role": "user",
                        "content": [{
                            "type": "input_text",
                            "text": format!("{prefix}-{index}")
                        }]
                    }
                }))
                .unwrap(),
            );
            contents.push('\n');
        }
        contents
    };
    std::fs::write(&source, fixture("initial", true)).unwrap();

    let database = temp.path().join("work.sqlite");
    let mut store = Store::open(&database).unwrap();
    let options = CodexSessionImportOptions {
        machine_id: "rewrite-capacity-machine".to_owned(),
        source_path: Some(source_root),
        imported_at: "2026-01-02T00:00:00Z".parse().unwrap(),
        ..CodexSessionImportOptions::default()
    };
    let initial =
        import_codex_native_session_files(vec![source.clone()], &mut store, options.clone())
            .unwrap();
    assert_eq!(initial.imported_events, EVENT_COUNT);
    assert_eq!(initial.failed, 1);

    std::fs::write(&source, fixture("repaired-and-longer", false)).unwrap();
    let rewritten =
        import_codex_native_session_files(vec![source.clone()], &mut store, options.clone())
            .unwrap();
    // Removing the malformed row shifts one stable ordinal into a new insert;
    // the other rows are canonical in-place fallback-hash replacements.
    assert_eq!(rewritten.imported_events, 1);
    assert_eq!(rewritten.failed, 0);
    let inspection = rusqlite::Connection::open(&database).unwrap();
    let (live_count, repaired_count, live_initial_count, retired_count): (
        usize,
        usize,
        usize,
        usize,
    ) = inspection
        .query_row(
            "SELECT SUM(deleted_at_ms IS NULL),
                    SUM(deleted_at_ms IS NULL
                        AND payload_json LIKE '%repaired-and-longer%'),
                    SUM(deleted_at_ms IS NULL AND payload_json LIKE '%initial-%'),
                    SUM(deleted_at_ms IS NOT NULL)
             FROM events",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(live_count, EVENT_COUNT);
    assert_eq!(repaired_count, EVENT_COUNT);
    assert_eq!(live_initial_count, 0);
    assert_eq!(retired_count, 1);
    let generation_chunks: usize = inspection
        .query_row(
            "SELECT COUNT(*) FROM projection_journal_chunks
             WHERE generation = (SELECT MAX(generation) FROM projection_journal_chunks)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        generation_chunks > 1,
        "a >2,043-event generation must use multiple bounded Store groups"
    );

    let observer = rusqlite::Connection::open(&database).unwrap();
    let before_data_version = observer
        .query_row("PRAGMA data_version", [], |row| row.get::<_, i64>(0))
        .unwrap();
    let before_database = std::fs::read(&database).unwrap();
    let wal_path = database.with_extension("sqlite-wal");
    let before_wal = std::fs::read(&wal_path).ok();
    let noop = import_codex_native_session_files(vec![source], &mut store, options).unwrap();
    assert_eq!(noop.imported_events, 0);
    assert_eq!(noop.failed, 0);
    assert_eq!(noop.skipped_sessions, 1);
    assert_eq!(
        observer
            .query_row("PRAGMA data_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        before_data_version
    );
    assert_eq!(std::fs::read(&database).unwrap(), before_database);
    assert_eq!(std::fs::read(&wal_path).ok(), before_wal);
}

#[test]
fn rejection_source_label_is_path_free_escaped_and_bounded() {
    let source = CodexCatalogSource {
        source_root: "/private/source/root".to_owned(),
        source_path: Path::new("/private/source/root/2026/07").join(format!(
            "rollout\n{}-secret.jsonl",
            "x".repeat(MAX_CODEX_REJECTION_SOURCE_LABEL_BYTES * 2)
        )),
        cataloged_at_ms: 0,
        catalog_observation: super::super::CodexFileObservation {
            len: 0,
            modified_at_ms: 0,
            change_token: [0; 32],
        },
        catalog_native_session_id: None,
        catalog_parent_native_session_id: None,
        catalog_root_native_session_id: None,
        opened: None,
        authority_root: None,
        authority_relative_path: None,
    };
    let label = bounded_rejection_source_label(&source);

    assert!(label.len() <= MAX_CODEX_REJECTION_SOURCE_LABEL_BYTES);
    assert!(!label.contains("/private/source/root"));
    assert!(label.contains("2026"));
    assert!(label.contains("rollout"));
    assert!(!label.contains('\n'));
    assert!(label.contains("\\n"));
    assert!(label.ends_with("..."));
}
