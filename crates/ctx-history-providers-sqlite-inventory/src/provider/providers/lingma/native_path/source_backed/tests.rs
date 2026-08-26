use std::path::Path;

use ctx_history_core::{CoreRecord, TypedKey};
use rusqlite::{config::DbConfig, params, Connection};

use super::{
    discovery::LingmaRootAuthorizedSource,
    parsing::{set_before_database_certification, LingmaSourceBackedScanV0},
    *,
};
use crate::provider::providers::lingma::native_path::source_backed::parsing::scan_lingma_source_backed_v0;
use crate::provider::providers::lingma::native_path::{
    lingma_query_counters, reset_lingma_query_counters, LingmaQueryCounters,
};

#[test]
fn root_scope_composes_with_lingma_databases_and_preserves_unqualified_identity() {
    use ctx_history_core::{
        derive_session_id, CaptureProvider, NativeSessionKey, SessionIdentityInput, SourceAnchor,
        SourceAnchorScope, SourceKey,
    };

    let lineage = TypedKey::utf8("installed-client-profile-database").unwrap();
    let released = SourceKey::derive(
        CaptureProvider::Lingma.as_str(),
        crate::LINGMA_SQLITE_SOURCE_FORMAT,
        SOURCE_SCHEMA_VARIANT,
        1,
        SourceAnchor::provider_native(SOURCE_ANCHOR_NAMESPACE, lineage.clone()).unwrap(),
    )
    .unwrap();
    let unqualified = LingmaDatabaseSourceV0::new("/tmp/lingma.db", lineage.clone())
        .unwrap()
        .source_key()
        .unwrap();
    assert!(released.exact_descriptor_eq(&unqualified));
    assert_eq!(
        released.identity().encode_canonical().unwrap(),
        unqualified.identity().encode_canonical().unwrap()
    );

    let scoped = |lineage, root| {
        LingmaDatabaseSourceV0::new_scoped(
            "/tmp/lingma.db",
            TypedKey::utf8(lineage).unwrap(),
            SourceAnchorScope::Lineage(root),
        )
        .unwrap()
        .source_key()
        .unwrap()
    };
    let first = scoped("installed-client-profile-database", [0x11; 32]);
    let second = scoped("installed-client-profile-database", [0x22; 32]);
    let native = NativeSessionKey::native_id(
        NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8("shared-session").unwrap(),
    )
    .unwrap();
    let session = |source| {
        derive_session_id(SessionIdentityInput {
            source,
            logical_session_kind: LOGICAL_SESSION_KIND,
            native_session_key: &native,
        })
        .unwrap()
    };
    assert_ne!(session(&first), session(&second));
    assert_ne!(
        first.identity(),
        scoped("sibling-client-profile-database", [0x11; 32]).identity()
    );
}

fn create_database(path: &Path) -> Connection {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "create table chat_record (
                 session_id text not null,
                 request_id text,
                 chat_prompt text not null,
                 summary text,
                 error_result text,
                 gmt_create integer,
                 extra text
             );",
        )
        .unwrap();
    connection
}

fn insert_row(
    connection: &Connection,
    session: &str,
    request: &str,
    prompt: &str,
    summary: Option<&str>,
) {
    connection
        .execute(
            "insert into chat_record (
                 session_id, request_id, chat_prompt, summary, error_result, gmt_create, extra
             ) values (?1, ?2, ?3, ?4, null, 1780000000, '{\"client\":\"lingma\"}')",
            params![session, request, prompt, summary],
        )
        .unwrap();
}

fn database(path: &Path, lineage: &str) -> LingmaDatabaseSourceV0 {
    LingmaDatabaseSourceV0::new(path, TypedKey::utf8(lineage).unwrap()).unwrap()
}

fn inventory(databases: Vec<LingmaDatabaseSourceV0>) -> LingmaSourceInventoryV0 {
    LingmaSourceInventoryV0::new(TypedKey::utf8("installed-clients").unwrap(), databases).unwrap()
}

fn all_records(scan: &LingmaSourceBackedScanV0) -> Vec<&CoreRecord> {
    scan.databases()
        .iter()
        .flat_map(|database| database.records())
        .collect()
}

fn scan_records(source_inventory: LingmaSourceInventoryV0) -> Vec<CoreRecord> {
    let closing = source_inventory.clone();
    let scan = scan_lingma_source_backed_v0(
        crate::test_provider_sqlite_data_root(),
        source_inventory,
        || Ok(closing),
    )
    .unwrap();
    all_records(&scan)
        .into_iter()
        .map(|record| {
            record.validate_contract().unwrap();
            record.clone()
        })
        .collect()
}

#[cfg(target_os = "linux")]
#[test]
fn stock_sqlite_snapshot_finish_rejects_leaf_swap_after_open() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("local.db");
    let replacement = temp.path().join("replacement.db");
    let original = temp.path().join("original.db");
    let connection = create_database(&path);
    insert_row(&connection, "expected", "expected", "expected", None);
    drop(connection);
    let replacement_connection = create_database(&replacement);
    insert_row(
        &replacement_connection,
        "attacker",
        "attacker",
        "attacker",
        None,
    );
    drop(replacement_connection);

    let authority =
        LingmaRootAuthorizedSource::retain(crate::test_provider_sqlite_data_root(), &path).unwrap();
    let snapshot = authority.open_snapshot().unwrap();
    std::fs::rename(&path, &original).unwrap();
    std::fs::rename(&replacement, &path).unwrap();
    assert!(snapshot.finish().is_err());
}

#[test]
fn cold_scan_is_bounded_deterministic_and_emits_valid_stable_core() {
    const ROW_COUNT: i64 = 257;

    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("local.db");
    let connection = create_database(&path);
    for rowid in 1..=ROW_COUNT {
        insert_row(
            &connection,
            &format!("session-{rowid}"),
            &format!("request-{rowid}"),
            &format!("prompt-{rowid}"),
            None,
        );
    }
    drop(connection);
    let source_inventory = inventory(vec![database(&path, "vscode:stable:bounded-sets")]);

    reset_lingma_query_counters();
    let first = scan_records(source_inventory.clone());
    assert_eq!(first.len(), usize::try_from(ROW_COUNT).unwrap());
    assert_eq!(
        first
            .iter()
            .map(|record| record.content.meaningful_text())
            .collect::<Vec<_>>(),
        (1..=ROW_COUNT)
            .map(|rowid| format!("prompt-{rowid}"))
            .collect::<Vec<_>>()
    );
    for (index, record) in first.iter().enumerate() {
        let rowid = i64::try_from(index).unwrap() + 1;
        assert_eq!(record.event_sequence, u64::try_from(index).unwrap() * 2);
        assert_eq!(
            record.native_event_id,
            Some(TypedKey::Composite(vec![
                TypedKey::Composite(vec![
                    TypedKey::Utf8("request".to_owned()),
                    TypedKey::Utf8(format!("session-{rowid}")),
                    TypedKey::Utf8(format!("request-{rowid}")),
                ]),
                TypedKey::Utf8(USER_PROMPT_COORDINATE.to_owned()),
            ]))
        );
    }
    assert_eq!(
        lingma_query_counters(),
        LingmaQueryCounters {
            candidate_set_reads: 5,
            raw_row_set_reads: 4,
            raw_rows_read: 257,
        }
    );

    reset_lingma_query_counters();
    let replay = scan_records(source_inventory);
    assert_eq!(replay, first);
    assert_eq!(
        lingma_query_counters(),
        LingmaQueryCounters {
            candidate_set_reads: 5,
            raw_row_set_reads: 4,
            raw_rows_read: 257,
        }
    );
}

#[test]
fn row_local_projection_failure_rejects_only_its_chat_record() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("local.db");
    let connection = create_database(&path);
    insert_row(
        &connection,
        &"x".repeat(70 * 1024),
        "bad-request",
        "bad prompt",
        None,
    );
    insert_row(
        &connection,
        "healthy-session",
        "healthy-request",
        "healthy prompt",
        None,
    );
    drop(connection);
    let opening = inventory(vec![database(&path, "vscode:stable:row-local")]);
    let closing = opening.clone();

    let scan =
        scan_lingma_source_backed_v0(crate::test_provider_sqlite_data_root(), opening, || {
            Ok(closing)
        })
        .unwrap();
    let records = all_records(&scan);
    let counts = scan.databases()[0].certificate.counts();

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].content.meaningful_text(), "healthy prompt");
    assert_eq!(counts.complete_records, 2);
    assert_eq!(counts.retained_records, 1);
    assert_eq!(counts.rejected_records, 1);
    assert_eq!(counts.indexed_documents, 1);
}

#[test]
fn row_local_projection_filter_preserves_core_invariants() {
    assert!(lingma_row_projection_error(
        &LingmaSourceBackedErrorV0::EmptySelectedBody
    ));
    for error in [
        LingmaSourceBackedErrorV0::Projection(ProjectionContractError::SourceChanged),
        LingmaSourceBackedErrorV0::Projection(ProjectionContractError::InvalidDerivedIdentity),
        LingmaSourceBackedErrorV0::CoreRecord(CoreRecordError::Projection(
            ProjectionContractError::SourceChanged,
        )),
        LingmaSourceBackedErrorV0::CoreRecord(CoreRecordError::InvalidIdentityRelationship),
    ] {
        assert!(!lingma_row_projection_error(&error), "{error:?}");
    }
}

#[test]
fn finite_inventory_certifies_complete_bodies_and_order_independent_ids() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let first_path = temp.path().join("vscode-local.db");
    let second_path = temp.path().join("jetbrains-local.db");
    let long_prompt = format!("vscode prompt {} lingma-full-body-tail", "v".repeat(20_000));
    let first = create_database(&first_path);
    insert_row(
        &first,
        "vscode-session",
        "vscode-request",
        &long_prompt,
        Some("vscode summary"),
    );
    drop(first);
    let second = create_database(&second_path);
    insert_row(
        &second,
        "jetbrains-session",
        "jetbrains-request",
        "jetbrains prompt",
        Some("jetbrains summary"),
    );
    drop(second);

    let opening = inventory(vec![
        database(&first_path, "vscode:stable:default"),
        database(&second_path, "jetbrains:idea:2026.2"),
    ]);
    let closing = opening.clone();
    let scan =
        scan_lingma_source_backed_v0(crate::test_provider_sqlite_data_root(), opening, || {
            Ok(closing)
        })
        .unwrap();
    assert_eq!(scan.databases().len(), 2);
    assert_eq!(all_records(&scan).len(), 4);
    let long_user = all_records(&scan)
        .into_iter()
        .find(|record| {
            record.provider_session_id.as_deref() == Some("vscode-session")
                && record.role.as_deref() == Some("user")
        })
        .unwrap();
    assert_eq!(
        long_user.content.normalized_body.as_deref(),
        Some(long_prompt.as_str())
    );
    assert!(all_records(&scan).iter().all(|record| {
        record.parent_session_id.is_none()
            && record.root_session_id.is_none()
            && record.provider_session_id.is_some()
            && record.native_event_id.is_some()
            && record.session_relationship.is_none()
    }));
    assert!(scan.databases().iter().all(|database| {
        database.certificate.counts().indexed_documents == 2
            && database.certificate.counts().certified_bytes != 0
    }));

    let reversed = inventory(vec![
        database(&second_path, "jetbrains:idea:2026.2"),
        database(&first_path, "vscode:stable:default"),
    ]);
    let replay = scan_records(reversed);
    let mut first_ids = all_records(&scan)
        .into_iter()
        .map(|record| record.event_id.digest())
        .collect::<Vec<_>>();
    let mut replay_ids = replay
        .iter()
        .map(|record| record.event_id.digest())
        .collect::<Vec<_>>();
    first_ids.sort();
    replay_ids.sort();
    assert_eq!(first_ids, replay_ids);
}

#[test]
fn stock_sqlite_snapshot_scan_sees_complete_content_retained_in_active_wal() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("local.db");
    let writer = create_database(&path);
    insert_row(
        &writer,
        "wal-session",
        "wal-request",
        "main database prompt",
        None,
    );
    let mode: String = writer
        .query_row("pragma journal_mode = wal", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    writer
        .execute_batch("pragma wal_autocheckpoint = 0")
        .unwrap();
    writer
        .execute(
            "update chat_record
                    set chat_prompt = 'committed Lingma WAL prompt'
                  where request_id = 'wal-request'",
            [],
        )
        .unwrap();
    writer
        .set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
        .unwrap();
    drop(writer);
    assert!(path.with_file_name("local.db-wal").exists());
    assert!(path.with_file_name("local.db-shm").exists());

    let records = scan_records(inventory(vec![database(&path, "vscode:stable:wal")]));
    let user = records
        .iter()
        .find(|record| record.role.as_deref() == Some("user"))
        .unwrap();
    assert_eq!(
        user.content.normalized_body.as_deref(),
        Some("committed Lingma WAL prompt")
    );
}

#[cfg(target_os = "linux")]
#[test]
fn stock_sqlite_snapshot_finish_precedes_source_certification() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("local.db");
    let replacement = temp.path().join("replacement.db");
    let opening = create_database(&path);
    insert_row(&opening, "session", "request", "opening body", None);
    drop(opening);
    let replacement_connection = create_database(&replacement);
    insert_row(
        &replacement_connection,
        "session",
        "request",
        "replacement body",
        None,
    );
    drop(replacement_connection);
    let source_inventory = inventory(vec![database(&path, "vscode:stable:finish-order")]);
    let closing = source_inventory.clone();
    let replaced_path = path.clone();
    set_before_database_certification(Some(Box::new(move || {
        std::fs::rename(&replacement, &replaced_path).unwrap();
    })));

    let result = scan_lingma_source_backed_v0(
        crate::test_provider_sqlite_data_root(),
        source_inventory,
        || Ok(closing),
    );
    assert!(result.is_err());
}
