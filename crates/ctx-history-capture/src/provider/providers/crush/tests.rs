use std::{fs, io::Write};

use super::capture::{
    candidate_sql as crush_candidate_sql, decode_position as decode_crush_position,
    encode_position as encode_crush_position, fetch_candidate as crush_fetch_candidate,
    fetch_optional_candidate as crush_fetch_optional_candidate,
    initial_position as initial_crush_position, oversize_limit as crush_oversize_limit,
    sqlite_batch_error as crush_sqlite_batch_error,
    with_length_preflight as with_crush_length_preflight, CrushCandidate, CrushKeyset, CrushPhase,
    CrushRowFetcher, CRUSH_SQLITE_VALUE_OVERHEAD_BYTES,
};
use super::projection::{
    decode_message_child as decode_crush_message_child, CrushCapturedBatchProjector,
};
use super::source::{
    message_columns as crush_message_columns, optional_file_columns as crush_optional_file_columns,
    optional_read_file_columns as crush_optional_read_file_columns,
    session_columns as crush_session_columns, source_revision as crush_source_revision,
    source_snapshot as crush_source_snapshot,
};
use super::*;
use chrono::{DateTime, Utc};
use ctx_history_core::{EntityTimestamps, EventType, FileChangeKind, SyncCursor};
use rusqlite::Connection;
use serde_json::json;

use crate::captured_batch::{
    CapturedRecordPayload, CapturedSqliteValue, SourceObservation, CAPTURE_BATCH_MAX_RECORDS,
};
use crate::provider::file_touches::PROVIDER_FILE_TOUCH_LIMIT_REJECTION;
use crate::provider::importer::{
    captured_batch_cursor_stream, provider_path_identity, provider_source_cursor_stream_for_path,
    BoundedParserCheckpoint, CapturedBatchProjector, CertifiedProviderCursor,
    ProviderProjectionOutput, ProviderProjectionResult,
};
use crate::ProviderNormalizationResult;

const PREVIOUS_CRUSH_CAPTURE_REVISION: u32 = 2;
const UPSTREAM_CRUSH_INITIAL_DDL: &str = include_str!(
    "../../../../../../tests/fixtures/provider-history/crush/upstream-v1/20250424200609_initial.sql"
);

#[derive(Default)]
struct CollectingProjectionOutput {
    normalizations: Vec<ProviderNormalizationResult>,
    rejections: Vec<(usize, String)>,
}

impl ProviderProjectionOutput for CollectingProjectionOutput {
    fn emit_normalization(
        &mut self,
        normalization: ProviderNormalizationResult,
    ) -> ProviderProjectionResult<()> {
        self.normalizations.push(normalization);
        Ok(())
    }

    fn reject_record(&mut self, line_number: usize, reason: String) {
        self.rejections.push((line_number, reason));
    }
}

fn create_crush_tables(conn: &Connection) {
    conn.execute_batch(
        "create table sessions (
            id text primary key,
            parent_session_id text,
            title text,
            prompt_tokens integer,
            completion_tokens integer,
            cost real,
            created_at integer,
            updated_at integer,
            summary_message_id text
        );
        create table messages (
            id text primary key,
            session_id text not null,
            role text not null,
            parts text not null,
            created_at integer,
            updated_at integer,
            provider text,
            model text,
            is_summary_message integer not null default 0
        );
        create table files (
            session_id text,
            path text not null,
            version text,
            created_at integer,
            updated_at integer
        );
        create table read_files (
            session_id text not null,
            path text not null,
            read_at integer
        );",
    )
    .unwrap();
}

fn create_upstream_crush_tables(conn: &Connection) {
    let (up, _) = UPSTREAM_CRUSH_INITIAL_DDL
        .split_once("-- +goose Down")
        .expect("upstream Crush migration contains its Down boundary");
    conn.execute_batch(up).unwrap();
}

fn insert_upstream_crush_rows(conn: &Connection) {
    conn.execute(
        "insert into sessions (id, title, updated_at, created_at) values (?1, ?2, ?3, ?4)",
        rusqlite::params!["upstream-session", "Upstream Crush", 4_000_i64, 1_000_i64],
    )
    .unwrap();
    conn.execute(
        "insert into messages \
         (id, session_id, role, parts, model, created_at, updated_at, finished_at) \
         values (?1, ?2, 'user', ?3, ?4, ?5, ?6, null)",
        rusqlite::params![
            "upstream-message",
            "upstream-session",
            json!([{"type": "text", "data": {"text": "upstream integer version oracle"}}])
                .to_string(),
            "upstream-model",
            2_000_i64,
            3_000_i64,
        ],
    )
    .unwrap();
    conn.execute(
        "insert into files \
         (id, session_id, path, content, version, created_at, updated_at) \
         values (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            "upstream-file",
            "upstream-session",
            "src/upstream.rs",
            "fn upstream() {}",
            7_i64,
            2_500_i64,
            3_500_i64,
        ],
    )
    .unwrap();
}

fn upstream_terminal_position() -> crate::captured_batch::NativePosition {
    encode_crush_position(CrushKeyset {
        phase: CrushPhase::Files,
        next_ordinal: 3,
        rowid: 1,
    })
    .unwrap()
}

fn test_source(identity: &str) -> SourceObservation {
    SourceObservation::new(
        CaptureProvider::Crush,
        CRUSH_SQLITE_SOURCE_FORMAT,
        format!("crush-sqlite:{identity}"),
        format!("crush-snapshot:{identity}"),
        format!("provider:crush:{identity}"),
        CRUSH_CAPTURE_REVISION,
        CRUSH_POLICY_REVISION,
        None,
    )
    .unwrap()
}

fn test_fetcher(conn: &Connection) -> CrushRowFetcher<'_> {
    let sessions = crush_session_columns(conn).unwrap();
    let messages = crush_message_columns(conn).unwrap();
    let files = crush_optional_file_columns(conn).unwrap();
    let read_files = crush_optional_read_file_columns(conn).unwrap();
    CrushRowFetcher::new(
        conn,
        &sessions,
        &messages,
        files.as_ref(),
        read_files.as_ref(),
    )
    .unwrap()
}

#[test]
fn upstream_integer_file_version_is_cast_to_text_without_changing_position_bytes() {
    let conn = Connection::open_in_memory().unwrap();
    create_upstream_crush_tables(&conn);
    insert_upstream_crush_rows(&conn);

    let version_type = conn
        .query_row(
            "select type from pragma_table_info('files') where name = 'version'",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap();
    assert_eq!(version_type, "INTEGER");
    let version_storage = conn
        .query_row("select typeof(version) from files", [], |row| {
            row.get::<_, String>(0)
        })
        .unwrap();
    assert_eq!(version_storage, "integer");

    let mut fetcher = test_fetcher(&conn);
    let mut producer = SqliteLogicalRowBatchProducer::new(
        test_source("upstream-integer-version"),
        initial_crush_position().unwrap(),
        move |position| fetcher.fetch(position),
    );
    let batch = with_sqlite_read_snapshot(&conn, || {
        producer.next_batch().map_err(crush_sqlite_batch_error)
    })
    .unwrap()
    .unwrap();
    assert!(batch.source_exhausted());
    assert_eq!(batch.records().len(), 3);
    assert!(conn.is_autocommit());

    let file_record = &batch.records()[2];
    assert_eq!(file_record.ordinal(), 2);
    assert_eq!(file_record.record_kind().as_str(), CRUSH_FILE_RECORD_KIND);
    assert_eq!(
        file_record.locator().value(),
        &[3, 128, 0, 0, 0, 0, 0, 0, 1]
    );
    let CapturedRecordPayload::SqliteValues(values) = file_record.payload() else {
        panic!("upstream Crush file row was not captured as SQLite values");
    };
    assert!(matches!(
        &values[3],
        CapturedSqliteValue::Text(version) if version == "7"
    ));

    let terminal = upstream_terminal_position();
    assert_eq!(batch.range_end(), &terminal);
    assert_eq!(batch.range_end().kind(), "crush-sqlite-keyset-v1");
    assert_eq!(
        batch.range_end().value(),
        &[3, 0, 0, 0, 0, 0, 0, 0, 3, 128, 0, 0, 0, 0, 0, 0, 1]
    );

    let context = ProviderAdapterContext {
        machine_id: "crush-upstream-projection".to_owned(),
        source_path: None,
        source_root: None,
        imported_at: DateTime::<Utc>::UNIX_EPOCH,
    };
    let mut projector = CrushCapturedBatchProjector::new(
        context,
        "upstream-crush.db".to_owned(),
        0,
        "upstream-schema".to_owned(),
    );
    let mut output = CollectingProjectionOutput::default();
    for record in batch.records() {
        projector.project_record(record, &mut output).unwrap();
    }
    assert!(output.rejections.is_empty());
    let touches = output
        .normalizations
        .iter()
        .flat_map(|normalization| normalization.files_touched.iter())
        .collect::<Vec<_>>();
    assert_eq!(touches.len(), 1);
    assert_eq!(touches[0].1.path, "src/upstream.rs");
    assert_eq!(touches[0].1.metadata["version"], "7");
}

#[test]
fn capture_revision_three_resets_v2_then_replays_idempotently() {
    assert_eq!(CRUSH_CAPTURE_REVISION, PREVIOUS_CRUSH_CAPTURE_REVISION + 1);

    let directory = crate::test_support_paths::tempdir().unwrap();
    let source_path = directory.path().join("crush.db");
    let source_conn = Connection::open(&source_path).unwrap();
    create_upstream_crush_tables(&source_conn);
    insert_upstream_crush_rows(&source_conn);
    drop(source_conn);

    let canonical_path = fs::canonicalize(&source_path).unwrap();
    let snapshot = crush_source_snapshot(&source_path).unwrap();
    let cursor_path = provider_path_identity(&canonical_path).unwrap();
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Crush,
        CRUSH_SQLITE_SOURCE_FORMAT,
        &cursor_path,
    );
    let source_conn = open_provider_sqlite_readonly(&source_path).unwrap();
    let schema_fingerprint = sqlite_schema_fingerprint(&source_conn).unwrap();
    let current_source_revision = crush_source_revision(&snapshot, &schema_fingerprint);
    drop(source_conn);
    assert!(current_source_revision.contains("capture=3;"));
    let previous_source_revision = current_source_revision.replacen("capture=3;", "capture=2;", 1);
    assert_ne!(previous_source_revision, current_source_revision);
    let observed_source = SourceObservation::new(
        CaptureProvider::Crush,
        CRUSH_SQLITE_SOURCE_FORMAT,
        format!("crush-sqlite:{cursor_path}"),
        current_source_revision.clone(),
        cursor_stream,
        CRUSH_CAPTURE_REVISION,
        CRUSH_POLICY_REVISION,
        None,
    )
    .unwrap();
    let stream = captured_batch_cursor_stream(&observed_source);
    let terminal = upstream_terminal_position();
    let old_cursor = CertifiedProviderCursor::new(
        previous_source_revision,
        PREVIOUS_CRUSH_CAPTURE_REVISION,
        CRUSH_POLICY_REVISION,
        terminal.clone(),
        BoundedParserCheckpoint::from_serializable(&()).unwrap(),
    )
    .unwrap();
    assert_eq!(old_cursor.native_position(), &terminal);

    let context = ProviderAdapterContext {
        machine_id: "crush-revision-reset".to_owned(),
        source_path: Some(source_path.clone()),
        source_root: None,
        imported_at: DateTime::<Utc>::UNIX_EPOCH,
    };
    let mut store = Store::open(directory.path().join("store.sqlite")).unwrap();
    store
        .upsert_sync_cursor(&SyncCursor {
            id: crate::stable_capture_uuid("crush-v2-cursor", "provider-sync-cursor"),
            team_id: None,
            device_id: context.machine_id.clone(),
            stream: stream.clone(),
            cursor: old_cursor.encode().unwrap(),
            last_synced_at: Some(DateTime::<Utc>::UNIX_EPOCH),
            timestamps: EntityTimestamps {
                created_at: DateTime::<Utc>::UNIX_EPOCH,
                updated_at: DateTime::<Utc>::UNIX_EPOCH,
            },
        })
        .unwrap();

    let upgraded = import_crush_sqlite_batched(
        &source_path,
        &mut store,
        context.clone(),
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(upgraded.failed, 0, "{:?}", upgraded.failures);
    assert_eq!(upgraded.imported_sessions, 1);
    assert_eq!(upgraded.imported_events, 1);
    let session = store
        .session_by_external_session(CaptureProvider::Crush, "upstream-session")
        .unwrap()
        .unwrap();
    let events = store.events_for_session(session.id).unwrap();
    assert_eq!(events.len(), 1);
    assert!(events[0]
        .payload
        .to_string()
        .contains("upstream integer version oracle"));
    let archive = store.export_archive().unwrap();
    assert_eq!(archive.files_touched.len(), 1);
    assert_eq!(archive.files_touched[0].path, "src/upstream.rs");
    assert_eq!(
        archive.files_touched[0].sync.metadata["metadata"]["version"],
        "7"
    );

    let published = store
        .get_sync_cursor(None, &context.machine_id, &stream)
        .unwrap()
        .unwrap();
    let certified = CertifiedProviderCursor::decode(&published.cursor).unwrap();
    assert_eq!(certified.source_revision(), current_source_revision);
    assert_eq!(certified.parser_revision(), CRUSH_CAPTURE_REVISION);
    assert_eq!(certified.policy_revision(), CRUSH_POLICY_REVISION);
    assert_eq!(certified.native_position(), &terminal);

    let replay = import_crush_sqlite_batched(
        &source_path,
        &mut store,
        context.clone(),
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(replay.failed, 0, "{:?}", replay.failures);
    assert_eq!(replay.imported_sessions, 0);
    assert_eq!(replay.imported_events, 0);
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 1);
    assert_eq!(store.export_archive().unwrap().files_touched.len(), 1);
    let replayed = store
        .get_sync_cursor(None, &context.machine_id, &stream)
        .unwrap()
        .unwrap();
    assert_eq!(replayed.cursor, published.cursor);
}

fn insert_session(conn: &Connection, index: usize) {
    conn.execute(
        "insert into sessions values (?1, null, ?2, 1, 2, 0.5, ?3, ?4, null)",
        rusqlite::params![
            format!("session-{index:03}"),
            format!("Session {index}"),
            i64::try_from(index).unwrap(),
            i64::try_from(index + 1).unwrap(),
        ],
    )
    .unwrap();
}

#[test]
fn logical_rows_page_at_sixty_four_and_replay_the_exact_keyset() {
    let conn = Connection::open_in_memory().unwrap();
    create_crush_tables(&conn);
    for index in 0..=CAPTURE_BATCH_MAX_RECORDS {
        insert_session(&conn, index);
    }

    let mut fetcher = test_fetcher(&conn);
    let mut producer = SqliteLogicalRowBatchProducer::new(
        test_source("paging"),
        initial_crush_position().unwrap(),
        move |position| fetcher.fetch(position),
    );
    let first = with_sqlite_read_snapshot(&conn, || {
        producer.next_batch().map_err(crush_sqlite_batch_error)
    })
    .unwrap()
    .unwrap();
    assert_eq!(first.records().len(), CAPTURE_BATCH_MAX_RECORDS);
    assert!(conn.is_autocommit());
    let first_end = decode_crush_position(first.range_end()).unwrap().unwrap();
    assert_eq!(first_end.phase, CrushPhase::Sessions);
    assert_eq!(first_end.next_ordinal, 64);
    assert_eq!(first_end.rowid, 64);
    let replay_position = first.range_end().clone();
    drop(producer);

    let mut replay_fetcher = test_fetcher(&conn);
    let mut replay = SqliteLogicalRowBatchProducer::new(
        test_source("paging"),
        replay_position,
        move |position| replay_fetcher.fetch(position),
    );
    let second = with_sqlite_read_snapshot(&conn, || {
        replay.next_batch().map_err(crush_sqlite_batch_error)
    })
    .unwrap()
    .unwrap();
    assert_eq!(second.records().len(), 1);
    assert_eq!(second.records()[0].ordinal(), 64);
    assert!(conn.is_autocommit());
    assert!(with_sqlite_read_snapshot(&conn, || {
        replay.next_batch().map_err(crush_sqlite_batch_error)
    })
    .unwrap()
    .is_none());
}

#[test]
fn read_snapshot_is_released_before_the_next_batch() {
    let conn = Connection::open_in_memory().unwrap();
    create_crush_tables(&conn);
    // Keep one bounded lookahead row so the producer is not legitimately
    // exhausted while proving that the read transaction itself is gone.
    for index in 0..=CAPTURE_BATCH_MAX_RECORDS {
        insert_session(&conn, index);
    }
    let mut fetcher = test_fetcher(&conn);
    let mut producer = SqliteLogicalRowBatchProducer::new(
        test_source("snapshot-release"),
        initial_crush_position().unwrap(),
        move |position| fetcher.fetch(position),
    );
    let first = with_sqlite_read_snapshot(&conn, || {
        producer.next_batch().map_err(crush_sqlite_batch_error)
    })
    .unwrap()
    .unwrap();
    assert_eq!(first.records().len(), CAPTURE_BATCH_MAX_RECORDS);
    assert!(conn.is_autocommit());

    insert_session(&conn, CAPTURE_BATCH_MAX_RECORDS + 1);
    let second = with_sqlite_read_snapshot(&conn, || {
        producer.next_batch().map_err(crush_sqlite_batch_error)
    })
    .unwrap()
    .unwrap();
    assert_eq!(second.records().len(), 2);
    assert_eq!(second.records()[0].ordinal(), 64);
    assert_eq!(second.records()[1].ordinal(), 65);
    assert!(conn.is_autocommit());
}

#[test]
fn retained_length_preflight_marks_oversize_without_hydration() {
    let limit = crush_oversize_limit().unwrap();
    let retained_bytes = limit
        .checked_sub(CRUSH_SQLITE_VALUE_OVERHEAD_BYTES)
        .unwrap()
        .checked_add(1)
        .unwrap();
    let candidate = CrushCandidate {
        rowid: 7,
        retained_bytes: i64::try_from(retained_bytes).unwrap(),
    };
    assert!(candidate.observed_bytes().unwrap() > limit);
}

#[test]
fn crush_length_preflight_restores_limit_after_query_error() {
    use rusqlite::limits::Limit;

    let conn = Connection::open_in_memory().unwrap();
    let lowered_limit = 64 * 1024;
    conn.set_limit(Limit::SQLITE_LIMIT_LENGTH, lowered_limit);

    let result = with_crush_length_preflight(&conn, || {
        conn.query_row::<i64, _, _>("select missing from missing_table", [], |row| row.get(0))
    });

    assert!(result.is_err());
    assert_eq!(conn.limit(Limit::SQLITE_LIMIT_LENGTH), lowered_limit);
}

#[test]
fn capped_preflight_rejects_oversize_and_reuses_parent_for_valid_siblings() {
    use rusqlite::limits::Limit;

    let conn = Connection::open_in_memory().unwrap();
    create_crush_tables(&conn);
    insert_session(&conn, 0);
    conn.execute(
        "insert into messages values (?1, ?2, 'assistant', ?3, 1, 2, null, null, 0)",
        rusqlite::params![
            "message-oversize",
            "session-000",
            "x".repeat(crate::MAX_PROVIDER_SQLITE_VALUE_BYTES + 1),
        ],
    )
    .unwrap();
    for index in 1..=CAPTURE_BATCH_MAX_RECORDS + 1 {
        conn.execute(
            "insert into messages values (?1, ?2, 'user', ?3, ?4, ?4, null, null, 0)",
            rusqlite::params![
                format!("message-valid-{index}"),
                "session-000",
                json!([{"type": "text", "data": {"text": format!("sibling {index}")}}]).to_string(),
                i64::try_from(index).unwrap(),
            ],
        )
        .unwrap();
    }
    let sqlite_value_limit = i32::try_from(crate::MAX_PROVIDER_SQLITE_VALUE_BYTES).unwrap();
    conn.set_limit(Limit::SQLITE_LIMIT_LENGTH, sqlite_value_limit);

    let mut fetcher = test_fetcher(&conn);
    let mut producer = SqliteLogicalRowBatchProducer::new(
        test_source("capped-parent-cache"),
        initial_crush_position().unwrap(),
        |position| fetcher.fetch(position),
    );
    let mut batches = Vec::new();
    while let Some(batch) = with_sqlite_read_snapshot(&conn, || {
        producer.next_batch().map_err(crush_sqlite_batch_error)
    })
    .unwrap()
    {
        batches.push(batch);
    }
    drop(producer);

    assert_eq!(conn.limit(Limit::SQLITE_LIMIT_LENGTH), sqlite_value_limit);
    let records = batches
        .iter()
        .flat_map(|batch| batch.records())
        .collect::<Vec<_>>();
    assert_eq!(batches.len(), 2);
    assert_eq!(batches[0].records().len(), CAPTURE_BATCH_MAX_RECORDS);
    assert_eq!(batches[1].records().len(), 3);
    assert_eq!(records.len(), CAPTURE_BATCH_MAX_RECORDS + 3);
    assert!(matches!(
        records[1].payload(),
        CapturedRecordPayload::StructuralRejection { .. }
    ));
    for record in &records[2..] {
        let CapturedRecordPayload::SqliteValues(values) = record.payload() else {
            panic!("valid Crush sibling was not captured as SQLite values");
        };
        assert_eq!(values.len(), 13, "Crush message must remain child-local");
        let child = decode_crush_message_child(values).unwrap();
        assert_eq!(child.parent_rowid, Some(1));
        assert!(child.message.id.starts_with("message-valid-"));
    }
    assert_eq!(fetcher.session_hydration_query_count(), 1);
}

#[test]
fn capped_preflight_counts_flexible_numeric_storage_before_hydration() {
    use rusqlite::limits::Limit;

    let conn = Connection::open_in_memory().unwrap();
    create_crush_tables(&conn);
    insert_session(&conn, 0);
    let lowered_limit = 64 * 1024;
    let oversized_blob = vec![0_u8; usize::try_from(lowered_limit).unwrap() + 1];
    let oversized_text = "x".repeat(usize::try_from(lowered_limit).unwrap() + 1);
    conn.execute(
        "update sessions set created_at = ?1, updated_at = ?2, prompt_tokens = ?1, \
         completion_tokens = ?2, cost = ?2, summary_message_id = ?1 where rowid = 1",
        rusqlite::params![&oversized_blob, &oversized_text],
    )
    .unwrap();
    conn.execute(
        "insert into messages values \
         ('message-flexible', 'session-000', 'assistant', '[]', 1, 2, null, null, 0)",
        [],
    )
    .unwrap();
    conn.execute(
        "update messages set created_at = ?1, updated_at = ?2, \
         is_summary_message = ?1 where rowid = 1",
        rusqlite::params![&oversized_blob, &oversized_text],
    )
    .unwrap();
    conn.execute(
        "insert into files values ('session-000', 'src/lib.rs', null, 1, 2)",
        [],
    )
    .unwrap();
    conn.execute(
        "update files set created_at = ?1, updated_at = ?2 where rowid = 1",
        rusqlite::params![&oversized_blob, &oversized_text],
    )
    .unwrap();
    conn.execute(
        "insert into read_files values ('session-000', 'README.md', 1)",
        [],
    )
    .unwrap();
    conn.execute(
        "update read_files set read_at = ?1 where rowid = 1",
        [&oversized_blob],
    )
    .unwrap();
    conn.set_limit(Limit::SQLITE_LIMIT_LENGTH, lowered_limit);

    let mut fetcher = test_fetcher(&conn);
    let session = crush_fetch_candidate(&conn, &mut fetcher.session_candidates, None)
        .unwrap()
        .unwrap();
    assert!(session.retained_bytes > i64::from(lowered_limit));
    assert_eq!(conn.limit(Limit::SQLITE_LIMIT_LENGTH), lowered_limit);

    let message = crush_fetch_candidate(&conn, &mut fetcher.message_candidates, None)
        .unwrap()
        .unwrap();
    assert!(message.retained_bytes > i64::from(lowered_limit));
    assert_eq!(conn.limit(Limit::SQLITE_LIMIT_LENGTH), lowered_limit);

    let file = crush_fetch_optional_candidate(&conn, &mut fetcher.file_candidates, None)
        .unwrap()
        .unwrap();
    assert!(file.retained_bytes > i64::from(lowered_limit));
    assert_eq!(conn.limit(Limit::SQLITE_LIMIT_LENGTH), lowered_limit);

    let read_file = crush_fetch_optional_candidate(&conn, &mut fetcher.read_file_candidates, None)
        .unwrap()
        .unwrap();
    assert!(read_file.retained_bytes > i64::from(lowered_limit));
    assert_eq!(conn.limit(Limit::SQLITE_LIMIT_LENGTH), lowered_limit);
}

#[test]
fn next_rowid_candidates_are_indexed_and_near_tail_work_is_bounded() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    let conn = Connection::open_in_memory().unwrap();
    create_crush_tables(&conn);
    let plans = [
        ("sessions", "s.rowid", "sessions s"),
        (
            "messages",
            "m.rowid",
            "messages m left join sessions s on s.id = m.session_id",
        ),
        (
            "files",
            "f.rowid",
            "files f join sessions s on s.id = f.session_id",
        ),
        (
            "read_files",
            "r.rowid",
            "read_files r join sessions s on s.id = r.session_id",
        ),
    ];
    for (table, rowid, source) in plans {
        let plan = conn
            .prepare(&format!(
                "explain query plan {}",
                crush_candidate_sql(rowid, "0", source, true)
            ))
            .unwrap()
            .query_map([1_i64], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap()
            .join(" | ");
        let alias = rowid.trim_end_matches(".rowid");
        assert!(
            plan.contains(&format!(
                "SEARCH {alias} USING INTEGER PRIMARY KEY (rowid>?)"
            )),
            "{table}: {plan}"
        );
        assert!(!plan.contains(&format!("SCAN {alias}")), "{table}: {plan}");
        assert!(!plan.contains("USE TEMP B-TREE"), "{table}: {plan}");
    }

    let tx = conn.unchecked_transaction().unwrap();
    for index in 0..2_048_i64 {
        tx.execute(
            "insert into sessions values (?1, null, ?2, 1, 2, 0.5, ?3, ?4, null)",
            rusqlite::params![
                format!("session-{index:04}"),
                format!("Session {index}"),
                index,
                index + 1,
            ],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    let sqlite_value_limit = 64 * 1024;
    conn.set_limit(
        rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH,
        sqlite_value_limit,
    );

    let operations = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&operations);
    conn.progress_handler(
        1,
        Some(move || {
            observed.fetch_add(1, Ordering::Relaxed);
            false
        }),
    );
    let mut fetcher = test_fetcher(&conn);
    operations.store(0, Ordering::Relaxed);
    let start = encode_crush_position(CrushKeyset {
        phase: CrushPhase::Sessions,
        next_ordinal: 2_047,
        rowid: 2_047,
    })
    .unwrap();
    let tail = fetcher.fetch(start).unwrap().unwrap();
    assert_eq!(tail.ordinal(), 2_047);
    assert_eq!(
        decode_crush_position(tail.next_position())
            .unwrap()
            .unwrap()
            .rowid,
        2_048
    );
    let tail_operations = operations.load(Ordering::Relaxed);
    assert!(
        tail_operations < 2_000,
        "near-tail lookup revisited too much source state: {tail_operations}"
    );
    assert_eq!(
        conn.limit(rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH),
        sqlite_value_limit
    );

    assert!(fetcher
        .fetch(tail.next_position().clone())
        .unwrap()
        .is_none());
    let terminal_operations = operations.load(Ordering::Relaxed) - tail_operations;
    assert!(
        terminal_operations < 2_000,
        "terminal lookup revisited too much source state: {terminal_operations}"
    );
    assert_eq!(
        conn.limit(rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH),
        sqlite_value_limit
    );
    conn.progress_handler(0, None::<fn() -> bool>);
}

#[test]
fn alternating_message_parents_are_hydrated_once_in_the_sessions_phase() {
    let conn = Connection::open_in_memory().unwrap();
    create_crush_tables(&conn);
    insert_session(&conn, 0);
    insert_session(&conn, 1);
    for index in 0..70_i64 {
        let session_id = if index % 2 == 0 {
            "session-000"
        } else {
            "session-001"
        };
        conn.execute(
            "insert into messages values (?1, ?2, 'user', ?3, ?4, ?4, null, null, 0)",
            rusqlite::params![
                format!("message-{index:03}"),
                session_id,
                json!([{"type": "text", "data": {"text": format!("message {index}")}}]).to_string(),
                index,
            ],
        )
        .unwrap();
    }

    let mut fetcher = test_fetcher(&conn);
    let mut producer = SqliteLogicalRowBatchProducer::new(
        test_source("alternating-parents"),
        initial_crush_position().unwrap(),
        |position| fetcher.fetch(position),
    );
    let mut message_count = 0;
    while let Some(batch) = producer.next_batch().unwrap() {
        for record in batch.records() {
            if record.record_kind().as_str() != CRUSH_MESSAGE_CHILD_RECORD_KIND {
                continue;
            }
            let CapturedRecordPayload::SqliteValues(values) = record.payload() else {
                panic!("bounded Crush child was rejected");
            };
            assert_eq!(values.len(), 13);
            message_count += 1;
        }
    }
    drop(producer);

    assert_eq!(message_count, 70);
    assert_eq!(fetcher.session_hydration_query_count(), 2);
}

#[test]
fn source_snapshot_detects_database_mutation() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let path = directory.path().join("crush.db");
    fs::write(&path, b"crush-snapshot").unwrap();
    let snapshot = crush_source_snapshot(&path).unwrap();
    assert!(snapshot.revalidate(&path).unwrap());

    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(b"-changed").unwrap();
    file.sync_all().unwrap();
    assert!(!snapshot.revalidate(&path).unwrap());
}

#[test]
fn bounded_projection_preserves_expected_events_touches_and_metadata() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let path = directory.path().join("crush.db");
    let conn = Connection::open(&path).unwrap();
    create_crush_tables(&conn);
    conn.execute(
        "insert into sessions values (?1, null, 'Crush test', 3, 5, 1.25, 1000, 4000, null)",
        ["session-1"],
    )
    .unwrap();
    conn.execute(
        "insert into messages values (?1, ?2, 'assistant', ?3, 2000, 3000, 'test', 'model', 0)",
        rusqlite::params![
            "message-1",
            "session-1",
            json!([{
                "type": "tool_call",
                "data": {"name": "read_file", "input": {"path": "src/main.rs"}}
            }])
            .to_string(),
        ],
    )
    .unwrap();
    conn.execute(
        "insert into files values ('session-1', 'src/lib.rs', 'v1', 2000, 3000)",
        [],
    )
    .unwrap();
    conn.execute(
        "insert into read_files values ('session-1', 'README.md', 4)",
        [],
    )
    .unwrap();
    drop(conn);

    let context = ProviderAdapterContext {
        machine_id: "crush-equivalence".to_owned(),
        source_path: Some(path.clone()),
        source_root: None,
        imported_at: DateTime::<Utc>::UNIX_EPOCH,
    };
    let conn = open_provider_sqlite_readonly(&path).unwrap();
    let mut fetcher = test_fetcher(&conn);
    let mut producer = SqliteLogicalRowBatchProducer::new(
        test_source("equivalence"),
        initial_crush_position().unwrap(),
        move |position| fetcher.fetch(position),
    );
    let mut projector = CrushCapturedBatchProjector::new(
        context.clone(),
        path.display().to_string(),
        0,
        sqlite_schema_fingerprint(&conn).unwrap(),
    );
    let mut output = CollectingProjectionOutput::default();
    loop {
        let batch = with_sqlite_read_snapshot(&conn, || {
            producer.next_batch().map_err(crush_sqlite_batch_error)
        })
        .unwrap();
        let Some(batch) = batch else {
            break;
        };
        for record in batch.records() {
            projector.project_record(record, &mut output).unwrap();
        }
    }
    assert!(output.rejections.is_empty());

    let mut projected_captures = Vec::new();
    let mut projected_touches = Vec::new();
    for mut normalization in output.normalizations {
        projected_captures.append(&mut normalization.captures);
        projected_touches.append(&mut normalization.files_touched);
    }
    let projected_events = projected_captures
        .iter()
        .filter(|(_, capture)| capture.event.is_some())
        .map(|(_, capture)| capture)
        .collect::<Vec<_>>();
    assert_eq!(projected_events.len(), 1);
    let event_capture = projected_events[0];
    assert_eq!(event_capture.session.provider_session_id, "session-1");
    assert_eq!(
        event_capture.source.raw_source_path.as_deref(),
        path.to_str()
    );
    let event = event_capture.event.as_ref().unwrap();
    assert_eq!(event.event_type, EventType::ToolCall);
    assert_eq!(event.role, Some(ctx_history_core::EventRole::Assistant));
    assert!(event.payload["text"]
        .as_str()
        .unwrap()
        .starts_with("tool call: read_file"));
    assert!(event.payload["text"]
        .as_str()
        .unwrap()
        .contains("src/main.rs"));
    assert_eq!(event.metadata["message_id"], "message-1");

    let mut projected_touch_paths = projected_touches
        .iter()
        .map(|(_, touch)| touch.path.as_str())
        .collect::<Vec<_>>();
    projected_touch_paths.sort_unstable();
    assert_eq!(
        projected_touch_paths,
        ["README.md", "src/lib.rs", "src/main.rs"]
    );
    assert!(projected_touches.iter().all(|(_, touch)| {
        touch.provider_session_id == "session-1"
            && touch.raw_source_path.as_deref() == path.to_str()
            && touch.source_root.as_deref() == path.to_str()
    }));
    assert_eq!(
        projected_touches
            .iter()
            .find(|(_, touch)| touch.path == "src/lib.rs")
            .unwrap()
            .1
            .change_kind,
        Some(FileChangeKind::Modified)
    );
    assert_eq!(
        projected_touches
            .iter()
            .find(|(_, touch)| touch.path == "README.md")
            .unwrap()
            .1
            .change_kind,
        Some(FileChangeKind::Read)
    );
    assert_eq!(
        projected_touches
            .iter()
            .find(|(_, touch)| touch.path == "src/main.rs")
            .unwrap()
            .1
            .change_kind,
        Some(FileChangeKind::Read)
    );

    let shell = projected_captures
        .iter()
        .find(|(_, capture)| capture.event.is_none())
        .map(|(_, capture)| capture)
        .unwrap();
    assert_eq!(shell.session.provider_session_id, "session-1");
    assert_eq!(shell.session.metadata["title"], "Crush test");
    assert_eq!(shell.session.metadata["tokens"]["prompt"], 3);
    assert_eq!(shell.session.metadata["tokens"]["completion"], 5);
    assert_eq!(shell.session.metadata["cost"], 1.25);
    assert!(shell.source.cursor.is_none());
    assert_eq!(shell.source.raw_source_path.as_deref(), path.to_str());
    assert_eq!(shell.source.metadata["adapter"], CRUSH_SQLITE_SOURCE_FORMAT);
}

#[test]
fn real_import_streams_the_shared_touch_limit_through_store_rotations() {
    const OVERFLOWING_PATH_COUNT: usize =
        crate::provider::file_touches::MAX_PROVIDER_FILE_TOUCHES_PER_EVENT + 1;

    let directory = crate::test_support_paths::tempdir().unwrap();
    let path = directory.path().join("crush.db");
    let store_path = directory.path().join("store.sqlite");
    let conn = Connection::open(&path).unwrap();
    create_crush_tables(&conn);
    conn.execute(
        "insert into sessions values (?1, null, 'Crush touch limit', 3, 5, 1.25, \
         1000, 4000, null)",
        ["session-touch-limit"],
    )
    .unwrap();
    let paths = (0..OVERFLOWING_PATH_COUNT)
        .map(|index| json!({"path": format!("src/touch-{index:05}.rs")}))
        .collect::<Vec<_>>();
    let parts = json!([{
        "type": "tool_call",
        "data": {
            "name": "edit_many",
            "files": paths,
        },
    }]);
    conn.execute(
        "insert into messages values (?1, ?2, 'assistant', ?3, 2000, 3000, \
         'test', 'model', 0)",
        rusqlite::params![
            "message-touch-limit",
            "session-touch-limit",
            parts.to_string(),
        ],
    )
    .unwrap();
    drop(conn);

    let context = ProviderAdapterContext {
        machine_id: "crush-touch-limit-import".to_owned(),
        source_path: Some(path.clone()),
        source_root: None,
        imported_at: DateTime::<Utc>::UNIX_EPOCH,
    };
    let mut store = Store::open(&store_path).unwrap();
    let first = import_crush_sqlite_batched(
        &path,
        &mut store,
        context.clone(),
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();

    assert_eq!(first.imported_events, 1, "{:?}", first.failures);
    assert_eq!(
        first.accepted_content_records,
        crate::provider::file_touches::MAX_PROVIDER_FILE_TOUCHES_PER_EVENT + 1
    );
    assert_eq!(first.failed, 1, "{:?}", first.failures);
    assert_eq!(first.failures.len(), 1);
    assert_eq!(first.failures[0].error, PROVIDER_FILE_TOUCH_LIMIT_REJECTION);

    let store_reader = Connection::open(&store_path).unwrap();
    let touch_count = store_reader
        .query_row("select count(*) from files_touched", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    assert_eq!(
        touch_count,
        i64::try_from(crate::provider::file_touches::MAX_PROVIDER_FILE_TOUCHES_PER_EVENT).unwrap()
    );
    let event_count = store_reader
        .query_row("select count(*) from events", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    assert_eq!(event_count, 1);

    let read_boundary = |descending: bool| {
        let direction = if descending { "desc" } else { "asc" };
        store_reader
            .query_row(
                &format!(
                    "select id, path, event_id, \
                     cast(json_extract(metadata_json, '$.provider_touch_index') as integer) \
                     from files_touched order by 4 {direction} limit 1"
                ),
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .unwrap()
    };
    let first_touch = read_boundary(false);
    let last_touch = read_boundary(true);
    assert_eq!(first_touch.1, "src/touch-00000.rs");
    assert_eq!(last_touch.1, "src/touch-65535.rs");
    assert_eq!(first_touch.2, last_touch.2);
    assert!(first_touch.2.is_some());
    assert_eq!(first_touch.3 & 0xffff, 0);
    assert_eq!(last_touch.3, first_touch.3 + 65_535);

    let source_id = crate::provider::importer::provider_scoped_source_uuid(
        CaptureProvider::Crush,
        "session-touch-limit",
        CRUSH_SQLITE_SOURCE_FORMAT,
        path.to_str(),
    );
    let expected_touch_id = |provider_touch_index: i64| {
        crate::stable_capture_uuid(
            &format!("provider-source:{source_id}:file-touch:{provider_touch_index}"),
            "file-touch",
        )
        .to_string()
    };
    assert_eq!(first_touch.0, expected_touch_id(first_touch.3));
    assert_eq!(last_touch.0, expected_touch_id(last_touch.3));

    let replay = import_crush_sqlite_batched(
        &path,
        &mut store,
        context,
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(replay.failed, 1, "{:?}", replay.failures);
    assert_eq!(replay.imported_events, 0);
    assert_eq!(
        store_reader
            .query_row("select count(*) from files_touched", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        touch_count
    );
    assert_eq!(read_boundary(false), first_touch);
    assert_eq!(read_boundary(true), last_touch);
}

#[test]
fn real_import_rejects_bad_messages_without_losing_valid_siblings() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let path = directory.path().join("crush.db");
    let conn = Connection::open(&path).unwrap();
    create_crush_tables(&conn);
    conn.execute(
        "insert into sessions values (?1, null, 'Crush rejection test', 3, 5, 1.25, \
         1000, 4000, null)",
        ["session-valid"],
    )
    .unwrap();
    conn.execute(
        "insert into sessions values (?1, null, zeroblob(?2), 3, 5, 1.25, \
         1000, 4000, null)",
        rusqlite::params![
            "session-rejected-parent",
            i64::try_from(crush_oversize_limit().unwrap() + 1).unwrap(),
        ],
    )
    .unwrap();
    conn.execute(
        "insert into messages values (?1, ?2, 'user', ?3, 2000, 3000, 'test', 'model', 0)",
        rusqlite::params![
            "message-valid",
            "session-valid",
            json!([{"type": "text", "data": {"text": "valid Crush sibling"}}]).to_string(),
        ],
    )
    .unwrap();
    conn.execute(
        "insert into messages values (?1, ?2, 'assistant', ?3, 2100, 3100, \
         'test', 'model', 0)",
        rusqlite::params!["message-malformed", "session-valid", "not-json"],
    )
    .unwrap();
    conn.execute(
        "insert into messages values (?1, ?2, 'user', ?3, 2200, 3200, 'test', 'model', 0)",
        rusqlite::params![
            "message-orphan",
            "session-missing",
            json!([{"type": "text", "data": {"text": "orphan"}}]).to_string(),
        ],
    )
    .unwrap();
    conn.execute(
        "insert into messages values (?1, ?2, 'assistant', ?3, 2300, 3300, \
         'test', 'model', 0)",
        rusqlite::params![
            "message-orphaned-touch",
            "session-rejected-parent",
            json!([{
                "type": "tool_call",
                "data": {
                    "name": "write_file",
                    "input": {"path": "src/orphaned-crush.rs"},
                },
            }])
            .to_string(),
        ],
    )
    .unwrap();
    drop(conn);

    let context = ProviderAdapterContext {
        machine_id: "crush-rejection-import".to_owned(),
        source_path: Some(path.clone()),
        source_root: None,
        imported_at: DateTime::<Utc>::UNIX_EPOCH,
    };
    let mut store = Store::open(directory.path().join("store.sqlite")).unwrap();
    let first = import_crush_sqlite_batched(
        &path,
        &mut store,
        context.clone(),
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();

    assert_eq!(first.failed, 4, "{:?}", first.failures);
    assert_eq!(first.failures.len(), 4);
    assert!(first
        .failures
        .iter()
        .any(|failure| failure.error.contains("message-malformed")
            && failure.error.contains("invalid JSON")));
    assert!(first
        .failures
        .iter()
        .any(|failure| failure.error.contains("message-orphan")
            && failure.error.contains("session-missing")));
    assert_eq!(
        first
            .failures
            .iter()
            .filter(|failure| failure
                .error
                .contains("not already persisted for its exact source"))
            .count(),
        1,
        "the orphaned child must produce exactly one deterministic session rejection"
    );
    assert_eq!(first.imported_events, 1);
    let session = store
        .session_by_external_session(CaptureProvider::Crush, "session-valid")
        .unwrap()
        .unwrap();
    let events = store.events_for_session(session.id).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].payload["body"]["text"], "valid Crush sibling");
    assert!(store.export_archive().unwrap().files_touched.is_empty());
    assert!(store
        .session_by_external_session(CaptureProvider::Crush, "session-rejected-parent")
        .unwrap()
        .is_none());

    let replay = import_crush_sqlite_batched(
        &path,
        &mut store,
        context,
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(replay.failed, 4, "{:?}", replay.failures);
    assert!(replay.failures.is_empty());
    assert_eq!(replay.imported_events, 0);
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 1);
}
