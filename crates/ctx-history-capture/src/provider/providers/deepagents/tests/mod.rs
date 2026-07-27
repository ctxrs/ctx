use std::{cell::Cell, collections::BTreeSet, io::Write};

use ctx_history_core::{EventRole, EventType};
use rmpv::{encode::write_value as write_msgpack_value, Value as MsgpackValue};
use rusqlite::{limits::Limit, Connection};
use serde_json::json;

use super::*;
use crate::captured_batch::{
    CapturedBatch, CapturedRecord, CapturedRecordPayload, CapturedSqliteValue, NativePosition,
    StructuralRejectionKind, CAPTURE_BATCH_MAX_PAYLOAD_BYTES, CAPTURE_BATCH_MAX_RECORDS,
};
use crate::common::time::parse_rfc3339_utc;
use crate::complete_content::{
    VerifiedContentLocatorsV1, VerifiedContentRole, VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
};
use crate::provider::importer::{
    CapturedBatchCursorFinish, CapturedBatchProjector, ProviderProjectionOutput,
    ProviderProjectionResult,
};
use crate::provider::normalization::provider_line_from_index;
use crate::ProviderNormalizationResult;

use super::cursor::{deepagents_cursor_candidate, DeepAgentsPositionKey};
use super::ledger::{
    deepagents_message_identity, DeepAgentsMessageDedupeKey, DeepAgentsMessageLedger,
    DeepAgentsWritePlan,
};
use super::message::DeepAgentsMessage;
use super::record::decode_deepagents_write_values;
use super::source::deepagents_thread_summary;

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

struct CursorOnlyProjector;

impl CapturedBatchProjector for CursorOnlyProjector {
    fn project_record(
        &mut self,
        _record: &CapturedRecord,
        _output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        Ok(())
    }

    fn initial_cursor_candidate(
        &self,
        source: &SourceObservation,
        position: &NativePosition,
    ) -> Result<CertifiedProviderCursor> {
        deepagents_cursor_candidate(source, position)
    }

    fn finish_cursor(&self, batch: &CapturedBatch) -> Result<CapturedBatchCursorFinish> {
        Ok(CapturedBatchCursorFinish::Advance(
            deepagents_cursor_candidate(batch.source(), batch.range_end())?,
        ))
    }
}

fn context(path: Option<PathBuf>) -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: "deepagents-batch-test".to_owned(),
        source_path: path,
        source_root: None,
        imported_at: parse_rfc3339_utc("2026-07-04T19:30:00Z").unwrap(),
    }
}

fn create_tables(conn: &Connection) {
    conn.execute_batch(
        "create table checkpoints (
            thread_id text not null,
            checkpoint_ns text not null default '',
            checkpoint_id text not null,
            parent_checkpoint_id text,
            type text,
            checkpoint blob,
            metadata blob,
            primary key (thread_id, checkpoint_ns, checkpoint_id)
        );
        create table writes (
            thread_id text not null,
            checkpoint_ns text not null default '',
            checkpoint_id text not null,
            task_id text not null,
            idx integer not null,
            channel text not null,
            type text,
            value blob,
            primary key (thread_id, checkpoint_ns, checkpoint_id, task_id, idx)
        );",
    )
    .unwrap();
}

fn insert_checkpoint(conn: &Connection, thread_id: &str, checkpoint_id: &str) {
    let metadata = serde_json::to_vec(&json!({
        "updated_at": "2026-07-04T19:30:00Z",
        "agent_name": "deepagents-test-agent",
        "git_branch": "codex/deepagents-test",
        "cwd": "/workspace/deepagents-test",
    }))
    .unwrap();
    insert_checkpoint_with_metadata(conn, thread_id, checkpoint_id, &metadata);
}

fn insert_checkpoint_with_metadata(
    conn: &Connection,
    thread_id: &str,
    checkpoint_id: &str,
    metadata: &[u8],
) {
    conn.execute(
        "insert into checkpoints
         (thread_id, checkpoint_ns, checkpoint_id, checkpoint, metadata)
         values (?1, '', ?2, x'00', ?3)",
        rusqlite::params![thread_id, checkpoint_id, metadata],
    )
    .unwrap();
}

fn insert_oversized_checkpoint_metadata(conn: &Connection, thread_id: &str, checkpoint_id: &str) {
    let oversized = i64::try_from(CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES)
        .unwrap()
        .checked_add(1)
        .unwrap();
    conn.execute(
        "insert into checkpoints
         (thread_id, checkpoint_ns, checkpoint_id, checkpoint, metadata)
         values (?1, '', ?2, x'00', zeroblob(?3))",
        rusqlite::params![thread_id, checkpoint_id, oversized],
    )
    .unwrap();
}

fn message_value(role: &str, text: &str, message_id: &str) -> MsgpackValue {
    MsgpackValue::Map(vec![
        (
            MsgpackValue::String("type".into()),
            MsgpackValue::String(role.into()),
        ),
        (
            MsgpackValue::String("content".into()),
            MsgpackValue::String(text.into()),
        ),
        (
            MsgpackValue::String("id".into()),
            MsgpackValue::String(message_id.into()),
        ),
    ])
}

fn message_blob(messages: Vec<MsgpackValue>) -> Vec<u8> {
    let mut bytes = Vec::new();
    write_msgpack_value(&mut bytes, &MsgpackValue::Array(messages)).unwrap();
    bytes
}

fn insert_write(
    conn: &Connection,
    thread_id: &str,
    checkpoint_id: &str,
    task_id: &str,
    idx: i64,
    value: &[u8],
) {
    conn.execute(
        "insert into writes
         (thread_id, checkpoint_ns, checkpoint_id, task_id, idx, channel, type, value)
         values (?1, '', ?2, ?3, ?4, 'messages', 'msgpack', ?5)",
        rusqlite::params![thread_id, checkpoint_id, task_id, idx, value],
    )
    .unwrap();
}

fn test_source(identity: &str) -> SourceObservation {
    SourceObservation::new(
        CaptureProvider::DeepAgents,
        DEEPAGENTS_SQLITE_SOURCE_FORMAT,
        format!("deepagents-sqlite:{identity}"),
        format!("deepagents-snapshot:{identity}"),
        format!("provider:deepagents:{identity}"),
        DEEPAGENTS_CAPTURE_REVISION,
        DEEPAGENTS_POLICY_REVISION,
        None,
    )
    .unwrap()
}

fn produce_all(
    conn: &Connection,
    source: SourceObservation,
    start: NativePosition,
    context: ProviderAdapterContext,
) -> Vec<CapturedBatch> {
    let mut fetcher = DeepAgentsRowFetcher::new(conn, context, None).unwrap();
    let mut producer =
        SqliteLogicalRowBatchProducer::new(source, start, move |position| fetcher.fetch(position));
    let mut batches = Vec::new();
    loop {
        let batch = with_sqlite_read_snapshot(conn, || {
            producer.next_batch().map_err(deepagents_sqlite_batch_error)
        })
        .unwrap();
        let Some(batch) = batch else {
            break;
        };
        assert!(conn.is_autocommit());
        batches.push(batch);
    }
    batches
}

fn cumulative_plan(
    rows: &[Vec<DeepAgentsMessage>],
    reset_before_rows: &BTreeSet<usize>,
) -> (Vec<(usize, u32, u64)>, u64) {
    let mut ledger = DeepAgentsMessageLedger::new(None, None);
    let mut next_event_index = 1;
    let mut accepted = Vec::new();
    for (row_index, messages) in rows.iter().enumerate() {
        if reset_before_rows.contains(&row_index) {
            ledger.reset_for_batch_request();
        }
        ledger.begin_row();
        let DeepAgentsWritePlan::Accepted {
            next_event_index: next,
            accepted_offsets,
            accepted_event_indices,
        } = ledger
            .plan_messages("thread-a", messages, 0, next_event_index)
            .unwrap()
        else {
            panic!("bounded cumulative row should be accepted");
        };
        accepted.extend(
            accepted_offsets
                .into_iter()
                .zip(accepted_event_indices)
                .map(|(offset, event_index)| (row_index, offset, event_index)),
        );
        next_event_index = next;
    }
    (accepted, next_event_index)
}

fn insert_large_cumulative_writes(conn: &Connection) {
    insert_checkpoint(conn, "thread-a", "checkpoint-a");
    let large_text = "x".repeat(CAPTURE_BATCH_MAX_PAYLOAD_BYTES / 2 + 64 * 1024);
    insert_write(
        conn,
        "thread-a",
        "checkpoint-a",
        "task-a",
        0,
        &message_blob(vec![message_value("human", &large_text, "A")]),
    );
    insert_write(
        conn,
        "thread-a",
        "checkpoint-a",
        "task-a",
        1,
        &message_blob(vec![
            message_value("human", &large_text, "A"),
            message_value("ai", "B", "B"),
        ]),
    );
    insert_write(
        conn,
        "thread-a",
        "checkpoint-a",
        "task-a",
        2,
        &message_blob(vec![
            message_value("human", &large_text, "A"),
            message_value("ai", "B", "B"),
            message_value("human", "C", "C"),
        ]),
    );
}

mod ledger;
mod lifecycle;
mod producer;
mod projector;
mod source;
