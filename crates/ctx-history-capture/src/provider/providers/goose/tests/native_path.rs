use std::{collections::BTreeSet, fs, path::Path};

use rusqlite::{params, Connection};
use serde_json::json;

use super::super::{
    native_path::{
        GooseNativePage, GooseNativePathReader, GooseNativeScanSummary, GooseNativeSourceAuthority,
        GooseNativeSourceSelection,
    },
    normalization::{GooseNativeEventKind, GooseNativeRejectionKind},
    stream::{
        GooseNativePageLimits, GOOSE_NATIVE_DEFAULT_PAGE_BYTES, GOOSE_NATIVE_MAX_PAGE_BYTES,
        GOOSE_NATIVE_MAX_PAGE_ROWS,
    },
};
use super::{create_goose_tables, insert_message, insert_session};

fn create_native_database(path: &Path) -> Connection {
    let conn = Connection::open(path).unwrap();
    create_goose_tables(&conn);
    conn.pragma_update(None, "user_version", 14).unwrap();
    conn
}

fn native_reader(path: &Path) -> GooseNativePathReader {
    GooseNativePathReader::acquire(GooseNativeSourceSelection::exact(path)).unwrap()
}

fn insert_raw_message(
    conn: &Connection,
    id: i64,
    message_id: Option<&str>,
    session_id: &str,
    role: &str,
    content_json: &str,
) {
    conn.execute(
        "insert into messages (
            id, message_id, session_id, role, content_json, created_timestamp,
            timestamp, tokens, metadata_json
         ) values (?1, ?2, ?3, ?4, ?5, ?6, '2026-07-18T00:00:00Z',
                   '{\"input\":1}', '{\"source\":\"nativepath-test\"}')",
        params![
            id,
            message_id,
            session_id,
            role,
            content_json,
            1_784_332_800 + id
        ],
    )
    .unwrap();
}

fn collect_scan(
    reader: &GooseNativePathReader,
    limits: GooseNativePageLimits,
) -> (Vec<GooseNativePage>, GooseNativeScanSummary) {
    let mut scanner = reader.scanner(limits).unwrap();
    let mut pages = Vec::new();
    while let Some(page) = scanner.next_page().unwrap() {
        pages.push(page);
    }
    let summary = scanner.finish_core().unwrap();
    assert!(summary.complete);
    (pages, summary)
}

#[test]
fn goose_nativepath_classifies_before_hydration_and_never_materializes_output_surfaces() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("sessions.db");
    let conn = create_native_database(&path);
    insert_session(&conn, "session-a");
    insert_message(&conn, 1, "session-a", "safe retained user text");
    let safe_call = json!([{
        "type": "toolRequest",
        "toolCall": {
            "id": "call-safe",
            "name": "write_file",
            "arguments": {"path": "src/safe-goose.rs", "content": "safe"}
        }
    }])
    .to_string();
    insert_raw_message(
        &conn,
        2,
        Some("duplicate-id"),
        "session-a",
        "assistant",
        &safe_call,
    );
    let output_sentinel = format!(
        "OUTPUT_SENTINEL_DO_NOT_TRANSFER:{}:src/output-only.rs",
        "x".repeat(crate::MAX_PROVIDER_SQLITE_VALUE_BYTES + 1)
    );
    let output = json!([{
        "type": "toolResponse",
        "toolCallId": "call-safe",
        "toolResult": {
            "status": "success",
            "content": [{"type": "text", "text": output_sentinel}]
        }
    }])
    .to_string();
    insert_raw_message(
        &conn,
        3,
        Some("output-success"),
        "session-a",
        "tool",
        &output,
    );
    let failed_output = json!([
        {
            "type": "toolResponse",
            "toolResult": "*** Begin Patch\n*** Update File: src/output-failure.rs\n@@\n-old\n+new\n*** End Patch",
            "status": "failure"
        },
        {
            "type": "futureCompanion",
            "result": "a direct toolResponse still excludes the whole native row"
        }
    ])
    .to_string();
    insert_raw_message(
        &conn,
        4,
        Some("output-failure"),
        "session-a",
        "assistant",
        &failed_output,
    );
    insert_raw_message(
        &conn,
        5,
        Some("malformed"),
        "session-a",
        "assistant",
        "{not-json",
    );
    insert_raw_message(
        &conn,
        6,
        Some("future-block"),
        "session-a",
        "assistant",
        r#"[{"type":"futureResult","result":"must stay local"}]"#,
    );
    insert_raw_message(
        &conn,
        7,
        Some("orphan"),
        "missing-session",
        "user",
        r#"[{"type":"text","text":"orphan"}]"#,
    );
    insert_raw_message(
        &conn,
        8,
        Some("duplicate-id"),
        "session-a",
        "user",
        r#"[{"type":"text","text":"duplicate fallback"}]"#,
    );
    insert_raw_message(
        &conn,
        9,
        Some(""),
        "session-a",
        "user",
        r#"[{"type":"text","text":"empty fallback"}]"#,
    );
    drop(conn);

    let reader = GooseNativePathReader::acquire(
        GooseNativeSourceSelection::exact(&path)
            .with_inventory_observation_token(Some("goose-catalog-observation".to_owned())),
    )
    .unwrap();
    let limits = GooseNativePageLimits::new(3, GOOSE_NATIVE_DEFAULT_PAGE_BYTES).unwrap();
    let mut scanner = reader.scanner(limits).unwrap();
    assert!(!scanner.summary().complete);
    let mut pages = Vec::new();
    while let Some(page) = scanner.next_page().unwrap() {
        pages.push(page);
    }
    let summary = scanner.finish_core().unwrap();
    assert!(summary.complete);
    assert_eq!(summary.metrics.native_sessions, 1);
    assert_eq!(summary.metrics.native_messages, 9);
    assert_eq!(summary.metrics.retained_events, 5);
    assert_eq!(summary.metrics.excluded_outputs, 2);
    assert_eq!(
        summary.metrics.excluded_output_bytes_observed,
        output.len() as u64 + failed_output.len() as u64
    );
    assert_eq!(summary.metrics.rejected_records, 3);
    assert_eq!(summary.metrics.retained_content_cells_transferred, 4);
    assert_eq!(summary.metrics.outputs_success, 1);
    assert_eq!(summary.metrics.outputs_failure, 1);
    assert_eq!(summary.metrics.output_content_cells_transferred, 1);
    assert_eq!(
        summary.metrics.output_content_bytes_transferred,
        failed_output.len() as u64
    );
    assert_eq!(summary.metrics.output_hashes_built, 1);
    assert_eq!(summary.metrics.output_previews_built, 1);
    assert_eq!(summary.metrics.output_touches_built, 0);
    assert_eq!(summary.metrics.output_fts_documents_built, 0);
    assert_eq!(summary.metrics.generic_output_dtos_built, 0);

    let events = pages
        .iter()
        .flat_map(|page| page.events.iter())
        .collect::<Vec<_>>();
    assert_eq!(
        events
            .iter()
            .map(|event| event.native_order)
            .collect::<Vec<_>>(),
        [1, 2, 4, 8, 9]
    );
    assert_eq!(events[1].kind, GooseNativeEventKind::ToolCall);
    assert_eq!(
        events[1].native_identity,
        "goose-message-identity-v1:messages-id:16:8000000000000002"
    );
    assert!(events[1].identity_degraded);
    assert_eq!(
        events[3].native_identity,
        "goose-message-identity-v1:messages-id:16:8000000000000008"
    );
    assert_eq!(
        events[4].native_identity,
        "goose-message-identity-v1:messages-id:16:8000000000000009"
    );
    assert_eq!(events[1].file_touches.len(), 1);
    assert_eq!(events[1].file_touches[0].path, "src/safe-goose.rs");
    let retained_debug = format!("{events:?}");
    assert!(!retained_debug.contains("OUTPUT_SENTINEL_DO_NOT_TRANSFER"));
    assert!(!retained_debug.contains("src/output-only.rs"));
    assert!(retained_debug.contains("src/output-failure.rs"));

    assert!(pages.iter().all(|page| page.excluded_outputs.is_empty()));
    let rejections = pages
        .iter()
        .flat_map(|page| page.rejections.iter())
        .map(|rejection| rejection.kind)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        rejections,
        BTreeSet::from([
            GooseNativeRejectionKind::MalformedJson,
            GooseNativeRejectionKind::UnknownBlockType,
            GooseNativeRejectionKind::MissingSession,
        ])
    );
    let canonical = fs::canonicalize(path).unwrap();
    assert_eq!(
        summary.source_authority,
        GooseNativeSourceAuthority::ExactDispatchedDatabase {
            path: canonical.clone(),
            inventory_observation_token: Some("goose-catalog-observation".to_owned()),
        }
    );
    assert!(pages.iter().all(|page| {
        page.source_authority
            == GooseNativeSourceAuthority::ExactDispatchedDatabase {
                path: canonical.clone(),
                inventory_observation_token: Some("goose-catalog-observation".to_owned()),
            }
    }));
}

#[test]
fn goose_nativepath_rejects_bad_rows_and_children_of_rejected_sessions_locally() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("sessions.db");
    let conn = create_native_database(&path);
    insert_session(&conn, "accepted-parent");
    conn.execute(
        "insert into sessions(id, name) values ('rejected-parent', x'2a')",
        [],
    )
    .unwrap();
    insert_raw_message(
        &conn,
        1,
        Some("malformed-before"),
        "accepted-parent",
        "user",
        "{before",
    );
    insert_raw_message(
        &conn,
        2,
        Some("retained-middle"),
        "accepted-parent",
        "user",
        r#"[{"type":"text","text":"retained between malformed rows"}]"#,
    );
    insert_raw_message(
        &conn,
        3,
        Some("malformed-after"),
        "accepted-parent",
        "user",
        "{after",
    );
    insert_raw_message(
        &conn,
        4,
        Some("rejected-parent-child"),
        "rejected-parent",
        "user",
        r#"[{"type":"text","text":"must not publish"}]"#,
    );
    conn.execute(
        "insert into messages (
            id, message_id, session_id, role, content_json, created_timestamp
         ) values (
            5, 'wrong-storage-class', 'accepted-parent', 'user',
            '[{\"type\":\"text\",\"text\":\"must reject locally\"}]', 'not-an-integer'
         )",
        [],
    )
    .unwrap();
    drop(conn);

    let reader = native_reader(&path);
    let (pages, summary) = collect_scan(&reader, GooseNativePageLimits::default());
    let events = pages
        .iter()
        .flat_map(|page| page.events.iter())
        .collect::<Vec<_>>();
    let rejections = pages
        .iter()
        .flat_map(|page| page.rejections.iter())
        .collect::<Vec<_>>();

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].searchable_text, "retained between malformed rows");
    assert_eq!(summary.metrics.native_sessions, 2);
    assert_eq!(summary.metrics.native_messages, 5);
    assert_eq!(summary.metrics.rejected_records, 5);
    assert_eq!(
        rejections
            .iter()
            .filter(|rejection| rejection.kind == GooseNativeRejectionKind::MalformedJson)
            .count(),
        2
    );
    assert_eq!(
        rejections
            .iter()
            .filter(|rejection| {
                rejection.kind == GooseNativeRejectionKind::UnsupportedStorageClass
            })
            .count(),
        2
    );
    let rejected_child = rejections
        .iter()
        .find(|rejection| rejection.native_order == Some(4))
        .unwrap();
    assert_eq!(
        rejected_child.kind,
        GooseNativeRejectionKind::MissingSession
    );
    assert_eq!(
        rejected_child.session_identity.as_deref(),
        Some("rejected-parent")
    );
    assert!(!format!("{events:?}").contains("must not publish"));
}

#[test]
fn goose_nativepath_duplicate_keys_share_one_fail_closed_visitor_policy() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("sessions.db");
    let conn = create_native_database(&path);
    insert_session(&conn, "session-a");
    let duplicate_output =
        r#"[{"type":"text","text":"safe","type":"toolResponse","toolResult":"OUTPUT_SECRET"}]"#;
    insert_raw_message(
        &conn,
        1,
        Some("duplicate-output"),
        "session-a",
        "tool",
        duplicate_output,
    );
    insert_raw_message(
        &conn,
        2,
        Some("duplicate-type"),
        "session-a",
        "user",
        r#"[{"type":"text","type":"text","text":"rejected duplicate type"}]"#,
    );
    insert_raw_message(
        &conn,
        3,
        Some("last-wins-text"),
        "session-a",
        "user",
        r#"[{"type":"text","text":"first","text":"last"}]"#,
    );
    let reverse_duplicate_output =
        r#"[{"type":"toolResponse","toolResult":"OUTPUT_SECRET_REVERSED","type":"text"}]"#;
    insert_raw_message(
        &conn,
        4,
        Some("reverse-duplicate-output"),
        "session-a",
        "tool",
        reverse_duplicate_output,
    );
    drop(conn);

    let reader = native_reader(&path);
    let (pages, summary) = collect_scan(&reader, GooseNativePageLimits::default());
    let events = pages
        .iter()
        .flat_map(|page| page.events.iter())
        .collect::<Vec<_>>();
    let rejections = pages
        .iter()
        .flat_map(|page| page.rejections.iter())
        .collect::<Vec<_>>();

    assert_eq!(summary.metrics.native_messages, 4);
    assert_eq!(summary.metrics.retained_events, 1);
    assert_eq!(summary.metrics.excluded_outputs, 2);
    assert_eq!(summary.metrics.rejected_records, 1);
    assert_eq!(
        summary.metrics.excluded_output_bytes_observed,
        duplicate_output.len() as u64 + reverse_duplicate_output.len() as u64
    );
    assert_eq!(summary.metrics.retained_content_cells_transferred, 1);
    assert_eq!(summary.metrics.output_content_cells_transferred, 0);
    assert_eq!(summary.metrics.output_content_bytes_transferred, 0);
    assert_eq!(summary.metrics.output_hashes_built, 0);
    assert_eq!(summary.metrics.output_previews_built, 0);
    assert_eq!(summary.metrics.output_touches_built, 0);
    assert!(pages.iter().all(|page| page.excluded_outputs.is_empty()));
    assert_eq!(
        rejections[0].kind,
        GooseNativeRejectionKind::DuplicateBlockType
    );
    assert_eq!(events[0].searchable_text, "last");
    assert!(!format!("{pages:?}").contains("OUTPUT_SECRET"));
}

#[test]
fn goose_nativepath_tagged_length_delimited_identity_namespaces_never_collide() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("sessions.db");
    let conn = create_native_database(&path);
    insert_session(&conn, "session-a");
    insert_raw_message(
        &conn,
        1,
        Some("messages.id:2"),
        "session-a",
        "user",
        r#"[{"type":"text","text":"native id"}]"#,
    );
    insert_raw_message(
        &conn,
        2,
        Some(""),
        "session-a",
        "user",
        r#"[{"type":"text","text":"fallback id"}]"#,
    );
    let malicious = "goose-message-identity-v1:messages-id:16:8000000000000002";
    insert_raw_message(
        &conn,
        3,
        Some(malicious),
        "session-a",
        "user",
        r#"[{"type":"text","text":"malicious namespace-shaped id"}]"#,
    );
    drop(conn);

    let reader = native_reader(&path);
    let (pages, _) = collect_scan(&reader, GooseNativePageLimits::default());
    let events = pages
        .iter()
        .flat_map(|page| page.events.iter())
        .collect::<Vec<_>>();
    let identities = events
        .iter()
        .map(|event| event.native_identity.clone())
        .collect::<Vec<_>>();
    let unique = identities.iter().cloned().collect::<BTreeSet<_>>();

    assert_eq!(identities.len(), 3);
    assert_eq!(unique.len(), 3, "colliding identities: {identities:?}");
    assert_eq!(
        identities[0],
        "goose-message-identity-v1:message-id:13:messages.id:2"
    );
    assert_eq!(
        identities[1],
        "goose-message-identity-v1:messages-id:16:8000000000000002"
    );
    assert_eq!(
        identities[2],
        format!(
            "goose-message-identity-v1:message-id:{}:{malicious}",
            malicious.len()
        )
    );
    assert!(!events[0].identity_degraded);
    assert!(events[1].identity_degraded);
    assert!(!events[2].identity_degraded);
}

#[test]
fn goose_nativepath_unstarted_keysets_include_minimum_sqlite_rowid() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("sessions.db");
    let conn = create_native_database(&path);
    conn.execute(
        "insert into sessions(rowid, id) values (?1, 'minimum-rowid-session')",
        [i64::MIN],
    )
    .unwrap();
    insert_raw_message(
        &conn,
        i64::MIN,
        Some("minimum-rowid-message"),
        "minimum-rowid-session",
        "user",
        r#"[{"type":"text","text":"minimum rowid"}]"#,
    );
    drop(conn);

    let reader = native_reader(&path);
    let (pages, summary) = collect_scan(&reader, GooseNativePageLimits::default());

    assert_eq!(summary.metrics.native_sessions, 1);
    assert_eq!(summary.metrics.native_messages, 1);
    assert_eq!(
        pages
            .iter()
            .flat_map(|page| page.events.iter())
            .map(|event| event.sqlite_rowid)
            .collect::<Vec<_>>(),
        [i64::MIN]
    );
}

#[test]
fn goose_nativepath_capability_digest_tracks_schema_and_index_changes() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("sessions.db");
    drop(create_native_database(&path));

    let baseline = native_reader(&path);
    assert_eq!(baseline.schema().user_version, 14);
    assert_eq!(baseline.schema().schema_version, 14);
    let baseline_digest = baseline.schema().capability_digest.clone();
    drop(baseline);

    let conn = Connection::open(&path).unwrap();
    conn.execute(
        "create unique index idx_goose_native_message_id
         on messages(message_id)
         where message_id is not null",
        [],
    )
    .unwrap();
    drop(conn);
    let changed = native_reader(&path);
    assert_ne!(changed.schema().capability_digest, baseline_digest);
}

#[test]
fn goose_nativepath_reports_numeric_schema_mismatches_as_unsupported_schema() {
    for version in [13_u32, 15_u32] {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join(format!("sessions-v{version}.db"));
        let conn = create_native_database(&path);
        conn.execute("update schema_version set version = ?1", [version])
            .unwrap();
        drop(conn);

        let error = match GooseNativePathReader::acquire(GooseNativeSourceSelection::exact(&path)) {
            Ok(_) => panic!("schema version {version} unexpectedly opened"),
            Err(error) => error,
        };
        assert!(
            matches!(error, crate::CaptureError::UnsupportedSchemaVersion(found) if found == version)
        );
    }
}

#[test]
fn goose_nativepath_snapshot_is_immutable_and_live_mutation_invalidates_its_fence() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("sessions.db");
    let conn = create_native_database(&path);
    insert_session(&conn, "mutation-session");
    insert_message(&conn, 1, "mutation-session", "before-live-mutation");
    drop(conn);

    let reader = native_reader(&path);
    let writer = Connection::open(&path).unwrap();
    writer
        .execute(
            "update messages
             set content_json = '[{\"type\":\"text\",\"text\":\"after-live-mutation\"}]'
             where id = 1",
            [],
        )
        .unwrap();
    drop(writer);
    assert!(!reader.revalidate_live().unwrap());

    assert!(matches!(
        reader.scanner(GooseNativePageLimits::default()),
        Err(crate::CaptureError::SourceChangedDuringCapture)
    ));

    let refreshed = native_reader(&path);
    let (pages, _) = collect_scan(&refreshed, GooseNativePageLimits::default());
    let refreshed_text = pages
        .iter()
        .flat_map(|page| page.events.iter())
        .map(|event| event.searchable_text.as_str())
        .collect::<Vec<_>>();
    assert_eq!(refreshed_text, ["after-live-mutation"]);
}

#[test]
fn goose_nativepath_revalidates_each_query_guard_before_returning_a_page() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("sessions.db");
    let conn = create_native_database(&path);
    insert_session(&conn, "page-fence");
    insert_message(&conn, 1, "page-fence", "first generation");
    drop(conn);

    let reader = native_reader(&path);
    let limits = GooseNativePageLimits::new(1, GOOSE_NATIVE_DEFAULT_PAGE_BYTES).unwrap();
    let mut scanner = reader.scanner(limits).unwrap();
    let first = scanner.next_page().unwrap().unwrap();
    assert_eq!(first.sessions[0].native_identity, "page-fence");

    let writer = Connection::open(&path).unwrap();
    writer
        .execute(
            "update messages
             set content_json = '[{\"type\":\"text\",\"text\":\"changed between pages\"}]'
             where id = 1",
            [],
        )
        .unwrap();
    drop(writer);

    assert!(matches!(
        scanner.next_page(),
        Err(crate::CaptureError::SourceChangedDuringCapture)
    ));
}

#[test]
fn goose_nativepath_reads_latest_active_wal_rows_without_persistent_writes() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_dir = temp.path().join("provider");
    fs::create_dir(&source_dir).unwrap();
    let source_path = source_dir.join("sessions.db");
    let writer = Connection::open(&source_path).unwrap();
    writer.pragma_update(None, "journal_mode", "WAL").unwrap();
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    create_goose_tables(&writer);
    writer.pragma_update(None, "user_version", 14).unwrap();
    insert_session(&writer, "wal-session");
    insert_message(&writer, 1, "wal-session", "checkpointed-value");
    writer
        .query_row("pragma wal_checkpoint(truncate)", [], |_row| Ok(()))
        .unwrap();
    let checkpointed_database = fs::read(&source_path).unwrap();
    writer
        .execute(
            "update messages
             set content_json = '[{\"type\":\"text\",\"text\":\"committed-wal-value\"}]'
             where id = 1",
            [],
        )
        .unwrap();

    let source_wal = Path::new(&format!("{}-wal", source_path.display())).to_path_buf();
    assert_eq!(fs::read(&source_path).unwrap(), checkpointed_database);
    assert!(source_wal.metadata().unwrap().len() > 32);
    let database_before = fs::read(&source_path).unwrap();
    let wal_before = fs::read(&source_wal).unwrap();

    let reader =
        GooseNativePathReader::acquire(GooseNativeSourceSelection::exact(&source_path)).unwrap();
    let (pages, summary) = collect_scan(&reader, GooseNativePageLimits::default());
    assert_eq!(summary.metrics.retained_events, 1);
    assert_eq!(
        pages
            .iter()
            .flat_map(|page| page.events.iter())
            .map(|event| event.searchable_text.as_str())
            .collect::<Vec<_>>(),
        ["committed-wal-value"]
    );
    assert_eq!(fs::read(&source_path).unwrap(), database_before);
    assert_eq!(fs::read(&source_wal).unwrap(), wal_before);
    drop(writer);
}

#[test]
fn goose_nativepath_leaf_swap_is_rejected_before_provider_rows_are_exposed() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("sessions.db");
    let original = create_native_database(&path);
    insert_session(&original, "original-session");
    insert_message(
        &original,
        1,
        "original-session",
        "opened original generation",
    );
    drop(original);

    let reader = native_reader(&path);
    assert_eq!(reader.snapshot_path(), path);
    let displaced = temp.path().join("displaced.db");
    fs::rename(&path, &displaced).unwrap();
    let replacement = create_native_database(&path);
    insert_session(&replacement, "replacement-session");
    insert_message(
        &replacement,
        1,
        "replacement-session",
        "replacement must not be observed",
    );
    drop(replacement);

    assert!(matches!(
        reader.scanner(GooseNativePageLimits::default()),
        Err(crate::CaptureError::SourceChangedDuringCapture)
    ));
}

#[test]
fn goose_nativepath_core_classifies_all_output_outcomes_without_publishing_output_bodies() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("sessions.db");
    let conn = create_native_database(&path);
    insert_session(&conn, "fanout-session");
    for (id, message_id, payload) in [
        (
            1,
            "success-output",
            json!([{
                "type": "toolResponse",
                "toolCallId": "success-call",
                "status": "success",
                "toolResult": "success body"
            }]),
        ),
        (
            2,
            "failure-output",
            json!([{
                "type": "toolResponse",
                "toolCallId": "failure-call",
                "status": "failure",
                "toolResult": "failure body"
            }]),
        ),
        (
            3,
            "timeout-output",
            json!([{
                "type": "toolResponse",
                "toolCallId": "timeout-call",
                "timed_out": true,
                "toolResult": "timeout body"
            }]),
        ),
        (
            4,
            "unknown-output",
            json!([{
                "type": "toolResponse",
                "toolCallId": "unknown-call",
                "toolResult": "unknown body"
            }]),
        ),
    ] {
        insert_raw_message(
            &conn,
            id,
            Some(message_id),
            "fanout-session",
            "tool",
            &payload.to_string(),
        );
    }
    drop(conn);

    let reader = GooseNativePathReader::acquire(
        GooseNativeSourceSelection::exact(&path)
            .with_inventory_observation_token(Some("fanout-inventory".to_owned())),
    )
    .unwrap();
    let limits = GooseNativePageLimits::new(2, GOOSE_NATIVE_DEFAULT_PAGE_BYTES).unwrap();
    let mut scanner = reader.scanner(limits).unwrap();
    let mut pages = Vec::new();
    while let Some(page) = scanner.next_page().unwrap() {
        pages.push(page);
    }
    let summary = scanner.finish_core().unwrap();
    assert!(pages.iter().all(|page| page.excluded_outputs.is_empty()));
    assert_eq!(
        pages
            .iter()
            .flat_map(|page| page.events.iter())
            .map(|event| (event.native_order, event.kind))
            .collect::<Vec<_>>(),
        [
            (2, GooseNativeEventKind::ToolOutput),
            (3, GooseNativeEventKind::ToolOutput),
        ]
    );
    assert_eq!(summary.metrics.outputs_success, 1);
    assert_eq!(summary.metrics.outputs_failure, 1);
    assert_eq!(summary.metrics.outputs_timeout, 1);
    assert_eq!(summary.metrics.outputs_unknown, 1);
    assert_eq!(summary.metrics.output_content_cells_transferred, 2);
    assert_eq!(summary.metrics.output_hashes_built, 2);
    assert_eq!(summary.metrics.output_previews_built, 2);
    assert_eq!(summary.metrics.output_touches_built, 0);
    assert_eq!(summary.metrics.output_fts_documents_built, 0);
    assert_eq!(summary.metrics.generic_output_dtos_built, 0);
    let page_debug = format!("{pages:?}");
    assert!(!page_debug.contains("success body"));
    assert!(!page_debug.contains("unknown body"));
}

#[test]
fn goose_nativepath_core_pages_retry_idempotently_from_expected_frontiers() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("retry.db");
    let conn = create_native_database(&path);
    insert_session(&conn, "retry-session");
    for id in 1..=3 {
        insert_raw_message(
            &conn,
            id,
            Some(&format!("retry-output-{id}")),
            "retry-session",
            "tool",
            &json!([{
                "type": "toolResponse",
                "status": "success",
                "toolResult": format!("retry-body-{id}")
            }])
            .to_string(),
        );
    }
    drop(conn);
    let reader = native_reader(&path);
    let limits = GooseNativePageLimits::new(1, GOOSE_NATIVE_DEFAULT_PAGE_BYTES).unwrap();

    let mut first = reader.scanner(limits).unwrap();
    let _session_page = first.next_page().unwrap().unwrap();
    let core_page = first.next_page().unwrap().unwrap();
    drop(first);
    let mut core_retry = reader.scanner(limits).unwrap();
    core_retry
        .resume_core_from(core_page.expected_frontier)
        .unwrap();
    let repeated_core = core_retry.next_page().unwrap().unwrap();
    assert_eq!(repeated_core.identity, core_page.identity);
    assert_eq!(repeated_core.next_frontier, core_page.next_frontier);
    drop(core_retry);

    let mut core_resume = reader.scanner(limits).unwrap();
    core_resume
        .resume_core_from(core_page.next_frontier)
        .unwrap();
    let next_core = core_resume.next_page().unwrap().unwrap();
    assert_eq!(next_core.expected_frontier, core_page.next_frontier);
    assert_ne!(next_core.identity, core_page.identity);
}

#[test]
fn goose_nativepath_preserves_complete_zero_row_authority() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("sessions.db");
    drop(create_native_database(&path));
    let reader = GooseNativePathReader::acquire(
        GooseNativeSourceSelection::exact(&path)
            .with_inventory_observation_token(Some("zero-inventory-token".to_owned())),
    )
    .unwrap();
    let mut scanner = reader.scanner(GooseNativePageLimits::default()).unwrap();
    let incomplete = scanner.summary();
    assert!(!incomplete.complete);
    assert_eq!(incomplete.completed_inventory_token, None);
    assert!(scanner.next_page().unwrap().is_none());

    let complete = scanner.finish_core().unwrap();
    assert!(complete.complete);
    assert_eq!(
        complete.completed_inventory_token.as_deref(),
        Some("zero-inventory-token")
    );
    assert_eq!(complete.metrics.native_sessions, 0);
    assert_eq!(complete.metrics.native_messages, 0);
}

#[test]
fn goose_nativepath_enforces_frozen_page_maxima() {
    assert!(
        GooseNativePageLimits::new(GOOSE_NATIVE_MAX_PAGE_ROWS, GOOSE_NATIVE_MAX_PAGE_BYTES).is_ok()
    );
    assert!(GooseNativePageLimits::new(1, 1).is_err());
    assert!(GooseNativePageLimits::new(
        GOOSE_NATIVE_MAX_PAGE_ROWS + 1,
        GOOSE_NATIVE_MAX_PAGE_BYTES
    )
    .is_err());
    assert!(GooseNativePageLimits::new(
        GOOSE_NATIVE_MAX_PAGE_ROWS,
        GOOSE_NATIVE_MAX_PAGE_BYTES + 1
    )
    .is_err());
    assert!(GooseNativePageLimits::new(0, GOOSE_NATIVE_MAX_PAGE_BYTES).is_err());
    assert!(GooseNativePageLimits::new(1, 0).is_err());
}

#[test]
fn goose_nativepath_high_session_cardinality_summary_is_bounded() {
    const SESSION_COUNT: i64 = 4_096;
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("sessions.db");
    let mut conn = create_native_database(&path);
    let transaction = conn.transaction().unwrap();
    {
        let mut statement = transaction
            .prepare("insert into sessions(id) values (?1)")
            .unwrap();
        for index in 0..SESSION_COUNT {
            statement
                .execute([format!("high-cardinality-session-{index}")])
                .unwrap();
        }
    }
    transaction.commit().unwrap();
    drop(conn);

    let reader = GooseNativePathReader::acquire(
        GooseNativeSourceSelection::exact(&path)
            .with_inventory_observation_token(Some("bounded-summary-token".to_owned())),
    )
    .unwrap();
    let mut scanner = reader
        .scanner(
            GooseNativePageLimits::new(GOOSE_NATIVE_MAX_PAGE_ROWS, GOOSE_NATIVE_DEFAULT_PAGE_BYTES)
                .unwrap(),
        )
        .unwrap();
    while scanner.next_page().unwrap().is_some() {}
    let summary = scanner.finish_core().unwrap();

    assert_eq!(summary.inventory.native_session_rows, SESSION_COUNT as u64);
    assert_eq!(summary.inventory.native_message_rows, 0);
    assert_eq!(summary.inventory.session_identity_samples.len(), 8);
    assert!(summary
        .inventory
        .session_identity_samples
        .iter()
        .all(|sample| sample.len() == 64));
    assert_eq!(summary.inventory.session_identity_digest.len(), 64);
    assert_eq!(summary.metrics.session_page_queries, 64);
}

#[test]
fn goose_nativepath_local_scale_uses_set_wise_pages_and_keeps_output_transfer_zero() {
    const MESSAGE_COUNT: i64 = 4_096;
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("sessions.db");
    let mut conn = create_native_database(&path);
    for session in 0..4 {
        insert_session(&conn, &format!("scale-session-{session}"));
    }
    let transaction = conn.transaction().unwrap();
    {
        let mut statement = transaction
            .prepare(
                "insert into messages (
                    id, message_id, session_id, role, content_json, created_timestamp
                 ) values (?1, ?2, ?3, ?4, ?5, ?6)",
            )
            .unwrap();
        for id in 1..=MESSAGE_COUNT {
            let session = format!("scale-session-{}", id % 4);
            let (role, content) = if id % 4 == 0 {
                (
                    "tool",
                    json!([{
                        "type": "toolResponse",
                        "toolResult": {"content": [{"type": "text", "text": format!("output-{id}")}]}
                    }])
                    .to_string(),
                )
            } else {
                (
                    "user",
                    json!([{"type": "text", "text": format!("retained-{id}")}]).to_string(),
                )
            };
            statement
                .execute(params![
                    id,
                    format!("scale-message-{id}"),
                    session,
                    role,
                    content,
                    1_784_332_800 + id
                ])
                .unwrap();
        }
    }
    transaction.commit().unwrap();
    drop(conn);

    let reader = native_reader(&path);
    let limits =
        GooseNativePageLimits::new(GOOSE_NATIVE_MAX_PAGE_ROWS, GOOSE_NATIVE_DEFAULT_PAGE_BYTES)
            .unwrap();
    let mut scanner = reader.scanner(limits).unwrap();
    let mut page_count = 0;
    while scanner.next_page().unwrap().is_some() {
        page_count += 1;
    }
    let summary = scanner.finish_core().unwrap();
    assert!(summary.complete);
    assert_eq!(summary.metrics.native_sessions, 4);
    assert_eq!(summary.metrics.native_messages, MESSAGE_COUNT as u64);
    assert_eq!(summary.metrics.retained_events, 3_072);
    assert_eq!(summary.metrics.excluded_outputs, 1_024);
    assert_eq!(summary.metrics.identity_prescan_queries, 1);
    assert_eq!(summary.metrics.session_page_queries, 1);
    assert_eq!(summary.metrics.message_page_queries, 64);
    assert_eq!(summary.metrics.output_content_cells_transferred, 0);
    assert_eq!(summary.metrics.output_content_bytes_transferred, 0);
    assert_eq!(page_count, 65);
}
