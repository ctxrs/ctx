use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use rusqlite::{params, Connection};
use serde_json::{json, Value};
use tempfile::tempdir;

use super::query::ZED_THREAD_ID_MAX_BYTES;
use super::*;

#[derive(Default)]
struct CollectingSink {
    pages: Vec<ZedNativePage>,
}

impl ZedNativeSink for CollectingSink {
    fn push_page(&mut self, page: ZedNativePage) -> ZedNativeResult<()> {
        self.pages.push(page);
        Ok(())
    }
}

impl CollectingSink {
    fn sessions(&self) -> Vec<&ZedNativeSession> {
        self.pages
            .iter()
            .flat_map(|page| page.sessions.iter())
            .collect()
    }

    fn events(&self) -> Vec<&ZedNativeEvent> {
        self.pages
            .iter()
            .flat_map(|page| page.events.iter())
            .collect()
    }

    fn rejections(&self) -> Vec<&ZedNativeRejection> {
        self.pages
            .iter()
            .flat_map(|page| page.rejections.iter())
            .collect()
    }
}

#[derive(Default)]
struct DiscardingSink {
    pages: u64,
    rows: u64,
    max_page_rows: usize,
    max_page_bytes: usize,
}

#[derive(Default)]
struct RecordingProSink {
    progress: Mutex<BTreeMap<String, crate::ProOutputProgress>>,
    content: Mutex<Vec<Vec<u8>>>,
}

impl crate::ProOutputSink for RecordingProSink {
    fn inventory_generation(&self) -> u64 {
        1
    }

    fn materializer_revision(&self) -> &str {
        "zed-test-materializer-v1"
    }

    fn observe_source(
        &self,
        source: &crate::OutputSourceIdentity,
    ) -> std::result::Result<Option<crate::ProOutputProgress>, crate::ProOutputSinkError> {
        Ok(self
            .progress
            .lock()
            .unwrap()
            .get(&source.source_id)
            .cloned())
    }

    fn materialize_page(
        &self,
        page: crate::ProOutputMaterializationPage,
    ) -> std::result::Result<crate::ProOutputPageResult, crate::ProOutputSinkError> {
        let accepted_outputs = u32::try_from(page.observations.len()).unwrap_or(u32::MAX);
        self.content.lock().unwrap().extend(
            page.observations
                .into_iter()
                .map(|observation| observation.content),
        );
        self.progress.lock().unwrap().insert(
            page.source.source_id.clone(),
            crate::ProOutputProgress {
                source_epoch: page.source_epoch,
                observed_revision: page.observed_revision.clone(),
                cursor: Some(page.next_safe_cursor.clone()),
                parser_revision: page.parser_revision.clone(),
                materializer_revision: page.materializer_revision.clone(),
                terminal: page.terminal,
            },
        );
        Ok(crate::ProOutputPageResult {
            source_epoch: page.source_epoch,
            committed_cursor: page.next_safe_cursor,
            accepted_outputs,
            materialized_facts: accepted_outputs,
            replayed: false,
        })
    }
}

#[derive(Default)]
struct FailingProSink {
    behind: AtomicUsize,
}

impl crate::ProOutputSink for FailingProSink {
    fn inventory_generation(&self) -> u64 {
        1
    }

    fn materializer_revision(&self) -> &str {
        "zed-failing-materializer-v1"
    }

    fn observe_source(
        &self,
        _source: &crate::OutputSourceIdentity,
    ) -> std::result::Result<Option<crate::ProOutputProgress>, crate::ProOutputSinkError> {
        Ok(None)
    }

    fn materialize_page(
        &self,
        _page: crate::ProOutputMaterializationPage,
    ) -> std::result::Result<crate::ProOutputPageResult, crate::ProOutputSinkError> {
        Err(crate::ProOutputSinkError::new(
            "zed_test_failure",
            "intentional test failure",
        ))
    }

    fn mark_behind(&self, _error: crate::ProOutputSinkError) {
        self.behind.fetch_add(1, Ordering::SeqCst);
    }
}

impl ZedNativeSink for DiscardingSink {
    fn push_page(&mut self, page: ZedNativePage) -> ZedNativeResult<()> {
        self.pages = self.pages.saturating_add(1);
        self.rows = self
            .rows
            .saturating_add(u64::try_from(page.row_count()).unwrap_or(u64::MAX));
        self.max_page_rows = self.max_page_rows.max(page.row_count());
        self.max_page_bytes = self.max_page_bytes.max(page.estimated_bytes);
        Ok(())
    }
}

fn complete(outcome: ZedNativeScanOutcome) -> ZedNativeGenerationAuthority {
    match outcome {
        ZedNativeScanOutcome::Complete(authority) => *authority,
        ZedNativeScanOutcome::Incomplete(incomplete) => {
            panic!("expected complete Zed generation, got {incomplete:?}")
        }
    }
}

fn create_schema(connection: &Connection) {
    connection
        .execute_batch(
            "PRAGMA user_version = 3;
             CREATE TABLE threads (
                 id TEXT PRIMARY KEY,
                 summary TEXT NOT NULL,
                 updated_at TEXT NOT NULL,
                 data_type TEXT NOT NULL,
                 data BLOB NOT NULL,
                 parent_id TEXT,
                 folder_paths TEXT,
                 folder_paths_order TEXT,
                 created_at TEXT
             );",
        )
        .unwrap();
}

fn thread(messages: Vec<Value>) -> Value {
    json!({
        "version": "0.3.0",
        "title": "NativePath Zed thread",
        "updated_at": "2026-07-24T12:00:10Z",
        "messages": messages,
        "model": {"provider": "zed.dev", "model": "fixture"},
        "profile": "write"
    })
}

fn user(id: &str, text: &str) -> Value {
    json!({"User": {"id": id, "content": [{"Text": text}]}})
}

fn assistant(text: &str) -> Value {
    json!({"Agent": {"content": [{"Text": text}], "tool_results": {}}})
}

fn output_message(call_id: &str, input_path: &str, output_body: &str, is_error: bool) -> Value {
    json!({
        "Agent": {
            "content": [{
                "ToolUse": {
                    "id": call_id,
                    "name": "write_file",
                    "input": {"path": input_path, "content": "safe input"},
                    "raw_input": "{\"path\":\"safe\"}",
                    "is_input_complete": true
                }
            }],
            "tool_results": {
                call_id: {
                    "tool_name": "write_file",
                    "tool_use_id": call_id,
                    "is_error": is_error,
                    "content": [{"Text": output_body}],
                    "output": {"status": if is_error {"error"} else {"ok"}}
                }
            }
        }
    })
}

fn encode_thread(payload: &Value, data_type: &str) -> Vec<u8> {
    let json = serde_json::to_vec(payload).unwrap();
    match data_type {
        "json" => json,
        "zstd" => zstd::stream::encode_all(json.as_slice(), 3).unwrap(),
        _ => json,
    }
}

fn insert_thread(
    connection: &Connection,
    id: &str,
    parent_id: Option<&str>,
    data_type: &str,
    payload: &Value,
) {
    let data = encode_thread(payload, data_type);
    connection
        .execute(
            "INSERT INTO threads (
                 id, summary, updated_at, data_type, data, parent_id,
                 folder_paths, folder_paths_order, created_at
             ) VALUES (?1, ?2, '2026-07-24T12:00:10Z', ?3, ?4, ?5, ?6, '0',
                       '2026-07-24T12:00:00Z')",
            params![
                id,
                format!("summary {id}"),
                data_type,
                data,
                parent_id,
                format!("/workspace/{id}")
            ],
        )
        .unwrap();
}

fn update_thread(connection: &Connection, id: &str, data_type: &str, payload: &Value) {
    connection
        .execute(
            "UPDATE threads SET data_type=?2, data=?3, updated_at='2026-07-24T12:00:11Z'
             WHERE id=?1",
            params![id, data_type, encode_thread(payload, data_type)],
        )
        .unwrap();
}

fn new_database(path: &Path) -> Connection {
    let connection = Connection::open(path).unwrap();
    create_schema(&connection);
    connection
}

fn scan(path: &Path) -> (ZedNativeGenerationAuthority, CollectingSink) {
    let mut sink = CollectingSink::default();
    let outcome = scan_zed_nativepath(&ZedNativeSourceSelection::exact(path), &mut sink).unwrap();
    (complete(outcome), sink)
}

#[test]
fn json_zstd_and_mixed_blob_encodings_preserve_thread_and_message_order() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let connection = new_database(&path);
    insert_thread(
        &connection,
        "thread-json",
        None,
        "json",
        &thread(vec![
            user("user-json", "json user"),
            assistant("json agent"),
        ]),
    );
    insert_thread(
        &connection,
        "thread-zstd",
        Some("thread-json"),
        "zstd",
        &thread(vec![
            user("user-zstd", "zstd user"),
            assistant("zstd agent"),
        ]),
    );
    connection
        .execute(
            "UPDATE threads
             SET folder_paths='/workspace/thread-zstd\n/workspace/ordered-first',
                 folder_paths_order='1,0'
             WHERE id='thread-zstd'",
            [],
        )
        .unwrap();
    drop(connection);

    let (authority, sink) = scan(&path);

    assert!(authority.source_complete);
    assert!(!authority.zero_native_rows);
    assert_eq!(authority.counters.native_thread_rows, 2);
    assert_eq!(authority.counters.retained_events, 4);
    let sessions = sink.sessions();
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0].thread_id, "thread-json");
    assert_eq!(sessions[0].encoding, dto::ZedNativeEncoding::Json);
    assert_eq!(sessions[1].thread_id, "thread-zstd");
    assert_eq!(sessions[1].encoding, dto::ZedNativeEncoding::Zstd);
    assert_eq!(sessions[1].parent_thread_id.as_deref(), Some("thread-json"));
    assert_eq!(sessions[1].root_thread_id, "thread-json");
    assert_eq!(
        sessions[1].folder_paths,
        vec![
            "/workspace/thread-zstd".to_owned(),
            "/workspace/ordered-first".to_owned()
        ]
    );
    assert_eq!(sessions[1].cwd.as_deref(), Some("/workspace/ordered-first"));
    let events = sink.events();
    assert_eq!(
        events
            .iter()
            .map(|event| (
                event.identity.thread_id.as_str(),
                event.native_order.message_ordinal,
                event.body.as_str()
            ))
            .collect::<Vec<_>>(),
        vec![
            ("thread-json", 0, "json user"),
            ("thread-json", 1, "json agent"),
            ("thread-zstd", 0, "zstd user"),
            ("thread-zstd", 1, "zstd agent"),
        ]
    );
    assert_eq!(
        events[0].identity.message,
        ZedNativeMessageIdentity::ProviderId {
            value: "user-json".to_owned(),
            message_ordinal: 0,
        }
    );
    assert_eq!(
        events[1].identity.message,
        ZedNativeMessageIdentity::MessageOrdinal(1)
    );
}

#[test]
fn binary_id_order_and_digests_are_stable_across_reverse_reinsertion() {
    let directory = tempdir().unwrap();
    let first_path = directory.path().join("first.db");
    let second_path = directory.path().join("second.db");
    let first = new_database(&first_path);
    insert_thread(
        &first,
        "z-thread",
        None,
        "json",
        &thread(vec![user("z-user", "zed")]),
    );
    first
        .execute(
            "INSERT INTO threads (
                 id, summary, updated_at, data_type, data, created_at
             ) VALUES (
                 'm-malformed', 'malformed', '2026-07-24T12:00:10Z',
                 'json', CAST('{\"messages\":[' AS BLOB),
                 '2026-07-24T12:00:00Z'
             )",
            [],
        )
        .unwrap();
    insert_thread(
        &first,
        "a-thread",
        None,
        "json",
        &thread(vec![user("a-user", "alpha")]),
    );
    drop(first);
    let second = new_database(&second_path);
    insert_thread(
        &second,
        "a-thread",
        None,
        "json",
        &thread(vec![user("a-user", "alpha")]),
    );
    insert_thread(
        &second,
        "z-thread",
        None,
        "json",
        &thread(vec![user("z-user", "zed")]),
    );
    second
        .execute(
            "INSERT INTO threads (
                 id, summary, updated_at, data_type, data, created_at
             ) VALUES (
                 'm-malformed', 'malformed', '2026-07-24T12:00:10Z',
                 'json', CAST('{\"messages\":[' AS BLOB),
                 '2026-07-24T12:00:00Z'
             )",
            [],
        )
        .unwrap();
    drop(second);

    let (first, first_sink) = scan(&first_path);
    let (second, second_sink) = scan(&second_path);

    for sink in [&first_sink, &second_sink] {
        assert_eq!(
            sink.sessions()
                .iter()
                .map(|session| session.thread_id.as_str())
                .collect::<Vec<_>>(),
            vec!["a-thread", "z-thread"]
        );
        assert_eq!(
            sink.events()
                .iter()
                .map(|event| event.native_order.thread_ordinal)
                .collect::<Vec<_>>(),
            vec![0, 2]
        );
        assert_eq!(sink.rejections().len(), 1);
        assert_eq!(
            sink.rejections()[0].thread_id.as_deref(),
            Some("m-malformed")
        );
    }
    assert_eq!(
        first.source_integrity_digest,
        second.source_integrity_digest
    );
    assert_eq!(first.core_generation_digest, second.core_generation_digest);
}

#[test]
fn composite_duplicate_and_non_binary_thread_id_schemas_are_rejected() {
    let directory = tempdir().unwrap();
    let composite_path = directory.path().join("composite.db");
    let composite = Connection::open(&composite_path).unwrap();
    composite
        .execute_batch(
            "CREATE TABLE threads (
                 id TEXT NOT NULL,
                 partition INTEGER NOT NULL,
                 summary TEXT NOT NULL,
                 updated_at TEXT NOT NULL,
                 data_type TEXT NOT NULL,
                 data BLOB NOT NULL,
                 PRIMARY KEY (id, partition)
             );",
        )
        .unwrap();
    drop(composite);

    let mut sink = CollectingSink::default();
    let error = scan_zed_nativepath(&ZedNativeSourceSelection::exact(&composite_path), &mut sink)
        .unwrap_err();
    assert!(matches!(
        error,
        ZedNativePathError::UnsupportedSchema(ref reason)
            if reason.contains("unique ascending BINARY single-column index")
    ));

    let duplicate_path = directory.path().join("duplicates.db");
    let duplicate = Connection::open(&duplicate_path).unwrap();
    duplicate
        .execute_batch(
            "CREATE TABLE threads (
                 id TEXT NOT NULL,
                 summary TEXT NOT NULL,
                 updated_at TEXT NOT NULL,
                 data_type TEXT NOT NULL,
                 data BLOB NOT NULL
             );
             INSERT INTO threads VALUES (
                 'duplicate', 'one', '2026-07-24T12:00:10Z', 'json',
                 CAST('{\"messages\":[]}' AS BLOB)
             );
             INSERT INTO threads VALUES (
                 'duplicate', 'two', '2026-07-24T12:00:10Z', 'json',
                 CAST('{\"messages\":[]}' AS BLOB)
             );",
        )
        .unwrap();
    drop(duplicate);

    let mut sink = CollectingSink::default();
    let error = scan_zed_nativepath(&ZedNativeSourceSelection::exact(&duplicate_path), &mut sink)
        .unwrap_err();
    assert!(matches!(
        error,
        ZedNativePathError::UnsupportedSchema(ref reason)
            if reason.contains("unique ascending BINARY single-column index")
    ));

    let non_binary_path = directory.path().join("non-binary.db");
    let non_binary = Connection::open(&non_binary_path).unwrap();
    non_binary
        .execute_batch(
            "CREATE TABLE threads (
                 id TEXT PRIMARY KEY COLLATE NOCASE,
                 summary TEXT NOT NULL,
                 updated_at TEXT NOT NULL,
                 data_type TEXT NOT NULL,
                 data BLOB NOT NULL
             );",
        )
        .unwrap();
    drop(non_binary);

    let mut sink = CollectingSink::default();
    let error = scan_zed_nativepath(
        &ZedNativeSourceSelection::exact(&non_binary_path),
        &mut sink,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        ZedNativePathError::UnsupportedSchema(ref reason)
            if reason.contains("unique ascending BINARY single-column index")
    ));
}

#[test]
fn duplicate_user_message_ids_remain_distinct_stable_identities() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let connection = new_database(&path);
    insert_thread(
        &connection,
        "duplicate-message-ids",
        None,
        "json",
        &thread(vec![
            user("duplicate", "first"),
            user("duplicate", "second"),
        ]),
    );
    drop(connection);

    let (_, sink) = scan(&path);
    let events = sink.events();

    assert_eq!(events.len(), 2);
    assert_ne!(events[0].identity, events[1].identity);
    assert_eq!(
        events[0].identity.message,
        ZedNativeMessageIdentity::ProviderId {
            value: "duplicate".to_owned(),
            message_ordinal: 0,
        }
    );
    assert_eq!(
        events[1].identity.message,
        ZedNativeMessageIdentity::ProviderId {
            value: "duplicate".to_owned(),
            message_ordinal: 1,
        }
    );
}

#[test]
fn storage_class_is_rejected_before_hydration_and_valid_sibling_survives() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let connection = new_database(&path);
    insert_thread(
        &connection,
        "healthy",
        None,
        "json",
        &thread(vec![user("healthy-user", "healthy")]),
    );
    connection
        .execute(
            "INSERT INTO threads (
                 id, summary, updated_at, data_type, data, created_at
             ) VALUES (
                 'wrong-storage', 'wrong', '2026-07-24T12:00:10Z',
                 'json', 'TEXT NOT BLOB', '2026-07-24T12:00:00Z'
             )",
            [],
        )
        .unwrap();
    drop(connection);

    let (authority, sink) = scan(&path);

    assert_eq!(authority.counters.native_thread_rows, 2);
    assert_eq!(authority.counters.hydration_queries, 1);
    assert_eq!(sink.sessions().len(), 1);
    assert_eq!(sink.events().len(), 1);
    assert_eq!(sink.rejections().len(), 1);
    assert_eq!(
        sink.rejections()[0].kind,
        ZedNativeRejectionKind::InvalidStorageClass
    );
    assert_eq!(
        sink.rejections()[0].thread_id.as_deref(),
        Some("wrong-storage")
    );
}

#[test]
fn null_blob_is_a_row_local_rejection_between_valid_siblings() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE threads (
                 id TEXT PRIMARY KEY,
                 summary TEXT NOT NULL,
                 updated_at TEXT NOT NULL,
                 data_type TEXT NOT NULL,
                 data BLOB,
                 parent_id TEXT,
                 folder_paths TEXT,
                 folder_paths_order TEXT,
                 created_at TEXT
             );",
        )
        .unwrap();
    insert_thread(
        &connection,
        "a-valid",
        None,
        "json",
        &thread(vec![user("a-user", "first valid sibling")]),
    );
    connection
        .execute(
            "INSERT INTO threads (
                 id, summary, updated_at, data_type, data, created_at
             ) VALUES (
                 'b-null', 'null blob', '2026-07-24T12:00:10Z',
                 'json', NULL, '2026-07-24T12:00:00Z'
             )",
            [],
        )
        .unwrap();
    insert_thread(
        &connection,
        "c-valid",
        None,
        "json",
        &thread(vec![user("c-user", "second valid sibling")]),
    );
    drop(connection);

    let (authority, sink) = scan(&path);

    assert_eq!(authority.counters.native_thread_rows, 3);
    assert_eq!(authority.counters.hydration_queries, 2);
    assert_eq!(
        sink.sessions()
            .iter()
            .map(|session| session.thread_id.as_str())
            .collect::<Vec<_>>(),
        vec!["a-valid", "c-valid"]
    );
    assert_eq!(sink.rejections().len(), 1);
    assert_eq!(
        sink.rejections()[0].kind,
        ZedNativeRejectionKind::InvalidStorageClass
    );
    assert_eq!(sink.rejections()[0].thread_id.as_deref(), Some("b-null"));
}

#[test]
fn multi_megabyte_thread_id_is_rejected_before_materialization_and_sibling_survives() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let connection = new_database(&path);
    let oversized_id = format!("oversized-{}", "x".repeat(2 * 1024 * 1024));
    insert_thread(
        &connection,
        &oversized_id,
        None,
        "json",
        &thread(Vec::new()),
    );
    insert_thread(
        &connection,
        "valid-sibling",
        None,
        "json",
        &thread(Vec::new()),
    );
    drop(connection);

    let mut sink = CollectingSink::default();
    let authority =
        complete(scan_zed_nativepath(&ZedNativeSourceSelection::exact(&path), &mut sink).unwrap());

    assert_eq!(authority.counters.native_thread_rows, 2);
    assert_eq!(authority.counters.sessions_retained, 1);
    assert_eq!(authority.counters.rejected_threads, 1);
    assert_eq!(sink.sessions().len(), 1);
    assert_eq!(sink.sessions()[0].thread_id, "valid-sibling");
    assert_eq!(sink.rejections().len(), 1);
    assert_eq!(sink.rejections()[0].sqlite_rowid, 1);
    assert_eq!(sink.rejections()[0].thread_id, None);
    assert_eq!(
        sink.rejections()[0].kind,
        ZedNativeRejectionKind::OversizedEncodedCell
    );
    assert_eq!(
        sink.rejections()[0].reason,
        format!(
            "Zed SQLite row 1 has a thread id exceeding the {ZED_THREAD_ID_MAX_BYTES}-byte limit"
        )
    );
    assert!(sink.rejections()[0].reason.len() < 256);
    assert!(sink.pages.iter().all(|page| {
        page.row_count() <= ZED_NATIVE_PAGE_MAX_ROWS
            && page.estimated_bytes <= ZED_NATIVE_PAGE_MAX_BYTES
    }));
}

#[test]
fn invalid_compression_and_oversized_decompression_are_typed_record_rejections() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let connection = new_database(&path);
    insert_thread(
        &connection,
        "healthy",
        None,
        "json",
        &thread(vec![user("healthy-user", "healthy")]),
    );
    connection
        .execute(
            "INSERT INTO threads (
                 id, summary, updated_at, data_type, data, created_at
             ) VALUES (
                 'bad-zstd', 'bad', '2026-07-24T12:00:10Z',
                 'zstd', X'6e6f742d612d7a7374642d6672616d65',
                 '2026-07-24T12:00:00Z'
             )",
            [],
        )
        .unwrap();
    let oversized = thread(vec![user(
        "oversized-user",
        &"x".repeat(MAX_PROVIDER_SQLITE_VALUE_BYTES + 1_024),
    )]);
    insert_thread(&connection, "oversized-zstd", None, "zstd", &oversized);
    drop(connection);

    let (authority, sink) = scan(&path);

    assert_eq!(authority.counters.native_thread_rows, 3);
    assert_eq!(authority.counters.sessions_retained, 1);
    assert_eq!(authority.counters.rejected_threads, 2);
    assert_eq!(
        sink.rejections()
            .iter()
            .map(|rejection| rejection.kind)
            .collect::<Vec<_>>(),
        vec![
            ZedNativeRejectionKind::InvalidCompression,
            ZedNativeRejectionKind::OversizedDecompression
        ]
    );
}

#[test]
fn malformed_json_and_unknown_encoding_are_row_local() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let connection = new_database(&path);
    insert_thread(
        &connection,
        "healthy",
        None,
        "json",
        &thread(vec![user("healthy-user", "healthy")]),
    );
    connection
        .execute(
            "INSERT INTO threads (
                 id, summary, updated_at, data_type, data, created_at
             ) VALUES (
                 'malformed', 'malformed', '2026-07-24T12:00:10Z',
                 'json', CAST('{\"messages\":[' AS BLOB),
                 '2026-07-24T12:00:00Z'
             )",
            [],
        )
        .unwrap();
    drop(connection);

    let connection = Connection::open(&path).unwrap();
    insert_thread(
        &connection,
        "unknown-encoding",
        None,
        "brotli",
        &thread(vec![user("unknown", "must fail")]),
    );
    drop(connection);

    let (authority, sink) = scan(&path);
    assert_eq!(authority.counters.native_thread_rows, 3);
    assert_eq!(authority.counters.sessions_retained, 1);
    assert_eq!(sink.sessions()[0].thread_id, "healthy");
    assert_eq!(
        sink.rejections()
            .iter()
            .map(|rejection| rejection.kind)
            .collect::<Vec<_>>(),
        vec![
            ZedNativeRejectionKind::MalformedJson,
            ZedNativeRejectionKind::UnsupportedEncoding,
        ]
    );
}

#[test]
fn oversized_unknown_encoding_diagnostic_is_bounded_and_siblings_survive() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let connection = new_database(&path);
    insert_thread(
        &connection,
        "a-valid-before",
        None,
        "json",
        &thread(vec![user("before", "before")]),
    );
    let oversized_encoding = format!(
        "bad\n\0{}",
        "x".repeat(ZED_NATIVE_PAGE_MAX_BYTES.saturating_add(1_024))
    );
    let oversized_encoding_bytes = oversized_encoding.len();
    assert!(oversized_encoding_bytes > ZED_NATIVE_PAGE_MAX_BYTES);
    assert!(oversized_encoding_bytes < MAX_PROVIDER_SQLITE_VALUE_BYTES);
    connection
        .execute(
            "INSERT INTO threads (
                 id, summary, updated_at, data_type, data, created_at
             ) VALUES (
                 'm-oversized-encoding', 'oversized encoding',
                 '2026-07-24T12:00:10Z', ?1, ?2,
                 '2026-07-24T12:00:00Z'
             )",
            params![oversized_encoding, b"{}".as_slice()],
        )
        .unwrap();
    insert_thread(
        &connection,
        "z-valid-after",
        None,
        "json",
        &thread(vec![user("after", "after")]),
    );
    drop(connection);

    let (authority, sink) = scan(&path);

    assert_eq!(authority.counters.native_thread_rows, 3);
    assert_eq!(authority.counters.sessions_retained, 2);
    assert_eq!(authority.counters.rejected_threads, 1);
    assert_eq!(
        sink.sessions()
            .iter()
            .map(|session| session.thread_id.as_str())
            .collect::<Vec<_>>(),
        vec!["a-valid-before", "z-valid-after"]
    );
    let rejections = sink.rejections();
    let [rejection] = rejections.as_slice() else {
        panic!("expected exactly one oversized-encoding rejection");
    };
    assert_eq!(rejection.kind, ZedNativeRejectionKind::UnsupportedEncoding);
    assert!(rejection.reason.len() < 512);
    assert!(rejection
        .reason
        .chars()
        .all(|character| !character.is_control()));
    assert!(rejection
        .reason
        .contains(&oversized_encoding_bytes.to_string()));
}

#[test]
fn committed_wal_generation_is_snapshotted_without_mutating_provider_db_or_wal() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let connection = Connection::open(&path).unwrap();
    connection
        .pragma_update(None, "journal_mode", "WAL")
        .unwrap();
    connection
        .pragma_update(None, "wal_autocheckpoint", 0)
        .unwrap();
    create_schema(&connection);
    insert_thread(
        &connection,
        "thread-main",
        None,
        "json",
        &thread(vec![user("before", "before wal")]),
    );
    connection
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
        .unwrap();
    update_thread(
        &connection,
        "thread-main",
        "zstd",
        &thread(vec![user("after", "committed only in wal")]),
    );
    let wal_path = PathBuf::from(format!("{}-wal", path.display()));
    assert!(wal_path.metadata().unwrap().len() > 32);
    let database_before = fs::read(&path).unwrap();
    let wal_before = fs::read(&wal_path).unwrap();

    let (authority, sink) = scan(&path);

    assert_eq!(sink.events()[0].body, "committed only in wal");
    assert_eq!(sink.sessions()[0].encoding, dto::ZedNativeEncoding::Zstd);
    assert!(authority.snapshot_revision.contains("wal=length="));
    assert_eq!(fs::read(&path).unwrap(), database_before);
    assert_eq!(fs::read(&wal_path).unwrap(), wal_before);
    drop(connection);
}

#[test]
fn source_mutation_after_scan_is_reported_incomplete() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let connection = new_database(&path);
    insert_thread(
        &connection,
        "thread-main",
        None,
        "json",
        &thread(vec![user("one", "one")]),
    );
    drop(connection);

    let mut sink = CollectingSink::default();
    let selection = ZedNativeSourceSelection::exact(&path);
    let outcome = scan_zed_nativepath_with_finalizer(&selection, &mut sink, || {
        let connection = Connection::open(&path)?;
        connection.execute(
            "UPDATE threads SET summary='changed after snapshot' WHERE id='thread-main'",
            [],
        )?;
        Ok(())
    })
    .unwrap();
    let ZedNativeScanOutcome::Incomplete(incomplete) = outcome else {
        panic!("mutated live source must not produce an authority fence");
    };
    assert_eq!(
        incomplete.reason,
        ZedNativeIncompleteReason::SourceChangedAfterScan
    );
    assert!(incomplete.pages_emitted > 0);
}

#[test]
fn output_results_are_excluded_before_retained_body_hash_preview_and_touch_creation() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let output_sentinel = "CTX-ZED-RESULT-ONLY-SECRET";
    let output_path = "/workspace/result-only/secret.txt";
    let input_path = "src/retained-input.rs";
    let output_body = format!("{output_sentinel}\noutput_path={output_path}");
    let connection = new_database(&path);
    insert_thread(
        &connection,
        "output-thread",
        None,
        "zstd",
        &thread(vec![
            user("request", "please run the tool"),
            output_message("call-1", input_path, &output_body, false),
        ]),
    );
    drop(connection);

    let (first, sink) = scan(&path);

    assert_eq!(first.counters.output.native_results_observed, 1);
    assert_eq!(first.counters.output.native_results_success, 1);
    assert_eq!(first.counters.output.native_results_failure, 0);
    assert_eq!(first.counters.output.native_results_unknown, 0);
    assert_eq!(
        first.counters.output.result_body_bytes_observed,
        output_body.len() as u64
    );
    assert_eq!(first.counters.output.retained_result_body_bytes, 0);
    assert_eq!(
        first.counters.output.retained_result_body_strings_allocated,
        0
    );
    assert_eq!(first.counters.output.result_events_created, 0);
    assert_eq!(first.counters.output.result_hashes_created, 0);
    assert_eq!(first.counters.output.result_previews_created, 0);
    assert_eq!(first.counters.output.result_file_touches_created, 0);
    assert_eq!(first.counters.output.result_fts_documents_created, 0);
    assert_eq!(first.counters.output.result_handoffs_created, 0);
    let retained = sink
        .events()
        .iter()
        .flat_map(|event| {
            [
                event.body.as_str(),
                event.preview.as_str(),
                event.content_hash.as_str(),
            ]
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!retained.contains(output_sentinel));
    assert!(!retained.contains(output_path));
    let touches = sink
        .events()
        .iter()
        .flat_map(|event| event.safe_file_touches.iter())
        .collect::<Vec<_>>();
    assert_eq!(touches, vec![&input_path.to_owned()]);
    assert!(!touches.iter().any(|path| path.contains("result-only")));
    assert_eq!(
        sink.events()
            .iter()
            .map(|event| event.event_type)
            .collect::<Vec<_>>(),
        vec![
            ctx_history_core::EventType::Message,
            ctx_history_core::EventType::ToolCall
        ]
    );

    let connection = Connection::open(&path).unwrap();
    update_thread(
        &connection,
        "output-thread",
        "zstd",
        &thread(vec![
            user("request", "please run the tool"),
            output_message(
                "call-1",
                input_path,
                "a different excluded output body and /tmp/different-result.txt",
                false,
            ),
        ]),
    );
    drop(connection);
    let (second, _) = scan(&path);
    assert_ne!(
        first.source_integrity_digest,
        second.source_integrity_digest
    );
    assert_eq!(first.core_generation_digest, second.core_generation_digest);
}

#[test]
fn tool_result_classification_requires_explicit_consistent_success_evidence() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let connection = new_database(&path);
    let success = output_message("success-call", "src/success.rs", "success output", false);
    let failure = output_message("failure-call", "src/failure.rs", "failure output", true);
    let mut contradictory =
        output_message("unknown-call", "src/unknown.rs", "unknown output", false);
    contradictory["Agent"]["tool_results"]["unknown-call"]["output"]["status"] =
        Value::String("error".to_owned());
    insert_thread(
        &connection,
        "outcomes",
        None,
        "json",
        &thread(vec![success, failure, contradictory]),
    );
    drop(connection);

    let (authority, sink) = scan(&path);

    assert_eq!(authority.counters.output.native_results_observed, 3);
    assert_eq!(authority.counters.output.native_results_success, 1);
    assert_eq!(authority.counters.output.native_results_failure, 1);
    assert_eq!(authority.counters.output.native_results_unknown, 1);
    assert_eq!(authority.counters.output.retained_result_body_bytes, 0);
    assert_eq!(sink.events().len(), 3);
    assert!(sink.events().iter().all(|event| {
        event.event_type == ctx_history_core::EventType::ToolCall
            && !event.body.contains("output")
            && !event.preview.contains("output")
    }));
}

#[test]
fn duplicate_key_and_non_object_results_are_discarded_without_losing_conversation() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let connection = new_database(&path);
    let raw = r#"{
        "version": "0.3.0",
        "updated_at": "2026-07-24T12:00:10Z",
        "messages": [
            {"User": {"id": "request", "content": [{"Text": "valid user"}]}},
            {"Agent": {
                "content": [
                    {"Text": "valid assistant"},
                    {"ToolResult": 17},
                    {"ToolResult": {
                        "is_error": false,
                        "is_error": true,
                        "content": "RESULT-SECRET-A",
                        "content": {"nested": "RESULT-SECRET-B"},
                        "output": {"status": "ok"}
                    }}
                ],
                "tool_results": {
                    "numeric": 42,
                    "short": "RESULT-SECRET-C",
                    "duplicate": {
                        "is_error": false,
                        "content": "RESULT-SECRET-D",
                        "output": {"status": "ok", "status": "error"}
                    }
                }
            }},
            {"Agent": {
                "content": [{"Text": "valid scalar-container assistant"}],
                "tool_results": 42
            }}
        ]
    }"#;
    connection
        .execute(
            "INSERT INTO threads (
                 id, summary, updated_at, data_type, data, created_at
             ) VALUES (
                 'adversarial-results', 'results', '2026-07-24T12:00:10Z',
                 'json', ?1, '2026-07-24T12:00:00Z'
             )",
            [raw.as_bytes()],
        )
        .unwrap();
    drop(connection);

    let (authority, sink) = scan(&path);

    assert_eq!(authority.counters.sessions_retained, 1);
    assert_eq!(authority.counters.rejected_threads, 0);
    assert_eq!(authority.counters.output.native_results_observed, 5);
    assert_eq!(authority.counters.output.native_results_success, 0);
    assert_eq!(authority.counters.output.native_results_failure, 0);
    assert_eq!(authority.counters.output.native_results_unknown, 5);
    assert_eq!(authority.counters.output.retained_result_body_bytes, 0);
    assert_eq!(
        authority
            .counters
            .output
            .retained_result_body_strings_allocated,
        0
    );
    assert_eq!(
        sink.events()
            .iter()
            .map(|event| event.body.as_str())
            .collect::<Vec<_>>(),
        vec![
            "valid user",
            "valid assistant",
            "valid scalar-container assistant"
        ]
    );
    let retained = sink
        .events()
        .iter()
        .flat_map(|event| {
            [
                event.body.as_str(),
                event.preview.as_str(),
                event.content_hash.as_str(),
            ]
        })
        .collect::<Vec<_>>()
        .join("\n");
    for sentinel in [
        "RESULT-SECRET-A",
        "RESULT-SECRET-B",
        "RESULT-SECRET-C",
        "RESULT-SECRET-D",
    ] {
        assert!(!retained.contains(sentinel));
    }
}

#[test]
fn local_scale_scan_is_bounded_and_never_materializes_result_surfaces() {
    const THREADS: usize = 80;
    const MESSAGES_PER_THREAD: usize = 100;

    let directory = tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let mut connection = new_database(&path);
    let transaction = connection.transaction().unwrap();
    for thread_index in 0..THREADS {
        let id = format!("thread-{thread_index:04}");
        let mut messages = Vec::with_capacity(MESSAGES_PER_THREAD);
        for message_index in 0..MESSAGES_PER_THREAD {
            let sequence = thread_index * MESSAGES_PER_THREAD + message_index;
            if message_index % 10 == 9 {
                messages.push(output_message(
                    &format!("call-{sequence:08}"),
                    &format!("src/scale-{sequence:08}.rs"),
                    &format!("CTX-ZED-SCALE-OUTPUT-{sequence:08} /result-only/{sequence:08}.txt"),
                    false,
                ));
            } else if message_index % 2 == 0 {
                messages.push(user(
                    &format!("user-{sequence:08}"),
                    &format!("user message {sequence:08}"),
                ));
            } else {
                messages.push(assistant(&format!("assistant message {sequence:08}")));
            }
        }
        insert_thread(
            &transaction,
            &id,
            (thread_index > 0).then_some("thread-0000"),
            if thread_index % 2 == 0 {
                "json"
            } else {
                "zstd"
            },
            &thread(messages),
        );
    }
    transaction.commit().unwrap();
    drop(connection);

    let (authority, sink) = scan(&path);

    assert_eq!(authority.counters.native_thread_rows, THREADS as u64);
    assert_eq!(
        authority.counters.retained_events,
        (THREADS * MESSAGES_PER_THREAD) as u64
    );
    assert_eq!(
        authority.counters.output.native_results_observed,
        (THREADS * (MESSAGES_PER_THREAD / 10)) as u64
    );
    assert_eq!(authority.counters.output.result_events_created, 0);
    assert_eq!(authority.counters.output.result_hashes_created, 0);
    assert_eq!(authority.counters.output.result_previews_created, 0);
    assert_eq!(authority.counters.output.result_file_touches_created, 0);
    assert!(sink.pages.len() > 1);
    assert!(sink.pages.iter().all(|page| {
        page.row_count() <= ZED_NATIVE_PAGE_MAX_ROWS
            && page.estimated_bytes <= ZED_NATIVE_PAGE_MAX_BYTES
    }));
    assert!(sink.events().iter().all(|event| {
        !event.body.contains("CTX-ZED-SCALE-OUTPUT")
            && !event.preview.contains("CTX-ZED-SCALE-OUTPUT")
            && event
                .safe_file_touches
                .iter()
                .all(|path| !path.contains("result-only"))
    }));
}

#[test]
fn large_thread_id_event_amplification_streams_through_bounded_pages() {
    const MESSAGES: usize = ZED_NATIVE_PAGE_MAX_ROWS + 1;

    let directory = tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let connection = new_database(&path);
    let thread_id = format!("amplification-{}", "t".repeat(4 * 1024));
    assert!(thread_id.len().saturating_mul(MESSAGES) > ZED_NATIVE_PAGE_MAX_BYTES);
    let messages = (0..MESSAGES)
        .map(|_| assistant("bounded"))
        .collect::<Vec<_>>();
    insert_thread(&connection, &thread_id, None, "json", &thread(messages));
    drop(connection);

    let mut sink = DiscardingSink::default();
    let authority =
        complete(scan_zed_nativepath(&ZedNativeSourceSelection::exact(&path), &mut sink).unwrap());

    assert_eq!(authority.counters.native_thread_rows, 1);
    assert_eq!(authority.counters.sessions_retained, 1);
    assert_eq!(authority.counters.retained_events, MESSAGES as u64);
    assert_eq!(sink.rows, MESSAGES as u64 + 1);
    assert!(sink.pages > 1);
    assert!(sink.max_page_rows <= ZED_NATIVE_PAGE_MAX_ROWS);
    assert!(sink.max_page_bytes <= ZED_NATIVE_PAGE_MAX_BYTES);
}

#[test]
fn more_than_one_publication_page_uses_bounded_output_index() {
    const THREADS: usize = ZED_NATIVE_PAGE_MAX_ROWS + 1;

    let directory = tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let mut connection = new_database(&path);
    let transaction = connection.transaction().unwrap();
    for thread_index in (0..THREADS).rev() {
        insert_thread(
            &transaction,
            &format!("thread-{thread_index:05}"),
            None,
            "json",
            &thread(Vec::new()),
        );
    }
    transaction.commit().unwrap();
    drop(connection);

    let mut sink = DiscardingSink::default();
    let authority =
        complete(scan_zed_nativepath(&ZedNativeSourceSelection::exact(&path), &mut sink).unwrap());

    assert_eq!(authority.counters.native_thread_rows, THREADS as u64);
    assert_eq!(authority.counters.sessions_retained, THREADS as u64);
    assert!(authority.counters.candidate_page_queries < 32);
    assert!(sink.pages > 1);
    assert_eq!(sink.rows, THREADS as u64);
    assert!(sink.max_page_rows <= ZED_NATIVE_PAGE_MAX_ROWS);
    assert!(sink.max_page_bytes <= ZED_NATIVE_PAGE_MAX_BYTES);

    let index_path = authority.output_index.path().to_path_buf();
    let exact = Connection::open_with_flags(
        &index_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap();
    let indexed_threads: u64 = exact
        .query_row("select count(*) from output_threads", [], |row| row.get(0))
        .unwrap();
    assert_eq!(indexed_threads, THREADS as u64);
    drop(exact);
    drop(authority);
    assert!(!index_path.exists());
}

#[test]
fn nativepath_store_publication_is_core_first_resumable_and_idempotent() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let connection = new_database(&path);
    insert_thread(
        &connection,
        "root-thread",
        None,
        "json",
        &thread(vec![user("root-user", "root prompt")]),
    );
    insert_thread(
        &connection,
        "child-thread",
        Some("root-thread"),
        "json",
        &thread(vec![output_message(
            "call-child",
            "src/child.rs",
            "CTX-ZED-PRO-ONLY-SENTINEL",
            false,
        )]),
    );
    drop(connection);

    let store_path = directory.path().join("store.sqlite");
    let mut store = ctx_history_store::Store::open(&store_path).unwrap();
    let context = crate::ProviderAdapterContext {
        machine_id: "zed-nativepath-test-machine".to_owned(),
        source_path: Some(path.clone()),
        source_root: None,
        imported_at: chrono::DateTime::parse_from_rfc3339("2026-07-25T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc),
    };
    let options = crate::ProviderImportOptions {
        capture_work_limit: crate::CaptureWorkLimit::OneSafeGroup,
        ..crate::ProviderImportOptions::default()
    };

    let first = import_zed_nativepath(&path, &mut store, context.clone(), options.clone()).unwrap();
    assert!(first.work_remaining);
    assert_eq!(store.list_sessions().unwrap().len(), 2);
    drop(store);

    let mut store = ctx_history_store::Store::open(&store_path).unwrap();
    let second =
        import_zed_nativepath(&path, &mut store, context.clone(), options.clone()).unwrap();
    assert!(!second.work_remaining);
    let sessions = store.list_sessions().unwrap();
    let root = sessions
        .iter()
        .find(|session| session.external_session_id.as_deref() == Some("root-thread"))
        .unwrap();
    let child = sessions
        .iter()
        .find(|session| session.external_session_id.as_deref() == Some("child-thread"))
        .unwrap();
    assert_eq!(child.parent_session_id, Some(root.id));
    assert_eq!(child.root_session_id, Some(root.id));
    let child_events = store.events_for_session(child.id).unwrap();
    assert_eq!(child_events.len(), 1);
    assert_eq!(
        child_events[0].event_type,
        ctx_history_core::EventType::ToolCall
    );
    assert!(!serde_json::to_string(&child_events[0].payload)
        .unwrap()
        .contains("CTX-ZED-PRO-ONLY-SENTINEL"));

    let replay = import_zed_nativepath(&path, &mut store, context, options).unwrap();
    assert_eq!(replay.work_result(), crate::ProviderImportWorkResult::NoOp);
    assert_eq!(store.list_sessions().unwrap().len(), 2);
    assert_eq!(store.events_for_session(child.id).unwrap().len(), 1);
}

#[test]
fn nativepath_missing_source_retires_once_without_deleting_core_history() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let connection = new_database(&path);
    insert_thread(
        &connection,
        "retained-thread",
        None,
        "json",
        &thread(vec![user("retained-user", "retained prompt")]),
    );
    drop(connection);

    let mut store = ctx_history_store::Store::open(directory.path().join("store.sqlite")).unwrap();
    let context = crate::ProviderAdapterContext {
        machine_id: "zed-retirement-test-machine".to_owned(),
        source_path: Some(path.clone()),
        source_root: None,
        imported_at: chrono::DateTime::parse_from_rfc3339("2026-07-25T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc),
    };
    import_zed_nativepath(
        &path,
        &mut store,
        context.clone(),
        crate::ProviderImportOptions::default(),
    )
    .unwrap();
    fs::remove_file(&path).unwrap();

    let retired = import_zed_nativepath(
        &path,
        &mut store,
        context.clone(),
        crate::ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(
        retired.work_result(),
        crate::ProviderImportWorkResult::Changed
    );
    assert_eq!(store.list_sessions().unwrap().len(), 1);

    let replay = import_zed_nativepath(
        &path,
        &mut store,
        context,
        crate::ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(replay.work_result(), crate::ProviderImportWorkResult::NoOp);
    assert_eq!(store.list_sessions().unwrap().len(), 1);
}

#[test]
fn later_pro_activation_replays_exact_outputs_without_republishing_core() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let connection = new_database(&path);
    insert_thread(
        &connection,
        "output-thread",
        None,
        "json",
        &thread(vec![output_message(
            "output-call",
            "src/output.rs",
            "CTX-ZED-LATER-PRO-SENTINEL",
            false,
        )]),
    );
    drop(connection);

    let mut store = ctx_history_store::Store::open(directory.path().join("store.sqlite")).unwrap();
    let context = crate::ProviderAdapterContext {
        machine_id: "zed-output-replay-test-machine".to_owned(),
        source_path: Some(path.clone()),
        source_root: None,
        imported_at: chrono::DateTime::parse_from_rfc3339("2026-07-25T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc),
    };
    import_zed_nativepath(
        &path,
        &mut store,
        context.clone(),
        crate::ProviderImportOptions::default(),
    )
    .unwrap();
    let session = store
        .session_by_external_session(ctx_history_core::CaptureProvider::Zed, "output-thread")
        .unwrap()
        .unwrap();
    let core = store.events_for_session(session.id).unwrap();
    assert_eq!(core.len(), 1);
    assert!(!serde_json::to_string(&core[0].payload)
        .unwrap()
        .contains("CTX-ZED-LATER-PRO-SENTINEL"));

    let sink = Arc::new(RecordingProSink::default());
    let replay_options = crate::ProviderImportOptions {
        import_profile: crate::ImportProfile::ProReplayOnly(sink.clone()),
        ..crate::ProviderImportOptions::default()
    };
    let replay =
        import_zed_nativepath(&path, &mut store, context.clone(), replay_options.clone()).unwrap();
    assert_eq!(replay.work_result(), crate::ProviderImportWorkResult::NoOp);
    let output = sink.content.lock().unwrap();
    assert_eq!(output.len(), 1);
    assert!(String::from_utf8_lossy(&output[0]).contains("CTX-ZED-LATER-PRO-SENTINEL"));
    drop(output);
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 1);

    import_zed_nativepath(&path, &mut store, context, replay_options).unwrap();
    assert_eq!(sink.content.lock().unwrap().len(), 1);
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 1);
}

#[test]
fn pro_failure_marks_only_output_behind_after_core_commit() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let connection = new_database(&path);
    insert_thread(
        &connection,
        "pro-failure-thread",
        None,
        "json",
        &thread(vec![output_message(
            "failing-call",
            "src/failing.rs",
            "CTX-ZED-FAILED-PRO-SENTINEL",
            false,
        )]),
    );
    drop(connection);

    let mut store = ctx_history_store::Store::open(directory.path().join("store.sqlite")).unwrap();
    let sink = Arc::new(FailingProSink::default());
    let summary = import_zed_nativepath(
        &path,
        &mut store,
        crate::ProviderAdapterContext {
            machine_id: "zed-pro-failure-test-machine".to_owned(),
            source_path: Some(path.clone()),
            source_root: None,
            imported_at: chrono::DateTime::parse_from_rfc3339("2026-07-25T12:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        },
        crate::ProviderImportOptions {
            import_profile: crate::ImportProfile::CoreAndPro(sink.clone()),
            ..crate::ProviderImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(
        summary.work_result(),
        crate::ProviderImportWorkResult::Changed
    );
    assert_eq!(sink.behind.load(Ordering::SeqCst), 1);
    let session = store
        .session_by_external_session(ctx_history_core::CaptureProvider::Zed, "pro-failure-thread")
        .unwrap()
        .unwrap();
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 1);
}
