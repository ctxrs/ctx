use std::{
    fs,
    path::{Path, PathBuf},
};

use chrono::{TimeZone, Utc};
use ctx_history_core::CaptureProvider;
use ctx_history_store::Store;
use rusqlite::Connection;
use tempfile::TempDir;

use super::*;
use crate::captured_batch::sqlite_logical_rows::SqliteLogicalRowBatchProducer;
use crate::captured_batch::{
    CapturedBatch, CapturedRecordPayload, NativePosition, SourceObservation,
    CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES, CAPTURE_BATCH_MAX_RECORDS,
};
use crate::provider::custom_history_jsonl::push_provider_import_failure;
use crate::provider::importer::{
    CapturedBatchProjector, ProviderProjectionOutput, ProviderProjectionResult,
};
use crate::provider::sqlite::open_provider_sqlite_readonly;
use crate::provider::sqlite::{sqlite_schema_fingerprint, with_sqlite_read_snapshot};
use crate::{NormalizedProviderImportOptions, ProviderNormalizationResult};

use super::project::NanoClawProjectSnapshot;
use super::source::NanoClawRowFetcher;

#[path = "tests/import.rs"]
mod import;
#[path = "tests/position.rs"]
mod position;
#[path = "tests/project.rs"]
mod project;
#[path = "tests/projection.rs"]
mod projection;
#[path = "tests/source.rs"]
mod source;

pub(super) struct CollectingOutput {
    pub(super) normalization: ProviderNormalizationResult,
}

impl ProviderProjectionOutput for CollectingOutput {
    fn emit_normalization(
        &mut self,
        normalization: ProviderNormalizationResult,
    ) -> ProviderProjectionResult<()> {
        self.normalization.summary.merge(normalization.summary);
        self.normalization.captures.extend(normalization.captures);
        self.normalization
            .files_touched
            .extend(normalization.files_touched);
        Ok(())
    }

    fn reject_record(&mut self, line_number: usize, reason: String) {
        push_provider_import_failure(&mut self.normalization.summary, line_number, reason);
    }
}

pub(super) fn context(root: &Path) -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: "machine-nanoclaw-test".to_owned(),
        source_path: Some(root.to_path_buf()),
        source_root: None,
        imported_at: Utc
            .with_ymd_and_hms(2026, 7, 18, 12, 0, 0)
            .single()
            .unwrap(),
    }
}

pub(super) fn create_project(temp: &TempDir, name: &str, sessions: usize) -> PathBuf {
    let root = temp.path().join(name);
    let data = root.join("data");
    fs::create_dir_all(data.join("v2-sessions")).unwrap();
    let central = Connection::open(data.join("v2.db")).unwrap();
    central
        .execute_batch(
            "create table agent_groups (
                id text primary key, name text, folder text, agent_provider text
            );
            create table messaging_groups (
                id text primary key, channel_type text, platform_id text,
                instance text, name text
            );
            create table sessions (
                id text primary key, agent_group_id text not null,
                messaging_group_id text, thread_id text, agent_provider text,
                status text, container_status text, last_active integer,
                created_at integer
            );
            insert into agent_groups values (
                'ag-1', 'Personal', '/workspace/nanoclaw', 'codex'
            );
            insert into messaging_groups values (
                'mg-1', 'telegram', 'chat-1', 'default', 'DM'
            );",
        )
        .unwrap();
    for index in 0..sessions {
        central
            .execute(
                "insert into sessions values (
                    ?1, 'ag-1', 'mg-1', ?2, 'codex', 'active', 'running',
                    ?3, ?4
                )",
                rusqlite::params![
                    format!("session-{index:04}"),
                    format!("thread-{index:04}"),
                    1_782_259_202_000_i64 + index as i64,
                    1_782_259_200_000_i64 + index as i64,
                ],
            )
            .unwrap();
    }
    root
}

pub(super) fn create_message_stores(root: &Path, session_id: &str) -> (PathBuf, PathBuf) {
    let session_dir = root
        .join("data")
        .join("v2-sessions")
        .join("ag-1")
        .join(session_id);
    fs::create_dir_all(&session_dir).unwrap();
    let inbound_path = session_dir.join("inbound.db");
    let inbound = Connection::open(&inbound_path).unwrap();
    inbound
        .execute_batch(
            "create table messages_in (
                id text primary key, seq integer, kind text, timestamp integer,
                status text, trigger text, platform_id text, channel_type text,
                thread_id text, content text, source_session_id text, on_wake integer
            );",
        )
        .unwrap();
    let outbound_path = session_dir.join("outbound.db");
    let outbound = Connection::open(&outbound_path).unwrap();
    outbound
        .execute_batch(
            "create table messages_out (
                id text primary key, seq integer, in_reply_to text, timestamp integer,
                kind text, platform_id text, channel_type text, thread_id text,
                content text
            );",
        )
        .unwrap();
    (inbound_path, outbound_path)
}

pub(super) fn insert_inbound(path: &Path, id: &str, seq: i64, timestamp: i64, content: &str) {
    Connection::open(path)
        .unwrap()
        .execute(
            "insert into messages_in values (
                ?1, ?2, 'chat', ?3, 'done', 'message', 'chat-1', 'telegram',
                'thread', ?4, null, 0
            )",
            rusqlite::params![id, seq, timestamp, content],
        )
        .unwrap();
}

pub(super) fn insert_outbound(path: &Path, id: &str, seq: i64, timestamp: i64, content: &str) {
    Connection::open(path)
        .unwrap()
        .execute(
            "insert into messages_out values (
                ?1, ?2, null, ?3, 'chat', 'chat-1', 'telegram', 'thread', ?4
            )",
            rusqlite::params![id, seq, timestamp, content],
        )
        .unwrap();
}

pub(super) fn capture_batches(root: &Path, start: NativePosition) -> Vec<CapturedBatch> {
    let central_path = root.join("data").join("v2.db");
    let snapshot = NanoClawProjectSnapshot::read(root, &central_path).unwrap();
    let conn = open_provider_sqlite_readonly(&central_path).unwrap();
    let user_version: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    let schema_fingerprint = sqlite_schema_fingerprint(&conn).unwrap();
    let source = SourceObservation::new(
        CaptureProvider::NanoClaw,
        NANOCLAW_SOURCE_FORMAT,
        "nanoclaw-project:test",
        snapshot.source_revision(user_version, &schema_fingerprint),
        "provider:nanoclaw:test",
        NANOCLAW_CAPTURE_REVISION,
        NANOCLAW_POLICY_REVISION,
        None,
    )
    .unwrap();
    let mut fetcher = NanoClawRowFetcher::new(&conn, &snapshot).unwrap();
    let mut producer =
        SqliteLogicalRowBatchProducer::new(source, start, move |position| fetcher.fetch(position));
    let mut batches = Vec::new();
    while let Some(batch) = with_sqlite_read_snapshot(&conn, || {
        producer.next_batch().map_err(nanoclaw_sqlite_batch_error)
    })
    .unwrap()
    {
        batches.push(batch);
    }
    batches
}

pub(super) fn record_kinds(batches: &[CapturedBatch]) -> Vec<&str> {
    batches
        .iter()
        .flat_map(|batch| batch.records())
        .map(|record| record.record_kind().as_str())
        .collect()
}
