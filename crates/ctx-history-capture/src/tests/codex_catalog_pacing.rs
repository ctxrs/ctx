use ctx_history_core::{AgentType, CaptureProvider};
use ctx_history_store::{CatalogSession, ImportWorkClass, Store};
use serde_json::json;

use crate::provider::codex::catalog::{
    catalog_parallelism, catalog_session_persist_bytes_for_test,
    persist_initial_catalog_sessions_bounded_for_test,
};
use crate::{CaptureError, DiskIoPacer, CODEX_SESSION_SOURCE_FORMAT};

#[test]
fn daemon_pacing_caps_catalog_cpu_parallelism() {
    let _pacing = crate::install_disk_io_pacer(DiskIoPacer::new(8 * 1024 * 1024, 1024 * 1024));

    assert_eq!(catalog_parallelism(100, Some(32)), 2);
}

#[test]
fn initial_codex_catalog_persistence_is_bounded_paced_and_atomically_published() {
    let temp = tempfile::tempdir().unwrap();
    let source_root = "/home/user/.codex/sessions";
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let generation = store
        .allocate_catalog_inventory_generation(CaptureProvider::Codex, source_root)
        .unwrap();
    let sessions = (0..65)
        .map(|index| catalog_persist_test_session(source_root, index))
        .collect::<Vec<_>>();
    let expected_bytes = sessions
        .iter()
        .map(|session| catalog_session_persist_bytes_for_test(session).unwrap())
        .sum::<u64>();
    let pacer = DiskIoPacer::new(u64::MAX, u64::MAX);
    let _pacing = crate::install_disk_io_pacer(pacer.clone());
    let mut committed = Vec::new();

    assert!(persist_initial_catalog_sessions_bounded_for_test(
        &store,
        generation,
        &sessions,
        |persisted| committed.push(persisted),
    )
    .unwrap());

    assert_eq!(committed, vec![64, 65]);
    assert_eq!(pacer.charged_bytes(), expected_bytes);
    assert!(store
        .list_catalog_sessions_for_source(CaptureProvider::Codex, source_root)
        .unwrap()
        .is_empty());
    assert!(store
        .list_catalog_import_work(
            CaptureProvider::Codex,
            source_root,
            ImportWorkClass::Fresh,
            1,
        )
        .unwrap()
        .is_empty());

    store.begin_immediate_batch().unwrap();
    assert!(store
        .complete_catalog_inventory_generation(CaptureProvider::Codex, source_root, generation,)
        .unwrap());
    store.commit_batch().unwrap();
    assert_eq!(
        store
            .list_catalog_sessions_for_source(CaptureProvider::Codex, source_root)
            .unwrap()
            .len(),
        sessions.len()
    );
    assert_eq!(
        store
            .list_catalog_import_work(
                CaptureProvider::Codex,
                source_root,
                ImportWorkClass::Fresh,
                sessions.len(),
            )
            .unwrap()
            .len(),
        sessions.len()
    );
}

#[test]
fn initial_catalog_path_rekey_falls_back_to_one_atomic_publication() {
    let temp = tempfile::tempdir().unwrap();
    let first_root = "/history/old-root";
    let second_root = "/history/new-root";
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let first_generation = store
        .allocate_catalog_inventory_generation(CaptureProvider::Codex, first_root)
        .unwrap();
    let first = catalog_persist_test_session(first_root, 0);
    store.begin_immediate_batch().unwrap();
    store
        .upsert_catalog_sessions(first_generation, std::slice::from_ref(&first))
        .unwrap();
    assert!(
        store
            .complete_catalog_inventory_generation(
                CaptureProvider::Codex,
                first_root,
                first_generation,
            )
            .unwrap()
    );
    store.commit_batch().unwrap();

    let second_generation = store
        .allocate_catalog_inventory_generation(CaptureProvider::Codex, second_root)
        .unwrap();
    let mut second = catalog_persist_test_session(second_root, 0);
    second.source_path = first.source_path.clone();
    assert!(!persist_initial_catalog_sessions_bounded_for_test(
        &store,
        second_generation,
        std::slice::from_ref(&second),
        |_| {},
    )
    .unwrap());
    assert_eq!(
        store
            .list_catalog_sessions_for_source(CaptureProvider::Codex, first_root)
            .unwrap()
            .len(),
        1
    );

    store.begin_immediate_batch().unwrap();
    store
        .upsert_catalog_sessions(second_generation, std::slice::from_ref(&second))
        .unwrap();
    store.rollback_batch().unwrap();
    assert_eq!(
        store
            .list_catalog_sessions_for_source(CaptureProvider::Codex, first_root)
            .unwrap()
            .len(),
        1
    );

    store.begin_immediate_batch().unwrap();
    store
        .upsert_catalog_sessions(second_generation, std::slice::from_ref(&second))
        .unwrap();
    assert!(store
        .complete_catalog_inventory_generation(
            CaptureProvider::Codex,
            second_root,
            second_generation,
        )
        .unwrap());
    store.commit_batch().unwrap();
    assert!(store
        .list_catalog_sessions_for_source(CaptureProvider::Codex, first_root)
        .unwrap()
        .is_empty());
    assert_eq!(
        store
            .list_catalog_sessions_for_source(CaptureProvider::Codex, second_root)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn initial_catalog_path_rekey_race_falls_back_without_stealing_published_rows() {
    let temp = tempfile::tempdir().unwrap();
    let first_root = "/history/new-root";
    let second_root = "/history/racing-root";
    let db_path = temp.path().join("work.sqlite");
    let store = Store::open(&db_path).unwrap();
    let racing_store = Store::open(&db_path).unwrap();
    let first_generation = store
        .allocate_catalog_inventory_generation(CaptureProvider::Codex, first_root)
        .unwrap();
    let sessions = (0..65)
        .map(|index| catalog_persist_test_session(first_root, index))
        .collect::<Vec<_>>();
    let mut second_generation = None;

    assert!(!persist_initial_catalog_sessions_bounded_for_test(
        &store,
        first_generation,
        &sessions,
        |persisted| {
            if persisted != 64 || second_generation.is_some() {
                return;
            }
            let generation = racing_store
                .allocate_catalog_inventory_generation(CaptureProvider::Codex, second_root)
                .unwrap();
            let mut racing = catalog_persist_test_session(second_root, 0);
            racing.source_path = sessions[64].source_path.clone();
            racing_store.begin_immediate_batch().unwrap();
            racing_store
                .upsert_catalog_sessions(generation, std::slice::from_ref(&racing))
                .unwrap();
            assert!(racing_store
                .complete_catalog_inventory_generation(
                    CaptureProvider::Codex,
                    second_root,
                    generation,
                )
                .unwrap());
            racing_store.commit_batch().unwrap();
            second_generation = Some(generation);
        },
    )
    .unwrap());

    assert!(second_generation.is_some());
    assert!(store
        .list_catalog_sessions_for_source(CaptureProvider::Codex, first_root)
        .unwrap()
        .is_empty());
    assert_eq!(
        store
            .list_catalog_sessions_for_source(CaptureProvider::Codex, second_root)
            .unwrap()
            .len(),
        1
    );

    store.begin_immediate_batch().unwrap();
    store
        .upsert_catalog_sessions(first_generation, &sessions)
        .unwrap();
    assert!(
        store
            .complete_catalog_inventory_generation(
                CaptureProvider::Codex,
                first_root,
                first_generation,
            )
            .unwrap()
    );
    store.commit_batch().unwrap();
    assert_eq!(
        store
            .list_catalog_sessions_for_source(CaptureProvider::Codex, first_root)
            .unwrap()
            .len(),
        sessions.len()
    );
    assert!(store
        .list_catalog_sessions_for_source(CaptureProvider::Codex, second_root)
        .unwrap()
        .is_empty());
}

#[test]
fn superseded_initial_codex_catalog_batches_remain_hidden_and_recoverable() {
    let temp = tempfile::tempdir().unwrap();
    let source_root = "/home/user/.codex/sessions";
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let first_generation = store
        .allocate_catalog_inventory_generation(CaptureProvider::Codex, source_root)
        .unwrap();
    let sessions = (0..65)
        .map(|index| catalog_persist_test_session(source_root, index))
        .collect::<Vec<_>>();
    let mut next_generation = None;

    let error = persist_initial_catalog_sessions_bounded_for_test(
        &store,
        first_generation,
        &sessions,
        |_| {
            next_generation.get_or_insert_with(|| {
                store
                    .allocate_catalog_inventory_generation(CaptureProvider::Codex, source_root)
                    .unwrap()
            });
        },
    )
    .unwrap_err();

    assert!(matches!(error, CaptureError::InventorySuperseded));
    assert!(store
        .list_catalog_sessions_for_source(CaptureProvider::Codex, source_root)
        .unwrap()
        .is_empty());
    let (deleted, bytes) = store
        .delete_unpublished_catalog_sessions_batch(
            CaptureProvider::Codex,
            source_root,
            next_generation.unwrap(),
            128,
        )
        .unwrap()
        .unwrap();
    assert_eq!(deleted, 64);
    assert!(bytes > 0);
}

fn catalog_persist_test_session(source_root: &str, index: usize) -> CatalogSession {
    CatalogSession {
        provider: CaptureProvider::Codex,
        source_format: CODEX_SESSION_SOURCE_FORMAT.to_owned(),
        source_root: source_root.to_owned(),
        source_path: format!("{source_root}/{index:04}.jsonl"),
        external_session_id: Some(format!("session-{index:04}")),
        parent_external_session_id: None,
        agent_type: AgentType::Primary,
        role_hint: Some("primary".to_owned()),
        external_agent_id: None,
        cwd: Some("/workspace".to_owned()),
        session_started_at_ms: Some(index as i64),
        file_size_bytes: 1_024 + index as u64,
        file_modified_at_ms: index as i64,
        import_revision: 1,
        cataloged_at_ms: 1_000,
        metadata: json!({"file_observation_token_v1": format!("token-{index:04}")}),
    }
}
