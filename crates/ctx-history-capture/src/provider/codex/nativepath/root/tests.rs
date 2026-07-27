use super::*;

#[test]
fn large_source_publishes_exact_journal_envelopes_and_replays_as_noop() {
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
    assert_eq!(replay.skipped_sessions, 1);
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

/// Rewrites an already imported store into the shape a released v0.25 store
/// has after it migrates to the current schema.
///
/// A released store has no projection journal at all, no NativePath
/// publication cursors, and capture-source identities that predate the current
/// canonical identity. Its session rows also predate the current canonical
/// actor columns, so the upgrading import rewrites the actor that every event
/// observation cites.
fn released_store_shape(store: Store, database: &Path) {
    // v0.25 had no projection journal, so it also had no writer fence.
    store.disable_projection_journal().unwrap();
    drop(store);
    let conn = rusqlite::Connection::open(database).unwrap();
    conn.execute("DELETE FROM sync_cursors", []).unwrap();
    conn.execute(
        "UPDATE capture_sources SET source_identity = '46b1b4bc-66b2-773d-89ef-30b895fef4a2'",
        [],
    )
    .unwrap();
    conn.execute("UPDATE sessions SET role_hint = NULL", [])
        .unwrap();
}

#[test]
fn released_store_upgrade_import_publishes_and_then_replays_as_a_noop() {
    const EVENT_COUNT: usize = 512;
    const BODY_BYTES: usize = 32 * 1024;
    const SESSION_ID: &str = "00000000-0000-0000-0000-000000000025";

    let temp = tempfile::TempDir::new().unwrap();
    let source_root = temp.path().join("sessions");
    std::fs::create_dir(&source_root).unwrap();
    let source = source_root.join(format!("rollout-2026-01-01T00-00-00-{SESSION_ID}.jsonl"));
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
                        "text": format!("upgrade-arm-{index}-{}", "x".repeat(BODY_BYTES))
                    }]
                }
            }))
            .unwrap(),
        );
        contents.push('\n');
    }
    std::fs::write(&source, contents).unwrap();

    let database = temp.path().join("work.sqlite");
    let options = CodexSessionImportOptions {
        machine_id: "released-upgrade-machine".to_owned(),
        source_path: Some(source_root.clone()),
        imported_at: "2026-01-02T00:00:00Z".parse().unwrap(),
        ..CodexSessionImportOptions::default()
    };

    let mut released = Store::open(&database).unwrap();
    let first = import_codex_native_session_root(&source_root, &mut released, options.clone())
        .expect("released import");
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, EVENT_COUNT);
    released_store_shape(released, &database);

    let mut store = Store::open(&database).unwrap();
    let upgrade = import_codex_native_session_root(&source_root, &mut store, options.clone())
        .expect("upgrade import must not fail on a released store");
    assert_eq!(upgrade.failed, 0);
    assert_eq!(
        upgrade.imported_events + upgrade.skipped_events,
        EVENT_COUNT
    );

    // A second run over the unchanged corpus is a no-op.
    let replay = import_codex_native_session_root(&source_root, &mut store, options)
        .expect("repeat import after upgrade");
    assert_eq!(replay.imported_events, 0);
    assert_eq!(replay.imported_sessions, 0);
    assert_eq!(replay.failed, 0);
    drop(store);

    let conn = rusqlite::Connection::open(&database).unwrap();
    assert_eq!(
        conn.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert_eq!(
        conn.prepare("PRAGMA foreign_key_check")
            .unwrap()
            .query_map([], |_| Ok(()))
            .unwrap()
            .count(),
        0
    );
    // This test proves publication: the upgrade commits instead of failing on
    // a group bound, and the store it leaves is structurally sound. It
    // deliberately makes no claim about the canonical event set, because a
    // count assertion here would have to be a floor and a floor accepts a
    // doubled corpus. That claim belongs to
    // `released_store_upgrade_matches_a_fresh_install_canonical_set` below.
    let (events, distinct_events) = conn
        .query_row(
            "SELECT COUNT(*), COUNT(DISTINCT id) FROM events WHERE deleted_at_ms IS NULL",
            [],
            |row| Ok((row.get::<_, usize>(0)?, row.get::<_, usize>(1)?)),
        )
        .unwrap();
    assert_eq!(
        events, distinct_events,
        "canonical event ids must be unique"
    );
    assert!(
        conn.query_row(
            "SELECT COUNT(*) FROM projection_journal_chunks",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
            > 0
    );
}

/// Counts the canonical rows an import is supposed to produce.
fn canonical_counts(database: &Path) -> (i64, i64, i64, i64) {
    let conn = rusqlite::Connection::open(database).unwrap();
    let count = |sql: &str| conn.query_row(sql, [], |row| row.get::<_, i64>(0)).unwrap();
    (
        count("SELECT COUNT(*) FROM events WHERE deleted_at_ms IS NULL"),
        count("SELECT COUNT(*) FROM sessions WHERE deleted_at_ms IS NULL"),
        count("SELECT COUNT(*) FROM session_edges WHERE deleted_at_ms IS NULL"),
        count("SELECT COUNT(*) FROM capture_sources"),
    )
}

/// The acceptance test for upgrading a released store without duplicating it.
///
/// An upgrading import must leave exactly the canonical set a fresh install of
/// the same build produces from the same corpus — same events, sessions, edges
/// and capture sources, not a floor. A floor is what a duplicating upgrade
/// passes: on the 1.087 GB Codex corpus the upgraded store held 284,923 events
/// against a fresh install's 142,950, and every assertion that only required
/// "at least as many" reported success.
#[test]
#[ignore = "fails until upgrades rebuild and replace a pre-0.26 provider projection \
            instead of publishing alongside it: v0.26 derives capture-source identities \
            that do not match released rows, so it mints new sources and new event \
            identities and the released rows are never retired. Reconciliation was \
            rejected by the product owner; the re-derive branch owns the fix and this \
            test is its acceptance gate. Do not weaken it to a floor."]
fn released_store_upgrade_matches_a_fresh_install_canonical_set() {
    const EVENT_COUNT: usize = 512;
    const BODY_BYTES: usize = 32 * 1024;
    const SESSION_ID: &str = "00000000-0000-0000-0000-000000000026";

    let temp = tempfile::TempDir::new().unwrap();
    let source_root = temp.path().join("sessions");
    std::fs::create_dir(&source_root).unwrap();
    let source = source_root.join(format!("rollout-2026-01-01T00-00-00-{SESSION_ID}.jsonl"));
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
                        "text": format!("canonical-set-{index}-{}", "x".repeat(BODY_BYTES))
                    }]
                }
            }))
            .unwrap(),
        );
        contents.push('\n');
    }
    std::fs::write(&source, contents).unwrap();

    let options = |root: &Path| CodexSessionImportOptions {
        machine_id: "canonical-set-machine".to_owned(),
        source_path: Some(root.to_path_buf()),
        imported_at: "2026-01-02T00:00:00Z".parse().unwrap(),
        ..CodexSessionImportOptions::default()
    };

    // Control: a fresh install of this build over this corpus.
    let control_database = temp.path().join("fresh.sqlite");
    let mut control = Store::open(&control_database).unwrap();
    import_codex_native_session_root(&source_root, &mut control, options(&source_root))
        .expect("fresh install import");
    drop(control);
    let expected = canonical_counts(&control_database);
    assert_eq!(expected.0, EVENT_COUNT as i64, "control lost events");

    // Subject: the same corpus imported into a released store.
    let database = temp.path().join("upgraded.sqlite");
    let mut released = Store::open(&database).unwrap();
    import_codex_native_session_root(&source_root, &mut released, options(&source_root))
        .expect("released import");
    released_store_shape(released, &database);
    let mut store = Store::open(&database).unwrap();
    import_codex_native_session_root(&source_root, &mut store, options(&source_root))
        .expect("upgrade import");
    drop(store);

    assert_eq!(
        canonical_counts(&database),
        expected,
        "upgrading a released store must leave exactly the fresh-install canonical set \
         (events, sessions, edges, capture sources)"
    );
}
