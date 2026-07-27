use std::convert::Infallible;
use std::fs;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use ctx_history_core::{CaptureProvider, ProviderCaptureEnvelope};
use rusqlite::{params, Connection};
use serde_json::json;

use super::capture::{
    decode_opencode_position, encode_opencode_position, initial_opencode_position,
    opencode_parent_ordinal, opencode_sqlite_batch_error, validate_opencode_resume_position,
    OpenCodeKeyset, OpenCodePositionPhase, OpenCodeRowFetcher, OPENCODE_END_RECORD_KIND,
    OPENCODE_MESSAGE_PART_RECORD_KIND, OPENCODE_RECORD_KIND, OPENCODE_SESSION_PARENT_RECORD_KIND,
};
use super::normalization::{
    opencode_event, opencode_message_part_role, opencode_patch_file_touch_drafts,
    opencode_tool_part_event_data, OPENCODE_MESSAGE_PART_DEFAULT_ROLE,
};
use super::projection::{
    opencode_integer_value, opencode_text_value, OpenCodeCapturedBatchProjector,
    OpenCodeProjectionSource,
};
use super::schema::{
    opencode_captured_shape, opencode_session_candidate_sql, opencode_session_id_lookup_index,
    OpenCodeCapturedShape, OpenCodeMessageRow, OpenCodeRowSql, OpenCodeRowidSeek,
    OpenCodeSessionSql, OPENCODE_SESSION_PARENT_OVERHEAD_BYTES,
};
use super::{
    opencode_source_snapshot, OPENCODE_CAPTURE_REVISION, OPENCODE_POLICY_REVISION,
    OPENCODE_SQLITE_DIALECT,
};
use crate::captured_batch::sqlite_logical_rows::SqliteLogicalRowBatchProducer;
use crate::captured_batch::{
    CapturedRecordPayload, ProviderRecordKind, SourceObservation, CAPTURE_BATCH_MAX_RECORDS,
};
use crate::provider::file_touches::{ProviderFileTouchEnvelopeContext, ProviderFileTouchVisitor};
use crate::provider::importer::{
    BoundedParserCheckpoint, CapturedBatchCursorFinish, CapturedBatchProjector,
    CertifiedProviderCursor, ExistingSessionEventOutcome, ProviderProjectionOutput,
    ProviderProjectionResult,
};
use crate::provider::normalization::provider_line_from_index;
use crate::provider::sqlite::{
    open_provider_sqlite_readonly, sqlite_ident, with_sqlite_read_snapshot,
};
use crate::{ProviderAdapterContext, ProviderNormalizationResult, OPENCODE_SQLITE_SOURCE_FORMAT};

#[derive(Default)]
struct CollectingProjectionOutput {
    normalizations: Vec<ProviderNormalizationResult>,
    rejections: Vec<(usize, String)>,
    reject_existing_events: bool,
    existing_event_calls: usize,
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

    fn emit_existing_session_event(
        &mut self,
        line_number: usize,
        capture: ProviderCaptureEnvelope,
    ) -> ProviderProjectionResult<ExistingSessionEventOutcome> {
        self.existing_event_calls += 1;
        if self.reject_existing_events {
            return Ok(ExistingSessionEventOutcome::Rejected);
        }
        self.emit_normalization(ProviderNormalizationResult {
            captures: vec![(line_number, capture)],
            ..ProviderNormalizationResult::default()
        })?;
        Ok(ExistingSessionEventOutcome::Accepted)
    }
}

fn test_context() -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: "opencode-batch-test-machine".to_owned(),
        source_path: Some(PathBuf::from("/tmp/opencode-test.db")),
        source_root: None,
        imported_at: "2026-07-18T12:00:00Z".parse().unwrap(),
    }
}

#[test]
fn real_tool_part_materializes_bounded_result_evidence_and_outcome() {
    let raw = json!({
        "tool": "bash",
        "tool_call_id": "tool-1",
        "state": {
            "status": "completed",
            "metadata": {"exit": 0},
            "output": "[main 0123456789ab] bounded commit",
        },
    });
    let data = opencode_tool_part_event_data("message-1", "part-1", "tool", 1, &raw)
        .expect("tool part must produce an event");
    let row = OpenCodeMessageRow {
        id: "message-1".to_owned(),
        session_id: "session-1".to_owned(),
        entry_type: "tool_result".to_owned(),
        seq: 1,
        time_created: 1,
        time_updated: 1,
    };
    let event = opencode_event(
        &row,
        &data,
        "2026-07-21T12:00:00Z".parse().unwrap(),
        1,
        &OPENCODE_SQLITE_DIALECT,
    );
    assert_eq!(event.payload["result_outcome"], "success");
    assert_eq!(
        event.payload["result_evidence"],
        json!([
            {"kind": "call_id", "value": "tool-1"},
            {"kind": "git_commit_summary_id", "value": "0123456789ab"},
        ])
    );
    assert!(!event.payload.to_string().contains("bounded commit"));
}

fn create_session_message_schema(conn: &Connection) {
    conn.execute_batch(
        "create table session (
                id text primary key,
                parent_id text,
                title text,
                directory text,
                time_created integer,
                time_updated integer
            );
            create table session_message (
                id text primary key,
                session_id text not null,
                type text,
                seq integer,
                time_created integer,
                time_updated integer,
                data text not null
            );",
    )
    .unwrap();
    conn.execute(
        "insert into session values (?1, null, ?2, ?3, ?4, ?4)",
        params![
            "session-1",
            "Session",
            "/tmp/project",
            1_700_000_000_000_i64
        ],
    )
    .unwrap();
}

fn test_source() -> SourceObservation {
    SourceObservation::new(
        CaptureProvider::OpenCode,
        OPENCODE_SQLITE_SOURCE_FORMAT,
        "opencode-sqlite:test",
        "snapshot:test",
        "provider:opencode:test",
        OPENCODE_CAPTURE_REVISION,
        OPENCODE_POLICY_REVISION,
        None,
    )
    .unwrap()
}

fn create_message_part_schema(conn: &Connection) {
    conn.execute_batch(
        "create table session (
                id text primary key,
                parent_id text,
                title text,
                directory text,
                time_created integer,
                time_updated integer
            );
            create table message (
                id text primary key,
                session_id text not null,
                time_created integer not null,
                time_updated integer not null,
                data text not null
            );
            create table part (
                id text primary key,
                message_id text not null,
                session_id text not null,
                time_created integer not null,
                time_updated integer not null,
                data text not null,
                type text
            );
            insert into session values (
                'session-1', null, 'Session', '/tmp/project',
                1700000000000, 1700000000000
            );
            insert into message values (
                'message-1', 'session-1', 1700000000000, 1700000000000,
                '{\"role\":\"assistant\"}'
            );",
    )
    .unwrap();
}

#[test]
fn structural_selection_prefers_session_message_without_content_preflight() {
    let conn = Connection::open_in_memory().unwrap();
    create_session_message_schema(&conn);
    conn.execute_batch(
        "create table message (
                id text primary key,
                session_id text not null,
                time_created integer not null,
                time_updated integer not null,
                data text not null
            );
            insert into message values (
                'legacy-1', 'session-1', 1700000000000, 1700000000000,
                '{\"role\":\"user\",\"text\":\"legacy\"}'
            );",
    )
    .unwrap();

    assert_eq!(
        opencode_captured_shape(&conn, &OPENCODE_SQLITE_DIALECT).unwrap(),
        OpenCodeCapturedShape::SessionMessage
    );
}

#[test]
fn logical_rows_split_at_sixty_four_and_release_each_read_snapshot() {
    let conn = Connection::open_in_memory().unwrap();
    create_session_message_schema(&conn);
    for index in 0..65_i64 {
        conn.execute(
            "insert into session_message values (?1, 'session-1', 'user', ?2, ?3, ?3, ?4)",
            params![
                format!("message-{index:03}"),
                index,
                1_700_000_000_000_i64 + index,
                json!({"role":"user","text":format!("message {index}")}).to_string(),
            ],
        )
        .unwrap();
    }
    let shape = opencode_captured_shape(&conn, &OPENCODE_SQLITE_DIALECT).unwrap();
    let mut fetcher = OpenCodeRowFetcher::new(
        &conn,
        shape,
        ProviderRecordKind::new(OPENCODE_RECORD_KIND).unwrap(),
    )
    .unwrap();
    let mut producer = SqliteLogicalRowBatchProducer::new(
        test_source(),
        initial_opencode_position(shape).unwrap(),
        move |position| fetcher.fetch(position),
    );

    let first = with_sqlite_read_snapshot(&conn, || {
        producer.next_batch().map_err(opencode_sqlite_batch_error)
    })
    .unwrap()
    .unwrap();
    assert!(conn.is_autocommit());
    assert_eq!(first.records().len(), CAPTURE_BATCH_MAX_RECORDS);

    let second = with_sqlite_read_snapshot(&conn, || {
        producer.next_batch().map_err(opencode_sqlite_batch_error)
    })
    .unwrap()
    .unwrap();
    assert!(conn.is_autocommit());
    assert_eq!(second.records().len(), 3);

    let exhausted = with_sqlite_read_snapshot(&conn, || {
        producer.next_batch().map_err(opencode_sqlite_batch_error)
    })
    .unwrap();
    assert!(conn.is_autocommit());
    assert!(exhausted.is_none());
}

fn assert_child_phase_restart_parent_lookup(session_rowid: i64) {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let database_path = directory.path().join("opencode.sqlite");
    {
        let conn = Connection::open(&database_path).unwrap();
        create_session_message_schema(&conn);
        conn.execute(
            "update session set rowid = ?1 where id = 'session-1'",
            [session_rowid],
        )
        .unwrap();
        conn.execute(
            "insert into session values (?1, null, ?2, ?3, ?4, ?4)",
            params![
                "session-2",
                "Rejected session",
                "/tmp/rejected",
                1_700_000_000_001_i64,
            ],
        )
        .unwrap();
        for index in 0..65_i64 {
            let (session_id, data) = if index == 62 {
                ("session-2", "{malformed".to_owned())
            } else {
                (
                    "session-1",
                    json!({"role":"user","text":format!("message {index}")}).to_string(),
                )
            };
            conn.execute(
                "insert into session_message values (?1, ?2, 'user', ?3, ?4, ?4, ?5)",
                params![
                    format!("message-{index:03}"),
                    session_id,
                    index,
                    1_700_000_000_000_i64 + index,
                    data,
                ],
            )
            .unwrap();
        }
    }

    let snapshot = opencode_source_snapshot(&database_path).unwrap();
    let shape = OpenCodeCapturedShape::SessionMessage;
    let first_conn = open_provider_sqlite_readonly(&database_path).unwrap();
    let mut first_fetcher = OpenCodeRowFetcher::new(
        &first_conn,
        shape,
        ProviderRecordKind::new(OPENCODE_RECORD_KIND).unwrap(),
    )
    .unwrap();
    let mut first_producer = SqliteLogicalRowBatchProducer::new(
        test_source(),
        initial_opencode_position(shape).unwrap(),
        move |position| first_fetcher.fetch(position),
    );
    let first_batch = first_producer.next_batch().unwrap().unwrap();
    assert_eq!(first_batch.records().len(), CAPTURE_BATCH_MAX_RECORDS);
    let first_end = decode_opencode_position(first_batch.range_end(), shape).unwrap();
    assert_eq!(first_end.phase, OpenCodePositionPhase::Child);
    assert_eq!(first_end.rowid, 62);
    let mut first_projector = OpenCodeCapturedBatchProjector::new(
        test_context(),
        OpenCodeProjectionSource {
            database_path: database_path.clone(),
            conn: &first_conn,
            snapshot: snapshot.clone(),
        },
        &OPENCODE_SQLITE_DIALECT,
        0,
        "test-schema".to_owned(),
        shape,
    )
    .unwrap();
    let mut first_output = CollectingProjectionOutput::default();
    for record in first_batch.records() {
        first_projector
            .project_record(record, &mut first_output)
            .unwrap();
    }
    let first_parent_line = first_output
        .normalizations
        .iter()
        .flat_map(|normalization| normalization.captures.iter())
        .find_map(|(line_number, capture)| capture.event.is_none().then_some(*line_number))
        .expect("initial batch must emit the referenced parent");
    let CapturedBatchCursorFinish::Advance(first_cursor) =
        first_projector.finish_cursor(&first_batch).unwrap()
    else {
        panic!("OpenCode child frontier must be publishable");
    };
    assert_eq!(first_cursor.native_position(), first_batch.range_end());
    drop(first_producer);
    drop(first_projector);
    drop(first_conn);

    let resumed_conn = open_provider_sqlite_readonly(&database_path).unwrap();
    let mut resumed_fetcher = OpenCodeRowFetcher::new(
        &resumed_conn,
        shape,
        ProviderRecordKind::new(OPENCODE_RECORD_KIND).unwrap(),
    )
    .unwrap();
    let mut resumed_producer = SqliteLogicalRowBatchProducer::new(
        test_source(),
        first_cursor.native_position().clone(),
        move |position| resumed_fetcher.fetch(position),
    );
    let resumed_batch = resumed_producer.next_batch().unwrap().unwrap();
    let resumed_message_ids = resumed_batch
        .records()
        .iter()
        .filter(|record| record.record_kind().as_str() == OPENCODE_RECORD_KIND)
        .map(|record| match record.payload() {
            CapturedRecordPayload::SqliteValues(values) => {
                opencode_text_value(values, 2).unwrap().to_owned()
            }
            payload => panic!("unexpected resumed payload: {payload:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        resumed_message_ids,
        ["message-062", "message-063", "message-064"]
    );

    let mut resumed_projector = OpenCodeCapturedBatchProjector::new(
        test_context(),
        OpenCodeProjectionSource {
            database_path: database_path.clone(),
            conn: &resumed_conn,
            snapshot,
        },
        &OPENCODE_SQLITE_DIALECT,
        0,
        "test-schema".to_owned(),
        shape,
    )
    .unwrap();
    let mut resumed_output = CollectingProjectionOutput::default();
    for record in resumed_batch.records() {
        resumed_projector
            .project_record(record, &mut resumed_output)
            .unwrap();
    }
    assert_eq!(resumed_output.rejections.len(), 1);
    let resumed_captures = resumed_output
        .normalizations
        .iter()
        .flat_map(|normalization| normalization.captures.iter())
        .map(|(_, capture)| capture)
        .collect::<Vec<_>>();
    assert_eq!(resumed_captures.len(), 3);
    assert!(resumed_captures
        .iter()
        .all(|capture| capture.session.provider_session_id == "session-1"));
    assert_eq!(
        resumed_captures
            .iter()
            .filter(|capture| capture.event.is_none())
            .count(),
        1,
        "restart lookup must cache and emit one logical parent"
    );
    let resumed_parent_line = resumed_output
        .normalizations
        .iter()
        .flat_map(|normalization| normalization.captures.iter())
        .find_map(|(line_number, capture)| capture.event.is_none().then_some(*line_number))
        .expect("resumed batch must emit the referenced parent");
    assert_eq!(resumed_parent_line, first_parent_line);
    assert_eq!(
        resumed_parent_line,
        provider_line_from_index(opencode_parent_ordinal(session_rowid))
    );
    assert!(!resumed_captures
        .iter()
        .any(|capture| capture.session.provider_session_id == "session-2"));
    let CapturedBatchCursorFinish::Advance(resumed_cursor) =
        resumed_projector.finish_cursor(&resumed_batch).unwrap()
    else {
        panic!("OpenCode terminal frontier must be publishable");
    };
    assert_eq!(resumed_cursor.native_position(), resumed_batch.range_end());
}

#[test]
fn child_phase_restart_uses_indexed_parent_lookup_without_replaying_prefix() {
    assert_child_phase_restart_parent_lookup(1);
}

#[test]
fn child_phase_restart_preserves_sparse_parent_provenance() {
    assert_child_phase_restart_parent_lookup(5);
}

#[test]
fn child_phase_restart_preserves_negative_parent_provenance() {
    assert_child_phase_restart_parent_lookup(-5);
}

#[test]
fn initial_keyset_includes_minimum_rowid() {
    let conn = Connection::open_in_memory().unwrap();
    create_session_message_schema(&conn);
    conn.execute(
        "insert into session_message (
                rowid, id, session_id, type, seq, time_created, time_updated, data
             ) values (?1, 'minimum', 'session-1', 'user', 0, 1700000000000,
                       1700000000000, '{\"role\":\"user\",\"text\":\"minimum\"}')",
        [i64::MIN],
    )
    .unwrap();
    let shape = OpenCodeCapturedShape::SessionMessage;
    let mut fetcher = OpenCodeRowFetcher::new(
        &conn,
        shape,
        ProviderRecordKind::new(OPENCODE_RECORD_KIND).unwrap(),
    )
    .unwrap();
    let parent = fetcher
        .fetch(initial_opencode_position(shape).unwrap())
        .unwrap()
        .unwrap();
    let row = fetcher
        .fetch(parent.next_position().clone())
        .unwrap()
        .unwrap();
    let keyset = decode_opencode_position(row.next_position(), shape).unwrap();

    assert!(keyset.has_after);
    assert_eq!(keyset.rowid, i64::MIN);
    assert_eq!(keyset.phase, OpenCodePositionPhase::Child);
}

#[test]
fn message_parts_emit_one_session_parent_and_child_local_rows() {
    let conn = Connection::open_in_memory().unwrap();
    create_message_part_schema(&conn);
    conn.execute(
        "insert into message values (
                'message-2', 'session-1', 1700000000001, 1700000000001,
                '{\"role\":\"user\"}'
            )",
        [],
    )
    .unwrap();
    for index in 0..65_i64 {
        conn.execute(
            "insert into part values (?1, ?2, 'session-1', ?3, ?3, ?4, 'text')",
            params![
                format!("part-{index:03}"),
                if index % 2 == 0 {
                    "message-1"
                } else {
                    "message-2"
                },
                1_700_000_000_000_i64 + index,
                json!({"type":"text","text":format!("part {index}")}).to_string(),
            ],
        )
        .unwrap();
    }
    let shape = OpenCodeCapturedShape::MessagePart;
    let mut fetcher = OpenCodeRowFetcher::new(
        &conn,
        shape,
        ProviderRecordKind::new(OPENCODE_RECORD_KIND).unwrap(),
    )
    .unwrap();
    let mut position = initial_opencode_position(shape).unwrap();
    let mut kinds = Vec::new();
    while let Some(row) = fetcher.fetch(position).unwrap() {
        position = row.next_position().clone();
        kinds.push(format!("{row:?}"));
    }

    assert_eq!(
        kinds
            .iter()
            .filter(|row| row.contains(OPENCODE_SESSION_PARENT_RECORD_KIND))
            .count(),
        1
    );
    assert_eq!(
        kinds
            .iter()
            .filter(|row| row.contains(OPENCODE_MESSAGE_PART_RECORD_KIND))
            .count(),
        65
    );
    assert_eq!(
        kinds
            .iter()
            .filter(|row| row.contains(OPENCODE_END_RECORD_KIND))
            .count(),
        1
    );
    assert_eq!(kinds.len(), 67);
}

#[test]
fn flat_shapes_emit_each_parent_once_and_keep_alternating_children_local() {
    let conn = Connection::open_in_memory().unwrap();
    create_session_message_schema(&conn);
    conn.execute(
        "update session set title = 'parent-one-metadata', directory = '/parent/one' \
             where id = 'session-1'",
        [],
    )
    .unwrap();
    conn.execute(
        "insert into session values (
                'session-2', null, 'parent-two-metadata', '/parent/two',
                1700000000001, 1700000000001
            )",
        [],
    )
    .unwrap();
    for index in 0..130_i64 {
        conn.execute(
            "insert into session_message values (?1, ?2, 'user', ?3, ?4, ?4, ?5)",
            params![
                format!("message-{index:03}"),
                if index % 2 == 0 {
                    "session-1"
                } else {
                    "session-2"
                },
                index,
                1_700_000_000_000_i64 + index,
                json!({"role":"user","text":format!("message {index}")}).to_string(),
            ],
        )
        .unwrap();
    }
    let shape = OpenCodeCapturedShape::SessionMessage;
    let mut fetcher = OpenCodeRowFetcher::new(
        &conn,
        shape,
        ProviderRecordKind::new(OPENCODE_RECORD_KIND).unwrap(),
    )
    .unwrap();
    let mut position = initial_opencode_position(shape).unwrap();
    let mut parents = Vec::new();
    let mut children = Vec::new();
    while let Some(row) = fetcher.fetch(position).unwrap() {
        position = row.next_position().clone();
        let debug = format!("{row:?}");
        if debug.contains(OPENCODE_SESSION_PARENT_RECORD_KIND) {
            parents.push(debug);
        } else if debug.contains(OPENCODE_RECORD_KIND) {
            children.push(debug);
        }
    }

    assert_eq!(parents.len(), 2);
    assert_eq!(children.len(), 130);
}

#[test]
fn native_rowid_resumes_are_indexed_and_near_tail_work_is_bounded() {
    let conn = Connection::open_in_memory().unwrap();
    create_message_part_schema(&conn);
    let transaction = conn.unchecked_transaction().unwrap();
    for index in 0..2_048_i64 {
        transaction
            .execute(
                "insert into part values (?1, 'message-1', 'session-1', ?2, ?2, ?3, 'text')",
                params![
                    format!("part-{index:04}"),
                    1_700_000_000_000_i64 + index,
                    json!({"type":"text","text":format!("part {index}")}).to_string(),
                ],
            )
            .unwrap();
    }
    transaction.commit().unwrap();

    let row_sql = OpenCodeRowSql::for_shape(&conn, OpenCodeCapturedShape::MessagePart).unwrap();
    for (seek, bound) in [
        (OpenCodeRowidSeek::First, i64::MIN),
        (OpenCodeRowidSeek::Next, 2_047_i64),
    ] {
        let child_plan = conn
            .prepare(&format!(
                "explain query plan {}",
                row_sql.candidate_sql(seek)
            ))
            .unwrap()
            .query_map([bound], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap()
            .join(" | ");
        assert!(
            child_plan.contains("SEARCH x USING INTEGER PRIMARY KEY"),
            "{child_plan}"
        );
        assert!(!child_plan.contains("USE TEMP B-TREE"), "{child_plan}");
    }

    let session = OpenCodeSessionSql::new(&conn).unwrap();
    let retained = [
        session.id.as_str(),
        session.parent_id.as_str(),
        session.title.as_str(),
        session.directory.as_str(),
        session.model.as_str(),
        session.agent.as_str(),
    ]
    .into_iter()
    .map(|expr| format!("coalesce(octet_length({expr}), 0)"))
    .collect::<Vec<_>>()
    .join(" + ");
    for (seek, bound) in [
        (OpenCodeRowidSeek::First, i64::MIN),
        (OpenCodeRowidSeek::Next, 0_i64),
    ] {
        let parent_plan = conn
            .prepare(&format!(
                "explain query plan {}",
                opencode_session_candidate_sql(&retained, seek)
            ))
            .unwrap()
            .query_map([bound], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap()
            .join(" | ");
        assert!(
            parent_plan.contains("SEARCH s USING INTEGER PRIMARY KEY"),
            "{parent_plan}"
        );
        assert!(!parent_plan.contains("USE TEMP B-TREE"), "{parent_plan}");
    }

    let session_index = opencode_session_id_lookup_index(&conn).unwrap();
    let lookup_plan = conn
        .prepare(&format!(
            "explain query plan select s.rowid, {OPENCODE_SESSION_PARENT_OVERHEAD_BYTES} + \
                        {retained} \
                 from session s indexed by {} \
                 where s.id collate binary = ?1 limit 1",
            sqlite_ident(&session_index),
        ))
        .unwrap()
        .query_map(["session-1"], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap()
        .join(" | ");
    assert!(
        lookup_plan.contains(&format!("USING INDEX {session_index}")),
        "{lookup_plan}"
    );
    assert!(!lookup_plan.contains("SCAN s"), "{lookup_plan}");

    let mut fetcher = OpenCodeRowFetcher::new(
        &conn,
        OpenCodeCapturedShape::MessagePart,
        ProviderRecordKind::new(OPENCODE_RECORD_KIND).unwrap(),
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
    let position = encode_opencode_position(OpenCodeKeyset {
        shape: OpenCodeCapturedShape::MessagePart,
        next_ordinal: 2_048,
        has_after: true,
        rowid: 2_047,
        phase: OpenCodePositionPhase::Child,
        next_part_ordinal: 2_047,
    })
    .unwrap();
    let tail = fetcher.fetch(position).unwrap().unwrap();
    assert_eq!(
        decode_opencode_position(tail.next_position(), OpenCodeCapturedShape::MessagePart)
            .unwrap()
            .rowid,
        2_048
    );
    assert!(operations.load(Ordering::Relaxed) < 2_000);
    conn.progress_handler(0, None::<fn() -> bool>);
}

#[test]
fn restart_parent_lookup_requires_unique_binary_session_id_index() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "create table session (id text, title text, directory text, time_created integer);
             create index session_id_nonunique on session(id);",
    )
    .unwrap();
    let missing = opencode_session_id_lookup_index(&conn).unwrap_err();
    assert!(missing.to_string().contains("UNIQUE BINARY index"));
    conn.execute_batch(
        "drop index session_id_nonunique;
             create unique index session_id_nocase on session(id collate nocase);",
    )
    .unwrap();
    let wrong_collation = opencode_session_id_lookup_index(&conn).unwrap_err();
    assert!(wrong_collation.to_string().contains("UNIQUE BINARY index"));
}

#[test]
fn message_part_session_mismatch_rejects_only_the_malformed_sibling() {
    let conn = Connection::open_in_memory().unwrap();
    create_message_part_schema(&conn);
    conn.execute(
        "insert into session values (
                'session-2', null, 'Session 2', '/tmp/project-2',
                1700000000001, 1700000000001
            )",
        [],
    )
    .unwrap();
    conn.execute(
        "insert into part values (
                'part-bad', 'message-1', 'session-2', 1700000000000, 1700000000000,
                '{\"type\":\"text\",\"text\":\"bad\"}', 'text'
            )",
        [],
    )
    .unwrap();
    conn.execute(
        "insert into part values (
                'part-good', 'message-1', 'session-1', 1700000000001, 1700000000001,
                '{\"type\":\"text\",\"text\":\"good\"}', 'text'
            )",
        [],
    )
    .unwrap();

    let shape = OpenCodeCapturedShape::MessagePart;
    let mut fetcher = OpenCodeRowFetcher::new(
        &conn,
        shape,
        ProviderRecordKind::new(OPENCODE_RECORD_KIND).unwrap(),
    )
    .unwrap();
    let mut producer = SqliteLogicalRowBatchProducer::new(
        test_source(),
        initial_opencode_position(shape).unwrap(),
        move |position| fetcher.fetch(position),
    );
    let batch = producer.next_batch().unwrap().unwrap();
    let child_parent_flags = batch
        .records()
        .iter()
        .filter(|record| record.record_kind().as_str() == OPENCODE_MESSAGE_PART_RECORD_KIND)
        .map(|record| match record.payload() {
            CapturedRecordPayload::SqliteValues(values) => {
                opencode_integer_value(values, 1).unwrap()
            }
            payload => panic!("unexpected child payload: {payload:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(child_parent_flags, vec![0, 1]);

    let snapshot_dir = crate::test_support_paths::tempdir().unwrap();
    let snapshot_path = snapshot_dir.path().join("opencode-test.db");
    fs::write(&snapshot_path, b"opencode-test-snapshot").unwrap();
    let snapshot = opencode_source_snapshot(&snapshot_path).unwrap();
    let mut projector = OpenCodeCapturedBatchProjector::new(
        test_context(),
        OpenCodeProjectionSource {
            database_path: snapshot_path,
            conn: &conn,
            snapshot,
        },
        &OPENCODE_SQLITE_DIALECT,
        0,
        "test-schema".to_owned(),
        shape,
    )
    .unwrap();
    let mut output = CollectingProjectionOutput::default();
    for record in batch.records() {
        projector.project_record(record, &mut output).unwrap();
    }
    assert_eq!(output.rejections.len(), 1);
    assert!(output.rejections[0].1.contains("mismatched parent session"));
    assert_eq!(
        output
            .normalizations
            .iter()
            .flat_map(|normalization| normalization.captures.iter())
            .count(),
        2
    );
}

#[test]
fn eventless_patch_emits_session_touch_without_invoking_event_seam() {
    let conn = Connection::open_in_memory().unwrap();
    create_message_part_schema(&conn);
    conn.execute(
        "insert into part values (
                'part-patch', 'message-1', 'session-1', 1700000000000, 1700000000000,
                '{\"type\":\"patch\",\"path\":\"src/rejected.rs\",\"text\":\"patch\"}',
                'patch'
            )",
        [],
    )
    .unwrap();
    let shape = OpenCodeCapturedShape::MessagePart;
    let mut fetcher = OpenCodeRowFetcher::new(
        &conn,
        shape,
        ProviderRecordKind::new(OPENCODE_RECORD_KIND).unwrap(),
    )
    .unwrap();
    let mut producer = SqliteLogicalRowBatchProducer::new(
        test_source(),
        initial_opencode_position(shape).unwrap(),
        move |position| fetcher.fetch(position),
    );
    let batch = producer.next_batch().unwrap().unwrap();
    let snapshot_dir = crate::test_support_paths::tempdir().unwrap();
    let snapshot_path = snapshot_dir.path().join("opencode-test.db");
    fs::write(&snapshot_path, b"opencode-test-snapshot").unwrap();
    let snapshot = opencode_source_snapshot(&snapshot_path).unwrap();
    let mut projector = OpenCodeCapturedBatchProjector::new(
        test_context(),
        OpenCodeProjectionSource {
            database_path: snapshot_path,
            conn: &conn,
            snapshot,
        },
        &OPENCODE_SQLITE_DIALECT,
        0,
        "test-schema".to_owned(),
        shape,
    )
    .unwrap();
    let mut output = CollectingProjectionOutput::default();
    for record in batch.records() {
        projector.project_record(record, &mut output).unwrap();
    }

    assert_eq!(output.existing_event_calls, 0);
    assert!(output
        .normalizations
        .iter()
        .any(|normalization| !normalization.files_touched.is_empty()));
}

#[test]
fn certified_position_round_trips_shape_rowid_and_ordinal() {
    let position = encode_opencode_position(OpenCodeKeyset {
        shape: OpenCodeCapturedShape::MessagePart,
        next_ordinal: 65,
        has_after: true,
        rowid: -17,
        phase: OpenCodePositionPhase::Child,
        next_part_ordinal: 63,
    })
    .unwrap();
    let certified = CertifiedProviderCursor::new(
        "snapshot:test",
        OPENCODE_CAPTURE_REVISION,
        OPENCODE_POLICY_REVISION,
        position,
        BoundedParserCheckpoint::from_serializable(&()).unwrap(),
    )
    .unwrap();
    let encoded = certified.encode().unwrap();
    let decoded = CertifiedProviderCursor::decode_if_certified(&encoded)
        .unwrap()
        .unwrap();
    let keyset = decode_opencode_position(
        decoded.native_position(),
        OpenCodeCapturedShape::MessagePart,
    )
    .unwrap();

    assert_eq!(keyset.next_ordinal, 65);
    assert_eq!(keyset.rowid, -17);
    assert!(keyset.has_after);
    assert_eq!(keyset.phase, OpenCodePositionPhase::Child);
    assert_eq!(keyset.next_part_ordinal, 63);
    assert_eq!(keyset.shape, OpenCodeCapturedShape::MessagePart);
}

#[test]
fn certified_message_part_parent_position_fails_closed() {
    let position = encode_opencode_position(OpenCodeKeyset {
        shape: OpenCodeCapturedShape::MessagePart,
        next_ordinal: 1,
        has_after: true,
        rowid: 7,
        phase: OpenCodePositionPhase::Parent,
        next_part_ordinal: 0,
    })
    .unwrap();

    let error = validate_opencode_resume_position(&position, OpenCodeCapturedShape::MessagePart)
        .unwrap_err();
    assert!(error.to_string().contains("transient parent group"));
}

#[test]
fn message_part_role_policy_is_part_local_and_fixed() {
    assert_eq!(
        opencode_message_part_role(&json!({"role": "user", "type": "text"})),
        "user"
    );
    assert_eq!(
        opencode_message_part_role(&json!({"role": "assistant", "type": "text"})),
        "assistant"
    );
    assert_eq!(
        opencode_message_part_role(&json!({"type": "text"})),
        OPENCODE_MESSAGE_PART_DEFAULT_ROLE
    );
    assert_eq!(OPENCODE_MESSAGE_PART_DEFAULT_ROLE, "assistant");
}

#[test]
fn source_snapshot_detects_database_change() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("opencode.db");
    let conn = Connection::open(&path).unwrap();
    create_session_message_schema(&conn);
    drop(conn);
    let original_length = fs::metadata(&path).unwrap().len();
    let snapshot = opencode_source_snapshot(&path).unwrap();
    assert!(snapshot.revalidate(&path).unwrap());

    let conn = Connection::open(&path).unwrap();
    conn.execute(
            "insert into session_message values ('changed', 'session-1', 'user', 1, 1700000000001, 1700000000001, ?1)",
            params![json!({"text": "x".repeat(64 * 1024)}).to_string()],
        )
        .unwrap();
    drop(conn);

    assert!(fs::metadata(&path).unwrap().len() > original_length);
    assert!(!snapshot.revalidate(&path).unwrap());
}

#[test]
fn raw_and_explicit_part_touches_share_order_dedup_and_identity() {
    let raw = json!({
        "patch": "*** Begin Patch\n*** Update File: src/a.rs\n@@\n-old\n+new\n*** End Patch",
    });
    let explicit = json!({
        "path": "src/a.rs",
        "files": ["src/b.rs", "src/a.rs"],
    });
    let mut touches = Vec::new();
    let mut visitor = ProviderFileTouchVisitor::new(
        ProviderFileTouchEnvelopeContext {
            provider: CaptureProvider::OpenCode,
            provider_session_id: "session-1",
            source_format: OPENCODE_SQLITE_SOURCE_FORMAT,
            raw_source_path: Some("/tmp/opencode.db"),
            source_root: Some("/tmp/opencode.db"),
            occurred_at: "2026-07-18T00:00:00Z".parse().unwrap(),
            provider_event_index: Some(7),
            provider_touch_base_index: 7_u64 << 16,
            line_number: 8,
        },
        |(line, touch)| {
            touches.push((line, touch));
            Ok::<(), Infallible>(())
        },
    );

    visitor.visit_raw_value(&raw, true).unwrap();
    visitor
        .visit_drafts(opencode_patch_file_touch_drafts(
            &explicit, "part-1", "patch",
        ))
        .unwrap();
    let outcome = visitor.finish();

    assert_eq!(outcome.emitted(), 2);
    assert!(!outcome.limit_exceeded());
    assert_eq!(touches[0].0, 8);
    assert_eq!(touches[0].1.path, "src/a.rs");
    assert_eq!(touches[0].1.provider_touch_index, 7_u64 << 16);
    assert_eq!(touches[0].1.metadata["source"], "apply_patch_update");
    assert_eq!(touches[1].0, 8);
    assert_eq!(touches[1].1.path, "src/b.rs");
    assert_eq!(touches[1].1.provider_touch_index, (7_u64 << 16) | 1);
    assert_eq!(
        touches[1].1.metadata["source"],
        "opencode_message_part_metadata"
    );
    assert_eq!(touches[1].1.metadata["part_id"], "part-1");
    assert_eq!(touches[1].1.metadata["part_type"], "patch");
}
