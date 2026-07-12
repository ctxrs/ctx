use super::support::*;

#[test]
fn native_hermes_partial_import_crosses_batches_and_replays_idempotently() {
    let temp = tempdir();
    let fixture = write_hermes_batched_db(&temp, 130);
    let db_path = temp.path().join("work.sqlite");
    let mut store = Store::open(&db_path).unwrap();
    let options = HermesSqliteImportOptions {
        source_path: Some(fixture.clone()),
        allow_partial_failures: true,
        ..HermesSqliteImportOptions::default()
    };

    let first = import_hermes_sqlite(&fixture, &mut store, options.clone()).unwrap();
    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 130);
    assert_eq!(
        store
            .search_event_hits("hermes-batched-message-129", 10)
            .unwrap()
            .len(),
        1
    );
    assert_bounded_wal(&db_path);

    let replay = import_hermes_sqlite(&fixture, &mut store, options).unwrap();
    assert_eq!(replay.failed, 0, "{:?}", replay.failures);
    assert_eq!(replay.imported_events, 0);
    assert_eq!(replay.skipped_events, 130);
    assert_eq!(
        store
            .search_event_hits("hermes-batched-message-129", 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn native_hermes_nonpartial_import_preserves_atomic_search_projection() {
    let temp = tempdir();
    let fixture = write_hermes_batched_db(&temp, 70);
    let db_path = temp.path().join("work.sqlite");
    let mut store = Store::open(&db_path).unwrap();

    let summary = import_hermes_sqlite(
        &fixture,
        &mut store,
        HermesSqliteImportOptions {
            source_path: Some(fixture.clone()),
            ..HermesSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_events, 70);
    assert_eq!(
        store
            .search_event_hits("hermes-batched-message-69", 10)
            .unwrap()
            .len(),
        1
    );
    assert_bounded_wal(&db_path);
}

fn assert_bounded_wal(db_path: &Path) {
    let wal_path = PathBuf::from(format!("{}-wal", db_path.display()));
    let wal_bytes = fs::metadata(&wal_path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    assert!(
        wal_bytes <= 4 * 1024 * 1024,
        "WAL remained at {wal_bytes} bytes"
    );
}

fn write_hermes_batched_db(temp: &TempDir, messages: usize) -> PathBuf {
    let path = temp.path().join("hermes-batched.db");
    let mut conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE sessions (
            id TEXT PRIMARY KEY,
            source TEXT NOT NULL,
            started_at REAL NOT NULL
        );
        CREATE TABLE messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL,
            role TEXT NOT NULL,
            content TEXT,
            timestamp REAL NOT NULL,
            active INTEGER NOT NULL DEFAULT 1,
            compacted INTEGER NOT NULL DEFAULT 0
        );
        INSERT INTO sessions VALUES ('hermes-batched', 'acp', 1782259200.0);",
    )
    .unwrap();
    let transaction = conn.transaction().unwrap();
    for index in 0..messages {
        let role = if index % 2 == 0 { "user" } else { "assistant" };
        transaction
            .execute(
                "INSERT INTO messages (session_id, role, content, timestamp)
                 VALUES ('hermes-batched', ?1, ?2, ?3)",
                rusqlite::params![
                    role,
                    format!("hermes-batched-message-{index}"),
                    1782259201.0 + index as f64,
                ],
            )
            .unwrap();
    }
    transaction.commit().unwrap();
    path
}
