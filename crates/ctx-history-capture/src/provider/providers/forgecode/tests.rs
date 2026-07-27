use std::fs;
use std::io::Write;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use chrono::{DateTime, Utc};
use ctx_history_core::CaptureProvider;
use ctx_history_store::Store;
use rusqlite::{limits::Limit as SqliteLimit, Connection};
use serde_json::{json, Value};

use crate::captured_batch::sqlite_logical_rows::SqliteLogicalRowBatchProducer;
use crate::captured_batch::{
    CapturedBatch, CapturedRecordPayload, NativePosition, ProviderRecordKind, SourceObservation,
    CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES, CAPTURE_BATCH_MAX_RECORDS,
};
use crate::provider::file_touches::MAX_PROVIDER_FILE_TOUCHES_PER_EVENT;
use crate::provider::importer::{
    CapturedBatchProjector, ProviderProjectionOutput, ProviderProjectionResult,
};
use crate::provider::sqlite::with_sqlite_read_snapshot;
use crate::{
    NormalizedProviderImportOptions, ProviderAdapterContext, ProviderNormalizationResult,
    FORGECODE_SQLITE_SOURCE_FORMAT,
};

use super::event::{forgecode_for_each_metric_file_touch, forgecode_normalized_result_content};
use super::projection::ForgeCodeCapturedBatchProjector;
use super::source::{
    decode_forgecode_conversation, decode_forgecode_position, encode_forgecode_position,
    forgecode_candidate_sql, forgecode_conversation_columns, forgecode_oversize_limit,
    forgecode_retained_length_expr, forgecode_source_snapshot, forgecode_sqlite_batch_error,
    initial_forgecode_position, ForgeCodeConversationRow, ForgeCodeKeyset, ForgeCodeRowCandidate,
    ForgeCodeRowFetcher,
};
use super::{
    import_forgecode_sqlite_batched, FORGECODE_CAPTURE_REVISION, FORGECODE_POLICY_REVISION,
    FORGECODE_RECORD_KIND, FORGECODE_REJECTED_RECORD_KIND, FORGECODE_SQLITE_VALUE_OVERHEAD_BYTES,
};

#[test]
fn forgecode_result_content_uses_dto_order_and_explicit_variant_precedence() {
    let long = "x".repeat(crate::PROVIDER_MAX_TEXT_CHARS + 23);
    let body = json!({
        "output": {
            "values": [
                {"markdown": "markdown", "text": long.clone()},
                {"pair": [{"Text": "first"}, {"Text": "ignored"}]},
                {"unknown": {"output": "kept as json"}}
            ]
        }
    });

    assert_eq!(
        forgecode_normalized_result_content(&body),
        Some(format!(
            "{long}\nfirst\n{{\"unknown\":{{\"output\":\"kept as json\"}}}}"
        ))
    );
    assert_eq!(
        forgecode_normalized_result_content(&json!({"output": {"is_error": true}})),
        None
    );
}

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

#[derive(Default)]
struct CountingProjectionOutput {
    emissions: usize,
    captures: usize,
    files_touched: usize,
    rejections: Vec<(usize, String)>,
}

impl ProviderProjectionOutput for CountingProjectionOutput {
    fn emit_normalization(
        &mut self,
        normalization: ProviderNormalizationResult,
    ) -> ProviderProjectionResult<()> {
        assert!(normalization.captures.len() <= 1);
        assert!(normalization.files_touched.len() <= 1);
        self.emissions = self.emissions.saturating_add(1);
        self.captures = self.captures.saturating_add(normalization.captures.len());
        self.files_touched = self
            .files_touched
            .saturating_add(normalization.files_touched.len());
        Ok(())
    }

    fn reject_record(&mut self, line_number: usize, reason: String) {
        self.rejections.push((line_number, reason));
    }
}

fn create_conversations_table(conn: &Connection) {
    conn.execute_batch(
        "create table conversations (
            conversation_id text not null,
            title text,
            workspace_id integer not null,
            context text,
            created_at text not null,
            updated_at text,
            metrics text
        );",
    )
    .unwrap();
}

fn test_source(identity: &str) -> SourceObservation {
    SourceObservation::new(
        CaptureProvider::ForgeCode,
        FORGECODE_SQLITE_SOURCE_FORMAT,
        format!("forgecode-sqlite:{identity}"),
        format!("forgecode-snapshot:{identity}"),
        format!("provider:forgecode:{identity}"),
        FORGECODE_CAPTURE_REVISION,
        FORGECODE_POLICY_REVISION,
        None,
    )
    .unwrap()
}

fn captured_conversation_batch(conn: &Connection, identity: &str) -> CapturedBatch {
    let columns = forgecode_conversation_columns(conn).unwrap();
    let mut fetcher = ForgeCodeRowFetcher::new(
        conn,
        &columns,
        ProviderRecordKind::new(FORGECODE_RECORD_KIND).unwrap(),
    )
    .unwrap();
    let mut producer = SqliteLogicalRowBatchProducer::new(
        test_source(identity),
        initial_forgecode_position().unwrap(),
        move |position| fetcher.fetch(position),
    );
    with_sqlite_read_snapshot(conn, || {
        producer.next_batch().map_err(forgecode_sqlite_batch_error)
    })
    .unwrap()
    .unwrap()
}

#[test]
fn logical_rows_page_at_sixty_four_and_replay_the_exact_keyset() {
    let conn = Connection::open_in_memory().unwrap();
    create_conversations_table(&conn);
    for index in 0..=CAPTURE_BATCH_MAX_RECORDS {
        conn.execute(
            "insert into conversations values (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                format!("conversation-{index}"),
                format!("Conversation {index}"),
                i64::try_from(index).unwrap(),
                r#"{"messages":[]}"#,
                format!("2026-01-01T00:00:{:02}Z", index % 60),
                Option::<String>::None,
                Option::<String>::None,
            ],
        )
        .unwrap();
    }

    let columns = forgecode_conversation_columns(&conn).unwrap();
    let source = test_source("paging-test");
    let mut fetcher = ForgeCodeRowFetcher::new(
        &conn,
        &columns,
        ProviderRecordKind::new(FORGECODE_RECORD_KIND).unwrap(),
    )
    .unwrap();
    let mut producer = SqliteLogicalRowBatchProducer::new(
        source.clone(),
        initial_forgecode_position().unwrap(),
        move |position| fetcher.fetch(position),
    );

    let first = with_sqlite_read_snapshot(&conn, || {
        producer.next_batch().map_err(forgecode_sqlite_batch_error)
    })
    .unwrap()
    .unwrap();
    assert_eq!(first.records().len(), CAPTURE_BATCH_MAX_RECORDS);
    assert!(conn.is_autocommit());
    let replay_position = first.range_end().clone();
    let replay_keyset = decode_forgecode_position(&replay_position)
        .unwrap()
        .unwrap();
    assert_eq!(replay_keyset.next_ordinal, 64);
    assert_eq!(replay_keyset.rowid, 64);

    let mut replay_fetcher = ForgeCodeRowFetcher::new(
        &conn,
        &columns,
        ProviderRecordKind::new(FORGECODE_RECORD_KIND).unwrap(),
    )
    .unwrap();
    let mut replay = SqliteLogicalRowBatchProducer::new(source, replay_position, move |position| {
        replay_fetcher.fetch(position)
    });
    let second = with_sqlite_read_snapshot(&conn, || {
        replay.next_batch().map_err(forgecode_sqlite_batch_error)
    })
    .unwrap()
    .unwrap();
    assert_eq!(second.records().len(), 1);
    assert_eq!(second.records()[0].ordinal(), 64);
    assert!(conn.is_autocommit());
    assert!(with_sqlite_read_snapshot(&conn, || {
        replay.next_batch().map_err(forgecode_sqlite_batch_error)
    })
    .unwrap()
    .is_none());
    assert!(conn.is_autocommit());
}

#[test]
fn rowid_resume_is_indexed_and_near_tail_work_is_bounded() {
    let conn = Connection::open_in_memory().unwrap();
    create_conversations_table(&conn);
    let tx = conn.unchecked_transaction().unwrap();
    for index in 1..=2_048_i64 {
        tx.execute(
            "insert into conversations values (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                format!("conversation-{index}"),
                "Bounded resume",
                index,
                r#"{"messages":[]}"#,
                "2026-01-01T00:00:00Z",
                Option::<String>::None,
                Option::<String>::None,
            ],
        )
        .unwrap();
    }
    tx.commit().unwrap();

    let retained_bytes = forgecode_retained_length_expr(&[
        "conversation_id".to_owned(),
        "title".to_owned(),
        "CASE WHEN typeof(workspace_id) = 'integer' THEN NULL ELSE workspace_id END".to_owned(),
        "context".to_owned(),
        "created_at".to_owned(),
        "updated_at".to_owned(),
        "metrics".to_owned(),
    ]);
    let resume_sql = forgecode_candidate_sql(
        &retained_bytes,
        "title",
        "context",
        "updated_at",
        "metrics",
        true,
    );
    assert!(resume_sql.contains("octet_length("), "{resume_sql}");
    assert!(
        !resume_sql.to_ascii_lowercase().contains("cast("),
        "raised-limit candidate must not materialize SQLite values: {resume_sql}"
    );
    let plan = conn
        .prepare(&format!("explain query plan {resume_sql}"))
        .unwrap()
        .query_map([2_047_i64], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap()
        .join(" | ");
    assert!(
        plan.contains("SEARCH conversations USING INTEGER PRIMARY KEY (rowid>?)"),
        "{plan}"
    );
    assert!(!plan.contains("SCAN conversations"), "{plan}");
    assert!(!plan.contains("USE TEMP B-TREE"), "{plan}");

    // Statement preparation is fixed setup. Count only the resumed candidate
    // preflight, exact-row hydration, and terminal native seek.
    let columns = forgecode_conversation_columns(&conn).unwrap();
    let mut fetcher = ForgeCodeRowFetcher::new(
        &conn,
        &columns,
        ProviderRecordKind::new(FORGECODE_RECORD_KIND).unwrap(),
    )
    .unwrap();
    let operations = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&operations);
    conn.progress_handler(
        1,
        Some(move || {
            observed.fetch_add(1, Ordering::Relaxed);
            false
        }),
    );
    let start = encode_forgecode_position(ForgeCodeKeyset {
        next_ordinal: 2_047,
        rowid: 2_047,
    })
    .unwrap();
    let tail = fetcher.fetch(start).unwrap().unwrap();
    assert_eq!(tail.ordinal(), 2_047);
    let tail_position = decode_forgecode_position(tail.next_position())
        .unwrap()
        .unwrap();
    assert_eq!(tail_position.next_ordinal, 2_048);
    assert_eq!(tail_position.rowid, 2_048);
    assert!(fetcher
        .fetch(tail.next_position().clone())
        .unwrap()
        .is_none());
    conn.progress_handler(0, None::<fn() -> bool>);
    let operations = operations.load(Ordering::Relaxed);
    assert!(
        operations < 2_000,
        "ForgeCode near-tail resume used {operations} SQLite VM operations"
    );
}

#[test]
fn projector_emits_each_conversation_in_one_deterministic_pass() {
    let conn = Connection::open_in_memory().unwrap();
    create_conversations_table(&conn);
    conn.execute(
        "insert into conversations values (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            "conversation-1",
            "One pass",
            7_i64,
            serde_json::to_string(&json!({
                "messages": [
                    {"message": {"Text": {"role": "user", "content": "hello"}}},
                    {"message": {"Text": {"role": "assistant", "content": "world"}}}
                ]
            }))
            .unwrap(),
            "2026-01-01T00:00:00Z",
            "2026-01-01T00:00:01Z",
            r#"{"files_accessed":["src/main.rs"]}"#,
        ],
    )
    .unwrap();
    let columns = forgecode_conversation_columns(&conn).unwrap();
    let mut fetcher = ForgeCodeRowFetcher::new(
        &conn,
        &columns,
        ProviderRecordKind::new(FORGECODE_RECORD_KIND).unwrap(),
    )
    .unwrap();
    let row = fetcher
        .fetch(initial_forgecode_position().unwrap())
        .unwrap()
        .unwrap();
    let mut row = Some(row);
    let mut producer = SqliteLogicalRowBatchProducer::new(
        test_source("projection-test"),
        initial_forgecode_position().unwrap(),
        move |position: NativePosition| {
            if position.value() == [0] {
                Ok(row.take())
            } else {
                Ok(None)
            }
        },
    );
    let batch = producer
        .next_batch()
        .map_err(forgecode_sqlite_batch_error)
        .unwrap()
        .unwrap();
    let context = ProviderAdapterContext {
        machine_id: "forgecode-projection-test".to_owned(),
        source_path: Some("/tmp/.forge.db".into()),
        source_root: Some("/tmp/project".into()),
        imported_at: DateTime::<Utc>::UNIX_EPOCH,
    };
    let mut projector = ForgeCodeCapturedBatchProjector {
        context,
        raw_source_path: "/tmp/.forge.db".to_owned(),
        user_version: 0,
        schema_fingerprint: "schema:test".to_owned(),
    };
    let mut output = CollectingProjectionOutput::default();
    projector
        .project_record(&batch.records()[0], &mut output)
        .unwrap();

    assert!(output.rejections.is_empty());
    assert_eq!(output.normalizations.len(), 3);
    assert!(output
        .normalizations
        .iter()
        .all(|normalization| normalization.captures.len() <= 1
            && normalization.files_touched.len() <= 1));
    assert_eq!(
        output
            .normalizations
            .iter()
            .map(|normalization| normalization.captures.len())
            .sum::<usize>(),
        2
    );
    assert_eq!(
        output
            .normalizations
            .iter()
            .map(|normalization| normalization.files_touched.len())
            .sum::<usize>(),
        1
    );
    assert!(output
        .normalizations
        .iter()
        .flat_map(|normalization| &normalization.captures)
        .all(|(_, capture)| {
            capture.source.raw_source_path.as_deref() == Some("/tmp/.forge.db")
                && capture.source.source_root.as_deref() == Some("/tmp/project")
        }));
    assert!(output
        .normalizations
        .iter()
        .flat_map(|normalization| &normalization.files_touched)
        .all(|(_, touch)| touch.source_root.as_deref() == Some("/tmp/project")));
}

#[test]
fn projector_streams_many_messages_without_collecting_normalizations() {
    let conn = Connection::open_in_memory().unwrap();
    create_conversations_table(&conn);
    let messages = (0..512)
        .map(|index| {
            json!({
                "message": {
                    "text": {
                        "role": if index % 2 == 0 { "user" } else { "assistant" },
                        "content": format!("bounded ForgeCode message {index}")
                    }
                }
            })
        })
        .collect::<Vec<_>>();
    conn.execute(
        "insert into conversations values (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            "conversation-many-messages",
            "Streaming projection",
            9_i64,
            serde_json::to_string(&json!({"messages": messages})).unwrap(),
            "2026-01-01T00:00:00Z",
            "2026-01-01T00:00:01Z",
            Option::<String>::None,
        ],
    )
    .unwrap();
    let batch = captured_conversation_batch(&conn, "many-message-projection-test");
    assert_eq!(batch.records().len(), 1);
    let mut projector = ForgeCodeCapturedBatchProjector {
        context: ProviderAdapterContext {
            machine_id: "forgecode-many-message-test".to_owned(),
            source_path: Some("/tmp/.forge.db".into()),
            source_root: Some("/tmp/project".into()),
            imported_at: DateTime::<Utc>::UNIX_EPOCH,
        },
        raw_source_path: "/tmp/.forge.db".to_owned(),
        user_version: 0,
        schema_fingerprint: "schema:test".to_owned(),
    };
    let mut output = CountingProjectionOutput::default();
    projector
        .project_record(&batch.records()[0], &mut output)
        .unwrap();

    assert_eq!(output.emissions, 512);
    assert_eq!(output.captures, 512);
    assert_eq!(output.files_touched, 0);
    assert!(output.rejections.is_empty());
}

#[test]
fn production_import_rejects_malformed_row_and_advances_past_valid_sibling() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let source_path = directory.path().join("malformed-and-valid.forge.db");
    let source = Connection::open(&source_path).unwrap();
    create_conversations_table(&source);
    source
        .execute(
            "insert into conversations values (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                "conversation-malformed",
                "Malformed projection",
                11_i64,
                "{not-context-json",
                "2026-01-01T00:00:00Z",
                Option::<String>::None,
                "[not-metrics-json",
            ],
        )
        .unwrap();
    source
        .execute(
            "insert into conversations values (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                "conversation-valid-sibling",
                "Valid sibling",
                12_i64,
                serde_json::to_string(&json!({
                    "messages": [{
                        "message": {"text": {
                            "role": "assistant",
                            "content": "valid sibling persisted",
                            "tool_calls": [{
                                "name": "write",
                                "arguments": {"path": "src/valid.rs"}
                            }]
                        }}
                    }]
                }))
                .unwrap(),
                "2026-01-01T00:00:01Z",
                Option::<String>::None,
                serde_json::to_string(&json!({
                    "files_accessed": ["Cargo.toml"]
                }))
                .unwrap(),
            ],
        )
        .unwrap();
    drop(source);

    let store_path = directory.path().join("work.sqlite");
    let mut store = Store::open(&store_path).unwrap();
    let context = ProviderAdapterContext {
        machine_id: "forgecode-malformed-production-test".to_owned(),
        source_path: Some(source_path.clone()),
        source_root: Some(directory.path().to_path_buf()),
        imported_at: DateTime::<Utc>::UNIX_EPOCH,
    };
    let first = import_forgecode_sqlite_batched(
        &source_path,
        &mut store,
        context.clone(),
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();

    assert_eq!(first.failed, 2, "{:?}", first.failures);
    assert_eq!(first.failures.len(), 2);
    assert!(first.failures[0].error.contains("conversations.context"));
    assert!(first.failures[1].error.contains("conversations.metrics"));
    assert!(first
        .failures
        .iter()
        .all(|failure| failure.error.len() <= 4 * 1024));
    assert_eq!(first.imported_sessions, 2);
    assert_eq!(first.imported_events, 1);

    let stored = Connection::open(&store_path).unwrap();
    let event_count: i64 = stored
        .query_row(
            "select count(*) from events e join sessions s on s.id = e.session_id \
             where s.provider = 'forgecode'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(event_count, 1);
    let touch_count: i64 = stored
        .query_row(
            "select count(*) from ctx_files_touched where provider = 'forgecode'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(touch_count, 2);

    let second = import_forgecode_sqlite_batched(
        &source_path,
        &mut store,
        context,
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    // Certified no-op replay preserves the two deterministic row failures
    // even though bounded diagnostic text is not duplicated in the cursor.
    assert_eq!(second.failed, 2, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(
        stored
            .query_row(
                "select count(*) from events e join sessions s on s.id = e.session_id \
                 where s.provider = 'forgecode'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        event_count
    );
    assert_eq!(
        stored
            .query_row(
                "select count(*) from ctx_files_touched where provider = 'forgecode'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        touch_count
    );
}

#[test]
fn production_import_persists_every_multi_message_unit() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let source_path = directory.path().join(".forge.db");
    let source = Connection::open(&source_path).unwrap();
    create_conversations_table(&source);
    source
        .execute(
            "insert into conversations values (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                "conversation-production",
                "Production splitter",
                7_i64,
                serde_json::to_string(&json!({
                    "messages": [
                        {"message": {"text": {"role": "user", "content": "hello"}}},
                        {"message": {"text": {
                            "role": "assistant",
                            "content": "done",
                            "tool_calls": [{
                                "name": "write",
                                "arguments": {"path": "src/lib.rs"}
                            }]
                        }}}
                    ]
                }))
                .unwrap(),
                "2026-01-01T00:00:00Z",
                "2026-01-01T00:00:01Z",
                serde_json::to_string(&json!({
                    "files_changed": {
                        "src/lib.rs": {"tool": "write", "lines_added": 1}
                    },
                    "files_accessed": ["Cargo.toml"]
                }))
                .unwrap(),
            ],
        )
        .unwrap();
    drop(source);

    let store_path = directory.path().join("work.sqlite");
    let mut store = Store::open(&store_path).unwrap();
    let summary = import_forgecode_sqlite_batched(
        &source_path,
        &mut store,
        ProviderAdapterContext {
            machine_id: "forgecode-production-splitter".to_owned(),
            source_path: Some(source_path.clone()),
            source_root: Some(directory.path().to_path_buf()),
            imported_at: DateTime::<Utc>::UNIX_EPOCH,
        },
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 2);
    let stored = Connection::open(&store_path).unwrap();
    let event_count: i64 = stored
        .query_row(
            "select count(*) from events e join sessions s on s.id = e.session_id \
             where s.provider = 'forgecode'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(event_count, 2);
    let touch_count: i64 = stored
        .query_row(
            "select count(*) from ctx_files_touched where provider = 'forgecode'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(touch_count, 3);
}

#[test]
fn retained_length_preflight_rejects_oversize_before_hydration() {
    let limit = forgecode_oversize_limit().unwrap();
    let retained_bytes = limit
        .checked_sub(FORGECODE_SQLITE_VALUE_OVERHEAD_BYTES)
        .unwrap()
        .checked_add(1)
        .unwrap();
    let candidate = ForgeCodeRowCandidate {
        rowid: 1,
        retained_bytes: i64::try_from(retained_bytes).unwrap(),
        storage_classes: [
            "text".to_owned(),
            "null".to_owned(),
            "integer".to_owned(),
            "null".to_owned(),
            "text".to_owned(),
            "null".to_owned(),
            "null".to_owned(),
        ],
    };
    assert!(candidate.observed_bytes().unwrap() > limit);
}

#[test]
fn capped_connection_rejects_oversize_row_and_continues_to_healthy_sibling() {
    use crate::captured_batch::StructuralRejectionKind;

    let conn = Connection::open_in_memory().unwrap();
    create_conversations_table(&conn);
    let oversize = i64::try_from(CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES)
        .unwrap()
        .checked_add(1)
        .unwrap();
    conn.execute(
        "insert into conversations values ('conversation-oversize', 'Oversize', \
         zeroblob(?1), '{\"messages\":[]}', '2026-01-01T00:00:00Z', NULL, NULL)",
        [oversize],
    )
    .unwrap();
    conn.execute(
        "insert into conversations values (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            "conversation-healthy",
            "Healthy sibling",
            2_i64,
            r#"{"messages":[]}"#,
            "2026-01-01T00:00:01Z",
            Option::<String>::None,
            Option::<String>::None,
        ],
    )
    .unwrap();
    let sqlite_value_limit = i32::try_from(crate::MAX_PROVIDER_SQLITE_VALUE_BYTES).unwrap();
    conn.set_limit(SqliteLimit::SQLITE_LIMIT_LENGTH, sqlite_value_limit);

    let batch = captured_conversation_batch(&conn, "capped-oversize-sibling");

    assert_eq!(
        conn.limit(SqliteLimit::SQLITE_LIMIT_LENGTH),
        sqlite_value_limit
    );
    assert_eq!(batch.records().len(), 2);
    assert!(matches!(
        batch.records()[0].payload(),
        CapturedRecordPayload::StructuralRejection {
            kind: StructuralRejectionKind::OversizeRecord,
            observed_bytes,
        } if *observed_bytes > CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES as u64
    ));
    let CapturedRecordPayload::SqliteValues(values) = batch.records()[1].payload() else {
        panic!("healthy ForgeCode sibling was not hydrated");
    };
    assert_eq!(
        decode_forgecode_conversation(values)
            .unwrap()
            .conversation_id,
        "conversation-healthy"
    );
}

#[test]
fn capped_connection_rejects_malformed_workspace_storage_class_and_continues() {
    let conn = Connection::open_in_memory().unwrap();
    create_conversations_table(&conn);
    conn.execute(
        "insert into conversations values ('conversation-malformed-workspace', \
         'Malformed workspace', 'not-an-integer', '{\"messages\":[]}', \
         '2026-01-01T00:00:00Z', NULL, NULL)",
        [],
    )
    .unwrap();
    conn.execute(
        "insert into conversations values (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            "conversation-healthy",
            "Healthy sibling",
            2_i64,
            r#"{"messages":[]}"#,
            "2026-01-01T00:00:01Z",
            Option::<String>::None,
            Option::<String>::None,
        ],
    )
    .unwrap();
    let sqlite_value_limit = i32::try_from(crate::MAX_PROVIDER_SQLITE_VALUE_BYTES).unwrap();
    conn.set_limit(SqliteLimit::SQLITE_LIMIT_LENGTH, sqlite_value_limit);

    let batch = captured_conversation_batch(&conn, "capped-malformed-workspace-sibling");

    assert_eq!(
        conn.limit(SqliteLimit::SQLITE_LIMIT_LENGTH),
        sqlite_value_limit
    );
    assert_eq!(batch.records().len(), 2);
    assert_eq!(
        batch.records()[0].record_kind().as_str(),
        FORGECODE_REJECTED_RECORD_KIND
    );
    assert_eq!(
        batch.records()[1].record_kind().as_str(),
        FORGECODE_RECORD_KIND
    );

    let mut projector = ForgeCodeCapturedBatchProjector {
        context: ProviderAdapterContext {
            machine_id: "forgecode-malformed-workspace-test".to_owned(),
            source_path: Some("/tmp/.forge.db".into()),
            source_root: Some("/tmp/project".into()),
            imported_at: DateTime::<Utc>::UNIX_EPOCH,
        },
        raw_source_path: "/tmp/.forge.db".to_owned(),
        user_version: 0,
        schema_fingerprint: "schema:test".to_owned(),
    };
    let mut output = CollectingProjectionOutput::default();
    for record in batch.records() {
        projector.project_record(record, &mut output).unwrap();
    }
    assert_eq!(output.rejections.len(), 1);
    assert!(output.rejections[0]
        .1
        .contains("workspace_id has an unsupported SQLite storage class"));
    assert_eq!(output.normalizations.len(), 1);
    assert_eq!(output.normalizations[0].captures.len(), 1);
    assert_eq!(
        output.normalizations[0].captures[0]
            .1
            .session
            .provider_session_id,
        "conversation-healthy"
    );
}

#[test]
fn metric_file_touches_stream_source_order_with_a_fixed_unique_ceiling() {
    let paths = (0..=MAX_PROVIDER_FILE_TOUCHES_PER_EVENT)
        .rev()
        .map(|index| Value::String(format!("src/path-{index}.rs")))
        .collect::<Vec<_>>();
    let metrics = json!({"files_accessed": paths});
    let row = ForgeCodeConversationRow {
        rowid: 1,
        conversation_id: "conversation-touch-ceiling".to_owned(),
        title: None,
        workspace_id: 1,
        context: None,
        created_at: "2026-01-01T00:00:00Z".to_owned(),
        updated_at: None,
        metrics: None,
    };
    let mut emitted = 0_usize;
    let mut first_path = None;
    let mut last_path = None;

    let limit_exceeded = forgecode_for_each_metric_file_touch(
        &row,
        &metrics,
        "/tmp/.forge.db",
        DateTime::<Utc>::UNIX_EPOCH,
        |(_, touch)| {
            first_path.get_or_insert_with(|| touch.path.clone());
            last_path = Some(touch.path);
            emitted = emitted.saturating_add(1);
            Ok::<(), ()>(())
        },
    )
    .unwrap();

    assert!(limit_exceeded);
    assert_eq!(emitted, MAX_PROVIDER_FILE_TOUCHES_PER_EVENT);
    let expected_first = format!("src/path-{}.rs", MAX_PROVIDER_FILE_TOUCHES_PER_EVENT);
    assert_eq!(first_path.as_deref(), Some(expected_first.as_str()));
    assert_eq!(last_path.as_deref(), Some("src/path-1.rs"));
}

#[test]
fn source_snapshot_detects_database_changes() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let path = directory.path().join(".forge.db");
    fs::write(&path, b"forgecode-snapshot").unwrap();
    let snapshot = forgecode_source_snapshot(&path).unwrap();
    assert!(snapshot.revalidate(&path).unwrap());

    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(b"-changed").unwrap();
    file.sync_all().unwrap();
    assert!(!snapshot.revalidate(&path).unwrap());
}
