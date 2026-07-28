use std::{fs, io::Write as _, path::Path};

use ctx_history_core::database_path;
use ctx_history_index::VerifiedIndex;
use rusqlite::{params, Connection, OpenFlags};
use tempfile::tempdir;

use super::legacy::{
    inspect_legacy_projection, MAX_AVAILABLE_SOURCES, MAX_LEGACY_METADATA_CHARS,
    MAX_LEGACY_PREVIEW_CHARS, MAX_MIGRATION_CHUNK_BYTES, MAX_MIGRATION_CHUNK_ROWS,
};
use super::{
    complete_source_rebuild, inspect, journal_path, lexical_projection_path, prepare,
    prepare_with_chunk_limit, read_last_marker, record_source_rebuild_failure,
    rollback_unpublished, AvailableProviderSource, MigrationDecision, MigrationOrigin,
    MigrationPhase,
};

const LEGACY_FIXTURE_DDL: &str = r#"
CREATE TABLE capture_sources (
    id TEXT PRIMARY KEY,
    provider TEXT NOT NULL,
    source_format TEXT,
    raw_source_path TEXT,
    source_root TEXT
);
CREATE TABLE sessions (
    id TEXT PRIMARY KEY,
    provider TEXT NOT NULL,
    external_session_id TEXT,
    started_at_ms INTEGER
);
CREATE TABLE events (
    id TEXT PRIMARY KEY,
    seq INTEGER NOT NULL UNIQUE,
    session_id TEXT,
    capture_source_id TEXT,
    occurred_at_ms INTEGER NOT NULL,
    event_type TEXT NOT NULL,
    role TEXT,
    payload_json TEXT NOT NULL,
    deleted_at_ms INTEGER
);
CREATE TABLE event_search_lookup (
    event_id TEXT PRIMARY KEY,
    preview_text TEXT NOT NULL
);
PRAGMA user_version = 46;
"#;

struct LegacyFixture {
    source_present: std::path::PathBuf,
    source_missing: std::path::PathBuf,
    database_before: Vec<u8>,
}

fn create_current_store(data_root: &Path) {
    fs::create_dir_all(data_root).unwrap();
    Connection::open(database_path(data_root.to_path_buf()))
        .unwrap()
        .execute_batch(
            "CREATE TABLE current_marker (value TEXT);
             INSERT INTO current_marker VALUES ('unchanged');
             PRAGMA user_version = 47;",
        )
        .unwrap();
}

fn create_legacy_store(data_root: &Path, event_count: usize) -> LegacyFixture {
    fs::create_dir_all(data_root).unwrap();
    let source_present = data_root.join("provider-present.jsonl");
    let source_missing = data_root.join("provider-missing.jsonl");
    fs::write(&source_present, b"provider-owned source remains").unwrap();
    let db_path = database_path(data_root.to_path_buf());
    let mut conn = Connection::open(&db_path).unwrap();
    conn.execute_batch(LEGACY_FIXTURE_DDL).unwrap();
    conn.execute(
        "INSERT INTO capture_sources
             (id, provider, source_format, raw_source_path, source_root)
         VALUES ('source-present', 'codex', 'codex_session_jsonl_tree', ?1, ?1)",
        [source_present.to_string_lossy().as_ref()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO capture_sources
             (id, provider, source_format, raw_source_path, source_root)
         VALUES ('source-missing', 'codex', 'codex_session_jsonl_tree', ?1, ?1)",
        [source_missing.to_string_lossy().as_ref()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO sessions
             (id, provider, external_session_id, started_at_ms)
         VALUES ('session-present', 'codex', 'native-present', 1000),
                ('session-missing', 'codex', 'native-missing', 2000)",
        [],
    )
    .unwrap();
    let tx = conn.transaction().unwrap();
    for index in 0..event_count {
        let present = index % 2 == 0;
        let source = if present {
            "source-present"
        } else {
            "source-missing"
        };
        let session = if present {
            "session-present"
        } else {
            "session-missing"
        };
        let event_id = format!("event-{index:04}");
        let preview = if index == 1 {
            "界".repeat(MAX_LEGACY_PREVIEW_CHARS + 400)
        } else {
            format!("bounded legacy preview {index}")
        };
        tx.execute(
            "INSERT INTO events (
                 id, seq, session_id, capture_source_id, occurred_at_ms,
                 event_type, role, payload_json, deleted_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'message', 'user', ?6, NULL)",
            params![
                event_id,
                index as i64,
                session,
                source,
                10_000_i64 + index as i64,
                format!(r#"{{"text":"canonical body {index}"}}"#),
            ],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO event_search_lookup (event_id, preview_text) VALUES (?1, ?2)",
            params![event_id, preview],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    drop(conn);
    let database_before = fs::read(&db_path).unwrap();
    LegacyFixture {
        source_present,
        source_missing,
        database_before,
    }
}

fn available_source(path: &Path) -> AvailableProviderSource {
    AvailableProviderSource::new("codex", "codex_session_jsonl_tree", path.to_path_buf())
}

#[test]
fn inspection_is_read_only_for_a_fresh_root() {
    let temp = tempdir().unwrap();
    let data_root = temp.path().join("not-created");

    let marker = inspect(&data_root).unwrap().unwrap();

    assert_eq!(marker.origin, MigrationOrigin::Fresh);
    assert_eq!(marker.phase, MigrationPhase::Detected);
    assert!(!data_root.exists());
}

#[test]
fn fresh_prepare_initializes_only_an_empty_disposable_projection() {
    let temp = tempdir().unwrap();
    let data_root = temp.path().join("data");

    let first = prepare(&data_root, &[]).unwrap();
    let first_marker = first.marker();
    let generation = first_marker.lexical_generation_id.as_deref().unwrap();

    assert!(matches!(first, MigrationDecision::RebuildFromSources(_)));
    assert!(first.daemon_rebuild_required());
    assert_eq!(first_marker.origin, MigrationOrigin::Fresh);
    assert_eq!(first_marker.phase, MigrationPhase::RebuildPending);
    assert!(!database_path(data_root.clone()).exists());
    let index = VerifiedIndex::open(lexical_projection_path(&data_root)).unwrap();
    assert_eq!(index.generation_id(), generation);
    assert_eq!(index.document_count(), 0);
    assert!(index.manifest().sources.is_empty());

    let second = prepare(&data_root, &[]).unwrap();
    assert_eq!(
        second.marker().lexical_generation_id.as_deref(),
        Some(generation)
    );
    assert_eq!(second.marker().migration_id, first_marker.migration_id);
}

#[test]
fn source_rebuild_failure_resumes_and_completion_verifies_the_generation() {
    let temp = tempdir().unwrap();
    let data_root = temp.path().join("data");
    let prepared = prepare(&data_root, &[]).unwrap();
    let generation = prepared
        .marker()
        .lexical_generation_id
        .as_deref()
        .unwrap()
        .to_owned();

    let failed = record_source_rebuild_failure(&data_root, "provider source was busy").unwrap();
    assert_eq!(failed.phase, MigrationPhase::SourceRebuildFailed);
    assert!(failed.resumable);
    assert_eq!(failed.error.as_deref(), Some("provider source was busy"));

    let mismatch = complete_source_rebuild(&data_root, "wrong-generation").unwrap_err();
    assert!(format!("{mismatch}").contains("generation mismatch"));
    assert_eq!(
        read_last_marker(&data_root).unwrap().unwrap().phase,
        MigrationPhase::SourceRebuildFailed
    );

    let resumed = prepare(&data_root, &[]).unwrap();
    assert_eq!(resumed.marker().phase, MigrationPhase::RebuildPending);
    assert!(resumed.marker().error.is_none());
    let completed = complete_source_rebuild(&data_root, &generation).unwrap();
    assert!(matches!(completed, MigrationDecision::Ready(_)));
    assert_eq!(completed.marker().phase, MigrationPhase::Ready);
    assert!(!completed.daemon_rebuild_required());
    assert!(!completed.marker().resumable);

    let idempotent = prepare(&data_root, &[]).unwrap();
    assert!(matches!(idempotent, MigrationDecision::Ready(_)));
}

#[test]
fn current_store_is_never_opened_through_writable_store_migration() {
    let temp = tempdir().unwrap();
    let data_root = temp.path().join("data");
    create_current_store(&data_root);
    let db_path = database_path(data_root.clone());
    let before = fs::read(&db_path).unwrap();

    let decision = prepare(&data_root, &[]).unwrap();

    assert_eq!(decision.marker().origin, MigrationOrigin::CurrentV47);
    assert_eq!(decision.marker().phase, MigrationPhase::RebuildPending);
    assert_eq!(fs::read(&db_path).unwrap(), before);
    let conn = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        47
    );
}

#[test]
fn legacy_exception_contains_only_rows_whose_exact_source_is_missing() {
    let temp = tempdir().unwrap();
    let data_root = temp.path().join("data");
    let fixture = create_legacy_store(&data_root, 4);
    let db_path = database_path(data_root.clone());

    let decision = prepare(&data_root, &[available_source(&fixture.source_present)]).unwrap();
    let marker = decision.marker();
    let legacy = marker.legacy_projection.as_ref().unwrap();

    assert!(matches!(
        decision,
        MigrationDecision::RebuildFromSourcesWithLegacyException(_)
    ));
    assert_eq!(marker.origin, MigrationOrigin::ReleasedV46);
    assert_eq!(marker.phase, MigrationPhase::RebuildPending);
    assert!(marker.source_rebuild_required);
    assert_eq!(legacy.examined_events, 4);
    assert_eq!(legacy.source_backed_events, 2);
    assert_eq!(legacy.legacy_only_events, 2);
    assert_eq!(fs::read(&db_path).unwrap(), fixture.database_before);
    assert!(!fixture.source_missing.exists());

    let inspection = inspect_legacy_projection(&legacy.path).unwrap();
    assert!(inspection.complete);
    assert_eq!(inspection.legacy_only_events, 2);
    assert!(!inspection
        .columns
        .iter()
        .any(|column| column == "payload_json"));
    assert!(!inspection.columns.iter().any(|column| column == "body"));
    assert!(fs::metadata(&legacy.path).unwrap().permissions().readonly());

    let projection =
        Connection::open_with_flags(&legacy.path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let ids = projection
        .prepare("SELECT legacy_event_id FROM legacy_events ORDER BY legacy_event_seq")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(ids, ["event-0001", "event-0003"]);
    let longest: i64 = projection
        .query_row(
            "SELECT MAX(length(preview_text)) FROM legacy_events",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(longest as usize, MAX_LEGACY_PREVIEW_CHARS);
}

#[test]
fn legacy_store_with_all_sources_surviving_needs_no_exception_rows() {
    let temp = tempdir().unwrap();
    let data_root = temp.path().join("data");
    let fixture = create_legacy_store(&data_root, 2);
    fs::write(&fixture.source_missing, b"restored provider source").unwrap();

    let decision = prepare(
        &data_root,
        &[
            available_source(&fixture.source_present),
            available_source(&fixture.source_missing),
        ],
    )
    .unwrap();

    assert!(matches!(decision, MigrationDecision::RebuildFromSources(_)));
    assert!(decision.marker().legacy_projection.is_none());
    assert!(!super::migration_directory(&data_root)
        .join("legacy-read-only-v0.sqlite")
        .exists());
    assert_eq!(
        fs::read(database_path(data_root)).unwrap(),
        fixture.database_before
    );
}

#[test]
fn chunk_progress_resumes_from_the_stage_database() {
    let temp = tempdir().unwrap();
    let data_root = temp.path().join("data");
    let fixture = create_legacy_store(&data_root, 130);

    let partial = prepare_with_chunk_limit(
        &data_root,
        &[available_source(&fixture.source_present)],
        Some(1),
    )
    .unwrap();
    let marker = partial.marker();
    assert_eq!(marker.phase, MigrationPhase::LegacyProjectionBuilding);
    let stage = marker.legacy_projection.as_ref().unwrap();
    assert_eq!(stage.examined_events, MAX_MIGRATION_CHUNK_ROWS as u64);

    // Append a valid but stale outer marker. Resume must trust the progress
    // committed in the stage transaction, not this advisory copy.
    let mut stale = marker.clone();
    stale.legacy_projection.as_mut().unwrap().examined_events = 0;
    super::append_marker(&data_root, &stale).unwrap();

    let completed = prepare(&data_root, &[available_source(&fixture.source_present)]).unwrap();
    let legacy = completed.marker().legacy_projection.as_ref().unwrap();
    assert_eq!(legacy.examined_events, 130);
    assert_eq!(legacy.source_backed_events, 65);
    assert_eq!(legacy.legacy_only_events, 65);
    assert_eq!(
        fs::read(database_path(data_root)).unwrap(),
        fixture.database_before
    );
}

#[test]
fn changed_legacy_store_fails_closed_and_leaves_stage_resumable() {
    let temp = tempdir().unwrap();
    let data_root = temp.path().join("data");
    let fixture = create_legacy_store(&data_root, 70);

    let partial = prepare_with_chunk_limit(
        &data_root,
        &[available_source(&fixture.source_present)],
        Some(1),
    )
    .unwrap();
    let stage_path = partial
        .marker()
        .legacy_projection
        .as_ref()
        .unwrap()
        .path
        .clone();
    assert!(stage_path.exists());

    let db_path = database_path(data_root.clone());
    Connection::open(&db_path)
        .unwrap()
        .execute(
            "INSERT INTO events (
                 id, seq, session_id, capture_source_id, occurred_at_ms,
                 event_type, role, payload_json, deleted_at_ms
             ) VALUES ('late-event', 1000, 'session-missing', 'source-missing',
                       9000, 'message', 'user', '{}', NULL)",
            [],
        )
        .unwrap();

    let error = prepare(&data_root, &[available_source(&fixture.source_present)]).unwrap_err();

    assert!(
        format!("{error:#}").contains("changed since this legacy projection attempt began"),
        "{error:#}"
    );
    assert!(stage_path.exists());
    let marker = read_last_marker(&data_root).unwrap().unwrap();
    assert_eq!(marker.phase, MigrationPhase::LegacyProjectionFailed);
    assert!(marker.resumable);
    assert!(!marker
        .legacy_projection
        .as_ref()
        .is_some_and(|summary| summary.path.ends_with("legacy-read-only-v0.sqlite")));
}

#[test]
fn changed_provider_source_availability_fails_closed() {
    let temp = tempdir().unwrap();
    let data_root = temp.path().join("data");
    let fixture = create_legacy_store(&data_root, 70);

    let partial = prepare_with_chunk_limit(
        &data_root,
        &[available_source(&fixture.source_present)],
        Some(1),
    )
    .unwrap();
    assert_eq!(
        partial.marker().phase,
        MigrationPhase::LegacyProjectionBuilding
    );
    fs::remove_file(&fixture.source_present).unwrap();

    let error = prepare(&data_root, &[available_source(&fixture.source_present)]).unwrap_err();

    assert!(
        format!("{error:#}").contains("provider source availability changed"),
        "{error:#}"
    );
    assert_eq!(
        read_last_marker(&data_root).unwrap().unwrap().phase,
        MigrationPhase::LegacyProjectionFailed
    );
    assert_eq!(
        fs::read(database_path(data_root)).unwrap(),
        fixture.database_before
    );
}

#[test]
fn oversized_legacy_metadata_fails_without_copying_the_row() {
    let temp = tempdir().unwrap();
    let data_root = temp.path().join("data");
    let fixture = create_legacy_store(&data_root, 1);
    let db_path = database_path(data_root.clone());
    Connection::open(&db_path)
        .unwrap()
        .execute(
            "UPDATE events SET event_type = ?1 WHERE id = 'event-0000'",
            ["x".repeat(MAX_LEGACY_METADATA_CHARS + 1)],
        )
        .unwrap();
    let before = fs::read(&db_path).unwrap();

    let error = prepare(&data_root, &[available_source(&fixture.source_present)]).unwrap_err();

    assert!(
        format!("{error:#}").contains("event type exceeds"),
        "{error:#}"
    );
    assert_eq!(fs::read(&db_path).unwrap(), before);
    assert_eq!(
        read_last_marker(&data_root).unwrap().unwrap().phase,
        MigrationPhase::LegacyProjectionFailed
    );
    assert!(!super::migration_directory(&data_root)
        .join("legacy-read-only-v0.sqlite")
        .exists());
}

#[test]
fn rollback_discards_only_unpublished_projection_work() {
    let temp = tempdir().unwrap();
    let data_root = temp.path().join("data");
    let fixture = create_legacy_store(&data_root, 70);

    let partial = prepare_with_chunk_limit(
        &data_root,
        &[available_source(&fixture.source_present)],
        Some(1),
    )
    .unwrap();
    let stage = partial
        .marker()
        .legacy_projection
        .as_ref()
        .unwrap()
        .path
        .clone();

    let rolled_back = rollback_unpublished(&data_root).unwrap().unwrap();

    assert_eq!(rolled_back.phase, MigrationPhase::RolledBack);
    assert!(!stage.exists());
    assert_eq!(
        fs::read(database_path(data_root)).unwrap(),
        fixture.database_before
    );
    assert!(rolled_back.lexical_projection_path.exists());
}

#[test]
fn torn_journal_tail_preserves_the_last_durable_state() {
    let temp = tempdir().unwrap();
    let data_root = temp.path().join("data");
    let decision = prepare(&data_root, &[]).unwrap();
    let expected = decision.marker().clone();
    let path = journal_path(&data_root);
    OpenOptionsExt::append_bytes(&path, br#"{"schema_version":"#);

    let observed = read_last_marker(&data_root).unwrap().unwrap();

    assert_eq!(observed, expected);

    let failed = record_source_rebuild_failure(&data_root, "retryable").unwrap();
    let observed = read_last_marker(&data_root).unwrap().unwrap();
    assert_eq!(observed, failed);
}

#[test]
fn unsupported_store_schema_is_rejected_without_mutation() {
    let temp = tempdir().unwrap();
    let data_root = temp.path().join("data");
    fs::create_dir_all(&data_root).unwrap();
    let db_path = database_path(data_root.clone());
    Connection::open(&db_path)
        .unwrap()
        .execute_batch("CREATE TABLE future (value TEXT); PRAGMA user_version = 48;")
        .unwrap();
    let before = fs::read(&db_path).unwrap();

    let error = prepare(&data_root, &[]).unwrap_err();

    assert!(format!("{error}").contains("unsupported ctx Store schema 48"));
    assert_eq!(fs::read(&db_path).unwrap(), before);
    assert!(!lexical_projection_path(&data_root).exists());
    assert!(!super::migration_directory(&data_root).exists());
}

#[test]
fn migration_bounds_are_explicit_and_small() {
    assert_eq!(MAX_MIGRATION_CHUNK_ROWS, 64);
    assert_eq!(MAX_MIGRATION_CHUNK_BYTES, 8 * 1024 * 1024);
    assert_eq!(MAX_LEGACY_PREVIEW_CHARS, 2_048);
    assert_eq!(MAX_LEGACY_METADATA_CHARS, 4_096);
    assert_eq!(MAX_AVAILABLE_SOURCES, 256);
}

struct OpenOptionsExt;

impl OpenOptionsExt {
    fn append_bytes(path: &Path, bytes: &[u8]) {
        let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(bytes).unwrap();
        file.sync_all().unwrap();
    }
}
