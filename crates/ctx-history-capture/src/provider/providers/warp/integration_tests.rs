use std::cell::RefCell;
use std::num::NonZeroUsize;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use super::position::{encode_warp_position, WarpKeyset, WarpPhase};
use super::projection::{
    decode_warp_invalid_task_key, warp_message_identity_index, WarpCapturedBatchProjector,
    WarpParserCheckpoint,
};
use super::sqlite::{
    warp_fetch_test_counts, warp_reset_fetch_test_counts, warp_sqlite_batch_error,
    warp_start_task_key_hydration_trace, warp_take_task_key_hydration_trace,
    warp_task_keyset_index, WarpRowFetcher, WARP_ORDERING_KEY_MAX_BYTES,
    WARP_TASK_INVALID_KEY_RECORD_KIND, WARP_TASK_RECORD_KIND,
};
use super::*;
use crate::test_support_paths::tempdir;
use crate::{
    captured_batch::{CapturedRecordPayload, StructuralRejectionKind, CAPTURE_BATCH_MAX_RECORDS},
    MAX_PROVIDER_SQLITE_VALUE_BYTES,
};
use rusqlite::limits::Limit;

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

fn create_warp_schema(conn: &Connection) {
    conn.execute_batch(
        "create table agent_conversations (\
                id integer primary key,\
                conversation_id text not null unique,\
                conversation_data text not null,\
                last_modified_at text not null\
             );\
             create table agent_tasks (\
                id integer primary key,\
                conversation_id text not null,\
                task_id text not null unique,\
                task blob not null,\
                last_modified_at text not null\
             );",
    )
    .unwrap();
}

fn create_warp_schema_without_task_keyset_index(conn: &Connection) {
    conn.execute_batch(
        "create table agent_conversations (\
                id integer primary key,\
                conversation_id text not null unique,\
                conversation_data text not null,\
                last_modified_at text not null\
             );\
             create table agent_tasks (\
                id integer primary key,\
                conversation_id text not null,\
                task_id text not null,\
                task blob not null,\
                last_modified_at text not null\
             );",
    )
    .unwrap();
}

fn insert_conversation(
    conn: &Connection,
    conversation_id: &str,
    modified: &str,
    parent: Option<&str>,
) {
    conn.execute(
        "insert into agent_conversations \
             (conversation_id, conversation_data, last_modified_at) values (?1, ?2, ?3)",
        rusqlite::params![
            conversation_id,
            json!({
                "agent_name": format!("Warp {conversation_id}"),
                "parent_conversation_id": parent,
                "run_id": format!("run-{conversation_id}"),
            })
            .to_string(),
            modified,
        ],
    )
    .unwrap();
}

fn insert_task(conn: &Connection, conversation_id: &str, task_id: &str, task: &[u8]) {
    conn.execute(
        "insert into agent_tasks \
             (conversation_id, task_id, task, last_modified_at) \
             values (?1, ?2, ?3, '2026-07-18 12:00:01')",
        rusqlite::params![conversation_id, task_id, task],
    )
    .unwrap();
}

fn proto_field(field: u32, payload: &[u8]) -> Vec<u8> {
    let mut encoded = proto_varint_bytes(u64::from(field) << 3 | 2);
    encoded.extend(proto_varint_bytes(payload.len() as u64));
    encoded.extend_from_slice(payload);
    encoded
}

fn proto_varint_bytes(mut value: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        bytes.push(byte);
        if value == 0 {
            return bytes;
        }
    }
}

fn warp_task_bytes(task_id: &str, text: &str) -> Vec<u8> {
    let mut user_query = Vec::new();
    user_query.extend(proto_field(1, text.as_bytes()));
    let mut message = Vec::new();
    message.extend(proto_field(1, format!("message-{task_id}").as_bytes()));
    message.extend(proto_field(2, &user_query));
    let mut task = Vec::new();
    task.extend(proto_field(1, task_id.as_bytes()));
    task.extend(proto_field(2, format!("description {task_id}").as_bytes()));
    task.extend(proto_field(5, &message));
    task.extend(proto_field(6, format!("summary {task_id}").as_bytes()));
    task
}

fn warp_task_with_shell_result(task_id: &str, message_text: &str, result_text: &str) -> Vec<u8> {
    let user_query = proto_field(1, message_text.as_bytes());
    let mut user_message = proto_field(1, b"warp-message-long");
    user_message.extend(proto_field(2, &user_query));

    let finished = proto_field(1, result_text.as_bytes());
    let run_shell = proto_field(5, &finished);
    let mut tool_result = proto_field(1, b"warp-call-1");
    tool_result.extend(proto_field(2, &run_shell));
    let mut result_message = proto_field(1, b"warp-message-result");
    result_message.extend(proto_field(5, &tool_result));

    let mut task = proto_field(1, task_id.as_bytes());
    task.extend(proto_field(5, &user_message));
    task.extend(proto_field(5, &result_message));
    task
}

fn test_source(label: &str) -> SourceObservation {
    SourceObservation::new(
        CaptureProvider::Warp,
        WARP_SQLITE_SOURCE_FORMAT,
        format!("warp-test-source:{label}"),
        format!("warp-test-revision:{label}"),
        format!("warp-test-stream:{label}"),
        WARP_CAPTURE_REVISION,
        WARP_POLICY_REVISION,
        None,
    )
    .unwrap()
}

fn test_context(path: &Path) -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: "warp-batch-test-machine".to_owned(),
        source_path: Some(path.to_path_buf()),
        source_root: None,
        imported_at: "2026-07-18T12:00:00Z".parse().unwrap(),
    }
}

fn collect_batches(
    conn: &Connection,
    source: SourceObservation,
    start: NativePosition,
) -> Vec<CapturedBatch> {
    let mut fetcher = WarpRowFetcher::new(conn, &start).unwrap();
    let mut producer =
        SqliteLogicalRowBatchProducer::new(source, start, move |position| fetcher.fetch(position));
    let mut batches = Vec::new();
    while let Some(batch) = producer.next_batch().unwrap() {
        batches.push(batch);
    }
    batches
}

type GroupedImportResult = (
    ProviderImportSummary,
    Vec<Vec<u8>>,
    Vec<(usize, usize, usize)>,
);

fn import_with_group_limit(
    conn: &Connection,
    store: &mut Store,
    source: SourceObservation,
    context: ProviderAdapterContext,
    max_batches: NonZeroUsize,
) -> GroupedImportResult {
    let initial_position = initial_warp_position().unwrap();
    let admission = CapturedSourceAdmission::conversation_for_context(&source, &context).unwrap();
    let stream = captured_batch_cursor_stream(&source);
    let mut fetcher = WarpRowFetcher::new(conn, &initial_position).unwrap();
    let mut producer =
        SqliteLogicalRowBatchProducer::new(source, initial_position.clone(), move |position| {
            fetcher.fetch(position)
        });
    let raw_source_path = context
        .source_path
        .as_deref()
        .unwrap()
        .display()
        .to_string();
    let mut projector = WarpCapturedBatchProjector {
        context: context.clone(),
        raw_source_path,
        user_version: 0,
        schema_fingerprint: "warp-schema-test".to_owned(),
        checkpoint: WarpParserCheckpoint::default(),
    };
    let mut expected = None;
    let mut summary = ProviderImportSummary::default();
    let mut checkpoints = Vec::new();
    let mut fetch_counts = Vec::new();
    loop {
        let call_fetch_counts = RefCell::new(Vec::new());
        let outcome = crate::provider::importer::import_captured_batches(
            store,
            &admission,
            NormalizedProviderImportOptions::default(),
            &context.machine_id,
            context.imported_at,
            expected.as_ref(),
            &initial_position,
            CapturedBatchCursorMode::Resume,
            max_batches,
            &mut projector,
            || producer.next_batch().map_err(warp_sqlite_batch_error),
            || {
                call_fetch_counts
                    .borrow_mut()
                    .push(warp_fetch_test_counts());
                Ok(true)
            },
        )
        .unwrap();
        summary.merge(outcome.summary);
        if outcome.batches_imported == 0 {
            break;
        }
        fetch_counts.extend(call_fetch_counts.into_inner());
        expected = store
            .get_sync_cursor(None, &context.machine_id, &stream)
            .unwrap();
        let certified =
            CertifiedProviderCursor::decode_if_certified(&expected.as_ref().unwrap().cursor)
                .unwrap()
                .unwrap();
        checkpoints.push(certified.parser_checkpoint().as_bytes().to_vec());
        if outcome.source_exhausted {
            break;
        }
    }
    (summary, checkpoints, fetch_counts)
}

#[test]
fn warp_native_keysets_use_indexes_without_temp_preparation_or_sorts() {
    let conn = Connection::open_in_memory().unwrap();
    create_warp_schema(&conn);
    insert_conversation(&conn, "conversation-1", "2026-07-18 12:00:00", None);
    insert_task(
        &conn,
        "conversation-1",
        "task-1",
        &warp_task_bytes("task-1", "hello"),
    );
    let start = initial_warp_position().unwrap();
    let _fetcher = WarpRowFetcher::new(&conn, &start).unwrap();
    assert_eq!(
        conn.query_row("select count(*) from temp.sqlite_schema", [], |row| row
            .get::<_, i64>(0),)
            .unwrap(),
        0,
        "Warp fetch setup must not materialize TEMP state"
    );

    let conversation_plan = conn
        .prepare(
            "explain query plan \
                 select c.rowid from agent_conversations c \
                 where c.rowid > ?1 order by c.rowid limit 1",
        )
        .unwrap()
        .query_map([0_i64], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    let task_index = warp_task_keyset_index(&conn).unwrap();
    let task_plan = conn
        .prepare(
            "explain query plan \
                 select t.rowid from agent_tasks t indexed by sqlite_autoindex_agent_tasks_1 \
                 where t.task_id collate binary > (\
                       select previous.task_id from agent_tasks previous \
                       where previous.rowid = ?1\
                   ) \
                 order by t.task_id collate binary limit 1",
        )
        .unwrap()
        .query_map([0_i64], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(task_index, "sqlite_autoindex_agent_tasks_1");
    for plan in [&conversation_plan, &task_plan] {
        assert!(
            plan.iter()
                .all(|detail| !detail.contains("USE TEMP B-TREE")),
            "Warp keyset lookup must not sort: {plan:?}"
        );
        assert!(
            plan.iter().any(|detail| detail.contains("SEARCH")),
            "Warp keyset lookup must use a native index seek: {plan:?}"
        );
    }
    assert!(
        task_plan.iter().any(|detail| {
            detail.contains("sqlite_autoindex_agent_tasks_1") && detail.contains("task_id>?")
        }),
        "Warp task lookup must seek the certified global task_id index: {task_plan:?}"
    );
}

#[test]
fn warp_missing_or_nonbinary_task_keyset_index_fails_closed() {
    let missing = Connection::open_in_memory().unwrap();
    create_warp_schema_without_task_keyset_index(&missing);
    let Err(error) = WarpRowFetcher::new(&missing, &initial_warp_position().unwrap()) else {
        panic!("Warp accepted a task table without a bounded keyset index");
    };
    assert!(error
        .to_string()
        .contains("requires a non-partial ascending UNIQUE BINARY index on task_id"));

    let nonbinary = Connection::open_in_memory().unwrap();
    create_warp_schema_without_task_keyset_index(&nonbinary);
    nonbinary
        .execute_batch(
            "create unique index warp_agent_tasks_nocase \
                 on agent_tasks (task_id collate nocase)",
        )
        .unwrap();
    let Err(error) = WarpRowFetcher::new(&nonbinary, &initial_warp_position().unwrap()) else {
        panic!("Warp accepted a non-BINARY task keyset index");
    };
    assert!(error
        .to_string()
        .contains("requires a non-partial ascending UNIQUE BINARY index on task_id"));
}

#[test]
fn warp_duplicate_task_keys_fail_closed_instead_of_skipping_a_row() {
    let conn = Connection::open_in_memory().unwrap();
    create_warp_schema_without_task_keyset_index(&conn);
    insert_conversation(&conn, "conversation-1", "2026-07-18 12:00:00", None);
    insert_task(
        &conn,
        "conversation-1",
        "duplicate-task",
        &warp_task_bytes("duplicate-task-a", "first"),
    );
    insert_task(
        &conn,
        "conversation-1",
        "duplicate-task",
        &warp_task_bytes("duplicate-task-b", "second"),
    );
    let Err(error) = WarpRowFetcher::new(&conn, &initial_warp_position().unwrap()) else {
        panic!("Warp accepted duplicate task keys without a uniqueness contract");
    };
    assert!(error
        .to_string()
        .contains("requires a non-partial ascending UNIQUE BINARY index on task_id"));
}

#[test]
fn warp_interleaved_global_task_ids_advance_one_native_index_entry() {
    let conn = Connection::open_in_memory().unwrap();
    create_warp_schema(&conn);
    insert_conversation(&conn, "conversation-target", "2026-07-18 12:00:00", None);
    insert_conversation(&conn, "conversation-noise", "2026-07-18 12:00:00", None);
    insert_task(
        &conn,
        "conversation-target",
        "task-0000",
        &warp_task_bytes("task-0000", "first target"),
    );
    let first_target_rowid = conn.last_insert_rowid();
    let mut first_noise_rowid = None;
    for index in 1..=4_096 {
        let task_id = format!("task-{index:04}");
        insert_task(
            &conn,
            "conversation-noise",
            &task_id,
            &warp_task_bytes(&task_id, "noise"),
        );
        first_noise_rowid.get_or_insert(conn.last_insert_rowid());
    }
    insert_task(
        &conn,
        "conversation-target",
        "task-9999",
        &warp_task_bytes("task-9999", "second target"),
    );
    let second_target_rowid = conn.last_insert_rowid();

    let position = encode_warp_position(WarpKeyset {
        phase: WarpPhase::Tasks,
        next_ordinal: 2,
        rowid: first_target_rowid,
        key_valid: true,
    })
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
    let mut fetcher = WarpRowFetcher::new(&conn, &position).unwrap();
    let row = fetcher.fetch(position).unwrap().unwrap();
    conn.progress_handler(0, None::<fn() -> bool>);
    let keyset = decode_warp_position(row.next_position()).unwrap().unwrap();
    assert_eq!(keyset.phase, WarpPhase::Tasks);
    assert_eq!(keyset.rowid, first_noise_rowid.unwrap());
    assert!(
        operations.load(Ordering::Relaxed) < 2_000,
        "global task_id seek revisited interleaved source share"
    );
    assert!(second_target_rowid > keyset.rowid);
}

#[test]
fn warp_terminal_reopen_does_only_bounded_native_keyset_setup() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("warp.sqlite");
    let conn = Connection::open(&path).unwrap();
    create_warp_schema(&conn);
    insert_conversation(&conn, "conversation-1", "2026-07-18 12:00:00", None);
    insert_task(
        &conn,
        "conversation-1",
        "task-1",
        &warp_task_bytes("task-1", "hello"),
    );
    drop(conn);
    let mut store = Store::open(directory.path().join("store.sqlite")).unwrap();
    let context = test_context(&path);

    warp_reset_fetch_test_counts();
    import_warp_sqlite_batched(
        &path,
        &mut store,
        context.clone(),
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(warp_fetch_test_counts(), (1, 1, 1));

    warp_reset_fetch_test_counts();
    let summary = import_warp_sqlite_batched(
        &path,
        &mut store,
        context,
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    let mut expected = ProviderImportSummary::default();
    expected.set_work_result(crate::ProviderImportWorkResult::NoOp);
    assert_eq!(summary, expected);
    assert_eq!(warp_fetch_test_counts(), (1, 0, 0));
    let conn = Connection::open(&path).unwrap();
    assert_eq!(
        conn.query_row("select count(*) from temp.sqlite_schema", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        0
    );
}

#[test]
fn warp_near_tail_lookup_including_setup_has_constant_work() {
    let conn = Connection::open_in_memory().unwrap();
    create_warp_schema(&conn);
    insert_conversation(&conn, "conversation-1", "2026-07-18 12:00:00", None);
    for index in 0..2_048 {
        let task_id = format!("task-{index:04}");
        insert_task(
            &conn,
            "conversation-1",
            &task_id,
            &warp_task_bytes(&task_id, "hello"),
        );
    }
    let operations = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&operations);
    conn.progress_handler(
        1,
        Some(move || {
            observed.fetch_add(1, Ordering::Relaxed);
            false
        }),
    );
    let position = encode_warp_position(WarpKeyset {
        phase: WarpPhase::Tasks,
        next_ordinal: 2_048,
        rowid: 2_047,
        key_valid: true,
    })
    .unwrap();
    let mut fetcher = WarpRowFetcher::new(&conn, &position).unwrap();
    assert!(fetcher.fetch(position).unwrap().is_some());
    conn.progress_handler(0, None::<fn() -> bool>);
    assert!(
        operations.load(Ordering::Relaxed) < 2_000,
        "near-tail lookup revisited too much source state"
    );
}

#[test]
fn warp_batches_sixty_five_tasks_and_resumes_exact_task_keyset() {
    let conn = Connection::open_in_memory().unwrap();
    create_warp_schema(&conn);
    insert_conversation(&conn, "conversation-1", "2026-07-18 12:00:00", None);
    for index in 0..65 {
        let task_id = format!("task-{index:03}");
        insert_task(
            &conn,
            "conversation-1",
            &task_id,
            &warp_task_bytes(&task_id, "hello"),
        );
    }
    let batches = collect_batches(
        &conn,
        test_source("sixty-five"),
        initial_warp_position().unwrap(),
    );
    assert_eq!(batches.len(), 2);
    assert_eq!(batches[0].records().len(), CAPTURE_BATCH_MAX_RECORDS);
    assert_eq!(batches[1].records().len(), 2);
    let first_end = decode_warp_position(batches[0].range_end())
        .unwrap()
        .unwrap();
    assert_eq!(first_end.phase, WarpPhase::Tasks);
    assert_eq!(first_end.next_ordinal, CAPTURE_BATCH_MAX_RECORDS as u64);
    assert_eq!(first_end.rowid, 63);
    let second_end = decode_warp_position(batches[1].range_end())
        .unwrap()
        .unwrap();
    assert_eq!(second_end.phase, WarpPhase::Tasks);
    assert_eq!(second_end.next_ordinal, 66);
    assert_eq!(second_end.rowid, 65);

    let path = PathBuf::from("/tmp/warp-sixty-five.sqlite");
    let mut projector = WarpCapturedBatchProjector {
        context: test_context(&path),
        raw_source_path: path.display().to_string(),
        user_version: 0,
        schema_fingerprint: "warp-schema-test".to_owned(),
        checkpoint: WarpParserCheckpoint::default(),
    };
    let mut output = CollectingProjectionOutput::default();
    for record in batches.iter().flat_map(|batch| batch.records()) {
        projector.project_record(record, &mut output).unwrap();
    }
    let CapturedBatchCursorFinish::Advance(finished) =
        projector.finish_cursor(batches.last().unwrap()).unwrap()
    else {
        panic!("Warp phased traversal must always publish its bounded unit checkpoint");
    };
    assert_eq!(
        finished.native_position(),
        batches.last().unwrap().range_end()
    );
    let checkpoint: WarpParserCheckpoint = finished.parser_checkpoint().deserialize().unwrap();
    assert_eq!(checkpoint.next_event_index, 65);

    let replayed = collect_batches(
        &conn,
        test_source("sixty-five"),
        initial_warp_position().unwrap(),
    );
    assert_eq!(replayed, batches);
    let event_indices = output
        .normalizations
        .iter()
        .flat_map(|normalization| normalization.captures.iter())
        .filter_map(|(_, capture)| {
            capture
                .event
                .as_ref()
                .map(|event| event.provider_event_index)
        })
        .collect::<Vec<_>>();
    assert_eq!(event_indices, (0_u64..65).collect::<Vec<_>>());
}
#[test]
fn warp_failure_after_projection_before_cursor_publish_replays_idempotently() {
    let directory = tempdir().unwrap();
    let source_path = directory.path().join("warp.sqlite");
    let conn = Connection::open_in_memory().unwrap();
    create_warp_schema(&conn);
    insert_conversation(&conn, "conversation-1", "2026-07-18 12:00:00", None);
    for index in 0..65 {
        let task_id = format!("task-{index:03}");
        insert_task(
            &conn,
            "conversation-1",
            &task_id,
            &warp_task_bytes(&task_id, "hello"),
        );
    }
    let source = test_source("crash-replay");
    let stream = captured_batch_cursor_stream(&source);
    let context = test_context(&source_path);
    let admission = CapturedSourceAdmission::conversation_for_context(&source, &context).unwrap();
    let initial = initial_warp_position().unwrap();
    let mut store = Store::open(directory.path().join("crash.sqlite")).unwrap();

    warp_reset_fetch_test_counts();
    let mut first_fetcher = WarpRowFetcher::new(&conn, &initial).unwrap();
    let mut first_producer =
        SqliteLogicalRowBatchProducer::new(source.clone(), initial.clone(), move |position| {
            first_fetcher.fetch(position)
        });
    let mut first_projector = WarpCapturedBatchProjector {
        context: context.clone(),
        raw_source_path: source_path.display().to_string(),
        user_version: 0,
        schema_fingerprint: "warp-schema-test".to_owned(),
        checkpoint: WarpParserCheckpoint::default(),
    };
    let error = crate::provider::importer::import_captured_batches(
        &mut store,
        &admission,
        NormalizedProviderImportOptions::default(),
        &context.machine_id,
        context.imported_at,
        None,
        &initial,
        CapturedBatchCursorMode::Resume,
        NonZeroUsize::new(1).unwrap(),
        &mut first_projector,
        || first_producer.next_batch().map_err(warp_sqlite_batch_error),
        || {
            Err(CaptureError::InvalidPayload(
                "simulated process interruption after projection".to_owned(),
            ))
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("simulated process interruption"));
    // Projection has persisted the first safe batch, but source revalidation fails before the
    // cursor CAS. Replay must dedupe those 63 events and continue from the initial frontier.
    assert_eq!(warp_fetch_test_counts(), (1, 1, 64));
    let retained = store
        .get_sync_cursor(None, &context.machine_id, &stream)
        .unwrap();
    assert!(retained.is_none());
    assert_eq!(store.list_sessions().unwrap().len(), 1);
    let partial_session = store.list_sessions().unwrap().pop().unwrap();
    assert_eq!(
        store.events_for_session(partial_session.id).unwrap().len(),
        63
    );

    let expected = store
        .get_sync_cursor(None, &context.machine_id, &stream)
        .unwrap();
    let mut replay_fetcher = WarpRowFetcher::new(&conn, &initial).unwrap();
    let mut replay_producer =
        SqliteLogicalRowBatchProducer::new(source.clone(), initial.clone(), move |position| {
            replay_fetcher.fetch(position)
        });
    let mut replay_projector = WarpCapturedBatchProjector {
        context: context.clone(),
        raw_source_path: source_path.display().to_string(),
        user_version: 0,
        schema_fingerprint: "warp-schema-test".to_owned(),
        checkpoint: WarpParserCheckpoint::default(),
    };
    crate::provider::importer::drain_captured_batches(
        &mut store,
        &admission,
        NormalizedProviderImportOptions::default(),
        &context.machine_id,
        context.imported_at,
        expected,
        &initial,
        CapturedBatchCursorMode::Resume,
        &stream,
        &mut replay_projector,
        || {
            replay_producer
                .next_batch()
                .map_err(warp_sqlite_batch_error)
        },
        || Ok(true),
    )
    .unwrap();
    // The retained initial cursor makes the replay hydrate all 65 tasks; together with the
    // interrupted producer's 64 hydrations (including its lookahead), the cumulative count is
    // 129. The store and final cursor parity checks below prove that lookahead was not committed.
    assert_eq!(warp_fetch_test_counts(), (2, 2, 129));

    let mut one_shot = Store::open(directory.path().join("one-shot.sqlite")).unwrap();
    let mut one_shot_fetcher = WarpRowFetcher::new(&conn, &initial).unwrap();
    let mut one_shot_producer =
        SqliteLogicalRowBatchProducer::new(source, initial.clone(), move |position| {
            one_shot_fetcher.fetch(position)
        });
    let mut one_shot_projector = WarpCapturedBatchProjector {
        context: context.clone(),
        raw_source_path: source_path.display().to_string(),
        user_version: 0,
        schema_fingerprint: "warp-schema-test".to_owned(),
        checkpoint: WarpParserCheckpoint::default(),
    };
    crate::provider::importer::drain_captured_batches(
        &mut one_shot,
        &admission,
        NormalizedProviderImportOptions::default(),
        &context.machine_id,
        context.imported_at,
        None,
        &initial,
        CapturedBatchCursorMode::Resume,
        &stream,
        &mut one_shot_projector,
        || {
            one_shot_producer
                .next_batch()
                .map_err(warp_sqlite_batch_error)
        },
        || Ok(true),
    )
    .unwrap();

    assert_eq!(
        store.list_sessions().unwrap(),
        one_shot.list_sessions().unwrap()
    );
    let session = store.list_sessions().unwrap().pop().unwrap();
    assert_eq!(
        store.events_for_session(session.id).unwrap(),
        one_shot.events_for_session(session.id).unwrap()
    );
    assert_eq!(
        store
            .get_sync_cursor(None, &context.machine_id, &stream)
            .unwrap(),
        one_shot
            .get_sync_cursor(None, &context.machine_id, &stream)
            .unwrap()
    );
}

#[test]
fn warp_group_four_five_resume_matches_one_shot_with_one_large_parent_hydration() {
    let directory = tempdir().unwrap();
    let source_path = directory.path().join("warp.sqlite");
    let conn = Connection::open_in_memory().unwrap();
    create_warp_schema(&conn);
    insert_conversation(
        &conn,
        "conversation-large-parent",
        "2026-07-18 12:00:00",
        Some("parent-conversation"),
    );
    let parent_payload = "parent-payload-text".repeat(65_536);
    conn.execute(
        "update agent_conversations set conversation_data = ?1 \
             where conversation_id = 'conversation-large-parent'",
        [json!({
            "agent_name": parent_payload,
            "parent_conversation_id": "parent-conversation",
            "run_id": "run-large-parent",
            "conversation_usage_metadata": {
                "padding": "parent-payload-text".repeat(65_536),
            },
        })
        .to_string()],
    )
    .unwrap();
    for index in (0..257).rev() {
        let task_id = format!("task-{index:03}");
        insert_task(
            &conn,
            "conversation-large-parent",
            &task_id,
            &warp_task_bytes(&task_id, &format!("message {index:03}")),
        );
    }
    let source = test_source("group-four-five");
    let cursor_stream = captured_batch_cursor_stream(&source);
    let context = test_context(&source_path);

    let mut grouped_store = Store::open(directory.path().join("grouped.sqlite")).unwrap();
    warp_reset_fetch_test_counts();
    let (grouped_summary, grouped_checkpoints, grouped_fetch_counts) = import_with_group_limit(
        &conn,
        &mut grouped_store,
        source.clone(),
        context.clone(),
        NonZeroUsize::new(4).unwrap(),
    );
    assert_eq!(warp_fetch_test_counts(), (1, 1, 257));
    // Group four has hydrated exactly one permitted logical-row lookahead, not a fifth batch.
    // Group five consumes it without refetching, hydrates the one remaining task, and tags its
    // final batch exhausted without a terminal importer poll.
    assert_eq!(grouped_fetch_counts, vec![(1, 1, 256), (1, 1, 257)]);
    assert_eq!(grouped_checkpoints.len(), 2);
    for (checkpoint, expected_event_index) in grouped_checkpoints.iter().zip([255_u64, 257]) {
        let decoded: WarpParserCheckpoint = serde_json::from_slice(checkpoint).unwrap();
        assert_eq!(decoded.next_event_index, expected_event_index);
        assert!(!String::from_utf8_lossy(checkpoint).contains("parent-payload-text"));
    }

    let mut one_shot_store = Store::open(directory.path().join("one-shot.sqlite")).unwrap();
    warp_reset_fetch_test_counts();
    let (one_shot_summary, one_shot_checkpoints, one_shot_fetch_counts) = import_with_group_limit(
        &conn,
        &mut one_shot_store,
        source,
        context,
        NonZeroUsize::new(64).unwrap(),
    );
    assert_eq!(warp_fetch_test_counts(), (1, 1, 257));
    assert_eq!(one_shot_fetch_counts, vec![(1, 1, 257)]);
    assert_eq!(one_shot_checkpoints.len(), 1);
    assert_eq!(grouped_summary, one_shot_summary);
    assert_eq!(
        grouped_store
            .get_sync_cursor(None, "warp-batch-test-machine", &cursor_stream)
            .unwrap(),
        one_shot_store
            .get_sync_cursor(None, "warp-batch-test-machine", &cursor_stream)
            .unwrap()
    );

    let grouped_sessions = grouped_store.list_sessions().unwrap();
    let one_shot_sessions = one_shot_store.list_sessions().unwrap();
    assert_eq!(grouped_sessions, one_shot_sessions);
    assert_eq!(
        grouped_sessions.len(),
        2,
        "parent placeholder plus real session"
    );
    let mut observed_task_ids = Vec::new();
    for session in &grouped_sessions {
        let grouped_events = grouped_store.events_for_session(session.id).unwrap();
        assert_eq!(
            grouped_events,
            one_shot_store.events_for_session(session.id).unwrap()
        );
        observed_task_ids.extend(grouped_events.iter().filter_map(|event| {
            event
                .sync
                .metadata
                .get("metadata")
                .and_then(|metadata| metadata.get("task_id"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        }));
    }
    assert_eq!(
        observed_task_ids,
        (0..257)
            .map(|index| format!("task-{index:03}"))
            .collect::<Vec<_>>(),
        "native task_id keyset changed Warp provider event order"
    );

    let batches = collect_batches(
        &conn,
        test_source("task-local-shape"),
        initial_warp_position().unwrap(),
    );
    assert_eq!(batches.len(), 5);
    let task_records = batches
        .iter()
        .flat_map(|batch| batch.records())
        .filter(|record| record.record_kind().as_str() == WARP_TASK_RECORD_KIND)
        .collect::<Vec<_>>();
    assert_eq!(task_records.len(), 257);
    for record in task_records {
        let CapturedRecordPayload::SqliteValues(values) = record.payload() else {
            panic!("Warp task record must be a logical SQLite row");
        };
        assert_eq!(values.len(), 5, "task row repeated parent payload columns");
    }
}

#[test]
fn warp_preflight_rejects_oversize_protobuf_before_blob_hydration() {
    let conn = Connection::open_in_memory().unwrap();
    create_warp_schema(&conn);
    insert_conversation(&conn, "conversation-oversize", "2026-07-18 12:00:00", None);
    conn.execute(
        "insert into agent_tasks \
             (conversation_id, task_id, task, last_modified_at) \
             values ('conversation-oversize', 'task-oversize', zeroblob(?1), \
                     '2026-07-18 12:00:01')",
        [i64::try_from(CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES + 1).unwrap()],
    )
    .unwrap();
    let batches = collect_batches(
        &conn,
        test_source("oversize"),
        initial_warp_position().unwrap(),
    );
    assert!(batches
        .iter()
        .flat_map(|batch| batch.records())
        .any(|record| {
            matches!(
                record.payload(),
                CapturedRecordPayload::StructuralRejection {
                    kind: StructuralRejectionKind::OversizeRecord,
                    ..
                }
            )
        }));
}

#[test]
fn warp_oversize_ordering_key_is_rejected_without_string_hydration() {
    let conn = Connection::open_in_memory().unwrap();
    create_warp_schema(&conn);
    insert_conversation(&conn, "conversation-1", "2026-07-18 12:00:00", None);
    insert_task(
        &conn,
        "conversation-1",
        "task-valid",
        &warp_task_bytes("task-valid", "hello"),
    );
    let valid_task_rowid = conn.last_insert_rowid();
    let oversize_task_id = "x".repeat(MAX_PROVIDER_SQLITE_VALUE_BYTES + 1);
    insert_task(
        &conn,
        "conversation-1",
        &oversize_task_id,
        &warp_task_bytes("oversize-proto-id", "ignored"),
    );
    let oversize_task_rowid = conn.last_insert_rowid();
    let sqlite_value_limit = i32::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).unwrap();
    conn.set_limit(Limit::SQLITE_LIMIT_LENGTH, sqlite_value_limit);
    assert_eq!(conn.limit(Limit::SQLITE_LIMIT_LENGTH), sqlite_value_limit);
    let oversize_key_bytes: i64 = conn
        .query_row(
            "select octet_length(task_id) from agent_tasks where rowid = ?1",
            [oversize_task_rowid],
            |row| row.get(0),
        )
        .unwrap();
    assert!(oversize_key_bytes > i64::from(sqlite_value_limit));

    warp_start_task_key_hydration_trace();
    let first = collect_batches(
        &conn,
        test_source("oversize-ordering-key"),
        initial_warp_position().unwrap(),
    );
    let first_hydrated_rowids = warp_take_task_key_hydration_trace();
    warp_start_task_key_hydration_trace();
    let replay = collect_batches(
        &conn,
        test_source("oversize-ordering-key"),
        initial_warp_position().unwrap(),
    );
    let replay_hydrated_rowids = warp_take_task_key_hydration_trace();
    assert_eq!(first, replay);
    assert_eq!(first_hydrated_rowids, vec![valid_task_rowid]);
    assert_eq!(replay_hydrated_rowids, first_hydrated_rowids);
    assert!(!replay_hydrated_rowids.contains(&oversize_task_rowid));
    let records = first
        .iter()
        .flat_map(|batch| batch.records())
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 3);
    assert_eq!(records[1].record_kind().as_str(), WARP_TASK_RECORD_KIND);
    assert_eq!(
        records[2].record_kind().as_str(),
        WARP_TASK_INVALID_KEY_RECORD_KIND
    );
    let (_, observed_key_bytes) = decode_warp_invalid_task_key(records[2].payload()).unwrap();
    assert!(observed_key_bytes > WARP_ORDERING_KEY_MAX_BYTES as i64);
    let end = decode_warp_position(first.last().unwrap().range_end())
        .unwrap()
        .unwrap();
    assert_eq!(end.phase, WarpPhase::Tasks);
    assert_eq!(end.next_ordinal, 3);
}

#[test]
fn warp_cursor_checkpoint_is_bounded_and_excludes_source_payloads() {
    let projector = WarpCapturedBatchProjector {
        context: test_context(Path::new("/tmp/warp-unit-checkpoint.sqlite")),
        raw_source_path: "/tmp/warp-unit-checkpoint.sqlite".to_owned(),
        user_version: 0,
        schema_fingerprint: "warp-schema-test".to_owned(),
        checkpoint: WarpParserCheckpoint::default(),
    };
    let cursor = projector
        .initial_cursor_candidate(
            &test_source("unit-checkpoint"),
            &initial_warp_position().unwrap(),
        )
        .unwrap();
    let checkpoint: WarpParserCheckpoint = cursor.parser_checkpoint().deserialize().unwrap();
    assert_eq!(checkpoint.next_event_index, 0);
    assert!(cursor.parser_checkpoint().as_bytes().len() < 64);
}
#[test]
fn warp_snapshot_detects_database_mutation() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("warp.sqlite");
    fs::write(&path, b"warp-snapshot").unwrap();
    let snapshot = warp_source_snapshot(&path).unwrap();
    assert!(snapshot.revalidate(&path).unwrap());
    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    use std::io::Write;
    file.write_all(b"-changed").unwrap();
    file.sync_all().unwrap();
    assert!(!snapshot.revalidate(&path).unwrap());
}

#[test]
fn warp_projection_attaches_exact_message_and_result_locators_without_raw_result() {
    use crate::complete_content::{
        VerifiedContentLocatorsV1, VerifiedContentRole, VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
    };

    let directory = tempdir().unwrap();
    let path = directory.path().join("warp-content-locator.sqlite");
    let conn = Connection::open(&path).unwrap();
    create_warp_schema(&conn);
    insert_conversation(&conn, "conversation-content", "2026-07-18 12:00:00", None);
    let message_text = "m".repeat(crate::PROVIDER_MAX_TEXT_CHARS + 32);
    let result_text = "exact Warp shell result\nUnicode: 🦀";
    insert_task(
        &conn,
        "conversation-content",
        "task-content",
        &warp_task_with_shell_result("task-content", &message_text, result_text),
    );
    let mut projector = WarpCapturedBatchProjector {
        context: test_context(&path),
        raw_source_path: path.display().to_string(),
        user_version: 0,
        schema_fingerprint: "warp-schema-test".to_owned(),
        checkpoint: WarpParserCheckpoint::default(),
    };
    let batches = collect_batches(
        &conn,
        test_source("content-locator"),
        initial_warp_position().unwrap(),
    );
    let mut output = CollectingProjectionOutput::default();
    for record in batches.iter().flat_map(|batch| batch.records()) {
        projector.project_record(record, &mut output).unwrap();
    }
    let events = output
        .normalizations
        .iter()
        .flat_map(|normalization| normalization.captures.iter())
        .filter_map(|(_, capture)| capture.event.as_ref())
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 2);

    let message_locators = VerifiedContentLocatorsV1::from_metadata_value(
        &events[0].metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY],
    )
    .unwrap();
    let message_locator = message_locators
        .locator(VerifiedContentRole::MessageBody)
        .unwrap();
    assert_eq!(message_locator.kind(), "warp-task-message-v1");
    assert_eq!(message_locator.source_locator().unwrap().value().len(), 12);

    let result_locators = VerifiedContentLocatorsV1::from_metadata_value(
        &events[1].metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY],
    )
    .unwrap();
    let result_locator = result_locators
        .locator(VerifiedContentRole::ResultBody)
        .unwrap();
    assert_eq!(result_locator.kind(), "warp-task-message-v1");
    assert!(result_locator
        .content_ref()
        .verifies(result_text.as_bytes()));
    assert!(events[1].payload.get("result_content_ref").is_some());
    assert!(!serde_json::to_string(&events[1])
        .unwrap()
        .contains(result_text));

    drop(conn);
    let mut store = Store::open(directory.path().join("store.sqlite")).unwrap();
    let summary = import_warp_sqlite_batched(
        &path,
        &mut store,
        test_context(&path),
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(summary.failed, 0);
    assert_eq!(summary.imported_events, 2);
    let session = store
        .list_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.external_session_id.as_deref() == Some("conversation-content"))
        .unwrap();
    let stored = store.events_for_session(session.id).unwrap();
    assert_eq!(stored.len(), 2);
    let stored_result = stored
        .iter()
        .find(|event| event.event_type == EventType::ToolOutput)
        .unwrap();
    let serialized = serde_json::to_string(stored_result).unwrap();
    assert!(!serialized.contains(result_text));
    assert!(stored_result.payload["body"]
        .get("result_content_ref")
        .is_some());
    assert!(stored_result.sync.metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY].is_object());
}

#[test]
fn warp_child_before_parent_projects_scoped_relationship_and_exact_order() {
    let conn = Connection::open_in_memory().unwrap();
    create_warp_schema(&conn);
    insert_conversation(&conn, "child", "2026-07-18 11:00:00", Some("parent"));
    insert_conversation(&conn, "parent", "2026-07-18 12:00:00", None);
    insert_task(
        &conn,
        "child",
        "child-task",
        &warp_task_bytes("child-task", "child message"),
    );
    insert_task(
        &conn,
        "parent",
        "parent-task",
        &warp_task_bytes("parent-task", "parent message"),
    );
    let path = PathBuf::from("/tmp/warp-child-order.sqlite");
    let context = test_context(&path);
    let mut projector = WarpCapturedBatchProjector {
        context,
        raw_source_path: path.display().to_string(),
        user_version: 0,
        schema_fingerprint: "warp-schema-test".to_owned(),
        checkpoint: WarpParserCheckpoint::default(),
    };
    let batches = collect_batches(
        &conn,
        test_source("child-order"),
        initial_warp_position().unwrap(),
    );
    let mut output = CollectingProjectionOutput::default();
    for record in batches.iter().flat_map(|batch| batch.records()) {
        projector.project_record(record, &mut output).unwrap();
    }
    let session_captures = output
        .normalizations
        .iter()
        .flat_map(|normalization| normalization.captures.iter())
        .filter(|(_, capture)| capture.event.is_none())
        .collect::<Vec<_>>();
    assert_eq!(session_captures.len(), 2);
    assert_eq!(session_captures[0].1.session.provider_session_id, "child");
    assert_eq!(
        session_captures[0]
            .1
            .session
            .parent_provider_session_id
            .as_deref(),
        Some("parent")
    );
    let event_captures = output
        .normalizations
        .iter()
        .flat_map(|normalization| normalization.captures.iter())
        .filter(|(_, capture)| capture.event.is_some())
        .collect::<Vec<_>>();
    assert_eq!(event_captures.len(), 2);
    assert_eq!(event_captures[0].1.session.provider_session_id, "child");
    assert!(event_captures[0]
        .1
        .session
        .parent_provider_session_id
        .is_none());
    assert_eq!(event_captures[1].1.session.provider_session_id, "parent");
}

#[test]
fn warp_phased_projection_matches_fixed_session_and_event_oracle() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("warp.sqlite");
    let conn = Connection::open(&path).unwrap();
    create_warp_schema(&conn);
    insert_conversation(&conn, "conversation-legacy", "2026-07-18 12:00:00", None);
    insert_task(
        &conn,
        "conversation-legacy",
        "task-legacy",
        &warp_task_bytes("task-legacy", "legacy oracle"),
    );
    drop(conn);

    let context = test_context(&path);
    let conn = open_provider_sqlite_readonly(&path).unwrap();
    let schema_fingerprint = sqlite_schema_fingerprint(&conn).unwrap();
    let mut projector = WarpCapturedBatchProjector {
        context: context.clone(),
        raw_source_path: path.display().to_string(),
        user_version: 0,
        schema_fingerprint: schema_fingerprint.clone(),
        checkpoint: WarpParserCheckpoint::default(),
    };
    let batches = collect_batches(
        &conn,
        test_source("legacy-equivalence"),
        initial_warp_position().unwrap(),
    );
    let mut output = CollectingProjectionOutput::default();
    for record in batches.iter().flat_map(|batch| batch.records()) {
        projector.project_record(record, &mut output).unwrap();
    }
    assert!(output.rejections.is_empty());
    let captures = output
        .normalizations
        .iter()
        .flat_map(|normalization| normalization.captures.iter().cloned())
        .collect::<Vec<_>>();
    assert_eq!(captures.len(), 2);
    assert_eq!(captures[0].0, 1);
    assert_eq!(captures[1].0, 2);

    let expected_session_metadata = json!({
        "source_format": "warp_sqlite",
        "title": "Warp conversation-legacy",
        "agent_name": "Warp conversation-legacy",
        "parent_conversation_id": null,
        "run_id": "run-conversation-legacy",
        "server_conversation_token_present": false,
        "forked_from_server_conversation_token_present": false,
        "conversation_usage_metadata": null,
        "task_summaries": [],
    });
    let session_capture = &captures[0].1;
    assert_eq!(
        session_capture.session.provider_session_id,
        "conversation-legacy"
    );
    assert!(session_capture.event.is_none());
    assert_eq!(session_capture.session.metadata, expected_session_metadata);
    assert_eq!(
        session_capture.session.started_at,
        "2026-07-18T12:00:00Z".parse::<DateTime<Utc>>().unwrap()
    );
    assert_eq!(
        session_capture.session.ended_at,
        Some("2026-07-18T12:00:00Z".parse::<DateTime<Utc>>().unwrap(),)
    );

    let event_capture = &captures[1].1;
    assert_eq!(
        event_capture.session.provider_session_id,
        "conversation-legacy"
    );
    assert_eq!(event_capture.session.metadata, Value::Null);
    let expected_identity =
        warp_message_identity_index("conversation-legacy", "task-legacy", "message-task-legacy");
    let expected_event = ProviderEventEnvelope {
        provider_event_index: 0,
        provider_event_hash: Some("message-task-legacy".to_owned()),
        cursor: Some("agent_task:task-legacy:message:0".to_owned()),
        event_type: EventType::Message,
        role: Some(EventRole::User),
        occurred_at: "2026-07-18T12:00:01Z".parse().unwrap(),
        fidelity: Fidelity::Imported,
        idempotency_key: Some(
            "provider-event:warp:conversation-legacy:message-task-legacy".to_owned(),
        ),
        artifacts: Vec::new(),
        payload: json!({
            "kind": "user_query",
            "message_id": "message-task-legacy",
            "task_id": "task-legacy",
            "request_id": null,
            "text": "legacy oracle",
            "text_retention": {
                "mode": "bounded",
                "limit_chars": 16_000,
                "truncated": false,
                "omission_policy": "none",
                "omission_applied": false,
            },
            "result_evidence": null,
            "result_outcome": null,
            "body": {
                "text": "legacy oracle",
                "message_index": 0,
            },
        }),
        metadata: json!({
            "source": "warp_sqlite",
            "source_format": "warp_sqlite",
            "message_kind": "user_query",
            "task_id": "task-legacy",
            "proto_task_id": null,
            "request_id": null,
            "provider_event_identity_index": expected_identity,
        }),
    };
    assert_eq!(event_capture.event.as_ref(), Some(&expected_event));

    let mut store = Store::open(directory.path().join("store.sqlite")).unwrap();
    let summary = import_warp_sqlite_batched(
        &path,
        &mut store,
        context,
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(summary.failed, 0);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 1);
    let sessions = store.list_sessions().unwrap();
    assert_eq!(sessions.len(), 1);
    let session = &sessions[0];
    assert_eq!(
        session.external_session_id.as_deref(),
        Some("conversation-legacy")
    );
    assert_eq!(session.sync.metadata["metadata"], expected_session_metadata);
    let stored_events = store.events_for_session(session.id).unwrap();
    assert_eq!(stored_events.len(), 1);
    assert_eq!(stored_events[0].payload["body"]["text"], "legacy oracle");
    assert_eq!(
        stored_events[0].sync.metadata["metadata"]["provider_event_identity_index"],
        expected_identity
    );
    assert_eq!(
        stored_events[0].sync.metadata["provider_event_hash"],
        "message-task-legacy"
    );
}
