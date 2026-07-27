use std::path::PathBuf;

use chrono::{DateTime, Utc};
use ctx_history_core::CaptureProvider;
use rusqlite::Connection;
use serde_json::json;

use crate::captured_batch::sqlite_logical_rows::SqliteLogicalRowBatchProducer;
use crate::captured_batch::{
    CapturedRecordPayload, SourceObservation, CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES,
};
use crate::provider::importer::{
    CapturedBatchProjector, ProviderProjectionOutput, ProviderProjectionResult,
};
use crate::{ProviderAdapterContext, ProviderNormalizationResult};

use super::projection::TraeCapturedBatchProjector;
use super::sqlite::{
    decode_trae_position, initial_trae_position, trae_sqlite_batch_error, TraeRowFetcher,
};
use super::{
    TRAE_CAPTURE_REVISION, TRAE_CHAT_KEYS, TRAE_CN_INPUT_HISTORY_KEY, TRAE_POLICY_REVISION,
    TRAE_STATE_VSCDB_SOURCE_FORMAT,
};

#[derive(Default)]
struct CollectingOutput {
    event_captures: usize,
    metadata_captures: usize,
    rejections: usize,
    event_hashes: Vec<String>,
    event_texts: Vec<String>,
    verified_message_locators: usize,
}

impl ProviderProjectionOutput for CollectingOutput {
    fn emit_normalization(
        &mut self,
        normalization: ProviderNormalizationResult,
    ) -> ProviderProjectionResult<()> {
        for (_, capture) in normalization.captures {
            if let Some(event) = capture.event {
                if event
                    .metadata
                    .get(crate::complete_content::VERIFIED_CONTENT_LOCATORS_METADATA_KEY)
                    .is_some()
                {
                    self.verified_message_locators =
                        self.verified_message_locators.saturating_add(1);
                }
                self.event_captures = self.event_captures.saturating_add(1);
                self.event_hashes
                    .push(event.provider_event_hash.unwrap_or_default());
                self.event_texts.push(
                    event.payload["text"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                );
            } else {
                self.metadata_captures = self.metadata_captures.saturating_add(1);
            }
        }
        Ok(())
    }

    fn reject_record(&mut self, _line_number: usize, _reason: String) {
        self.rejections = self.rejections.saturating_add(1);
    }
}

#[test]
fn long_message_gets_one_path_free_itemtable_locator() {
    let conn = Connection::open_in_memory().unwrap();
    create_item_table(&conn);
    let body = format!(
        "Trae complete message {}",
        "x".repeat(crate::PROVIDER_MAX_TEXT_CHARS + 64)
    );
    let value = json!({
        "list": [{
            "id": "session-1",
            "messages": [{"id": "message-1", "role": "user", "content": body}]
        }],
    })
    .to_string();
    conn.execute(
        "insert into ItemTable (key, value) values (?1, ?2)",
        rusqlite::params![TRAE_CHAT_KEYS[0], value],
    )
    .unwrap();
    let mut fetcher = TraeRowFetcher::new(&conn, 1).unwrap();
    let logical = fetcher
        .fetch(initial_trae_position().unwrap())
        .unwrap()
        .unwrap();
    let mut projector = TraeCapturedBatchProjector {
        context: context(),
        workspace_id: "workspace".to_owned(),
        workspace_folder: None,
        workspace_ordinal: 1,
        projected_chat_values: 0,
    };
    let mut output = CollectingOutput::default();
    projector
        .project_record(logical.record(), &mut output)
        .unwrap();
    assert_eq!(output.event_captures, 1);
    assert_eq!(output.verified_message_locators, 1);
}

fn create_item_table(conn: &Connection) {
    conn.execute_batch("create table ItemTable (key text primary key, value);")
        .unwrap();
}

fn imported_at() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-07-18T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn context() -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: "trae-test-machine".to_owned(),
        source_path: Some(PathBuf::from("/tmp/trae/workspace/state.vscdb")),
        source_root: Some(PathBuf::from("/tmp/trae")),
        imported_at: imported_at(),
    }
}

#[test]
fn near_limit_multi_page_chat_is_hydrated_and_parent_scanned_once() {
    let conn = Connection::open_in_memory().unwrap();
    create_item_table(&conn);
    let messages = (0..130)
        .map(|index| {
            json!({
                "id": format!("message-{index}"),
                "role": if index % 2 == 0 { "user" } else { "assistant" },
                "content": format!("message {index}"),
                "timestamp": "2026-07-18T00:00:00Z",
            })
        })
        .collect::<Vec<_>>();
    let value = json!({
        "padding": "x".repeat(CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES - 512 * 1024),
        "list": [{"id": "session-1", "messages": messages}],
    })
    .to_string();
    assert!(value.len() > 15 * 1024 * 1024);
    assert!(value.len() < CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES);
    conn.execute(
        "insert into ItemTable (key, value) values (?1, ?2)",
        rusqlite::params![TRAE_CHAT_KEYS[0], value],
    )
    .unwrap();

    let mut fetcher = TraeRowFetcher::new(&conn, 1).unwrap();
    let logical = fetcher
        .fetch(initial_trae_position().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(fetcher.candidate_queries, 1);
    assert_eq!(fetcher.hydrated_chat_values, 1);
    assert!(matches!(
        logical.record().payload(),
        CapturedRecordPayload::NativeBytes(bytes) if bytes.len() > 15 * 1024 * 1024
    ));

    let mut projector = TraeCapturedBatchProjector {
        context: context(),
        workspace_id: "workspace".to_owned(),
        workspace_folder: None,
        workspace_ordinal: 1,
        projected_chat_values: 0,
    };
    let mut output = CollectingOutput::default();
    projector
        .project_record(logical.record(), &mut output)
        .unwrap();
    assert_eq!(projector.projected_chat_values, 1);
    assert_eq!(output.event_captures, 130);
    assert_eq!(output.metadata_captures, 1);
    assert_eq!(output.rejections, 0);
    assert_eq!(
        output.event_hashes.first().map(String::as_str),
        Some("workspace/session-1:message-0")
    );
    assert_eq!(
        output.event_hashes.last().map(String::as_str),
        Some("workspace/session-1:message-129")
    );
    assert_eq!(
        output.event_texts.first().map(String::as_str),
        Some("message 0")
    );
    assert_eq!(
        output.event_texts.last().map(String::as_str),
        Some("message 129")
    );
}

#[test]
fn shared_producer_preserves_projection_output_and_advances_past_a_malformed_sibling() {
    let conn = Connection::open_in_memory().unwrap();
    create_item_table(&conn);
    let malformed = r#"{
            "list": [{
                "id": "malformed-session",
                "messages": [
                    {"id": "would-have-emitted", "content": "must not commit"},
                    {"id": "malformed-late", "content": {"nested": [1, 2,]}}
                ]
            }]
        }"#;
    conn.execute(
        "insert into ItemTable (key, value) values (?1, ?2)",
        rusqlite::params![TRAE_CHAT_KEYS[0], malformed],
    )
    .unwrap();
    conn.execute(
        "insert into ItemTable (key, value) values (?1, ?2)",
        rusqlite::params![
            TRAE_CN_INPUT_HISTORY_KEY,
            json!([{"id": "valid-sibling", "inputText": "sibling survives"}]).to_string(),
        ],
    )
    .unwrap();
    let source = SourceObservation::new(
        CaptureProvider::Trae,
        TRAE_STATE_VSCDB_SOURCE_FORMAT,
        "trae-sqlite:malformed-late",
        "snapshot:malformed-late",
        "provider:trae:malformed-late",
        TRAE_CAPTURE_REVISION,
        TRAE_POLICY_REVISION,
        None,
    )
    .unwrap();
    let mut fetcher = TraeRowFetcher::new(&conn, 1).unwrap();
    let mut producer = SqliteLogicalRowBatchProducer::new(
        source,
        initial_trae_position().unwrap(),
        move |position| fetcher.fetch(position),
    );
    let batch = producer
        .next_batch()
        .map_err(trae_sqlite_batch_error)
        .unwrap()
        .unwrap();
    assert!(batch.source_exhausted());
    assert_eq!(batch.records().len(), 3);
    let terminal = batch.range_end().clone();
    let terminal_position = decode_trae_position(&terminal).unwrap().unwrap();
    assert_eq!(
        usize::from(terminal_position.key_index),
        TRAE_CHAT_KEYS.len()
    );

    let mut projector = TraeCapturedBatchProjector {
        context: context(),
        workspace_id: "workspace".to_owned(),
        workspace_folder: None,
        workspace_ordinal: 1,
        projected_chat_values: 0,
    };
    let mut output = CollectingOutput::default();
    for record in batch.records() {
        projector.project_record(record, &mut output).unwrap();
    }
    assert_eq!(output.rejections, 1);
    assert_eq!(output.event_captures, 1);
    assert_eq!(output.metadata_captures, 1);
    assert_eq!(projector.projected_chat_values, 1);
    assert_eq!(
        output.event_hashes,
        ["workspace/trae-cn-input-history:valid-sibling"]
    );
    assert_eq!(output.event_texts, ["sibling survives"]);

    let mut replay = TraeRowFetcher::new(&conn, 1).unwrap();
    assert!(replay.fetch(terminal).unwrap().is_none());
    assert_eq!(replay.candidate_queries, 0);
    assert_eq!(replay.hydrated_chat_values, 0);
}
