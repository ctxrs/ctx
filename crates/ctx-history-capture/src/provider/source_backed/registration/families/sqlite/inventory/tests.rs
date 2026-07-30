use std::{
    collections::{BTreeMap, HashMap},
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Barrier, Mutex},
};

use ctx_history_core::{
    derive_event_id, derive_session_id, EventIdentityInput, LocatorRevisionPolicy, NativeItemKey,
    NativeRecordCoordinate, NativeSessionKey, ScannedSourceCounts, SessionIdentityInput,
    SourceAnchor, SourceObservation, SourceRecordLocator,
};
use ctx_history_index::{LexicalDocument, VerifiedIndex, WriterOptions};
use rusqlite::Connection;
use sha2::{Digest, Sha256};

use crate::{
    provider::source_backed::family::document::DocumentLeafExecutionPolicy, ProviderCatalogSupport,
};

use super::{
    shared::{
        sqlite_inventory_leaf_execution_policy, SqliteInventoryCatalog, SqliteInventoryCatalogLeaf,
        SqliteInventoryDocumentAdapter, SqliteInventoryProvider, SqliteInventorySnapshotCounters,
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
    mutate_after_seal: bool,
    scan_barrier: Option<Arc<Barrier>>,
    active_scans: usize,
    peak_scans: usize,
    active_scans_by_path: HashMap<PathBuf, usize>,
    max_active_scans_per_path: usize,
}

#[derive(Clone)]
struct TestProvider {
    catalog: Arc<Mutex<Vec<TestLeaf>>>,
    state: Arc<Mutex<TestState>>,
    test_leaf_workers: Option<usize>,
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
        TEST_PARSER_REVISION
    }

    fn logical_tables(&self) -> &'static [&'static str] {
        &["messages"]
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
            sink.emit_document(test_document(&leaf.source, *id, body))?;
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
            TEST_PARSER_REVISION,
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

    fn hydrate(
        &self,
        _request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure> {
        Err(hydration_failure(
            HydrationFailureKind::UnsupportedParserRevision,
            "test adapter does not hydrate",
        ))
    }

    fn after_snapshots_sealed(&self) {
        let mutate_after_seal = {
            let mut state = self.state.lock().unwrap();
            state.terminal_callbacks = state.terminal_callbacks.saturating_add(1);
            std::mem::take(&mut state.mutate_after_seal)
        };
        if mutate_after_seal {
            self.mutate("-after-seal");
        }
    }

    fn observe_snapshot_counters(&self, counters: SqliteInventorySnapshotCounters) {
        self.state.lock().unwrap().counters.push(counters);
    }

    fn test_leaf_execution_policy(&self) -> Option<DocumentLeafExecutionPolicy> {
        self.test_leaf_workers
            .map(DocumentLeafExecutionPolicy::IndependentWithWorkers)
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
            DocumentLeafExecutionPolicy::Independent
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
    let serial_provider = TestProvider::with_test_workers(Arc::clone(&catalog), 1);
    let parallel_provider = TestProvider::with_test_workers(Arc::clone(&catalog), 4);
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

    parallel_provider.use_scan_barrier(4);
    let before = directory_file_bytes(&provider_dir);
    let serial_cold = publish(&serial_index_root, &serial_registry);
    let parallel_cold = publish(&parallel_index_root, &parallel_registry);
    assert_eq!(directory_file_bytes(&provider_dir), before);
    assert_eq!(
        parallel_cold.commit.generation_id,
        serial_cold.commit.generation_id
    );
    assert_eq!(parallel_cold.sources, serial_cold.sources);
    assert_inventory_work(&serial_provider, DATABASES, 1, 2, 1, DATABASES);
    assert_inventory_work(&parallel_provider, DATABASES, 4, 2, 1, DATABASES);

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
    assert_inventory_work(&serial_provider, DATABASES, 0, 4, 2, DATABASES * 2);
    assert_inventory_work(&parallel_provider, DATABASES, 0, 4, 2, DATABASES * 2);

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
    assert_inventory_work(&serial_provider, DATABASES + 1, 1, 6, 3, DATABASES * 3);
    assert_inventory_work(&parallel_provider, DATABASES + 1, 1, 6, 3, DATABASES * 3);

    let deleted_source = catalog.lock().unwrap().pop().unwrap().source;
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
        DATABASES + 1,
        0,
        8,
        4,
        DATABASES * 3 + DATABASES - 1,
    );
    assert_inventory_work(
        &parallel_provider,
        DATABASES + 1,
        0,
        8,
        4,
        DATABASES * 3 + DATABASES - 1,
    );

    writers[0]
        .execute(
            "update messages set body = body || '-terminal-race' where id = 1",
            [],
        )
        .unwrap();
    parallel_provider.state.lock().unwrap().mutate_after_seal = true;
    let retained_generation = parallel_deleted.commit.generation_id;
    assert!(refresh_source_backed_generation(
        &parallel_index_root,
        &parallel_registry,
        writer_options()
    )
    .is_err());
    assert_eq!(
        VerifiedIndex::open(&parallel_index_root)
            .unwrap()
            .generation_id(),
        retained_generation
    );

    let settled = publish(&parallel_index_root, &parallel_registry);
    catalog.lock().unwrap()[0].path = provider_dir.join("unavailable.sqlite");
    assert!(refresh_source_backed_generation(
        &parallel_index_root,
        &parallel_registry,
        writer_options()
    )
    .is_err());
    assert_eq!(
        VerifiedIndex::open(&parallel_index_root)
            .unwrap()
            .generation_id(),
        settled.commit.generation_id
    );
    assert_no_snapshot_temp_leak(&serial_data_root);
    assert_no_snapshot_temp_leak(&parallel_data_root);
}

#[test]
fn one_snapshot_cold_replay_replacement_and_wal_transition() {
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
    assert_eq!(provider.state.lock().unwrap().projections, 1);
    assert_one_copied_snapshot(provider.state.lock().unwrap().counters[0]);

    let before = directory_file_bytes(&provider_dir);
    let restarted_registry = test_registry(&data_root, &database, provider.clone());
    let replay = publish(&index_root, &restarted_registry);
    assert_eq!(directory_file_bytes(&provider_dir), before);
    assert_eq!(replay.commit.generation_id, cold.commit.generation_id);
    assert_eq!(provider.state.lock().unwrap().projections, 1);
    assert_one_copied_snapshot(provider.state.lock().unwrap().counters[1]);

    writer
        .execute(
            "update messages set body = ?1 where id = 1",
            ["replacement body"],
        )
        .unwrap();
    let before = directory_file_bytes(&provider_dir);
    let replacement = publish(&index_root, &registry);
    assert_eq!(directory_file_bytes(&provider_dir), before);
    assert_ne!(replacement.commit.generation_id, cold.commit.generation_id);
    assert_eq!(provider.state.lock().unwrap().projections, 2);
    assert_one_copied_snapshot(provider.state.lock().unwrap().counters[2]);

    writer
        .execute_batch("pragma wal_checkpoint(truncate)")
        .unwrap();
    drop(writer);
    let before = directory_file_bytes(&provider_dir);
    let checkpoint_only = publish(&index_root, &registry);
    assert_eq!(directory_file_bytes(&provider_dir), before);
    assert_eq!(
        checkpoint_only.commit.generation_id,
        replacement.commit.generation_id
    );
    assert_eq!(provider.state.lock().unwrap().projections, 2);
    let counters = provider.state.lock().unwrap().counters[3];
    #[cfg(target_os = "linux")]
    {
        assert_eq!(counters.immutable_snapshot_opens, 1);
        assert_eq!(counters.copied_snapshot_opens, 0);
        assert_eq!(counters.source_bytes_copied, 0);
    }
    #[cfg(not(target_os = "linux"))]
    assert_one_copied_snapshot(counters);
    assert_eq!(counters.terminal_fences, 1);
    assert!(counters.terminal_revalidations >= 2);
    assert_eq!(counters.active_snapshots, 0);
    assert_eq!(counters.max_active_snapshots, 1);
    assert_no_snapshot_temp_leak(&data_root);
}

#[test]
fn mutation_before_and_after_seal_fails_closed_at_commit() {
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
    assert!(refresh_source_backed_generation(&index_root, &registry, writer_options()).is_err());
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().generation_id(),
        cold.commit.generation_id
    );

    let replacement = publish(&index_root, &registry);
    provider.state.lock().unwrap().mutate_after_seal = true;
    assert!(refresh_source_backed_generation(&index_root, &registry, writer_options()).is_err());
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().generation_id(),
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

fn test_document(source: &SourceKey, id: i64, body: &str) -> LexicalDocument {
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
    let digest: [u8; 32] = Sha256::digest(body.as_bytes()).into();
    let locator = SourceRecordLocator::new(
        source.clone(),
        NativeRecordCoordinate::ProviderSqlite {
            logical_relation: "messages".to_owned(),
            primary_key: TypedKey::I64(id),
            row_version: None,
        },
        LocatorRevisionPolicy::StableRecordEvidence,
        None,
        digest,
    )
    .unwrap();
    LexicalDocument {
        event_id,
        session_id,
        parent_session_id: None,
        root_session_id: session_id,
        source: source.clone(),
        locator,
        provider_session_id: Some(id.to_string()),
        branch: None,
        source_path: None,
        agent_type: "primary".to_owned(),
        is_primary: true,
        event_sequence: 1,
        occurred_at_unix_ms: Some(1),
        event_type: "message".to_owned(),
        role: Some("user".to_owned()),
        body: body.to_owned(),
        workspace: None,
        cwd: None,
        touched_files: Vec::new(),
    }
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
    peak_scans: usize,
    discoveries: usize,
    terminal_callbacks: usize,
    snapshot_observations: usize,
) {
    let state = provider.state.lock().unwrap();
    assert_eq!(state.projections, projections);
    assert_eq!(state.peak_scans, peak_scans);
    assert_eq!(state.discoveries, discoveries);
    assert_eq!(state.terminal_callbacks, terminal_callbacks);
    assert_eq!(state.active_scans, 0);
    assert!(state.active_scans_by_path.is_empty());
    assert_eq!(state.max_active_scans_per_path, usize::from(peak_scans > 0));
    assert_eq!(state.counters.len(), snapshot_observations);
    for counters in &state.counters {
        assert_one_copied_snapshot(*counters);
    }
}

fn assert_one_copied_snapshot(counters: SqliteInventorySnapshotCounters) {
    assert_eq!(counters.immutable_snapshot_opens, 0);
    assert_eq!(counters.copied_snapshot_opens, 1);
    assert!(counters.source_bytes_copied > 0);
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
