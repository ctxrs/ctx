use std::fs;

use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, EntityTimestamps, SyncCursor};
use ctx_history_store::Store;
use rusqlite::Connection;

use crate::captured_batch::sqlite_logical_rows::SqliteLogicalRowBatchProducer;
use crate::captured_batch::{NativePosition, SourceObservation, CAPTURE_BATCH_MAX_RECORDS};
use crate::provider::importer::{
    captured_batch_cursor_stream, provider_path_identity, provider_source_cursor_stream_for_path,
    BoundedParserCheckpoint, CertifiedProviderCursor,
};
use crate::provider::sqlite::open_provider_sqlite_readonly;
use crate::provider::sqlite::{sqlite_schema_fingerprint, with_sqlite_read_snapshot};
use crate::{
    NormalizedProviderImportOptions, ProviderAdapterContext, GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
};

use super::super::{
    import_goose_sessions_sqlite_batched,
    position::initial_goose_position,
    schema::goose_schema_version,
    source::{goose_source_observation, goose_source_revision, goose_source_snapshot},
    stream::{goose_sqlite_batch_error, GooseRowFetcher},
    GOOSE_CAPTURE_REVISION, GOOSE_POLICY_REVISION,
};
use super::{create_goose_tables, insert_message, insert_session, test_source};

#[test]
fn goose_source_revision_keeps_exact_snapshot_envelope() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("sessions.db");
    let conn = Connection::open(&path).unwrap();
    create_goose_tables(&conn);
    drop(conn);

    let snapshot = goose_source_snapshot(&path).unwrap();
    let snapshot_component = snapshot.revision_component();
    let expected_revision = format!(
        "goose-sqlite-snapshot-v1:capture=3;policy=5;user_version=23;schema_version=14;schema=schema-v1;{snapshot_component}"
    );
    assert_eq!(
        goose_source_revision(&snapshot, 23, Some(14), "schema-v1"),
        expected_revision
    );
    let source = goose_source_observation(
        &snapshot,
        "canonical-source-path",
        "provider:goose:cursor".to_owned(),
        23,
        Some(14),
        "schema-v1",
        None,
    )
    .unwrap();
    assert_eq!(source.provider(), CaptureProvider::Goose);
    assert_eq!(source.source_format(), GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT);
    assert_eq!(
        source.source_identity(),
        "goose-sqlite:canonical-source-path"
    );
    assert_eq!(source.source_revision(), expected_revision);
    assert_eq!(source.cursor_stream(), "provider:goose:cursor");
    assert_eq!(source.capture_revision(), 3);
    assert_eq!(source.policy_revision(), 5);
    assert!(snapshot.revalidate(&path).unwrap());
}

#[test]
fn goose_v2_capture_cursor_resets_before_v3_rowid_traversal() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_path = temp.path().join("sessions.db");
    let source_conn = Connection::open(&source_path).unwrap();
    create_goose_tables(&source_conn);
    insert_session(&source_conn, "revision-reset");
    insert_message(&source_conn, 1, "revision-reset", "must import after reset");
    drop(source_conn);

    let canonical_path = fs::canonicalize(&source_path).unwrap();
    let snapshot = goose_source_snapshot(&source_path).unwrap();
    let cursor_path = provider_path_identity(&canonical_path).unwrap();
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Goose,
        GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
        &cursor_path,
    );
    let conn = open_provider_sqlite_readonly(&source_path).unwrap();
    let user_version = conn
        .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
        .unwrap();
    let schema_fingerprint = sqlite_schema_fingerprint(&conn).unwrap();
    let schema_version = goose_schema_version(&conn).unwrap();
    let observed_source = SourceObservation::new(
        CaptureProvider::Goose,
        GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
        format!("goose-sqlite:{cursor_path}"),
        goose_source_revision(&snapshot, user_version, schema_version, &schema_fingerprint),
        cursor_stream,
        GOOSE_CAPTURE_REVISION,
        GOOSE_POLICY_REVISION,
        None,
    )
    .unwrap();
    let stream = captured_batch_cursor_stream(&observed_source);
    let old_position = NativePosition::new("goose-logical-row-keyset-v2", vec![0]).unwrap();
    let old_cursor = CertifiedProviderCursor::new(
        observed_source.source_revision(),
        2,
        observed_source.policy_revision(),
        old_position,
        BoundedParserCheckpoint::from_serializable(&()).unwrap(),
    )
    .unwrap();
    let context = ProviderAdapterContext {
        machine_id: "goose-revision-reset".to_owned(),
        source_path: Some(source_path.clone()),
        source_root: Some(temp.path().to_path_buf()),
        imported_at: DateTime::<Utc>::UNIX_EPOCH,
    };
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    store
        .upsert_sync_cursor(&SyncCursor {
            id: crate::stable_capture_uuid("goose-v2-cursor", "provider-sync-cursor"),
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
    let summary = import_goose_sessions_sqlite_batched(
        &source_path,
        &mut store,
        context.clone(),
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(summary.imported_events, 1);
    let published = store
        .get_sync_cursor(None, &context.machine_id, &stream)
        .unwrap()
        .unwrap();
    assert_eq!(
        CertifiedProviderCursor::decode(&published.cursor)
            .unwrap()
            .parser_revision(),
        GOOSE_CAPTURE_REVISION
    );
}

#[test]
fn goose_releases_batch_snapshot_and_detects_source_mutation() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("sessions.db");
    let writer = Connection::open(&path).unwrap();
    create_goose_tables(&writer);
    insert_session(&writer, "mutation-session");
    for index in 1..=CAPTURE_BATCH_MAX_RECORDS {
        insert_message(
            &writer,
            i64::try_from(index).unwrap(),
            "mutation-session",
            "before mutation",
        );
    }
    drop(writer);
    let snapshot = goose_source_snapshot(&path).unwrap();
    let reader = open_provider_sqlite_readonly(&path).unwrap();
    let mut fetcher = GooseRowFetcher::new(&reader).unwrap();
    let mut producer = SqliteLogicalRowBatchProducer::new(
        test_source("goose-snapshot:mutation"),
        initial_goose_position().unwrap(),
        move |position| fetcher.fetch(position),
    );

    let first = with_sqlite_read_snapshot(&reader, || {
        producer.next_batch().map_err(goose_sqlite_batch_error)
    })
    .unwrap()
    .unwrap();
    assert_eq!(first.records().len(), CAPTURE_BATCH_MAX_RECORDS);
    assert!(reader.is_autocommit());
    let writer = Connection::open(&path).unwrap();
    insert_message(&writer, 65, "mutation-session", "after mutation");
    drop(writer);

    assert!(!snapshot.revalidate(&path).unwrap());
    assert!(reader.is_autocommit());
}
