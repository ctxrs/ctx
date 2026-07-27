use std::{
    fs,
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, ProviderCaptureEnvelope};
use ctx_history_store::Store;
use rusqlite::{limits::Limit, Connection};
use serde_json::json;

use crate::captured_batch::sqlite_logical_rows::SqliteLogicalRowBatchProducer;
use crate::captured_batch::{
    CapturedBatch, CapturedRecord, CapturedRecordPayload, CapturedSqliteValue, NativePosition,
    ProviderRecordKind, SourceObservation, CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES,
    CAPTURE_BATCH_MAX_RECORDS,
};
use crate::provider::custom_history_jsonl::push_provider_import_failure;
use crate::provider::importer::{
    provider_path_identity, provider_source_cursor_stream_for_path, CapturedBatchProjector,
    CertifiedProviderCursor, ProviderProjectionOutput, ProviderProjectionResult,
};
use crate::provider::normalization::text_id_index;
use crate::provider::sqlite::{open_provider_sqlite_readonly, with_sqlite_read_snapshot};
use crate::{
    ProviderAdapterContext, ProviderNormalizationResult, MAX_PROVIDER_SQLITE_VALUE_BYTES,
    SHELLEY_SQLITE_SOURCE_FORMAT,
};

use super::{projector::*, relationships::*, row_stream::*, source::*, *};

fn create_shelley_tables(conn: &Connection) {
    conn.execute_batch(
        "create table conversations (
            conversation_id text primary key,
            slug text,
            user_initiated integer not null default 1,
            created_at text,
            updated_at text,
            cwd text,
            archived integer not null default 0,
            parent_conversation_id text,
            model text,
            conversation_options text,
            current_generation integer,
            agent_working integer not null default 0,
            tags text,
            is_draft integer not null default 0,
            draft text,
            queued_messages text
         );
         create table messages (
            message_id text primary key,
            conversation_id text not null,
            sequence_id integer not null,
            type text not null,
            llm_data text,
            user_data text,
            usage_data text,
            created_at text,
            display_data text,
            excluded_from_context integer not null default 0,
            generation integer,
            llm_api_url text,
            model_name text,
            forked_from_message_id text
         );
         create index idx_messages_conversation_sequence
             on messages(conversation_id, sequence_id);",
    )
    .unwrap();
}

fn create_pre_sequence_shelley_tables(conn: &Connection) {
    conn.execute_batch(
        "create table conversations (
            conversation_id text primary key,
            slug text,
            created_at text,
            updated_at text,
            cwd text
         );
         create table messages (
            message_id text primary key,
            conversation_id text not null,
            type text not null,
            user_data text,
            created_at text
         );
         create index idx_messages_conversation_id on messages(conversation_id);",
    )
    .unwrap();
}

fn insert_conversation(conn: &Connection, id: &str, created_at: &str) {
    conn.execute(
        "insert into conversations (
            conversation_id, slug, user_initiated, created_at, updated_at, cwd,
            conversation_options, current_generation, tags, queued_messages
         ) values (?1, ?2, 1, ?3, '2026-07-18T00:01:00Z', '/workspace/shelley',
                   '{\"mode\":\"agent\"}', 7, '[\"ctx\"]', '[]')",
        rusqlite::params![id, format!("Conversation {id}"), created_at],
    )
    .unwrap();
}

fn insert_message(
    conn: &Connection,
    message_id: &str,
    conversation_id: &str,
    sequence_id: i64,
    text: &str,
) {
    conn.execute(
        "insert into messages (
            message_id, conversation_id, sequence_id, type, user_data, usage_data,
            created_at, generation, model_name
         ) values (?1, ?2, ?3, 'user', ?4, '{\"input_tokens\":1}',
                   '2026-07-18T00:00:01Z', 7, 'shelley-test-model')",
        rusqlite::params![
            message_id,
            conversation_id,
            sequence_id,
            json!({"Text": text}).to_string(),
        ],
    )
    .unwrap();
}

fn insert_oversize_message(
    conn: &Connection,
    message_id: &str,
    conversation_id: &str,
    sequence_id: i64,
) {
    conn.execute(
        "insert into messages (
            message_id, conversation_id, sequence_id, type, llm_data, created_at
         ) values (?1, ?2, ?3, 'user', zeroblob(?4), '2026-07-18T00:00:01Z')",
        rusqlite::params![
            message_id,
            conversation_id,
            sequence_id,
            i64::try_from(CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES).unwrap(),
        ],
    )
    .unwrap();
}

fn test_source(revision: &str) -> SourceObservation {
    SourceObservation::new(
        CaptureProvider::Shelley,
        SHELLEY_SQLITE_SOURCE_FORMAT,
        "shelley-sqlite:test",
        revision,
        "provider:shelley:test",
        SHELLEY_CAPTURE_REVISION,
        SHELLEY_POLICY_REVISION,
        None,
    )
    .unwrap()
}

fn test_context(path: &Path) -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: "shelley-batch-test".to_owned(),
        source_path: Some(path.to_path_buf()),
        source_root: None,
        imported_at: DateTime::parse_from_rfc3339("2026-07-18T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
    }
}

fn produce_all(
    conn: &Connection,
    source: SourceObservation,
    start: NativePosition,
) -> Vec<CapturedBatch> {
    let terminal = decode_shelley_position(&start)
        .unwrap()
        .is_some_and(|keyset| keyset.exhausted);
    let mut fetcher = (!terminal)
        .then(|| ShelleyRowFetcher::new(conn))
        .transpose()
        .unwrap();
    let mut producer =
        SqliteLogicalRowBatchProducer::new(source, start, move |position| match fetcher.as_mut() {
            Some(fetcher) => fetcher.fetch(position),
            None => Ok(None),
        });
    let mut batches = Vec::new();
    loop {
        let batch = with_sqlite_read_snapshot(conn, || {
            producer.next_batch().map_err(shelley_sqlite_batch_error)
        })
        .unwrap_or_else(|error| {
            panic!(
                "Shelley batch {} failed from position {:?}: {error:?}",
                batches.len(),
                producer.current_position()
            )
        });
        let Some(batch) = batch else {
            break;
        };
        assert!(conn.is_autocommit());
        batches.push(batch);
    }
    batches
}

#[derive(Default)]
struct CollectingProjectionOutput {
    normalization: ProviderNormalizationResult,
}

impl ProviderProjectionOutput for CollectingProjectionOutput {
    fn emit_normalization(
        &mut self,
        mut normalization: ProviderNormalizationResult,
    ) -> ProviderProjectionResult<()> {
        self.normalization.summary.merge(normalization.summary);
        self.normalization
            .captures
            .append(&mut normalization.captures);
        self.normalization
            .files_touched
            .append(&mut normalization.files_touched);
        Ok(())
    }

    fn reject_record(&mut self, line_number: usize, reason: String) {
        push_provider_import_failure(&mut self.normalization.summary, line_number, reason);
    }
}

mod import;
mod projection;
mod row_stream;
