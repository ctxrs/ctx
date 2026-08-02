use std::{
    collections::{BTreeMap, HashMap},
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Barrier, Mutex},
};

use ctx_history_core::{
    derive_event_id, derive_session_id, CoreRecord, EventIdentityInput, NativeItemKey,
    NativeSessionKey, ScannedSourceCounts, SessionIdentityInput, SourceAnchor, SourceObservation,
};
use ctx_history_index::{VerifiedIndex, WriterOptions};
use rusqlite::Connection;
use sha2::{Digest, Sha256};

use crate::{
    provider::source_backed::{
        family::document::DocumentLeafExecutionPolicy, source_backed_leaf_worker_budget,
        SourceBackedRefreshOutcome, SourceBackedRefreshReceipt, SourceBackedSourceFailureClass,
        AUTOMATIC_SOURCE_DELETION_MISSING_INVENTORIES,
    },
    provider_sources::ProviderCatalogSupport,
};

use super::{
    shared::{
        sqlite_inventory_leaf_execution_policy, SqliteInventoryCatalog, SqliteInventoryCatalogLeaf,
        SqliteInventoryDocumentAdapter, SqliteInventoryProvider, SqliteInventorySnapshotCounters,
        SQLITE_INVENTORY_MAX_LEAF_WORKERS,
    },
    *,
};

const TEST_PARSER_REVISION: &str = "sqlite-inventory-family-test-v1";

#[derive(Clone)]
struct TestLeaf {
    source: SourceKey,
    path: PathBuf,
}

#[derive(Default)]
struct TestState {
    projections: usize,
    discoveries: usize,
    terminal_callbacks: usize,
    counters: Vec<SqliteInventorySnapshotCounters>,
    mutate_before_finish: bool,
    after_seal_action: Option<TestAfterSealAction>,
    scan_barrier: Option<Arc<Barrier>>,
    active_scans: usize,
    peak_scans: usize,
    active_scans_by_path: HashMap<PathBuf, usize>,
    max_active_scans_per_path: usize,
}

enum TestAfterSealAction {
    MutateDatabase,
    CreateEmptyWal,
    RemoveEmptyWal,
    CreateNonemptyWal,
    MutateSibling,
}

#[derive(Clone)]
struct TestProvider {
    catalog: Arc<Mutex<Vec<TestLeaf>>>,
    state: Arc<Mutex<TestState>>,
    test_leaf_workers: Option<usize>,
    parser_revision: &'static str,
}

impl TestProvider {
    fn mutate(&self, suffix: &str) {
        self.mutate_leaf(0, suffix);
    }

    fn mutate_leaf(&self, index: usize, suffix: &str) {
        let database = self.catalog.lock().unwrap()[index].path.clone();
        Connection::open(database)
            .unwrap()
            .execute(
                "update messages set body = body || ?1 where id = 1",
                [suffix],
            )
            .unwrap();
    }

    fn with_test_workers(catalog: Arc<Mutex<Vec<TestLeaf>>>, test_leaf_workers: usize) -> Self {
        Self {
            catalog,
            state: Arc::default(),
            test_leaf_workers: Some(test_leaf_workers),
            parser_revision: TEST_PARSER_REVISION,
        }
    }

    fn use_scan_barrier(&self, participants: usize) {
        self.state.lock().unwrap().scan_barrier = Some(Arc::new(Barrier::new(participants)));
    }

    fn clear_scan_barrier(&self) {
        self.state.lock().unwrap().scan_barrier = None;
    }

    fn reset_scan_activity(&self) {
        let mut state = self.state.lock().unwrap();
        assert_eq!(state.active_scans, 0);
        assert!(state.active_scans_by_path.is_empty());
        state.peak_scans = 0;
        state.max_active_scans_per_path = 0;
    }

    fn set_after_seal_action(&self, action: TestAfterSealAction) {
        let replaced = self.state.lock().unwrap().after_seal_action.replace(action);
        assert!(replaced.is_none(), "test terminal action was already set");
    }
}

struct TestScanActivity {
    state: Arc<Mutex<TestState>>,
    path: PathBuf,
}

impl TestScanActivity {
    fn begin(state: &Arc<Mutex<TestState>>, path: &Path) -> (Self, Option<Arc<Barrier>>) {
        let mut current = state.lock().unwrap();
        current.projections = current.projections.saturating_add(1);
        current.active_scans = current.active_scans.saturating_add(1);
        current.peak_scans = current.peak_scans.max(current.active_scans);
        let active_for_path = {
            let active = current
                .active_scans_by_path
                .entry(path.to_path_buf())
                .or_default();
            *active = active.saturating_add(1);
            *active
        };
        current.max_active_scans_per_path = current.max_active_scans_per_path.max(active_for_path);
        let barrier = current.scan_barrier.clone();
        drop(current);
        (
            Self {
                state: Arc::clone(state),
                path: path.to_path_buf(),
            },
            barrier,
        )
    }
}

impl Drop for TestScanActivity {
    fn drop(&mut self) {
        let mut state = self.state.lock().unwrap();
        state.active_scans = state.active_scans.saturating_sub(1);
        let remove_path = if let Some(active) = state.active_scans_by_path.get_mut(&self.path) {
            *active = active.saturating_sub(1);
            *active == 0
        } else {
            false
        };
        if remove_path {
            state.active_scans_by_path.remove(&self.path);
        }
    }
}

impl SqliteInventoryProvider for TestProvider {
    type Leaf = TestLeaf;

    fn parser_revision(&self) -> &'static str {
        self.parser_revision
    }

    fn discover(&self) -> SourceBackedRouteResult<SqliteInventoryCatalog<Self::Leaf>> {
        self.state.lock().unwrap().discoveries += 1;
        let leaves = self
            .catalog
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .map(|leaf| SqliteInventoryCatalogLeaf {
                source: leaf.source.clone(),
                path: leaf.path.clone(),
                provider_leaf: leaf,
            })
            .collect();
        Ok(SqliteInventoryCatalog {
            authority_fingerprint: [17; 32],
            leaves,
        })
    }

    fn scan(
        &self,
        leaf: &Self::Leaf,
        snapshot: SqliteSourceReadSnapshot,
        sink: &mut ChangedDocumentSink<'_, '_>,
    ) -> SourceBackedRouteResult<CertifiedSource> {
        let (_activity, barrier) = TestScanActivity::begin(&self.state, &leaf.path);
        if let Some(barrier) = barrier {
            barrier.wait();
        }
        let rows = {
            let connection = snapshot.connection().map_err(route_error)?;
            let mut statement = connection
                .prepare("select id, body from messages order by id")
                .map_err(|error| route_error(CaptureError::from(error)))?;
            let rows = statement
                .query_map([], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|error| route_error(CaptureError::from(error)))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| route_error(CaptureError::from(error)))?;
            rows
        };
        let mut content = Sha256::new();
        content.update(b"ctx.sqlite-inventory-family-test-content-v1\0");
        let mut certified_bytes = 0_u64;
        for (id, body) in &rows {
            content.update(id.to_be_bytes());
            content.update((body.len() as u64).to_be_bytes());
            content.update(body.as_bytes());
            certified_bytes = certified_bytes
                .checked_add(body.len() as u64)
                .ok_or_else(|| {
                    SourceBackedRouteError::new(
                        SourceBackedRouteErrorKind::Internal,
                        "test certified byte count overflowed",
                    )
                })?;
            sink.emit_core_record(test_document(&leaf.source, *id, body))?;
        }
        let content_digest: [u8; 32] = content.finalize().into();
        let mutate_before_finish = {
            let mut state = self.state.lock().unwrap();
            std::mem::take(&mut state.mutate_before_finish)
        };
        if mutate_before_finish {
            self.mutate("-before-finish");
        }
        snapshot.finish().map_err(route_error)?;
        let observation = SourceObservation::new(
            leaf.source.clone(),
            "sqlite-inventory-family-test-observation-v1",
            content_digest.to_vec(),
        )
        .map_err(route_error)?;
        let count = rows.len() as u64;
        CertifiedSource::certify(
            observation.clone(),
            observation,
            self.parser_revision(),
            content_digest,
            ScannedSourceCounts {
                complete_records: count,
                retained_records: count,
                rejected_records: 0,
                ignored_records: 0,
                indexed_documents: count,
                certified_bytes,
            },
        )
        .map_err(route_error)
    }

    fn after_snapshots_sealed(&self) {
        let action = {
            let mut state = self.state.lock().unwrap();
            state.terminal_callbacks = state.terminal_callbacks.saturating_add(1);
            state.after_seal_action.take()
        };
        let Some(action) = action else {
            return;
        };
        let database = self.catalog.lock().unwrap()[0].path.clone();
        match action {
            TestAfterSealAction::MutateDatabase => self.mutate("-after-seal"),
            TestAfterSealAction::CreateEmptyWal => {
                fs::write(sqlite_component_path(&database, "-wal"), b"").unwrap();
            }
            TestAfterSealAction::RemoveEmptyWal => {
                fs::remove_file(sqlite_component_path(&database, "-wal")).unwrap();
            }
            TestAfterSealAction::CreateNonemptyWal => {
                fs::write(
                    sqlite_component_path(&database, "-wal"),
                    b"nonempty concurrent WAL sentinel",
                )
                .unwrap();
            }
            TestAfterSealAction::MutateSibling => {
                fs::write(
                    database.parent().unwrap().join("unrelated-sibling"),
                    b"unrelated sibling churn",
                )
                .unwrap();
            }
        }
    }

    fn observe_snapshot_counters(&self, counters: SqliteInventorySnapshotCounters) {
        self.state.lock().unwrap().counters.push(counters);
    }

    fn test_leaf_execution_policy(&self) -> Option<DocumentLeafExecutionPolicy> {
        self.test_leaf_workers
            .map(DocumentLeafExecutionPolicy::IndependentCapped)
    }
}

#[test]
fn finite_sqlite_inventory_parallelism_is_explicit_and_default_safe() {
    for provider in [
        CaptureProvider::AstrBot,
        CaptureProvider::Lingma,
        CaptureProvider::Crush,
    ] {
        assert_eq!(
            sqlite_inventory_leaf_execution_policy(provider),
            DocumentLeafExecutionPolicy::IndependentCapped(SQLITE_INVENTORY_MAX_LEAF_WORKERS)
        );
    }
    for provider in [
        CaptureProvider::Shelley,
        CaptureProvider::Hermes,
        CaptureProvider::Zed,
        CaptureProvider::KiroCli,
        CaptureProvider::NanoClaw,
    ] {
        assert_eq!(
            sqlite_inventory_leaf_execution_policy(provider),
            DocumentLeafExecutionPolicy::Serial
        );
    }
}

#[test]
fn independent_databases_have_one_vs_four_parity_and_one_snapshot_each() {
    const DATABASES: usize = 8;
    const REQUESTED_PARALLEL_WORKERS: usize = 4;

    let temp = crate::test_support_paths::tempdir().unwrap();
    let provider_dir = temp.path().join("provider");
    let serial_data_root = temp.path().join("serial-data");
    let parallel_data_root = temp.path().join("parallel-data");
    let serial_index_root = temp.path().join("serial-index");
    let parallel_index_root = temp.path().join("parallel-index");
    fs::create_dir_all(&provider_dir).unwrap();

    let mut writers = Vec::with_capacity(DATABASES);
    let mut leaves = Vec::with_capacity(DATABASES);
    for index in 0..DATABASES {
        let name = format!("history-{index}.sqlite");
        let path = provider_dir.join(&name);
        writers.push(active_wal_database(&path, &format!("cold body {index}")));
        leaves.push(TestLeaf {
            source: test_source(&name),
            path,
        });
    }
    let selected_database = leaves[0].path.clone();
    let catalog = Arc::new(Mutex::new(leaves));
    let parallel_worker_count =
        effective_test_leaf_worker_count(REQUESTED_PARALLEL_WORKERS, DATABASES);
    let serial_provider = TestProvider::with_test_workers(Arc::clone(&catalog), 1);
    let parallel_provider =
        TestProvider::with_test_workers(Arc::clone(&catalog), REQUESTED_PARALLEL_WORKERS);
    let serial_registry = test_registry(
        &serial_data_root,
        &selected_database,
        serial_provider.clone(),
    );
    let parallel_registry = test_registry(
        &parallel_data_root,
        &selected_database,
        parallel_provider.clone(),
    );

    parallel_provider.use_scan_barrier(parallel_worker_count);
    let before = directory_file_bytes(&provider_dir);
    let serial_cold = publish(&serial_index_root, &serial_registry);
    let parallel_cold = publish(&parallel_index_root, &parallel_registry);
    assert_eq!(directory_file_bytes(&provider_dir), before);
    assert_eq!(
        parallel_cold.commit.generation_id,
        serial_cold.commit.generation_id
    );
    assert_eq!(parallel_cold.sources, serial_cold.sources);
    assert_inventory_work(&serial_provider, DATABASES, 1, 2, 1, DATABASES, DATABASES);
    assert_inventory_work(
        &parallel_provider,
        DATABASES,
        parallel_worker_count,
        2,
        1,
        DATABASES,
        DATABASES,
    );
    assert_eq!(
        parallel_provider.state.lock().unwrap().peak_scans,
        parallel_worker_count
    );

    serial_provider.reset_scan_activity();
    parallel_provider.clear_scan_barrier();
    parallel_provider.reset_scan_activity();
    let serial_noop = publish(&serial_index_root, &serial_registry);
    let parallel_noop = publish(&parallel_index_root, &parallel_registry);
    assert_eq!(
        serial_noop.commit.generation_id,
        serial_cold.commit.generation_id
    );
    assert_eq!(
        parallel_noop.commit.generation_id,
        parallel_cold.commit.generation_id
    );
    assert_eq!(parallel_noop.sources, serial_noop.sources);
    assert_inventory_work(
        &serial_provider,
        DATABASES * 2,
        1,
        4,
        2,
        DATABASES * 2,
        DATABASES,
    );
    assert_inventory_work(
        &parallel_provider,
        DATABASES * 2,
        parallel_worker_count,
        4,
        2,
        DATABASES * 2,
        DATABASES,
    );

    writers[3]
        .execute(
            "update messages set body = body || '-changed' where id = 1",
            [],
        )
        .unwrap();
    serial_provider.reset_scan_activity();
    parallel_provider.reset_scan_activity();
    let serial_changed = publish(&serial_index_root, &serial_registry);
    let parallel_changed = publish(&parallel_index_root, &parallel_registry);
    assert_ne!(
        serial_changed.commit.generation_id,
        serial_noop.commit.generation_id
    );
    assert_eq!(
        parallel_changed.commit.generation_id,
        serial_changed.commit.generation_id
    );
    assert_eq!(parallel_changed.sources, serial_changed.sources);
    assert_inventory_work(
        &serial_provider,
        DATABASES * 3,
        1,
        6,
        3,
        DATABASES * 3,
        DATABASES + 1,
    );
    assert_inventory_work(
        &parallel_provider,
        DATABASES * 3,
        parallel_worker_count,
        6,
        3,
        DATABASES * 3,
        DATABASES + 1,
    );

    let deleted_source = catalog.lock().unwrap().pop().unwrap().source;
    serial_provider.reset_scan_activity();
    parallel_provider.reset_scan_activity();
    for expected_missing in 1..AUTOMATIC_SOURCE_DELETION_MISSING_INVENTORIES {
        let serial_grace = publish(&serial_index_root, &serial_registry);
        let parallel_grace = publish(&parallel_index_root, &parallel_registry);
        assert!(serial_grace.removals.is_empty());
        assert!(parallel_grace.removals.is_empty());
        assert_eq!(serial_grace.sources.len(), DATABASES);
        assert_eq!(parallel_grace.sources.len(), DATABASES);
        assert_eq!(
            serial_grace
                .commit
                .manifest()
                .source_catalog()
                .missing_source(&deleted_source)
                .unwrap()
                .consecutive_missing()
                .get(),
            expected_missing
        );
        assert_eq!(
            parallel_grace
                .commit
                .manifest()
                .source_catalog()
                .missing_source(&deleted_source)
                .unwrap()
                .consecutive_missing()
                .get(),
            expected_missing
        );
    }
    serial_provider.reset_scan_activity();
    parallel_provider.reset_scan_activity();
    let serial_deleted = publish(&serial_index_root, &serial_registry);
    let parallel_deleted = publish(&parallel_index_root, &parallel_registry);
    assert_eq!(
        parallel_deleted.commit.generation_id,
        serial_deleted.commit.generation_id
    );
    assert_eq!(parallel_deleted.sources, serial_deleted.sources);
    assert_eq!(parallel_deleted.removals, serial_deleted.removals);
    assert!(parallel_deleted.removals.iter().any(|removal| removal
        .deletion
        .source()
        .exact_descriptor_eq(&deleted_source)));
    assert_inventory_work(
        &serial_provider,
        DATABASES * 3 + (DATABASES - 1) * 3,
        1,
        12,
        6,
        DATABASES * 3 + (DATABASES - 1) * 3,
        DATABASES + 1,
    );
    assert_inventory_work(
        &parallel_provider,
        DATABASES * 3 + (DATABASES - 1) * 3,
        parallel_worker_count,
        12,
        6,
        DATABASES * 3 + (DATABASES - 1) * 3,
        DATABASES + 1,
    );

    writers[0]
        .execute(
            "update messages set body = body || '-terminal-race' where id = 1",
            [],
        )
        .unwrap();
    parallel_provider.set_after_seal_action(TestAfterSealAction::MutateDatabase);
    let retained_generation = parallel_deleted.commit.generation_id.clone();
    let terminal_failed = refresh_source_backed_generation(
        &parallel_index_root,
        &parallel_registry,
        writer_options(),
    )
    .unwrap();
    assert_sqlite_source_failure(
        &terminal_failed,
        SourceBackedSourceFailureClass::SourceChanged,
        &selected_database,
    );
    assert_eq!(terminal_failed.commit.generation_id, retained_generation);
    assert_eq!(terminal_failed.sources, parallel_deleted.sources);
    assert_eq!(
        VerifiedIndex::open(&parallel_index_root)
            .unwrap()
            .generation_id(),
        retained_generation
    );
    assert_eq!(
        VerifiedIndex::open(&parallel_index_root)
            .unwrap()
            .manifest()
            .sources,
        parallel_deleted.sources
    );

    let settled = publish(&parallel_index_root, &parallel_registry);
    assert_eq!(settled.outcome, SourceBackedRefreshOutcome::Completed);
    assert_eq!(settled.successful_routes, 1);
    assert!(settled.source_failures.is_empty());
    assert_ne!(settled.commit.generation_id, retained_generation);
    let available_path = catalog.lock().unwrap()[0].path.clone();
    catalog.lock().unwrap()[0].path = provider_dir.join("unavailable.sqlite");
    let unavailable = refresh_source_backed_generation(
        &parallel_index_root,
        &parallel_registry,
        writer_options(),
    )
    .unwrap();
    assert_sqlite_source_failure(
        &unavailable,
        SourceBackedSourceFailureClass::Unreadable,
        &selected_database,
    );
    assert_eq!(
        unavailable.commit.generation_id,
        settled.commit.generation_id
    );
    assert_eq!(unavailable.sources, settled.sources);
    assert_eq!(
        VerifiedIndex::open(&parallel_index_root)
            .unwrap()
            .generation_id(),
        settled.commit.generation_id
    );
    assert_eq!(
        VerifiedIndex::open(&parallel_index_root)
            .unwrap()
            .manifest()
            .sources,
        settled.sources
    );
    catalog.lock().unwrap()[0].path = available_path;
    let unavailable_retried = publish(&parallel_index_root, &parallel_registry);
    assert_eq!(
        unavailable_retried.outcome,
        SourceBackedRefreshOutcome::Completed
    );
    assert_eq!(unavailable_retried.successful_routes, 1);
    assert!(unavailable_retried.source_failures.is_empty());
    assert_eq!(
        unavailable_retried.commit.generation_id,
        settled.commit.generation_id
    );
    assert_eq!(unavailable_retried.sources, settled.sources);
    assert_no_snapshot_temp_leak(&serial_data_root);
    assert_no_snapshot_temp_leak(&parallel_data_root);
}

#[test]
fn one_logical_snapshot_distinguishes_noop_insert_update_delete_and_rewrite() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let provider_dir = temp.path().join("provider");
    let data_root = temp.path().join("data");
    let index_root = temp.path().join("index");
    fs::create_dir_all(&provider_dir).unwrap();
    let database = provider_dir.join("history.sqlite");
    let writer = active_wal_database(&database, "cold body");
    let provider = test_provider(database.clone());
    let registry = test_registry(&data_root, &database, provider.clone());

    let before = directory_file_bytes(&provider_dir);
    let cold = publish(&index_root, &registry);
    assert_eq!(directory_file_bytes(&provider_dir), before);
    assert_one_active_wal_logical_snapshot(provider.state.lock().unwrap().counters[0], 1, false);

    let before = directory_file_bytes(&provider_dir);
    let noop = publish(&index_root, &registry);
    assert_eq!(noop.commit.generation_id, cold.commit.generation_id);
    assert_eq!(directory_file_bytes(&provider_dir), before);
    assert_one_active_wal_logical_snapshot(provider.state.lock().unwrap().counters[1], 1, true);

    writer
        .execute_batch(
            "create table provider_noise (id integer primary key, value text not null);
             insert into provider_noise (value) values ('irrelevant WAL growth');",
        )
        .unwrap();
    let irrelevant_wal_growth = publish(&index_root, &registry);
    assert_eq!(
        irrelevant_wal_growth.commit.generation_id,
        cold.commit.generation_id
    );
    assert_one_active_wal_logical_snapshot(provider.state.lock().unwrap().counters[2], 1, true);

    writer
        .execute("insert into messages (id, body) values (2, 'inserted')", [])
        .unwrap();
    let inserted = publish(&index_root, &registry);
    assert_ne!(inserted.commit.generation_id, cold.commit.generation_id);
    assert_one_active_wal_logical_snapshot(provider.state.lock().unwrap().counters[3], 2, false);

    writer
        .execute("update messages set body = 'updated' where id = 1", [])
        .unwrap();
    let updated = publish(&index_root, &registry);
    assert_ne!(updated.commit.generation_id, inserted.commit.generation_id);
    assert_one_active_wal_logical_snapshot(provider.state.lock().unwrap().counters[4], 2, false);

    writer
        .execute("delete from messages where id = 2", [])
        .unwrap();
    let deleted = publish(&index_root, &registry);
    assert_ne!(deleted.commit.generation_id, updated.commit.generation_id);
    assert_one_active_wal_logical_snapshot(provider.state.lock().unwrap().counters[5], 1, false);

    writer
        .execute_batch(
            "delete from messages where id = 1;
             insert into messages (id, body) values (1, 'rewritten');",
        )
        .unwrap();
    let rewritten = publish(&index_root, &registry);
    assert_ne!(rewritten.commit.generation_id, deleted.commit.generation_id);
    assert_one_active_wal_logical_snapshot(provider.state.lock().unwrap().counters[6], 1, false);

    writer
        .execute_batch("pragma wal_checkpoint(truncate)")
        .unwrap();
    let checkpoint_only = publish(&index_root, &registry);
    assert_eq!(
        checkpoint_only.commit.generation_id,
        rewritten.commit.generation_id
    );
    assert_one_active_wal_logical_snapshot(provider.state.lock().unwrap().counters[7], 1, true);
    assert_eq!(provider.state.lock().unwrap().projections, 8);
    assert_no_snapshot_temp_leak(&data_root);
}

#[cfg(unix)]
#[test]
fn active_wal_logical_noop_works_from_a_read_only_provider_tree() {
    use std::os::unix::fs::PermissionsExt;

    let temp = crate::test_support_paths::tempdir().unwrap();
    let provider_dir = temp.path().join("provider");
    let data_root = temp.path().join("data");
    let index_root = temp.path().join("index");
    fs::create_dir_all(&provider_dir).unwrap();
    let database = provider_dir.join("history.sqlite");
    let writer = active_wal_database(&database, "read only");
    let wal = database.with_file_name("history.sqlite-wal");
    let shared_memory = database.with_file_name("history.sqlite-shm");
    assert!(wal.is_file(), "active-WAL fixture must retain its WAL");
    assert!(
        fs::metadata(&wal).unwrap().len() > 0,
        "active-WAL fixture must retain WAL payload"
    );
    assert!(
        shared_memory.is_file(),
        "active-WAL fixture must retain shared memory"
    );
    for path in [&database, &wal, &shared_memory] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o444)).unwrap();
    }
    fs::set_permissions(&provider_dir, fs::Permissions::from_mode(0o555)).unwrap();
    let before = directory_file_bytes(&provider_dir);
    let provider = test_provider(database.clone());
    let registry = test_registry(&data_root, &database, provider.clone());

    let cold = publish(&index_root, &registry);
    assert_eq!(directory_file_bytes(&provider_dir), before);
    assert_one_active_wal_logical_snapshot(provider.state.lock().unwrap().counters[0], 1, false);
    let restarted = test_registry(&data_root, &database, provider.clone());
    let exact = publish(&index_root, &restarted);
    assert_eq!(exact.commit.generation_id, cold.commit.generation_id);
    assert_eq!(directory_file_bytes(&provider_dir), before);
    assert_one_active_wal_logical_snapshot(provider.state.lock().unwrap().counters[1], 1, true);
    assert_eq!(provider.state.lock().unwrap().projections, 2);

    fs::set_permissions(&provider_dir, fs::Permissions::from_mode(0o755)).unwrap();
    for path in [&database, &wal, &shared_memory] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o644)).unwrap();
    }
    assert_no_snapshot_temp_leak(&data_root);
    drop(writer);
}

#[test]
fn empty_wal_create_remove_and_sibling_churn_are_terminal_noops() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let provider_dir = temp.path().join("provider");
    let data_root = temp.path().join("data");
    let index_root = temp.path().join("index");
    fs::create_dir_all(&provider_dir).unwrap();
    let database = provider_dir.join("history.sqlite");
    let writer = idle_wal_database(&database, "baseline");
    let wal = sqlite_component_path(&database, "-wal");
    assert!(!wal.exists(), "idle WAL fixture must start without a WAL");
    let provider = test_provider(database.clone());
    let registry = test_registry(&data_root, &database, provider.clone());
    let cold = publish(&index_root, &registry);

    provider.set_after_seal_action(TestAfterSealAction::CreateEmptyWal);
    let after_create = publish(&index_root, &registry);
    assert_eq!(after_create.commit.generation_id, cold.commit.generation_id);
    assert_eq!(fs::metadata(&wal).unwrap().len(), 0);

    provider.set_after_seal_action(TestAfterSealAction::RemoveEmptyWal);
    let after_remove = publish(&index_root, &registry);
    assert_eq!(after_remove.commit.generation_id, cold.commit.generation_id);
    assert!(!wal.exists());

    provider.set_after_seal_action(TestAfterSealAction::MutateSibling);
    let after_sibling = publish(&index_root, &registry);
    assert_eq!(
        after_sibling.commit.generation_id,
        cold.commit.generation_id
    );
    assert_eq!(
        fs::read(provider_dir.join("unrelated-sibling")).unwrap(),
        b"unrelated sibling churn"
    );
    assert_no_snapshot_temp_leak(&data_root);
    drop(writer);
}

#[test]
fn nonempty_wal_creation_fails_closed_and_clean_retry_succeeds() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let provider_dir = temp.path().join("provider");
    let data_root = temp.path().join("data");
    let index_root = temp.path().join("index");
    fs::create_dir_all(&provider_dir).unwrap();
    let database = provider_dir.join("history.sqlite");
    let writer = idle_wal_database(&database, "baseline");
    let wal = sqlite_component_path(&database, "-wal");
    let provider = test_provider(database.clone());
    let registry = test_registry(&data_root, &database, provider.clone());
    let cold = publish(&index_root, &registry);

    provider.set_after_seal_action(TestAfterSealAction::CreateNonemptyWal);
    let failed =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_sqlite_source_failure(
        &failed,
        SourceBackedSourceFailureClass::SourceChanged,
        &database,
    );
    assert_eq!(failed.commit.generation_id, cold.commit.generation_id);
    assert_eq!(failed.sources, cold.sources);
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().generation_id(),
        cold.commit.generation_id
    );
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().manifest().sources,
        cold.sources
    );
    assert!(fs::metadata(&wal).unwrap().len() > 0);

    fs::remove_file(&wal).unwrap();
    let retried = publish(&index_root, &registry);
    assert_eq!(retried.outcome, SourceBackedRefreshOutcome::Completed);
    assert_eq!(retried.successful_routes, 1);
    assert!(retried.source_failures.is_empty());
    assert_eq!(retried.commit.generation_id, cold.commit.generation_id);
    assert_eq!(retried.sources, cold.sources);
    assert_no_snapshot_temp_leak(&data_root);
    drop(writer);
}

#[test]
fn concurrent_mutation_before_and_after_seal_fails_closed_and_retries() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let provider_dir = temp.path().join("provider");
    let data_root = temp.path().join("data");
    let index_root = temp.path().join("index");
    fs::create_dir_all(&provider_dir).unwrap();
    let database = provider_dir.join("history.sqlite");
    let writer = active_wal_database(&database, "baseline");
    let provider = test_provider(database.clone());
    let registry = test_registry(&data_root, &database, provider.clone());
    let cold = publish(&index_root, &registry);

    writer
        .execute("update messages set body = 'replacement' where id = 1", [])
        .unwrap();
    provider.state.lock().unwrap().mutate_before_finish = true;
    let before_finish_failed =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_sqlite_source_failure(
        &before_finish_failed,
        SourceBackedSourceFailureClass::Unreadable,
        &database,
    );
    assert_eq!(
        before_finish_failed.commit.generation_id,
        cold.commit.generation_id
    );
    assert_eq!(before_finish_failed.sources, cold.sources);
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().generation_id(),
        cold.commit.generation_id
    );
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().manifest().sources,
        cold.sources
    );

    let replacement = publish(&index_root, &registry);
    assert_eq!(replacement.outcome, SourceBackedRefreshOutcome::Completed);
    assert_eq!(replacement.successful_routes, 1);
    assert!(replacement.source_failures.is_empty());
    assert_ne!(replacement.commit.generation_id, cold.commit.generation_id);
    provider.set_after_seal_action(TestAfterSealAction::MutateDatabase);
    let after_seal_failed =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_sqlite_source_failure(
        &after_seal_failed,
        SourceBackedSourceFailureClass::SourceChanged,
        &database,
    );
    assert_eq!(
        after_seal_failed.commit.generation_id,
        replacement.commit.generation_id
    );
    assert_eq!(after_seal_failed.sources, replacement.sources);
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().generation_id(),
        replacement.commit.generation_id
    );
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().manifest().sources,
        replacement.sources
    );
    let retried = publish(&index_root, &registry);
    assert_eq!(retried.outcome, SourceBackedRefreshOutcome::Completed);
    assert_eq!(retried.successful_routes, 1);
    assert!(retried.source_failures.is_empty());
    assert_ne!(
        retried.commit.generation_id,
        replacement.commit.generation_id
    );
    assert_no_snapshot_temp_leak(&data_root);
    drop(writer);
}

fn test_provider(database: PathBuf) -> TestProvider {
    let source = test_source("history.sqlite");
    TestProvider {
        catalog: Arc::new(Mutex::new(vec![TestLeaf {
            source,
            path: database,
        }])),
        state: Arc::new(Mutex::new(TestState::default())),
        test_leaf_workers: None,
        parser_revision: TEST_PARSER_REVISION,
    }
}

fn test_source(native_key: &str) -> SourceKey {
    SourceKey::derive(
        CaptureProvider::Shelley.as_str(),
        "shelley_sqlite",
        "sqlite-inventory-family-test-v1",
        1,
        SourceAnchor::provider_native(
            "sqlite-inventory-family-test",
            TypedKey::utf8(native_key).unwrap(),
        )
        .unwrap(),
    )
    .unwrap()
}

fn test_registry(
    data_root: &Path,
    database: &Path,
    provider: TestProvider,
) -> SourceBackedProviderRegistry {
    let source = ProviderSource {
        provider: CaptureProvider::Shelley,
        path: database.to_path_buf(),
        exists: true,
        source_format: "shelley_sqlite",
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        status: ProviderSourceStatus::Available,
        unsupported_reason: None,
    };
    let mut registry = SourceBackedProviderRegistry::new();
    SqliteInventoryDocumentAdapter::register_replacement_document_tree_route_with_authority(
        &mut registry,
        source,
        SourceBackedRouteSelection::Automatic,
        SourceBackedSelectorAuthority::ExactCwd,
        data_root,
        CaptureProvider::Shelley,
        "shelley_sqlite",
        provider,
    )
    .unwrap();
    registry
}

fn publish(
    index_root: &Path,
    registry: &SourceBackedProviderRegistry,
) -> crate::provider::source_backed::SourceBackedRefreshReceipt {
    refresh_source_backed_generation(index_root, registry, writer_options()).unwrap()
}

fn writer_options() -> WriterOptions {
    WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    }
}

fn assert_sqlite_source_failure(
    receipt: &SourceBackedRefreshReceipt,
    class: SourceBackedSourceFailureClass,
    selector: &Path,
) {
    assert_eq!(
        receipt.outcome,
        SourceBackedRefreshOutcome::CompletedWithSourceFailures
    );
    assert_eq!(receipt.successful_routes, 0);
    assert_eq!(receipt.source_failures.total(), 1);
    assert_eq!(receipt.source_failures.omitted(), 0);
    let failure = &receipt.source_failures.failures()[0];
    assert_eq!(failure.provider, CaptureProvider::Shelley);
    assert_eq!(failure.class, class);
    assert!(failure.carried_forward);
    assert_eq!(failure.source_selector, selector.display().to_string());
    assert!(!failure.detail.is_empty());
    assert_eq!(failure.source_identity.len(), 64);
}

fn effective_test_leaf_worker_count(requested_workers: usize, leaf_count: usize) -> usize {
    requested_workers
        .min(leaf_count)
        .min(source_backed_leaf_worker_budget(
            writer_options().indexer_threads,
        ))
}

fn active_wal_database(path: &Path, body: &str) -> Connection {
    let connection = Connection::open(path).unwrap();
    let mode: String = connection
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .unwrap();
    if mode != "wal" {
        connection
            .pragma_update(None, "journal_mode", "wal")
            .unwrap();
    }
    connection
        .pragma_update(None, "wal_autocheckpoint", 0)
        .unwrap();
    connection
        .execute_batch("create table messages (id integer primary key, body text not null)")
        .unwrap();
    connection
        .execute("insert into messages (id, body) values (1, ?1)", [body])
        .unwrap();
    connection
}

fn idle_wal_database(path: &Path, body: &str) -> Connection {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch("create table messages (id integer primary key, body text not null)")
        .unwrap();
    connection
        .execute("insert into messages (id, body) values (1, ?1)", [body])
        .unwrap();
    let mode: String = connection
        .query_row("pragma journal_mode = wal", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    connection
        .pragma_update(None, "wal_autocheckpoint", 0)
        .unwrap();
    assert!(!sqlite_component_path(path, "-wal").exists());
    connection
}

fn sqlite_component_path(database: &Path, suffix: &str) -> PathBuf {
    let mut component = database.as_os_str().to_os_string();
    component.push(suffix);
    PathBuf::from(component)
}

fn test_document(source: &SourceKey, id: i64, body: &str) -> CoreRecord {
    let native_session_key =
        NativeSessionKey::native_id("sqlite-inventory-family-test.session", TypedKey::I64(id))
            .unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: "sqlite-inventory-family-test-session",
        native_session_key: &native_session_key,
    })
    .unwrap();
    let native_item_key =
        NativeItemKey::native_id("sqlite-inventory-family-test.message", TypedKey::I64(id))
            .unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: "sqlite-inventory-family-test-message",
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .unwrap();
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        session_id,
        source.clone(),
        1,
        "message",
        "primary",
        true,
        TEST_PARSER_REVISION,
        body,
    )
    .unwrap();
    record.provider_session_id = Some(id.to_string());
    record.native_event_id = Some(TypedKey::I64(id));
    record.occurred_at_unix_ms = Some(1);
    record.role = Some("user".to_owned());
    record
}

fn directory_file_bytes(path: &Path) -> BTreeMap<OsString, Vec<u8>> {
    fs::read_dir(path)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (entry.file_name(), fs::read(entry.path()).unwrap())
        })
        .collect()
}

fn assert_inventory_work(
    provider: &TestProvider,
    projections: usize,
    maximum_peak_scans: usize,
    discoveries: usize,
    terminal_callbacks: usize,
    snapshot_observations: usize,
    logical_replacements: usize,
) {
    let state = provider.state.lock().unwrap();
    assert_eq!(state.projections, projections);
    assert!((1..=maximum_peak_scans).contains(&state.peak_scans));
    assert_eq!(state.discoveries, discoveries);
    assert_eq!(state.terminal_callbacks, terminal_callbacks);
    assert_eq!(state.active_scans, 0);
    assert!(state.active_scans_by_path.is_empty());
    assert_eq!(state.max_active_scans_per_path, 1);
    assert_eq!(state.counters.len(), snapshot_observations);
    assert_eq!(
        state
            .counters
            .iter()
            .filter(|counters| counters.logical_replacements == 1)
            .count(),
        logical_replacements
    );
    for counters in &state.counters {
        assert_one_active_wal_logical_snapshot(*counters, 1, counters.logical_noops == 1);
    }
}

fn assert_one_active_wal_logical_snapshot(
    counters: SqliteInventorySnapshotCounters,
    expected_rows: u64,
    unchanged: bool,
) {
    assert_eq!(counters.immutable_snapshot_opens, 0);
    assert_eq!(counters.copied_snapshot_opens, 1);
    assert!(counters.source_bytes_copied > 0);
    assert_eq!(counters.logical_projection_passes, 1);
    assert_eq!(counters.logical_rows_projected, expected_rows);
    assert_eq!(counters.documents_staged, expected_rows);
    assert_eq!(counters.logical_noops, u64::from(unchanged));
    assert_eq!(counters.logical_replacements, u64::from(!unchanged));
    assert_eq!(counters.terminal_fences, 1);
    assert!(counters.terminal_revalidations >= 2);
    assert_eq!(counters.active_snapshots, 0);
    assert_eq!(counters.max_active_snapshots, 1);
}

fn assert_no_snapshot_temp_leak(data_root: &Path) {
    let snapshots = data_root.join("tmp/provider-sqlite");
    if snapshots.exists() {
        assert_eq!(fs::read_dir(snapshots).unwrap().count(), 0);
    }
}
