use std::{collections::BTreeMap, fs, path::Path};

use ctx_history_core::{CoreRecord, SourceAnchor};
use ctx_history_index::{IndexError, VerifiedIndex};
use ctx_history_provider_hermes::test_support::{
    hermes_work_counters, reset_hermes_work_counters, set_after_hermes_snapshot_seal_hook,
    set_before_hermes_snapshot_seal_hook,
};
use rusqlite::Connection;

use super::super::*;
use crate::test_support_paths::complete_lexical_events;
use crate::{
    hermes_route_control_exact_due, provider_source_for_path,
    register_hermes_explicit_source_backed_route,
};

fn create_fixture(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "create table sessions (
                 id text primary key,
                 source text not null,
                 parent_session_id text,
                 started_at real not null,
                 ended_at real,
                 message_count integer default 0,
                 cwd text,
                 git_branch text,
                 git_repo_root text
             );
             create table messages (
                 id integer primary key,
                 session_id text not null,
                 role text not null,
                 content text,
                 timestamp real not null,
                 active integer not null default 1,
                 compacted integer not null default 0
             );
             insert into sessions
                 (id, source, parent_session_id, started_at, message_count, cwd, git_branch, git_repo_root)
                 values
                 ('parent-session', 'acp', null, 1782259200.0, 1, '/repo/parent', 'main', '/repo'),
                 ('child-session', 'acp', 'parent-session', 1782259201.0, 1, '/repo/child', 'feature', '/repo');
             insert into messages (id, session_id, role, content, timestamp) values
                 (10, 'parent-session', 'assistant', 'parent stable needle', 1782259202.0),
                 (20, 'child-session', 'assistant', 'child stable needle', 1782259203.0);",
        )
        .unwrap();
}

fn create_many_session_fixture(path: &Path, sessions: usize) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "create table sessions (
                 id text primary key,
                 source text not null,
                 parent_session_id text,
                 started_at real not null,
                 message_count integer default 0
             );
             create table messages (
                 id integer primary key,
                 session_id text not null,
                 role text not null,
                 content text,
                 timestamp real not null
             );",
        )
        .unwrap();
    let transaction = connection.transaction().unwrap();
    {
        let mut insert_session = transaction
            .prepare(
                "insert into sessions (id, source, started_at, message_count)
                 values (?1, 'acp', ?2, 1)",
            )
            .unwrap();
        let mut insert_message = transaction
            .prepare(
                "insert into messages (id, session_id, role, content, timestamp)
                 values (?1, ?2, 'assistant', ?3, ?4)",
            )
            .unwrap();
        for index in 0..sessions {
            let session = format!("session-{index:04}");
            insert_session
                .execute((&session, 1_782_259_200_f64 + index as f64))
                .unwrap();
            insert_message
                .execute((
                    i64::try_from(index + 1).unwrap(),
                    &session,
                    format!("body {index:04}"),
                    1_782_260_000_f64 + index as f64,
                ))
                .unwrap();
        }
    }
    transaction.commit().unwrap();
}

fn certificates_by_identity(
    receipt: &SourceBackedRefreshReceipt,
) -> BTreeMap<[u8; 32], ctx_history_core::CertifiedSource> {
    receipt
        .sources
        .iter()
        .cloned()
        .map(|certificate| {
            (
                certificate.observation().source().identity().digest(),
                certificate,
            )
        })
        .collect()
}

fn replace_fixture(path: &Path, parent_body: &str) {
    let retired = path.with_extension("retired.db");
    fs::rename(path, retired).unwrap();
    create_fixture(path);
    Connection::open(path)
        .unwrap()
        .execute(
            "update messages set content = ?1 where id = 10",
            [parent_body],
        )
        .unwrap();
}

fn fixture_registry(data_root: &Path, database: &Path) -> SourceBackedProviderRegistry {
    let mut registry = SourceBackedProviderRegistry::new();
    register_hermes_explicit_source_backed_route(
        &mut registry,
        provider_source_for_path(CaptureProvider::Hermes, database.to_path_buf()),
        data_root,
        SourceAnchor::CatalogLineage([0x48; 32]),
    )
    .unwrap();
    registry
}

fn unique_search_record(index_root: &Path, needle: &str) -> CoreRecord {
    let index = VerifiedIndex::open_pinned(index_root).unwrap();
    let candidates = complete_lexical_events(&index, needle, Default::default(), 8);
    candidates
        .iter()
        .find_map(|candidate| {
            let record = index
                .core_record_by_id(candidate.event.event_id.as_uuid())
                .unwrap()
                .expect("Hermes indexed record");
            record
                .content
                .normalized_body
                .as_deref()
                .is_some_and(|body| body.contains(needle))
                .then_some(record)
        })
        .unwrap_or_else(|| {
            panic!("expected a search hit containing {needle:?}, got {candidates:?}")
        })
}

fn cold_fixture(
    data_root: &Path,
    index_root: &Path,
    database: &Path,
) -> (SourceBackedProviderRegistry, SourceBackedRefreshReceipt) {
    create_fixture(database);
    let registry = fixture_registry(data_root, database);
    reset_hermes_work_counters();
    let cold = refresh_source_backed_generation(
        index_root,
        &registry,
        source_backed_refresh_writer_options(),
    )
    .unwrap();
    assert_eq!(cold.sources.len(), 2);
    let cold_work = hermes_work_counters();
    assert_eq!(cold_work.logical_row_traversals, 2);
    assert_eq!(cold_work.inventory_observation_rows, 4);
    (registry, cold)
}

fn incremental_refresh(
    index_root: &Path,
    registry: &SourceBackedProviderRegistry,
    base: &SourceBackedRefreshReceipt,
) -> SourceBackedRefreshReceipt {
    SourceBackedRefreshExecutor::new(registry.clone(), source_backed_refresh_writer_options())
        .with_base_route_controls(base.route_controls.clone())
        .refresh_scope_with_detailed_progress_and_reconciliation(
            index_root,
            SourceBackedRefreshScope::All,
            SourceBackedReconciliationDemand::Incremental,
            |_| Ok(()),
        )
        .unwrap()
}

fn rewritten_route_controls(
    receipt: &SourceBackedRefreshReceipt,
    rewrite: impl FnOnce(&mut serde_json::Value),
) -> std::collections::BTreeMap<ctx_history_index::SourceRouteIdentity, Vec<u8>> {
    let mut controls = receipt.route_controls.clone();
    assert_eq!(controls.len(), 1);
    let control = controls.values_mut().next().unwrap();
    let mut parsed: serde_json::Value = serde_json::from_slice(control).unwrap();
    rewrite(&mut parsed);
    *control = serde_json::to_vec(&parsed).unwrap();
    controls
}

#[test]
fn incremental_cursor_advances_only_with_successful_core_publication() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data-root");
    let index_root = temp.path().join("index");
    let database = temp.path().join("source/state.db");
    let (registry, cold) = cold_fixture(&data_root, &index_root, &database);
    Connection::open(&database)
        .unwrap()
        .execute(
            "insert into messages (id, session_id, role, content, timestamp)
             values (30, 'parent-session', 'assistant', 'atomic cursor needle', 1782259215.0)",
            [],
        )
        .unwrap();

    let failed =
        SourceBackedRefreshExecutor::new(registry.clone(), source_backed_refresh_writer_options())
            .with_base_route_controls(cold.route_controls.clone())
            .refresh_scope_with_detailed_progress_publication_metadata_and_reconciliation(
                &index_root,
                SourceBackedRefreshScope::All,
                SourceBackedReconciliationDemand::Incremental,
                |_| Ok(()),
                |_| {
                    Err(IndexError::PublicationMetadata(
                        "injected Hermes publication failure".into(),
                    ))
                },
            );
    assert!(failed.is_err());
    assert_eq!(
        VerifiedIndex::open_pinned(&index_root)
            .unwrap()
            .generation_id(),
        cold.commit.generation_id
    );

    reset_hermes_work_counters();
    let retry = incremental_refresh(&index_root, &registry, &cold);
    let work = hermes_work_counters();
    assert_eq!(work.inventory_observation_rows, 1);
    assert_eq!(work.logical_row_traversals, 1);
    unique_search_record(&index_root, "atomic cursor needle");
    assert_ne!(retry.route_controls, cold.route_controls);
}

#[test]
fn failed_exhaustive_publication_retains_due_control_and_retry_converges() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data-root");
    let index_root = temp.path().join("index");
    let database = temp.path().join("source/state.db");
    let (registry, cold) = cold_fixture(&data_root, &index_root, &database);
    let overdue_controls = rewritten_route_controls(&cold, |control| {
        let object = control.as_object_mut().unwrap();
        object.insert("last_successful_exhaustive_at_ms".into(), 0.into());
        object.insert("exact_due_at_ms".into(), 0.into());
    });
    Connection::open(&database)
        .unwrap()
        .execute(
            "update messages set content = 'exhaustive retry needle' where id = 10",
            [],
        )
        .unwrap();

    let failed =
        SourceBackedRefreshExecutor::new(registry.clone(), source_backed_refresh_writer_options())
            .with_base_route_controls(overdue_controls.clone())
            .refresh_scope_with_detailed_progress_publication_metadata_and_reconciliation(
                &index_root,
                SourceBackedRefreshScope::All,
                SourceBackedReconciliationDemand::Exhaustive,
                |_| Ok(()),
                |_| {
                    Err(IndexError::PublicationMetadata(
                        "injected exhaustive failure".into(),
                    ))
                },
            );
    assert!(failed.is_err());
    assert_eq!(
        VerifiedIndex::open_pinned(&index_root)
            .unwrap()
            .generation_id(),
        cold.commit.generation_id
    );
    let due = overdue_controls.values().next().unwrap();
    assert_eq!(hermes_route_control_exact_due(due, 1), Some(true));

    let retry = SourceBackedRefreshExecutor::new(registry, source_backed_refresh_writer_options())
        .with_base_route_controls(overdue_controls)
        .refresh_scope_with_detailed_progress_and_reconciliation(
            &index_root,
            SourceBackedRefreshScope::All,
            SourceBackedReconciliationDemand::Exhaustive,
            |_| Ok(()),
        )
        .unwrap();
    unique_search_record(&index_root, "exhaustive retry needle");
    assert_eq!(
        hermes_route_control_exact_due(retry.route_controls.values().next().unwrap(), 1),
        Some(false)
    );
}

#[test]
fn hermes_mutation_before_and_after_snapshot_seal_retains_published_generation_and_retries() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data-root");
    let index_root = temp.path().join("index");
    let database = temp.path().join("source/state.db");
    let (registry, cold) = cold_fixture(&data_root, &index_root, &database);

    Connection::open(&database)
        .unwrap()
        .execute(
            "update messages set content = 'before seal candidate' where id = 10",
            [],
        )
        .unwrap();
    let before_database = database.clone();
    set_before_hermes_snapshot_seal_hook(move || {
        replace_fixture(&before_database, "before seal mutation");
    });
    let before = refresh_source_backed_generation(
        &index_root,
        &registry,
        source_backed_refresh_writer_options(),
    )
    .unwrap();
    assert_eq!(
        VerifiedIndex::open_pinned(&index_root)
            .unwrap()
            .generation_id(),
        cold.commit.generation_id
    );
    assert_eq!(before.failed_routes.len(), 1);
    assert_eq!(
        before.failed_routes[0].class,
        SourceBackedSourceFailureClass::SourceChanged
    );

    let after_before = refresh_source_backed_generation(
        &index_root,
        &registry,
        source_backed_refresh_writer_options(),
    )
    .unwrap();
    Connection::open(&database)
        .unwrap()
        .execute(
            "update messages set content = 'after seal candidate' where id = 10",
            [],
        )
        .unwrap();
    let after_database = database.clone();
    set_after_hermes_snapshot_seal_hook(move || {
        replace_fixture(&after_database, "after seal mutation");
    });
    let after = refresh_source_backed_generation(
        &index_root,
        &registry,
        source_backed_refresh_writer_options(),
    )
    .unwrap();
    assert_eq!(
        VerifiedIndex::open_pinned(&index_root)
            .unwrap()
            .generation_id(),
        after_before.commit.generation_id
    );
    assert_eq!(after.failed_routes.len(), 1);
    assert_eq!(
        after.failed_routes[0].class,
        SourceBackedSourceFailureClass::SourceChanged
    );

    let retried = refresh_source_backed_generation(
        &index_root,
        &registry,
        source_backed_refresh_writer_options(),
    )
    .unwrap();
    unique_search_record(&index_root, "after seal mutation");
    assert_ne!(
        retried.commit.generation_id,
        after_before.commit.generation_id
    );
}

#[test]
fn production_incremental_base_route_work_stays_touch_bounded_with_large_history() {
    const SESSIONS: usize = 129;

    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data-root");
    let index_root = temp.path().join("index");
    let database = temp.path().join("source/state.db");
    create_many_session_fixture(&database, SESSIONS);
    let registry = fixture_registry(&data_root, &database);
    let cold = refresh_source_backed_generation(
        &index_root,
        &registry,
        source_backed_refresh_writer_options(),
    )
    .unwrap();
    assert_eq!(cold.sources.len(), SESSIONS);
    let cold_sources = certificates_by_identity(&cold);

    reset_hermes_work_counters();
    let noop = incremental_refresh(&index_root, &registry, &cold);
    assert_eq!(noop.commit.generation_id, cold.commit.generation_id);
    assert_eq!(certificates_by_identity(&noop), cold_sources);
    let noop_work = hermes_work_counters();
    assert_eq!(noop_work.inventory_observation_rows, 0);
    assert_eq!(noop_work.logical_row_traversals, 0);
    assert_eq!(noop_work.document_base_route_source_visits, 0);
    assert!(noop_work.session_scans.is_empty());

    Connection::open(&database)
        .unwrap()
        .execute(
            "insert into messages (id, session_id, role, content, timestamp)
             values (?1, 'session-0128', 'assistant', 'large delta needle', 1782269999.0)",
            [i64::try_from(SESSIONS + 1).unwrap()],
        )
        .unwrap();
    reset_hermes_work_counters();
    let appended = incremental_refresh(&index_root, &registry, &noop);
    assert_eq!(appended.sources.len(), SESSIONS);
    assert_ne!(appended.commit.generation_id, noop.commit.generation_id);
    assert_ne!(appended.route_controls, noop.route_controls);
    let appended_sources = certificates_by_identity(&appended);
    let changed = appended_sources
        .iter()
        .filter_map(|(identity, certificate)| {
            (cold_sources.get(identity) != Some(certificate)).then_some((identity, certificate))
        })
        .collect::<Vec<_>>();
    assert_eq!(changed.len(), 1);
    assert_eq!(
        changed[0].1.observation().source().anchor(),
        cold_sources[changed[0].0].observation().source().anchor()
    );
    let appended_work = hermes_work_counters();
    assert_eq!(appended_work.inventory_observation_rows, 1);
    assert_eq!(appended_work.logical_row_traversals, 1);
    assert_eq!(appended_work.document_base_route_source_visits, 1);
    assert_eq!(
        appended_work
            .session_scans
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        vec!["session-0128".to_owned()]
    );
    unique_search_record(&index_root, "large delta needle");
}
