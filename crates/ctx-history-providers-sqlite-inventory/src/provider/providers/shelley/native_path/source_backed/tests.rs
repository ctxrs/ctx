use std::{
    fs,
    path::{Path, PathBuf},
};

use ctx_history_core::{CoreRecord, ScannedSourceCounts};
use rusqlite::{params, Connection};
use serde_json::{json, Value};

use super::super::scanner::{
    reset_shelley_query_counters, shelley_query_counters, ShelleyQueryCounters,
};
use super::*;

#[test]
fn root_scope_separates_identical_shelley_conversations_and_unqualified_is_released() {
    use ctx_history_core::{CaptureProvider, SourceAnchorScope};

    let released = SourceKey::derive(
        CaptureProvider::Shelley.as_str(),
        SHELLEY_SQLITE_SOURCE_FORMAT,
        SHELLEY_SOURCE_SCHEMA_VARIANT,
        1,
        SourceAnchor::provider_native(
            SHELLEY_SOURCE_ANCHOR_NAMESPACE,
            TypedKey::utf8(SHELLEY_SOURCE_ANCHOR_KEY).unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    let unqualified = shelley_source_key_scoped(SourceAnchorScope::Unqualified).unwrap();
    assert!(released.exact_descriptor_eq(&unqualified));
    assert_eq!(
        released.identity().encode_canonical().unwrap(),
        unqualified.identity().encode_canonical().unwrap()
    );

    let first = shelley_source_key_scoped(SourceAnchorScope::Lineage([0x11; 32])).unwrap();
    let second = shelley_source_key_scoped(SourceAnchorScope::Lineage([0x22; 32])).unwrap();
    assert_ne!(
        shelley_session_identity(&first, "shared-conversation").unwrap(),
        shelley_session_identity(&second, "shared-conversation").unwrap()
    );
}

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

#[test]
fn row_local_core_record_filter_does_not_absorb_capture_or_sqlite_failures() {
    assert!(shelley_row_projection_error(
        &ShelleySourceBackedError::CoreRecord(CoreRecordError::FieldTooLarge {
            field: "normalized_body",
            actual: 70 * 1024,
            maximum: 64 * 1024,
        })
    ));
    assert!(!shelley_row_projection_error(
        &ShelleySourceBackedError::Capture(CaptureError::InvalidPayload(
            "non-row capture failure".to_owned(),
        ))
    ));
    assert!(!shelley_row_projection_error(
        &ShelleySourceBackedError::SqliteSource(SqliteSourceAccessError::SourceChanged)
    ));
    for error in [
        ShelleySourceBackedError::Projection(ProjectionContractError::SourceChanged),
        ShelleySourceBackedError::Projection(ProjectionContractError::InvalidDerivedIdentity),
        ShelleySourceBackedError::CoreRecord(CoreRecordError::Projection(
            ProjectionContractError::SourceChanged,
        )),
        ShelleySourceBackedError::CoreRecord(CoreRecordError::InvalidIdentityRelationship),
        ShelleySourceBackedError::CoreRecord(CoreRecordError::InvalidSessionRelationship),
        ShelleySourceBackedError::CoreRecord(CoreRecordError::InvalidActivity),
    ] {
        assert!(!shelley_row_projection_error(&error), "{error:?}");
    }
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

#[test]
fn shelley_retains_complete_result_statuses_and_oversized_indivisible_rows() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = create_fixture(temp.path(), "ordinary message");
    let complete_success = format!("{}shelley-oversized-tail", "s".repeat(9 * 1024 * 1024));
    let fixtures = [
        json!({
            "Type": 6,
            "ToolResult": complete_success,
            "ToolUseID": "call-success",
            "ToolName": "large_tool",
            "Status": "success",
        }),
        json!({
            "Type": "ContentTypeToolResult",
            "ToolResult": "failure body",
            "ToolUseID": "call-failure",
            "Status": "failed",
        }),
        json!({
            "Type": 6,
            "ToolResult": "unknown body",
            "ToolUseID": "call-unknown",
        }),
        json!({
            "Type": 6,
            "ToolResult": "first representation",
            "Output": "second representation",
        }),
        json!({
            "Type": 6,
            "Display": "first fallback representation",
            "Results": "second fallback representation",
        }),
    ];
    let connection = Connection::open(&database).unwrap();
    for (offset, fixture) in fixtures.into_iter().enumerate() {
        connection
            .execute(
                "insert into messages (
                     message_id, conversation_id, sequence_id, type, user_data, created_at
                 ) values (?1, 'conversation-1', ?2, 'tool', ?3, ?4)",
                params![
                    format!("result-{offset}"),
                    8_i64 + offset as i64,
                    fixture.to_string(),
                    format!("2026-07-28T20:01:0{offset}Z"),
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
    let mut scan = adapter.start_scan().unwrap();
    let mut documents = Vec::new();
    let mut rejections = Vec::new();
    let mut oversized_page = false;
    while let Some(page) = scan.next_page().unwrap() {
        oversized_page |= page.retained_bytes > SHELLEY_PAGE_MAX_BYTES;
        documents.extend(page.documents);
        rejections.extend(page.rejections);
    }
    let receipt = scan.finish().unwrap();

    assert!(oversized_page);
    assert_eq!(
        receipt.certificate.parser_revision(),
        SHELLEY_SOURCE_PARSER_REVISION
    );
    assert_eq!(rejections.len(), 2);
    let outputs = documents
        .iter()
        .filter(|record| record.event_type == "tool_output")
        .collect::<Vec<_>>();
    assert_eq!(outputs.len(), 3);
    assert_eq!(body(outputs[0]), complete_success);
    assert!(body(outputs[0]).ends_with("shelley-oversized-tail"));
    assert_eq!(body(outputs[1]), "failure body");
    assert_eq!(body(outputs[2]), "unknown body");
    let activity = outputs[0].content.activity.as_ref().unwrap();
    assert!(activity.provider_call_id.is_some());
    assert!(matches!(
        activity
            .result
            .as_ref()
            .map(|result| &result.structured_content),
        Some(ctx_history_core::ActivityJsonCapture::Omitted { reason, .. })
            if reason == "size_limit"
    ));
}

fn sqlite_persistent_bytes(path: &Path) -> Vec<Vec<u8>> {
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
    let released_bucket = ctx_history_capture_model::normalization::text_id_index(
        &format!("conversation-1:{}", collision_ids[0]),
        4_096,
    );
    assert_eq!(
        ctx_history_capture_model::normalization::text_id_index(
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
    assert_eq!(cold.agent_scope, Some(AgentScope::Primary));
    assert_eq!(cold.parent_session_id, None);
    assert_eq!(cold.root_session_id, None);
    assert_eq!(cold.session_relationship, None);
    assert!(cold.native_event_id.is_some());
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
    let nested = documents
        .iter()
        .find(|document| document.provider_session_id.as_deref() == Some("conversation-grandchild"))
        .unwrap();
    let child_session_id =
        shelley_session_identity(adapter.source(), "conversation-child").unwrap();

    assert_eq!(nested.parent_session_id, Some(child_session_id));
    assert_eq!(nested.agent_scope, Some(AgentScope::Subagent));
    assert_eq!(nested.root_session_id, None);
    assert_eq!(
        nested.session_relationship,
        Some(ctx_history_core::ProviderNativeSessionRelationship::Delegated)
    );
    assert!(nested.native_event_id.is_some());
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
