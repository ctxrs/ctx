use std::fs;

use ctx_history_core::{CaptureProvider, CoreRecord};
use ctx_history_index::{VerifiedIndex, WriterOptions};
use rusqlite::{params, Connection};
use serde_json::{json, Value};

use crate::{
    refresh_source_backed_generation, register_shelley_source_backed_route, ProviderCatalogSupport,
    ProviderImportSupport, ProviderSource, ProviderSourceKind, ProviderSourceStatus,
    SourceBackedProviderRegistry,
};

use super::super::scanner::{
    reset_shelley_query_counters, shelley_query_counters, ShelleyQueryCounters,
};
use super::*;

fn create_fixture(root: &Path, text: &str) -> PathBuf {
    fs::create_dir_all(root).unwrap();
    let database = root.join("shelley.db");
    let conn = Connection::open(&database).unwrap();
    conn.execute_batch(
        "create table conversations (
             conversation_id text not null,
             user_initiated integer,
             created_at text,
             updated_at text,
             cwd text,
             parent_conversation_id text
         );
         create table messages (
             message_id text not null,
             conversation_id text not null,
             sequence_id integer not null,
             type text not null,
             user_data text,
             created_at text
         );
         create index messages_conversation_sequence
             on messages(conversation_id collate binary, sequence_id collate binary);",
    )
    .unwrap();
    conn.execute(
        "insert into conversations (
             conversation_id, user_initiated, created_at, updated_at, cwd,
             parent_conversation_id
         ) values (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            "conversation-1",
            1_i64,
            "2026-07-28T20:00:00Z",
            "2026-07-28T20:01:00Z",
            "/workspace/project",
            Option::<String>::None,
        ],
    )
    .unwrap();
    conn.execute(
        "insert into messages (
             message_id, conversation_id, sequence_id, type, user_data, created_at
         ) values (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            "message-1",
            "conversation-1",
            7_i64,
            "user",
            text,
            "2026-07-28T20:00:30Z"
        ],
    )
    .unwrap();
    database
}

fn insert_fixture_messages(database: &Path, through: i64, body_bytes: usize) {
    let mut connection = Connection::open(database).unwrap();
    let transaction = connection.transaction().unwrap();
    for rowid in 2..=through {
        transaction
            .execute(
                "insert into messages (
                     message_id, conversation_id, sequence_id, type, user_data, created_at
                 ) values (?1, 'conversation-1', ?2, 'user', ?3, ?4)",
                params![
                    format!("message-{rowid}"),
                    rowid + 6,
                    format!("message-{rowid}-{}", "x".repeat(body_bytes)),
                    format!("2026-07-28T20:00:{:02}Z", rowid % 60),
                ],
            )
            .unwrap();
    }
    transaction.commit().unwrap();
}

fn shelley_registry(data_root: &Path, cwd: &Path, database: &Path) -> SourceBackedProviderRegistry {
    let source = ProviderSource {
        provider: CaptureProvider::Shelley,
        path: database.to_path_buf(),
        exists: true,
        source_format: SHELLEY_SQLITE_SOURCE_FORMAT,
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        status: ProviderSourceStatus::Available,
        unsupported_reason: None,
    };
    let mut registry = SourceBackedProviderRegistry::new();
    register_shelley_source_backed_route(&mut registry, source, data_root, cwd).unwrap();
    registry
}

fn writer_options() -> WriterOptions {
    WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    }
}

#[cfg(target_os = "linux")]
#[test]
fn source_backed_open_does_not_follow_leaf_swap_after_authorization() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = create_fixture(&temp.path().join("live"), "expected");
    let attacker = create_fixture(&temp.path().join("attacker"), "attacker");
    let original = temp.path().join("original.db");

    let result = open_root_authorized_snapshot_with_hook(
        crate::test_provider_sqlite_data_root(),
        &path,
        || {
            fs::rename(&path, &original).unwrap();
            fs::rename(&attacker, &path).unwrap();
        },
    );
    assert!(matches!(
        result,
        Err(ShelleySourceBackedError::Capture(
            CaptureError::InvalidProviderTranscriptPath { .. },
        )) | Err(ShelleySourceBackedError::SqliteSource(
            SqliteSourceAccessError::SourceChanged,
        ))
    ));
}

#[test]
fn active_wal_scan_reads_latest_rows_without_persistent_source_writes() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = create_fixture(temp.path(), "before WAL");
    let writer = Connection::open(&path).unwrap();
    writer.pragma_update(None, "journal_mode", "wal").unwrap();
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    writer
        .execute_batch("pragma wal_checkpoint(truncate)")
        .unwrap();
    writer
        .execute(
            "update messages set user_data = ?1 where message_id = 'message-1'",
            ["Shelley active WAL sentinel"],
        )
        .unwrap();
    let before = sqlite_persistent_bytes(&path);
    let adapter = discover_shelley_source_backed_exact_cwd(
        crate::test_provider_sqlite_data_root(),
        temp.path(),
    )
    .unwrap()
    .unwrap();
    let (documents, _) = drain(&adapter);
    assert!(documents
        .iter()
        .any(|document| body(document).contains("Shelley active WAL sentinel")));
    assert_eq!(sqlite_persistent_bytes(&path), before);
    drop(writer);
}

fn drain(adapter: &ShelleySourceBackedAdapter) -> (Vec<CoreRecord>, ShelleySourceBackedReceipt) {
    let mut scan = adapter.start_scan().unwrap();
    let mut documents = Vec::new();
    while let Some(page) = scan.next_page().unwrap() {
        assert!(page.documents.len() <= SHELLEY_PAGE_MAX_UNITS);
        documents.extend(page.documents);
    }
    let receipt = scan.finish().unwrap();
    (documents, receipt)
}

fn body(record: &CoreRecord) -> &str {
    record.content.normalized_body.as_deref().unwrap()
}

fn sqlite_persistent_bytes(path: &Path) -> Vec<Vec<u8>> {
    // Stock WAL readers may update volatile SHM reader marks.
    ["", "-wal"]
        .into_iter()
        .map(|suffix| {
            let mut component = path.as_os_str().to_os_string();
            component.push(suffix);
            fs::read(PathBuf::from(component)).unwrap()
        })
        .collect()
}

fn shelley_query_shape(counters: ShelleyQueryCounters) -> [u64; 6] {
    [
        counters.candidate_set_reads,
        counters.message_set_reads,
        counters.conversation_candidate_set_reads,
        counters.conversation_set_reads,
        counters.relationship_set_reads,
        counters.rows_projected,
    ]
}

#[test]
fn shelley_scan_queries_are_bounded_by_row_sets_with_exact_core_replay() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = create_fixture(temp.path(), "message-1");
    let connection = Connection::open(&database).unwrap();
    let collision_ids = ["collision-507bc35f-9bs", "collision-a084ee59-mhm"];
    let released_bucket = crate::provider::normalization::text_id_index(
        &format!("conversation-1:{}", collision_ids[0]),
        4_096,
    );
    assert_eq!(
        crate::provider::normalization::text_id_index(
            &format!("conversation-1:{}", collision_ids[1]),
            4_096,
        ),
        released_bucket
    );
    for rowid in 2..=40_i64 {
        let (message_id, sequence_id) = match rowid {
            2 => (collision_ids[0].to_owned(), 8),
            3 => (collision_ids[1].to_owned(), 8),
            _ => (format!("message-{rowid}"), rowid + 6),
        };
        connection
            .execute(
                "insert into messages (
                     message_id, conversation_id, sequence_id, type, user_data, created_at
                ) values (?1, 'conversation-1', ?2, 'user', ?3, ?4)",
                params![
                    message_id,
                    sequence_id,
                    format!("message-{rowid}"),
                    format!("2026-07-28T20:{rowid:02}:30Z"),
                ],
            )
            .unwrap();
    }
    drop(connection);
    let adapter = discover_shelley_source_backed_exact_cwd(
        crate::test_provider_sqlite_data_root(),
        temp.path(),
    )
    .unwrap()
    .unwrap();

    reset_shelley_query_counters();
    let (documents, receipt) = drain(&adapter);
    assert_eq!(documents.len(), 40);
    assert_eq!(receipt.certificate.counts().complete_records, 40);
    assert_eq!(
        documents.iter().map(body).collect::<Vec<_>>(),
        (1..=40)
            .map(|rowid| format!("message-{rowid}"))
            .collect::<Vec<_>>()
    );
    assert_eq!(documents[1].event_sequence, 8 * 4_096 + released_bucket);
    assert_ne!(documents[2].event_sequence, documents[1].event_sequence);
    assert_ne!(documents[2].event_sequence & (1_u64 << 63), 0);
    assert!(documents
        .iter()
        .all(|record| record.native_event_id.is_some()));
    let cold_work = shelley_query_counters();
    assert_eq!(shelley_query_shape(cold_work), [4, 3, 3, 3, 6, 40]);
    assert_eq!(cold_work.pages_emitted, 1);
    assert_eq!(cold_work.peak_buffered_rows, 40);
    assert!(cold_work.peak_buffered_bytes > 0);
    assert_eq!(cold_work.hydration_snapshot_opens, 0);

    reset_shelley_query_counters();
    let (replay, replay_receipt) = drain(&adapter);
    assert_eq!(
        replay
            .iter()
            .map(|document| (
                document.event_id,
                document.event_sequence,
                document.native_event_id.clone(),
                body(document).to_owned(),
            ))
            .collect::<Vec<_>>(),
        documents
            .iter()
            .map(|document| (
                document.event_id,
                document.event_sequence,
                document.native_event_id.clone(),
                body(document).to_owned(),
            ))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        replay_receipt.certificate.content_digest(),
        receipt.certificate.content_digest()
    );
    let replay_work = shelley_query_counters();
    assert_eq!(shelley_query_shape(replay_work), [4, 3, 3, 3, 6, 40]);
    assert_eq!(replay_work.pages_emitted, 1);
    assert_eq!(replay_work.peak_buffered_rows, 40);
    assert!(replay_work.peak_buffered_bytes > 0);
    assert_eq!(replay_work.hydration_snapshot_opens, 0);
}

#[test]
fn large_shelley_projection_emits_first_page_with_page_bounded_result_memory() {
    const ROWS: i64 = 4_096;

    let temp = crate::test_support_paths::tempdir().unwrap();
    create_fixture(temp.path(), "message-1");
    insert_fixture_messages(&temp.path().join("shelley.db"), ROWS, 128);
    let adapter = discover_shelley_source_backed_exact_cwd(
        crate::test_provider_sqlite_data_root(),
        temp.path(),
    )
    .unwrap()
    .unwrap();

    reset_shelley_query_counters();
    let mut scan = adapter.start_scan().unwrap();
    let first = scan.next_page().unwrap().unwrap();
    assert_eq!(first.counts.complete_records, SHELLEY_PAGE_MAX_UNITS as u64);
    assert_eq!(first.documents.len(), SHELLEY_PAGE_MAX_UNITS);
    assert!(first.retained_bytes <= SHELLEY_PAGE_MAX_BYTES);
    assert!(scan.sqlite_snapshot.is_some());
    assert!(scan.receipt.is_none());
    let first_work = shelley_query_counters();
    assert_eq!(first_work.rows_projected, SHELLEY_PAGE_MAX_UNITS as u64);
    assert_eq!(first_work.pages_emitted, 1);
    assert_eq!(first_work.peak_buffered_rows, SHELLEY_PAGE_MAX_UNITS as u64);
    assert!(first_work.peak_buffered_bytes <= SHELLEY_PAGE_MAX_BYTES as u64);

    let mut complete_records = first.counts.complete_records;
    while let Some(page) = scan.next_page().unwrap() {
        complete_records += page.counts.complete_records;
    }
    let receipt = scan.finish().unwrap();
    assert_eq!(complete_records, ROWS as u64);
    assert_eq!(receipt.certificate.counts().complete_records, ROWS as u64);
    let complete_work = shelley_query_counters();
    assert_eq!(complete_work.rows_projected, ROWS as u64);
    assert_eq!(
        complete_work.pages_emitted,
        ROWS as u64 / SHELLEY_PAGE_MAX_UNITS as u64
    );
    assert_eq!(
        complete_work.peak_buffered_rows,
        SHELLEY_PAGE_MAX_UNITS as u64
    );
    assert!(complete_work.peak_buffered_bytes <= SHELLEY_PAGE_MAX_BYTES as u64);
}

#[test]
fn shelley_source_backed_cold_exact_and_replacement_keep_identity() {
    const TAIL: &str = "shelleypostsixteenkilobytesentinel";

    let temp = crate::test_support_paths::tempdir().unwrap();
    let tool_input = json!({
        "padding": "x".repeat(17_000),
        "tail": TAIL,
    });
    let native_body = json!({
        "Type": "ContentTypeToolUse",
        "ToolName": "write_file",
        "ToolInput": tool_input,
    })
    .to_string();
    let original = format!("tool call: write_file\ntool input: {tool_input}");
    assert!(original.find(TAIL).unwrap() > 16 * 1_024);
    let database = create_fixture(temp.path(), &native_body);
    let adapter = discover_shelley_source_backed_exact_cwd(
        crate::test_provider_sqlite_data_root(),
        temp.path(),
    )
    .unwrap()
    .unwrap();

    let (cold_documents, cold_receipt) = drain(&adapter);
    assert_eq!(cold_documents.len(), 1);
    let cold = &cold_documents[0];
    assert_eq!(body(cold), original);
    let structured: Value = serde_json::from_str(
        body(cold)
            .strip_prefix("tool call: write_file\ntool input: ")
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        structured.pointer("/tail").and_then(Value::as_str),
        Some(TAIL)
    );
    assert_eq!(cold.provider_session_id.as_deref(), Some("conversation-1"));
    assert_eq!(cold.parent_session_id, None);
    assert_eq!(cold.root_session_id, cold.session_id);
    assert_eq!(cold.branch, None);
    assert_eq!(cold.agent_type, AgentType::Primary.as_str());
    assert!(cold.is_primary);
    assert!(cold.native_event_id.is_some());
    assert!(!serde_json::to_string(cold)
        .unwrap()
        .contains(database.to_string_lossy().as_ref()));
    assert_eq!(
        cold_receipt.certificate.counts(),
        ScannedSourceCounts {
            complete_records: 1,
            retained_records: 1,
            rejected_records: 0,
            ignored_records: 0,
            indexed_documents: 1,
            certified_bytes: cold_receipt.certificate.counts().certified_bytes,
        }
    );
    let replacement = "replacement exact Shelley body";
    Connection::open(&database)
        .unwrap()
        .execute(
            "update messages set user_data = ?1 where rowid = 1",
            [replacement],
        )
        .unwrap();
    let (replacement_documents, replacement_receipt) = drain(&adapter);
    assert_eq!(replacement_documents.len(), 1);
    let replaced = &replacement_documents[0];
    assert_eq!(cold.session_id, replaced.session_id);
    assert_eq!(cold.event_id, replaced.event_id);
    assert_ne!(
        cold_receipt.certificate.content_digest(),
        replacement_receipt.certificate.content_digest()
    );
    assert_eq!(body(replaced), replacement);
    assert_eq!(cold.native_event_id, replaced.native_event_id);
}

#[test]
fn shelley_route_cold_noop_and_rewrite_keep_complete_core_records() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let provider_root = temp.path().join("provider");
    let data_root = temp.path().join("data");
    let index_root = temp.path().join("index");
    let database = create_fixture(&provider_root, "message-1");
    insert_fixture_messages(&database, 40, 32);
    let writer = Connection::open(&database).unwrap();
    writer.pragma_update(None, "journal_mode", "wal").unwrap();
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    writer
        .execute(
            "update messages set user_data = 'message-1-wal' where rowid = 1",
            [],
        )
        .unwrap();
    let persistent_before = sqlite_persistent_bytes(&database);
    let registry = shelley_registry(&data_root, &provider_root, &database);

    reset_shelley_query_counters();
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(cold.commit.indexed_documents, 40);
    assert_eq!(cold.sources.len(), 1);
    let cold_work = shelley_query_counters();
    assert_eq!(shelley_query_shape(cold_work), [4, 3, 3, 3, 6, 40]);
    assert_eq!(cold_work.pages_emitted, 1);
    assert_eq!(cold_work.hydration_snapshot_opens, 0);
    assert_eq!(sqlite_persistent_bytes(&database), persistent_before);

    reset_shelley_query_counters();
    let noop = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(noop.commit.generation_id, cold.commit.generation_id);
    assert_eq!(noop.commit.opstamp, cold.commit.opstamp);
    assert_eq!(noop.sources, cold.sources);
    let noop_work = shelley_query_counters();
    assert_eq!(shelley_query_shape(noop_work), [4, 3, 3, 3, 6, 40]);
    assert_eq!(noop_work.pages_emitted, 1);
    assert_eq!(noop_work.peak_buffered_rows, 40);
    assert!(noop_work.peak_buffered_bytes > 0);
    assert_eq!(noop_work.hydration_snapshot_opens, 0);
    assert_eq!(sqlite_persistent_bytes(&database), persistent_before);

    let verified = VerifiedIndex::open(&index_root).unwrap();
    let source = verified.manifest().sources[0]
        .observation()
        .source()
        .clone();
    let mut events = verified
        .core_source_event_page(&source, None, 40)
        .unwrap()
        .items;
    assert_eq!(events.len(), 40);
    events.reverse();
    assert_eq!(
        events
            .iter()
            .filter(|record| {
                record.core_record.content.normalized_body.as_deref() == Some("message-1-wal")
            })
            .count(),
        1
    );
    assert!(events.iter().all(|record| {
        record
            .core_record
            .content
            .normalized_body
            .as_deref()
            .is_some()
    }));
    assert_eq!(sqlite_persistent_bytes(&database), persistent_before);

    writer
        .execute(
            "update messages set user_data = 'replacement-40' where rowid = 40",
            [],
        )
        .unwrap();
    let rewritten_persistent = sqlite_persistent_bytes(&database);
    reset_shelley_query_counters();
    let rewrite =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_ne!(rewrite.commit.generation_id, noop.commit.generation_id);
    assert_ne!(rewrite.sources, noop.sources);
    assert_eq!(rewrite.commit.indexed_documents, 40);
    let rewrite_work = shelley_query_counters();
    assert_eq!(shelley_query_shape(rewrite_work), [4, 3, 3, 3, 6, 40]);
    assert_eq!(rewrite_work.pages_emitted, 1);
    assert_eq!(rewrite_work.hydration_snapshot_opens, 0);
    assert_eq!(sqlite_persistent_bytes(&database), rewritten_persistent);
    let replacement_records = VerifiedIndex::open(&index_root)
        .unwrap()
        .core_source_event_page(&source, None, 40)
        .unwrap()
        .items;
    assert!(replacement_records.iter().any(|record| {
        record.core_record.content.normalized_body.as_deref() == Some("replacement-40")
    }));
    drop(writer);
}

#[test]
fn shelley_source_backed_lineage_uses_native_parent_and_root_threads() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = create_fixture(temp.path(), "root");
    let conn = Connection::open(&database).unwrap();
    conn.execute(
        "insert into conversations (
             conversation_id, user_initiated, created_at, updated_at, cwd,
             parent_conversation_id
         ) values (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            "conversation-child",
            0_i64,
            "2026-07-28T20:02:00Z",
            "2026-07-28T20:03:00Z",
            "/workspace/project",
            "conversation-1",
        ],
    )
    .unwrap();
    conn.execute(
        "insert into conversations (
             conversation_id, user_initiated, created_at, updated_at, cwd,
             parent_conversation_id
         ) values (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            "conversation-grandchild",
            0_i64,
            "2026-07-28T20:04:00Z",
            "2026-07-28T20:05:00Z",
            "/workspace/project",
            "conversation-child",
        ],
    )
    .unwrap();
    conn.execute(
        "insert into messages (
             message_id, conversation_id, sequence_id, type, user_data, created_at
         ) values (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            "message-grandchild",
            "conversation-grandchild",
            1_i64,
            "assistant",
            "nested Shelley message",
            "2026-07-28T20:04:30Z",
        ],
    )
    .unwrap();
    drop(conn);

    let adapter = discover_shelley_source_backed_exact_cwd(
        crate::test_provider_sqlite_data_root(),
        temp.path(),
    )
    .unwrap()
    .unwrap();
    let (documents, _) = drain(&adapter);
    let root = documents
        .iter()
        .find(|document| document.provider_session_id.as_deref() == Some("conversation-1"))
        .unwrap();
    let nested = documents
        .iter()
        .find(|document| document.provider_session_id.as_deref() == Some("conversation-grandchild"))
        .unwrap();
    let child_session_id =
        shelley_session_identity(adapter.source(), "conversation-child").unwrap();

    assert_eq!(nested.parent_session_id, Some(child_session_id));
    assert_eq!(nested.root_session_id, root.session_id);
    assert_eq!(nested.agent_type, AgentType::Subagent.as_str());
    assert!(!nested.is_primary);
    assert_eq!(nested.branch, None);
    assert!(nested.native_event_id.is_some());
    assert!(!serde_json::to_string(nested)
        .unwrap()
        .contains(database.to_string_lossy().as_ref()));
}

#[test]
fn shelley_source_backed_releases_pages_before_terminal_source_certification() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = create_fixture(temp.path(), "before replacement");
    insert_fixture_messages(&database, 65, 16);
    let original = temp.path().join("original.db");
    let adapter = discover_shelley_source_backed_exact_cwd(
        crate::test_provider_sqlite_data_root(),
        temp.path(),
    )
    .unwrap()
    .unwrap();
    let mut scan = adapter.start_scan().unwrap();
    let page = scan.next_page().unwrap().unwrap();
    assert!(page
        .documents
        .iter()
        .any(|document| body(document).contains("before replacement")));
    assert_eq!(page.counts.complete_records, 64);
    assert!(scan.sqlite_snapshot.is_some());
    assert!(scan.receipt.is_none());

    fs::rename(&database, &original).unwrap();
    create_fixture(temp.path(), "after replacement");
    assert!(scan.next_page().is_err());
    assert!(matches!(
        scan.finish(),
        Err(ShelleySourceBackedError::ScanIncomplete)
    ));
}

#[test]
fn shelley_source_backed_inventory_is_exact_cwd_only() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let active = temp.path().join("active");
    let past = temp.path().join("past");
    let manual = active.join("manual-root");
    let parent = temp.path();
    create_fixture(&past, "past");
    create_fixture(&manual, "manual");
    create_fixture(parent, "parent");
    fs::create_dir_all(&active).unwrap();

    assert!(discover_shelley_source_backed_exact_cwd(
        crate::test_provider_sqlite_data_root(),
        &active
    )
    .unwrap()
    .is_none());
    let active_database = create_fixture(&active, "active");
    let adapter =
        discover_shelley_source_backed_exact_cwd(crate::test_provider_sqlite_data_root(), &active)
            .unwrap()
            .unwrap();
    assert_eq!(adapter.database_path(), active_database);
    assert_eq!(
        adapter.database_path().parent(),
        Some(fs::canonicalize(&active).unwrap().as_path())
    );
    assert_ne!(adapter.database_path(), past.join("shelley.db"));
    assert_ne!(adapter.database_path(), manual.join("shelley.db"));
    assert_ne!(adapter.database_path(), parent.join("shelley.db"));
}
