use std::path::Path;

use ctx_history_core::{AgentType, CaptureProvider, CoreRecord, TypedKey};
use ctx_history_index::{VerifiedIndex, WriterOptions};
use rusqlite::{config::DbConfig, params, Connection};
use serde_json::json;

use crate::{
    provider::source_backed::{
        refresh_source_backed_generation, register_lingma_source_backed_route,
        SourceBackedProviderRegistry, SourceBackedRouteSelection,
    },
    provider_sources::provider_source_for_path,
};

use super::{
    discovery::LingmaRootAuthorizedSource,
    parsing::{set_before_database_certification, LingmaSourceBackedScanV0},
    *,
};
use crate::provider::providers::lingma::native_path::{
    lingma_query_counters, reset_lingma_query_counters, LingmaQueryCounters,
};

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

fn writer_options() -> WriterOptions {
    WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    }
}

fn register_route(
    path: &Path,
    data_root: &Path,
    databases: Vec<(std::path::PathBuf, TypedKey)>,
) -> SourceBackedProviderRegistry {
    let mut registry = SourceBackedProviderRegistry::new();
    register_lingma_source_backed_route(
        &mut registry,
        provider_source_for_path(CaptureProvider::Lingma, path.to_path_buf()),
        SourceBackedRouteSelection::Automatic,
        data_root,
        TypedKey::utf8("installed-clients").unwrap(),
        databases,
    )
    .unwrap();
    registry
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
            && record.root_session_id == record.session_id
            && record.provider_session_id.is_some()
            && record.branch.is_none()
            && record.agent_type == AgentType::Primary.as_str()
            && record.is_primary
            && record.native_event_id.is_some()
            && record.repository_bindings.is_empty()
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

#[test]
fn tantivy_round_trip_is_complete_locator_free_and_replacement_lifecycle_is_exact() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("local.db");
    let data_root = temp.path().join("data-root");
    let index_root = temp.path().join("index");
    let original_text = format!("lingma-core-{}-original-tail", "x".repeat(20_000));
    let original_prompt = json!({
        "message": original_text,
        "native": {"format": "structured"},
    })
    .to_string();
    let summary = format!("lingma-summary-{}-summary-tail", "s".repeat(20_000));
    let connection = create_database(&path);
    insert_row(
        &connection,
        "core-session",
        "core-request",
        &original_prompt,
        Some(&summary),
    );
    drop(connection);
    let lineage = TypedKey::utf8("vscode:stable:core").unwrap();
    let expected = scan_records(inventory(vec![LingmaDatabaseSourceV0::new(
        &path,
        lineage.clone(),
    )
    .unwrap()]))
    .into_iter()
    .find(|record| record.role.as_deref() == Some("user"))
    .unwrap();
    let registry = register_route(&path, &data_root, vec![(path.clone(), lineage.clone())]);

    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let stored = VerifiedIndex::open(&index_root)
        .unwrap()
        .core_record_by_id(expected.event_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(stored, expected);
    assert_eq!(
        stored.content.normalized_body.as_deref(),
        Some(original_prompt.as_str())
    );
    assert_eq!(
        stored.content.structured_content.as_ref().unwrap()["message"],
        original_text
    );
    assert_eq!(
        stored.native_event_id,
        Some(TypedKey::Composite(vec![
            TypedKey::Composite(vec![
                TypedKey::Utf8("request".to_owned()),
                TypedKey::Utf8("core-session".to_owned()),
                TypedKey::Utf8("core-request".to_owned()),
            ]),
            TypedKey::Utf8(USER_PROMPT_COORDINATE.to_owned()),
        ]))
    );
    assert!(stored.repository_bindings.is_empty());
    assert!(stored.repository_abstentions.is_empty());
    assert!(stored.repository_file_observations.is_empty());
    assert!(stored.repository_vcs_observations.is_empty());
    let encoded = serde_json::to_string(&stored).unwrap();
    assert!(!encoded.contains("locator"));
    assert!(!encoded.contains("source_path"));
    assert!(!encoded.contains(path.to_string_lossy().as_ref()));

    let noop = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(noop.commit.generation_id, cold.commit.generation_id);

    let rewritten_text = format!("lingma-core-{}-rewritten-tail", "r".repeat(20_000));
    let rewritten_prompt = json!({
        "message": rewritten_text,
        "native": {"format": "structured"},
    })
    .to_string();
    Connection::open(&path)
        .unwrap()
        .execute(
            "update chat_record set chat_prompt = ?1 where request_id = 'core-request'",
            [&rewritten_prompt],
        )
        .unwrap();
    // The published Core record remains immutable until a new generation is committed.
    assert_eq!(
        VerifiedIndex::open(&index_root)
            .unwrap()
            .core_record_by_id(expected.event_id.as_uuid())
            .unwrap()
            .unwrap()
            .content
            .normalized_body
            .as_deref(),
        Some(original_prompt.as_str())
    );

    let rewritten =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_ne!(rewritten.commit.generation_id, cold.commit.generation_id);
    let rewritten_record = VerifiedIndex::open(&index_root)
        .unwrap()
        .core_record_by_id(expected.event_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(rewritten_record.event_id, expected.event_id);
    assert_eq!(
        rewritten_record.content.normalized_body.as_deref(),
        Some(rewritten_prompt.as_str())
    );

    let connection = Connection::open(&path).unwrap();
    insert_row(
        &connection,
        "appended-session",
        "appended-request",
        "appended prompt",
        None,
    );
    drop(connection);
    let appended = scan_records(inventory(vec![LingmaDatabaseSourceV0::new(
        &path,
        lineage.clone(),
    )
    .unwrap()]))
    .into_iter()
    .find(|record| record.provider_session_id.as_deref() == Some("appended-session"))
    .unwrap();
    let appended_generation =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_ne!(
        appended_generation.commit.generation_id,
        rewritten.commit.generation_id
    );
    assert!(VerifiedIndex::open(&index_root)
        .unwrap()
        .core_record_by_id(appended.event_id.as_uuid())
        .unwrap()
        .is_some());

    Connection::open(&path)
        .unwrap()
        .execute(
            "delete from chat_record where request_id = 'appended-request'",
            [],
        )
        .unwrap();
    let deleted =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_ne!(
        deleted.commit.generation_id,
        appended_generation.commit.generation_id
    );
    assert!(VerifiedIndex::open(&index_root)
        .unwrap()
        .core_record_by_id(appended.event_id.as_uuid())
        .unwrap()
        .is_none());

    let empty_registry = register_route(&path, &data_root, Vec::new());
    std::fs::remove_file(&path).unwrap();
    let first_missing =
        refresh_source_backed_generation(&index_root, &empty_registry, writer_options()).unwrap();
    assert_ne!(
        first_missing.commit.generation_id,
        deleted.commit.generation_id
    );
    assert!(VerifiedIndex::open(&index_root)
        .unwrap()
        .core_record_by_id(expected.event_id.as_uuid())
        .unwrap()
        .is_some());
    let second_missing =
        refresh_source_backed_generation(&index_root, &empty_registry, writer_options()).unwrap();
    assert_ne!(
        second_missing.commit.generation_id,
        first_missing.commit.generation_id
    );
    assert!(VerifiedIndex::open(&index_root)
        .unwrap()
        .core_record_by_id(expected.event_id.as_uuid())
        .unwrap()
        .is_some());
    let source_deleted =
        refresh_source_backed_generation(&index_root, &empty_registry, writer_options()).unwrap();
    assert_ne!(
        source_deleted.commit.generation_id,
        second_missing.commit.generation_id
    );
    assert!(VerifiedIndex::open(&index_root)
        .unwrap()
        .core_record_by_id(expected.event_id.as_uuid())
        .unwrap()
        .is_none());

    let provider_source = concat!(
        include_str!("../source_backed.rs"),
        include_str!("identity.rs"),
        include_str!("parsing.rs"),
        include_str!("discovery.rs"),
        include_str!("../records.rs"),
        include_str!("../../native_path.rs"),
        include_str!("../../../lingma.rs"),
    );
    for forbidden in [
        "LexicalDocument",
        "SourceRecordLocator",
        "source_path",
        "hydrate",
        "hydration",
        "resolver",
        "provider_local_preview",
        "MAX_BODY_PREVIEW_CHARS",
    ] {
        assert!(
            !provider_source.contains(forbidden),
            "Lingma direct-Core path contains forbidden token {forbidden}"
        );
    }
}
