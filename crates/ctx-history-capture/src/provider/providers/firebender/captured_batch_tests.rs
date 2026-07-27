use super::*;
use crate::captured_batch::CAPTURE_BATCH_MAX_RECORDS;

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

#[test]
fn one_pass_producer_keeps_tool_only_and_metadata_only_rows() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "create table chat_sessions (
            id text not null,
            name text not null,
            created_at integer not null,
            updated_at integer not null,
            messages_json text not null,
            metadata_json text not null
        );",
    )
    .unwrap();
    conn.execute(
        "insert into chat_sessions values (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            "tool-only",
            "Tool only",
            1_700_000_000_000_i64,
            1_700_000_000_001_i64,
            serde_json::to_string(&json!([{
                "role": "assistant",
                "tool_calls": [{
                    "id": "call-1",
                    "function": {"name": "read_file", "arguments": "{}"}
                }]
            }]))
            .unwrap(),
            "{}",
        ],
    )
    .unwrap();
    conn.execute(
        "insert into chat_sessions values (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            "metadata-only",
            "Metadata only",
            1_700_000_000_002_i64,
            1_700_000_000_003_i64,
            "[]",
            r#"{"source":"test"}"#,
        ],
    )
    .unwrap();

    let columns = sqlite_table_columns(&conn, "chat_sessions").unwrap();
    let source = SourceObservation::new(
        CaptureProvider::Firebender,
        FIREBENDER_SQLITE_SOURCE_FORMAT,
        "firebender-sqlite:test",
        "firebender-snapshot:test",
        "provider:firebender:test",
        FIREBENDER_CAPTURE_REVISION,
        FIREBENDER_POLICY_REVISION,
        None,
    )
    .unwrap();
    let fetch_calls = Cell::new(0_usize);
    let producer_fetch_calls = &fetch_calls;
    let mut fetcher = FirebenderRowFetcher::new(
        &conn,
        &columns,
        ProviderRecordKind::new(FIREBENDER_RECORD_KIND).unwrap(),
    )
    .unwrap();
    let mut producer = SqliteLogicalRowBatchProducer::new(
        source,
        initial_firebender_position().unwrap(),
        move |position| {
            producer_fetch_calls.set(producer_fetch_calls.get().saturating_add(1));
            fetcher.fetch(position)
        },
    );
    let context = ProviderAdapterContext {
        machine_id: "firebender-one-pass-test".to_owned(),
        source_path: Some("/tmp/chat_history.db".into()),
        source_root: None,
        imported_at: DateTime::<Utc>::UNIX_EPOCH,
    };
    let mut projector = FirebenderCapturedBatchProjector::new(
        context,
        "/tmp/chat_history.db".into(),
        "schema:test".to_owned(),
    );
    let mut output = CollectingProjectionOutput::default();
    let mut projected_records = 0_usize;

    while let Some(batch) = producer
        .next_batch()
        .map_err(firebender_sqlite_batch_error)
        .unwrap()
    {
        for record in batch.records() {
            projected_records = projected_records.saturating_add(1);
            projector.project_record(record, &mut output).unwrap();
        }
    }

    assert_eq!(fetch_calls.get(), 3);
    assert_eq!(projected_records, 2);
    assert!(output.rejections.is_empty());
    assert_eq!(output.normalizations.len(), 2);
    let tool_capture = &output.normalizations[0].captures[0].1;
    assert_eq!(tool_capture.session.provider_session_id, "tool-only");
    assert_eq!(
        tool_capture.event.as_ref().unwrap().event_type,
        EventType::ToolCall
    );
    let metadata_capture = &output.normalizations[1].captures[0].1;
    assert_eq!(
        metadata_capture.session.provider_session_id,
        "metadata-only"
    );
    assert!(metadata_capture.event.is_none());
}

#[test]
fn sqlite_batch_reads_end_snapshot_after_each_batch() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "create table chat_sessions (
            id text not null,
            name text not null,
            created_at integer not null,
            updated_at integer not null,
            messages_json text not null,
            metadata_json text not null
        );",
    )
    .unwrap();
    for index in 0..=CAPTURE_BATCH_MAX_RECORDS {
        let timestamp = i64::try_from(index).unwrap();
        conn.execute(
            "insert into chat_sessions values (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                format!("session-{index}"),
                format!("Session {index}"),
                timestamp,
                timestamp,
                "[]",
                "{}",
            ],
        )
        .unwrap();
    }

    let columns = sqlite_table_columns(&conn, "chat_sessions").unwrap();
    let source = SourceObservation::new(
        CaptureProvider::Firebender,
        FIREBENDER_SQLITE_SOURCE_FORMAT,
        "firebender-sqlite:batch-snapshot-test",
        "firebender-snapshot:batch-snapshot-test",
        "provider:firebender:batch-snapshot-test",
        FIREBENDER_CAPTURE_REVISION,
        FIREBENDER_POLICY_REVISION,
        None,
    )
    .unwrap();
    let mut fetcher = FirebenderRowFetcher::new(
        &conn,
        &columns,
        ProviderRecordKind::new(FIREBENDER_RECORD_KIND).unwrap(),
    )
    .unwrap();
    let mut producer = SqliteLogicalRowBatchProducer::new(
        source,
        initial_firebender_position().unwrap(),
        move |position| fetcher.fetch(position),
    );

    let first = with_sqlite_read_snapshot(&conn, || {
        producer.next_batch().map_err(firebender_sqlite_batch_error)
    })
    .unwrap()
    .unwrap();
    assert_eq!(first.records().len(), CAPTURE_BATCH_MAX_RECORDS);
    assert!(conn.is_autocommit());

    let second = with_sqlite_read_snapshot(&conn, || {
        producer.next_batch().map_err(firebender_sqlite_batch_error)
    })
    .unwrap()
    .unwrap();
    assert_eq!(second.records().len(), 1);
    assert!(conn.is_autocommit());

    let exhausted = with_sqlite_read_snapshot(&conn, || {
        producer.next_batch().map_err(firebender_sqlite_batch_error)
    })
    .unwrap();
    assert!(exhausted.is_none());
    assert!(conn.is_autocommit());
}
