use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde_json::json;

use super::*;
use crate::captured_batch::sqlite_logical_rows::SqliteLogicalRowBatchProducer;
use crate::captured_batch::{NativeLocator, ProviderRecordKind, SourceObservation};
use crate::provider::file_touches::MAX_PROVIDER_FILE_TOUCHES_PER_EVENT;
use crate::provider::providers::zed::source::{
    decode_zed_position, initial_zed_position, zed_thread_columns, ZedCapturePhase, ZedRowFetcher,
    ZedStorageClassError,
};
use crate::provider::providers::zed::thread::ZedThreadRow;
use crate::provider::providers::zed::{
    ZED_CAPTURE_REVISION, ZED_LOCATOR_KIND, ZED_MALFORMED_RECORD_KIND, ZED_POLICY_REVISION,
    ZED_RECORD_KIND,
};

#[derive(Default)]
struct CountingProjectionOutput {
    emissions: usize,
    captures: usize,
    files_touched: usize,
    capture_was_first: bool,
    first_touch: Option<(u64, String, Option<String>)>,
    last_touch: Option<(u64, String, Option<String>)>,
    rejections: Vec<(usize, String)>,
}

impl ProviderProjectionOutput for CountingProjectionOutput {
    fn emit_normalization(
        &mut self,
        normalization: ProviderNormalizationResult,
    ) -> ProviderProjectionResult<()> {
        assert!(normalization.captures.len() <= 1);
        assert!(normalization.files_touched.len() <= 1);
        assert!(normalization.captures.is_empty() || normalization.files_touched.is_empty());
        if !normalization.captures.is_empty() {
            self.capture_was_first = self.emissions == 0;
        }
        if let Some((_, touch)) = normalization.files_touched.first() {
            let projected = (
                touch.provider_touch_index,
                touch.path.clone(),
                touch.source_root.clone(),
            );
            if self.first_touch.is_none() {
                self.first_touch = Some(projected.clone());
            }
            self.last_touch = Some(projected);
        }
        self.emissions += 1;
        self.captures += normalization.captures.len();
        self.files_touched += normalization.files_touched.len();
        Ok(())
    }

    fn reject_record(&mut self, line_number: usize, reason: String) {
        self.rejections.push((line_number, reason));
    }
}

fn observed_at() -> DateTime<Utc> {
    "2026-07-18T12:00:00Z".parse().unwrap()
}

fn context(path: &Path) -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: "zed-batch-test-machine".to_owned(),
        source_path: Some(path.to_path_buf()),
        source_root: None,
        imported_at: observed_at(),
    }
}

fn create_threads_schema(conn: &Connection) {
    conn.execute_batch(
        "CREATE TABLE threads(\
            id TEXT PRIMARY KEY, parent_id TEXT, folder_paths TEXT, folder_paths_order TEXT,\
            summary TEXT NOT NULL, updated_at TEXT NOT NULL, data_type TEXT NOT NULL,\
            data BLOB NOT NULL, created_at TEXT\
         );",
    )
    .unwrap();
}

#[test]
fn zed_malformed_storage_classes_reject_and_preserve_healthy_sibling_under_cap() {
    let conn = Connection::open_in_memory().unwrap();
    create_threads_schema(&conn);
    let healthy_data = serde_json::to_vec(&json!({
        "title": "healthy sibling",
        "messages": [],
        "updated_at": "2026-07-18T12:00:00Z"
    }))
    .unwrap();
    let insert_valid = |id: &str| {
        conn.execute(
            "INSERT INTO threads(id, summary, updated_at, data_type, data) \
             VALUES (?1, 'summary', '2026-07-18T12:00:00Z', 'json', ?2)",
            rusqlite::params![id, &healthy_data],
        )
        .unwrap();
    };

    insert_valid("bad-id");
    conn.execute(
        "UPDATE threads SET id = x'6261642d6964' WHERE id = 'bad-id'",
        [],
    )
    .unwrap();
    for (id, column) in [
        ("bad-parent-id", "parent_id"),
        ("bad-folder-paths", "folder_paths"),
        ("bad-folder-paths-order", "folder_paths_order"),
        ("bad-summary", "summary"),
        ("bad-updated-at", "updated_at"),
        ("bad-data-type", "data_type"),
        ("bad-created-at", "created_at"),
    ] {
        insert_valid(id);
        conn.execute(
            &format!("UPDATE threads SET {column} = x'01' WHERE id = ?1"),
            [id],
        )
        .unwrap();
    }
    insert_valid("bad-data");
    conn.execute("UPDATE threads SET data = '{}' WHERE id = 'bad-data'", [])
        .unwrap();
    insert_valid("healthy");

    let sqlite_value_limit = 64 * 1024;
    conn.set_limit(
        rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH,
        sqlite_value_limit,
    );
    let columns = zed_thread_columns(&conn).unwrap();
    let mut fetcher = ZedRowFetcher::new(
        &conn,
        &columns,
        ProviderRecordKind::new(ZED_RECORD_KIND).unwrap(),
    )
    .unwrap();
    let source = SourceObservation::new(
        CaptureProvider::Zed,
        ZED_THREADS_SQLITE_SOURCE_FORMAT,
        "zed-malformed-source",
        "zed-malformed-revision",
        "zed-malformed-stream",
        ZED_CAPTURE_REVISION,
        ZED_POLICY_REVISION,
        None,
    )
    .unwrap();
    let mut producer = SqliteLogicalRowBatchProducer::new(
        source,
        initial_zed_position().unwrap(),
        move |position| fetcher.fetch(position),
    );
    let batch = producer.next_batch().unwrap().unwrap();
    assert_eq!(batch.records().len(), 10);
    assert_eq!(
        batch
            .records()
            .iter()
            .filter(|record| record.record_kind().as_str() == ZED_MALFORMED_RECORD_KIND)
            .count(),
        9
    );
    assert_eq!(
        batch
            .records()
            .iter()
            .filter(|record| record.record_kind().as_str() == ZED_RECORD_KIND)
            .count(),
        1
    );

    let mut projector = ZedCapturedBatchProjector {
        context: context(Path::new("/tmp/zed-malformed.db")),
        raw_source_path: "/tmp/zed-malformed.db".to_owned(),
        user_version: 0,
        schema_fingerprint: "zed-malformed-schema".to_owned(),
    };
    let mut output = CountingProjectionOutput::default();
    for record in batch.records() {
        projector.project_record(record, &mut output).unwrap();
    }
    assert_eq!(output.captures, 1);
    assert_eq!(output.rejections.len(), 9);
    for storage_error in [
        ZedStorageClassError::Id,
        ZedStorageClassError::ParentId,
        ZedStorageClassError::FolderPaths,
        ZedStorageClassError::FolderPathsOrder,
        ZedStorageClassError::Summary,
        ZedStorageClassError::UpdatedAt,
        ZedStorageClassError::DataType,
        ZedStorageClassError::Data,
        ZedStorageClassError::CreatedAt,
    ] {
        assert!(output
            .rejections
            .iter()
            .any(|(_, reason)| reason == storage_error.rejection_reason()));
    }
    assert_eq!(
        conn.limit(rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH),
        sqlite_value_limit
    );
    assert_eq!(
        decode_zed_position(batch.range_end())
            .unwrap()
            .unwrap()
            .phase,
        ZedCapturePhase::Exhausted
    );
}

#[test]
fn zed_streams_touches_after_capture_and_rejects_identity_overflow() {
    let raw_source_path = "/tmp/zed/threads.db";
    let paths = Value::Array(
        (0..=MAX_PROVIDER_FILE_TOUCHES_PER_EVENT)
            .map(|index| json!({ "path": format!(".zed-generated-{index}") }))
            .collect(),
    );
    let thread = json!({
        "title": "Zed touch ceiling",
        "messages": [{
            "Agent": {
                "content": [{
                    "ToolUse": {
                        "id": "zed-touch-ceiling",
                        "name": "write_file"
                    }
                }],
                "paths": paths
            }
        }],
        "updated_at": "2026-07-18T12:00:00Z"
    });
    let row = ZedThreadRow {
        rowid: 1,
        id: "zed-touch-ceiling".to_owned(),
        parent_id: None,
        folder_paths: None,
        folder_paths_order: None,
        summary: "Zed touch ceiling".to_owned(),
        updated_at: "2026-07-18T12:00:00Z".to_owned(),
        data_type: "json".to_owned(),
        data: serde_json::to_vec(&thread).unwrap(),
        created_at: Some("2026-07-18T11:00:00Z".to_owned()),
    };
    let mut output = CountingProjectionOutput::default();
    let locator = NativeLocator::new(ZED_LOCATOR_KIND, row.rowid.to_be_bytes().to_vec()).unwrap();
    let values = vec![
        CapturedSqliteValue::Integer(row.rowid),
        CapturedSqliteValue::Text(row.id.clone()),
        CapturedSqliteValue::Null,
        CapturedSqliteValue::Null,
        CapturedSqliteValue::Null,
        CapturedSqliteValue::Text(row.summary.clone()),
        CapturedSqliteValue::Text(row.updated_at.clone()),
        CapturedSqliteValue::Text(row.data_type.clone()),
        CapturedSqliteValue::Blob(row.data.clone()),
        CapturedSqliteValue::Text(row.created_at.clone().unwrap()),
    ];

    project_zed_thread_row(
        &row,
        &locator,
        &values,
        raw_source_path,
        0,
        "zed-test-schema",
        &context(Path::new(raw_source_path)),
        &mut output,
    )
    .unwrap();

    assert!(output.capture_was_first);
    assert_eq!(output.captures, 1);
    assert_eq!(output.files_touched, MAX_PROVIDER_FILE_TOUCHES_PER_EVENT);
    assert_eq!(
        output.emissions,
        MAX_PROVIDER_FILE_TOUCHES_PER_EVENT + output.captures
    );
    assert_eq!(
        output.first_touch,
        Some((
            0,
            ".zed-generated-0".to_owned(),
            Some(raw_source_path.to_owned())
        ))
    );
    assert_eq!(
        output.last_touch,
        Some((
            u64::try_from(MAX_PROVIDER_FILE_TOUCHES_PER_EVENT).unwrap() - 1,
            format!(".zed-generated-{}", MAX_PROVIDER_FILE_TOUCHES_PER_EVENT - 1),
            Some(raw_source_path.to_owned())
        ))
    );
    assert_eq!(
        output.rejections,
        vec![(
            zed_line_number(row.rowid, 0),
            PROVIDER_FILE_TOUCH_LIMIT_REJECTION.to_owned()
        )]
    );
}
