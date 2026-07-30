use std::fs;

use ctx_history_core::{NativeRecordCoordinate, TypedKey};
use rusqlite::{params, Connection};

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
        .any(|document| document.body.contains("Shelley active WAL sentinel")));
    assert_eq!(sqlite_persistent_bytes(&path), before);
    drop(writer);
}

fn drain(
    adapter: &ShelleySourceBackedAdapter,
) -> (Vec<LexicalDocument>, ShelleySourceBackedReceipt) {
    let mut scan = adapter.start_scan().unwrap();
    let mut documents = Vec::new();
    while let Some(page) = scan.next_page().unwrap() {
        assert!(page.documents.len() <= SHELLEY_PAGE_MAX_UNITS);
        documents.extend(page.documents);
    }
    let receipt = scan.finish().unwrap();
    (documents, receipt)
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

#[test]
fn shelley_source_backed_cold_exact_and_replacement_keep_identity() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let original = format!("{}shelley-tail", "x".repeat(4_096));
    let database = create_fixture(temp.path(), &original);
    let adapter = discover_shelley_source_backed_exact_cwd(
        crate::test_provider_sqlite_data_root(),
        temp.path(),
    )
    .unwrap()
    .unwrap();

    let (cold_documents, cold_receipt) = drain(&adapter);
    assert_eq!(cold_documents.len(), 1);
    let cold = &cold_documents[0];
    assert_eq!(cold.body, original);
    assert!(cold.body.ends_with("shelley-tail"));
    assert_eq!(cold.provider_session_id.as_deref(), Some("conversation-1"));
    assert_eq!(cold.parent_session_id, None);
    assert_eq!(cold.root_session_id, cold.session_id);
    assert_eq!(cold.branch, None);
    assert_eq!(cold.source_path.as_deref(), database.to_str());
    assert_eq!(cold.agent_type, AgentType::Primary.as_str());
    assert!(cold.is_primary);
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
    let NativeRecordCoordinate::ProviderSqlite {
        logical_relation,
        primary_key,
        row_version,
    } = cold.locator.coordinate()
    else {
        panic!("expected Shelley SQLite locator");
    };
    assert_eq!(logical_relation, SHELLEY_COMPOUND_LOCATOR_RELATION);
    assert_eq!(
        primary_key,
        &TypedKey::Composite(vec![
            TypedKey::Bool(true),
            TypedKey::I64(1),
            TypedKey::I64(1),
        ])
    );
    assert!(row_version.is_none());

    let exact = adapter.hydrate(&cold.locator).unwrap();
    assert_eq!(exact.text, original);
    assert_eq!(exact.native_record_digest, *cold.locator.record_digest());

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
    assert_ne!(
        cold.locator.record_digest(),
        replaced.locator.record_digest()
    );
    assert!(matches!(
        adapter.hydrate(&cold.locator),
        Err(ShelleySourceBackedError::StaleRecordEvidence)
    ));
    assert_eq!(
        adapter.hydrate(&replaced.locator).unwrap().text,
        replacement
    );
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
    assert_eq!(nested.source_path.as_deref(), database.to_str());
}

#[test]
fn shelley_source_backed_finishes_before_releasing_first_page() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = create_fixture(temp.path(), "before replacement");
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
        .any(|document| document.body.contains("before replacement")));
    assert!(scan.sqlite_snapshot.is_none());
    assert!(scan.receipt.is_some());

    fs::rename(&database, &original).unwrap();
    create_fixture(temp.path(), "after replacement");
    assert!(scan.next_page().unwrap().is_none());
    scan.finish().unwrap();
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
