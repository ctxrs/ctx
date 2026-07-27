use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::{
    cell::Cell,
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, EntityTimestamps, EventRole, SyncCursor};
use ctx_history_store::Store;
use rusqlite::{limits::Limit, Connection};
use serde::Serialize;
use serde_json::{json, Value};

use super::codec::*;
use super::preferences::*;
use super::producer::*;
use super::projector::*;
use super::relationships::*;
use super::source::*;
use super::*;
use crate::captured_batch::sqlite_logical_rows::SqliteLogicalRowBatchProducer;
use crate::captured_batch::{
    CapturedBatch, CapturedRecordPayload, NativePosition, SourceObservation,
    StructuralRejectionKind, CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES,
    CAPTURE_BATCH_MAX_PARSER_CHECKPOINT_BYTES, CAPTURE_BATCH_MAX_RECORDS,
};
use crate::provider::custom_history_jsonl::push_provider_import_failure;
use crate::provider::importer::{
    captured_batch_cursor_stream, import_provider_capture_line, provider_path_identity,
    provider_source_cursor_stream_for_path, BoundedParserCheckpoint, CapturedBatchCursorFinish,
    CapturedBatchProjector, CertifiedProviderCursor, ProviderImportCaches,
    ProviderProjectionOutput, ProviderProjectionResult,
};
use crate::provider::normalization::provider_timestamp_millis;
use crate::provider::sqlite::{
    open_provider_sqlite_readonly, sqlite_schema_fingerprint, with_sqlite_read_snapshot,
};
use crate::{
    CaptureError, NormalizedProviderImportOptions, ProviderAdapterContext, ProviderImportSummary,
    ProviderNormalizationResult, ASTRBOT_SQLITE_SOURCE_FORMAT,
};

#[derive(Serialize)]
struct LegacyCheckpointFixture<'a> {
    schema_version: u32,
    source_shape_validated: bool,
    conversation_rows: &'a BTreeMap<String, i64>,
    checkpoint_sessions: &'a BTreeMap<String, String>,
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

fn create_tables(conn: &Connection) {
    conn.execute_batch(
        "create table conversations ( \
             id integer primary key, \
             inner_conversation_id text, \
             conversation_id text not null, \
             platform_id text, \
             user_id text, \
             content text not null, \
             title text, \
             persona_id text, \
             token_usage text, \
             created_at integer, \
             updated_at integer \
         ); \
         create table platform_message_history ( \
             id integer primary key, \
             platform_id text, \
             user_id text, \
             sender_id text, \
             sender_name text, \
             content text, \
             llm_checkpoint_id text, \
             created_at integer \
         ); \
         create table preferences ( \
             key text not null, \
             value text, \
             scope text \
         );",
    )
    .unwrap();
}

fn insert_conversation(conn: &Connection, id: i64, session_id: &str, content: &str) {
    conn.execute(
        "insert into conversations ( \
             id, inner_conversation_id, conversation_id, platform_id, user_id, content, \
             title, persona_id, token_usage, created_at, updated_at \
         ) values (?1, ?2, ?3, 'platform-test', 'user-test', ?4, ?5, 'persona-test', \
                   '{\"input\":1}', ?6, ?7)",
        rusqlite::params![
            id,
            session_id,
            format!("conversation-{id}"),
            content,
            format!("Conversation {id}"),
            1_784_332_800_000_i64.saturating_add(id),
            1_784_332_900_000_i64.saturating_add(id),
        ],
    )
    .unwrap();
}

fn insert_platform_message(conn: &Connection, id: i64, checkpoint_id: Option<&str>, content: &str) {
    conn.execute(
        "insert into platform_message_history ( \
             id, platform_id, user_id, sender_id, sender_name, content, \
             llm_checkpoint_id, created_at \
         ) values (?1, 'platform-test', 'user-test', ?2, 'Sender', ?3, ?4, ?5)",
        rusqlite::params![
            id,
            if id % 2 == 0 {
                "user-test"
            } else {
                "assistant-test"
            },
            content,
            checkpoint_id,
            1_784_333_000_000_i64.saturating_add(id),
        ],
    )
    .unwrap();
}

fn context(path: Option<PathBuf>) -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: "astrbot-batch-test".to_owned(),
        source_path: path,
        source_root: None,
        imported_at: DateTime::parse_from_rfc3339("2026-07-18T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
    }
}

fn test_source(identity: &str) -> SourceObservation {
    SourceObservation::new(
        CaptureProvider::AstrBot,
        ASTRBOT_SQLITE_SOURCE_FORMAT,
        format!("astrbot-sqlite:{identity}"),
        format!("astrbot-snapshot:{identity}"),
        format!("provider:astrbot:{identity}"),
        ASTRBOT_CAPTURE_REVISION,
        ASTRBOT_POLICY_REVISION,
        None,
    )
    .unwrap()
}

fn import_source(path: &Path) -> (SourceObservation, String) {
    let canonical_path = fs::canonicalize(path).unwrap();
    let snapshot = astrbot_source_snapshot(path).unwrap();
    let cursor_path = provider_path_identity(&canonical_path).unwrap();
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::AstrBot,
        ASTRBOT_SQLITE_SOURCE_FORMAT,
        &cursor_path,
    );
    let conn = open_provider_sqlite_readonly(path).unwrap();
    let user_version = conn
        .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
        .unwrap();
    let schema_fingerprint = sqlite_schema_fingerprint(&conn).unwrap();
    let source = SourceObservation::new(
        CaptureProvider::AstrBot,
        ASTRBOT_SQLITE_SOURCE_FORMAT,
        format!("astrbot-sqlite:{cursor_path}"),
        astrbot_source_revision(&snapshot, user_version, &schema_fingerprint),
        cursor_stream,
        ASTRBOT_CAPTURE_REVISION,
        ASTRBOT_POLICY_REVISION,
        None,
    )
    .unwrap();
    let stream = captured_batch_cursor_stream(&source);
    (source, stream)
}

fn seed_certified_cursor(
    store: &Store,
    context: &ProviderAdapterContext,
    stream: &str,
    cursor: &CertifiedProviderCursor,
) -> SyncCursor {
    let observed_at = context
        .imported_at
        .checked_sub_signed(chrono::Duration::seconds(1))
        .unwrap();
    let seeded = SyncCursor {
        id: crate::stable_capture_uuid(
            &format!(
                "provider-cursor:{}:{}:{}",
                CaptureProvider::AstrBot.as_str(),
                context.machine_id,
                stream
            ),
            "provider-sync-cursor",
        ),
        team_id: None,
        device_id: context.machine_id.clone(),
        stream: stream.to_owned(),
        cursor: cursor.encode().unwrap(),
        last_synced_at: Some(observed_at),
        timestamps: EntityTimestamps {
            created_at: observed_at,
            updated_at: observed_at,
        },
    };
    store.upsert_sync_cursor(&seeded).unwrap();
    seeded
}

fn produce_all(
    conn: &Connection,
    source: SourceObservation,
    start: NativePosition,
) -> Vec<CapturedBatch> {
    let sql = AstrBotSql::new(conn).unwrap();
    let mut checkpoint = AstrBotParserCheckpoint::empty();
    checkpoint.source_shape_validated = true;
    if astrbot_relationship_projection_needed(conn, &sql, &start).unwrap() {
        astrbot_prepare_relationship_projection(conn, &sql).unwrap();
    }
    let mut fetcher = AstrBotRowFetcher::new(conn, sql, checkpoint).unwrap();
    let mut producer =
        SqliteLogicalRowBatchProducer::new(source, start, move |position| fetcher.fetch(position));
    let mut batches = Vec::new();
    loop {
        let batch = with_sqlite_read_snapshot(conn, || {
            producer.next_batch().map_err(astrbot_sqlite_batch_error)
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

fn explain_query_plan(
    conn: &Connection,
    sql: &str,
    params: impl IntoIterator<Item = i64>,
) -> Vec<String> {
    let mut statement = conn.prepare(&format!("explain query plan {sql}")).unwrap();
    statement
        .query_map(rusqlite::params_from_iter(params), |row| row.get(3))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap()
}

mod lifecycle;
mod preferences;
mod producer;
mod projection;
mod relationships;
