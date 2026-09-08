use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use ctx_history_capture_model::SourceRouteIdentity;
use ctx_history_capture_runtime::{
    CompleteInventoryOwner, DocumentInventoryAuthority, DocumentLeafExecutionPolicy,
    DocumentRecordSpool, SourceBackedCertifiedRemoval, SourceBackedGenerationSink,
    SourceBackedLogicalSourceFailures, SourceBackedRecordRejections, SourceBackedRouteError,
    SourceBackedRouteErrorKind, SourceBackedRouteResources, SourceBackedRouteResult, SourceOwner,
};
use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, CertifiedSource, CoreRecord,
    EventIdentityInput, NativeItemKey, NativeSessionKey, ScannedSourceCounts, SessionIdentityInput,
    SourceAnchor, SourceKey, SourceObservation, TypedKey,
};
use rusqlite::Connection;
use sha2::{Digest, Sha256};

use super::*;
use crate::{
    provider_sources::SqliteSourceAccessError,
    registration::{
        astrbot_scan_route_result, lingma_scan_route_result, AstrBotSourceBackedErrorV0,
        LingmaSourceBackedErrorV0,
    },
    CaptureError,
};

mod lifecycle_tests;

use lifecycle_tests::TestLifecycle;

const TEST_PARSER_REVISION: &str = "sqlite-inventory-pack-contract-v1";

fn test_route_identity() -> SourceRouteIdentity {
    SourceRouteIdentity::from_sha256("11".repeat(32)).unwrap()
}

#[derive(Clone)]
struct TestLeaf {
    source: SourceKey,
    path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProjectionOutput {
    certificate: CertifiedSource,
    rows: Vec<(i64, String)>,
}

#[derive(Default)]
struct TestState {
    projections: usize,
    discoveries: usize,
    terminal_callbacks: usize,
    counters: Vec<SqliteInventorySnapshotCounters>,
    outputs: Vec<ProjectionOutput>,
    after_seal_action: Option<TestAfterSealAction>,
}

enum TestAfterSealAction {
    CreateNonemptyWal,
}

#[derive(Clone)]
struct TestProvider {
    catalog: Arc<Mutex<Vec<TestLeaf>>>,
    state: Arc<Mutex<TestState>>,
}

impl TestProvider {
    fn production(catalog: Arc<Mutex<Vec<TestLeaf>>>) -> Self {
        Self {
            catalog,
            state: Arc::default(),
        }
    }

    fn set_after_seal_action(&self, action: TestAfterSealAction) {
        let replaced = self.state.lock().unwrap().after_seal_action.replace(action);
        assert!(replaced.is_none());
    }

    fn sorted_outputs(&self) -> Vec<ProjectionOutput> {
        let mut outputs = self.state.lock().unwrap().outputs.clone();
        outputs.sort_by_key(|output| {
            output
                .certificate
                .observation()
                .source()
                .identity()
                .digest()
        });
        outputs
    }

    fn reset_run(&self) {
        let mut state = self.state.lock().unwrap();
        state.projections = 0;
        state.discoveries = 0;
        state.terminal_callbacks = 0;
        state.counters.clear();
        state.outputs.clear();
    }
}

impl SqliteInventoryProvider<TestLifecycle, TestSpool> for TestProvider {
    type Leaf = TestLeaf;

    fn parser_revision(&self) -> &'static str {
        TEST_PARSER_REVISION
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
                physical_locator: leaf.path.clone(),
                provider_leaf: leaf,
            })
            .collect();
        Ok(SqliteInventoryCatalog {
            authority_fingerprint: [0x17; 32],
            leaves,
        })
    }

    fn scan(
        &self,
        leaf: &Self::Leaf,
        snapshot: SqliteSourceReadSnapshot,
        sink: &mut ChangedDocumentSink<'_, '_, TestLifecycle, TestSpool>,
    ) -> SourceBackedRouteResult<CertifiedSource> {
        self.state.lock().unwrap().projections += 1;
        let rows = {
            let connection = snapshot.connection().map_err(sqlite_source_route_error)?;
            let mut statement = connection
                .prepare("select id, body from messages order by id")
                .map_err(|error| {
                    sqlite_source_route_error(snapshot.diagnose_provider_query_error(
                        "querying the test provider database",
                        error,
                        crate::provider_sources::SqliteFailurePhase::Projection,
                    ))
                })?;
            let rows = statement
                .query_map([], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|error| {
                    sqlite_source_route_error(snapshot.diagnose_provider_query_error(
                        "querying the test provider database",
                        error,
                        crate::provider_sources::SqliteFailurePhase::Projection,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|error| {
                    sqlite_source_route_error(snapshot.diagnose_provider_query_error(
                        "reading the test provider database",
                        error,
                        crate::provider_sources::SqliteFailurePhase::Projection,
                    ))
                })?;
            rows
        };
        let mut content = Sha256::new();
        content.update(b"ctx.sqlite-inventory-pack-contract-content-v1\0");
        let mut certified_bytes = 0_u64;
        for (id, body) in &rows {
            content.update(id.to_be_bytes());
            content.update((body.len() as u64).to_be_bytes());
            content.update(body.as_bytes());
            certified_bytes = certified_bytes.saturating_add(body.len() as u64);
            sink.emit_core_record(test_document(&leaf.source, *id, body))?;
        }
        let content_digest: [u8; 32] = content.finalize().into();
        snapshot.finish().map_err(sqlite_source_route_error)?;
        let observation = SourceObservation::new(
            leaf.source.clone(),
            "sqlite-inventory-pack-contract-observation-v1",
            content_digest.to_vec(),
        )
        .map_err(sqlite_capture_error)?;
        let count = rows.len() as u64;
        let certificate = CertifiedSource::certify(
            observation.clone(),
            observation,
            TEST_PARSER_REVISION,
            content_digest,
            ScannedSourceCounts {
                complete_records: count,
                retained_records: count,
                indexed_documents: count,
                certified_bytes,
                ..ScannedSourceCounts::default()
            },
        )
        .map_err(sqlite_capture_error)?;
        self.state.lock().unwrap().outputs.push(ProjectionOutput {
            certificate: certificate.clone(),
            rows,
        });
        Ok(certificate)
    }

    fn after_snapshots_sealed(&self) {
        let action = {
            let mut state = self.state.lock().unwrap();
            state.terminal_callbacks = state.terminal_callbacks.saturating_add(1);
            state.after_seal_action.take()
        };
        if let Some(TestAfterSealAction::CreateNonemptyWal) = action {
            let database = self.catalog.lock().unwrap()[0].path.clone();
            fs::write(
                sqlite_component_path(&database, "-wal"),
                b"nonempty concurrent WAL sentinel",
            )
            .unwrap();
        }
    }

    fn observe_snapshot_counters(&self, counters: SqliteInventorySnapshotCounters) {
        self.state.lock().unwrap().counters.push(counters);
    }
}

#[derive(Default)]
struct TestSpool(Vec<CoreRecord>);

impl DocumentRecordSpool for TestSpool {
    fn new(_resources: SourceBackedRouteResources) -> SourceBackedRouteResult<Self> {
        Ok(Self::default())
    }

    fn push(&mut self, record: CoreRecord) -> SourceBackedRouteResult<()> {
        self.0.push(record);
        Ok(())
    }

    fn replay(
        self,
        mut emit: impl FnMut(CoreRecord) -> SourceBackedRouteResult<()>,
    ) -> SourceBackedRouteResult<()> {
        for record in self.0 {
            emit(record)?;
        }
        Ok(())
    }
}

struct SinkHarness {
    lifecycle: TestLifecycle,
    owners: HashMap<[u8; 32], SourceOwner>,
    complete_inventories: Vec<CompleteInventoryOwner>,
    applied_removals: Vec<SourceBackedCertifiedRemoval>,
    logical_source_failures: SourceBackedLogicalSourceFailures,
    record_rejections: SourceBackedRecordRejections,
}

impl SinkHarness {
    fn with_base(base_sources: Vec<CertifiedSource>) -> Self {
        Self {
            lifecycle: TestLifecycle::with_base(base_sources),
            owners: HashMap::new(),
            complete_inventories: Vec::new(),
            applied_removals: Vec::new(),
            logical_source_failures: SourceBackedLogicalSourceFailures::default(),
            record_rejections: SourceBackedRecordRejections::default(),
        }
    }

    fn sink(&mut self, workers: usize) -> SourceBackedGenerationSink<'_, TestLifecycle> {
        SourceBackedGenerationSink::new(
            &mut self.lifecycle,
            &mut self.owners,
            &mut self.complete_inventories,
            &mut self.applied_removals,
            0,
            test_route_identity(),
            None,
            SourceBackedRouteResources::production(workers),
            &mut self.logical_source_failures,
            &mut self.record_rejections,
            None,
            None,
            None,
        )
    }
}

fn run_provider(
    data_root: &Path,
    provider: TestProvider,
    workers: usize,
    base_sources: Vec<CertifiedSource>,
) -> SourceBackedRouteResult<SinkHarness> {
    let adapter = SqliteInventoryDocumentAdapter::new(
        data_root,
        CaptureProvider::Shelley,
        "shelley_sqlite",
        provider,
    );
    let driver = ctx_history_capture_runtime::replacement_document_tree_driver(
        DocumentInventoryAuthority::new(CaptureProvider::Shelley.as_str().to_owned(), [0x31; 32]),
        adapter,
    );
    let mut harness = SinkHarness::with_base(base_sources);
    driver.scan(&mut harness.sink(workers))?;
    for owned in &harness.complete_inventories {
        match driver.revalidate_complete_inventory(owned.inventory()) {
            Some(Ok(true)) => {}
            Some(Ok(false)) => {
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::SourceChanged,
                    "SQLite inventory failed complete-inventory revalidation",
                ));
            }
            Some(Err(error)) => return Err(error),
            None => panic!("SQLite inventory driver omitted complete-inventory revalidation"),
        }
    }
    if driver.publication_revalidation() == Some(false) {
        return Err(SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::SourceChanged,
            "SQLite inventory failed terminal publication revalidation",
        ));
    }
    Ok(harness)
}

fn test_source(native_key: &str) -> SourceKey {
    SourceKey::derive(
        CaptureProvider::Shelley.as_str(),
        "shelley_sqlite",
        "sqlite-inventory-pack-contract-v1",
        1,
        SourceAnchor::provider_native(
            "sqlite-inventory-pack-contract",
            TypedKey::utf8(native_key).unwrap(),
        )
        .unwrap(),
    )
    .unwrap()
}

fn test_document(source: &SourceKey, id: i64, body: &str) -> CoreRecord {
    let native_session_key =
        NativeSessionKey::native_id("sqlite-inventory-pack-contract.session", TypedKey::I64(id))
            .unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: "sqlite-inventory-pack-contract-session",
        native_session_key: &native_session_key,
    })
    .unwrap();
    let native_item_key =
        NativeItemKey::native_id("sqlite-inventory-pack-contract.message", TypedKey::I64(id))
            .unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: "sqlite-inventory-pack-contract-message",
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .unwrap();
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        source.clone(),
        1,
        "message",
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

fn active_wal_database(path: &Path, body: &str) -> Connection {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
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
    fs::create_dir_all(path.parent().unwrap()).unwrap();
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

fn assert_one_snapshot_per_database(provider: &TestProvider, databases: usize) {
    let state = provider.state.lock().unwrap();
    assert_eq!(state.projections, databases);
    assert_eq!(state.discoveries, 2);
    assert_eq!(state.terminal_callbacks, 1);
    assert_eq!(state.counters.len(), databases);
    for counters in &state.counters {
        assert_eq!(counters.immutable_snapshot_opens, 0);
        assert_eq!(counters.copied_snapshot_opens, 1);
        assert!(counters.source_bytes_copied > 0);
        assert_eq!(counters.logical_projection_passes, 1);
        assert_eq!(counters.logical_rows_projected, 1);
        assert_eq!(counters.documents_staged, 1);
        assert_eq!(counters.logical_noops, 0);
        assert_eq!(counters.logical_replacements, 1);
        assert_eq!(counters.terminal_fences, 1);
        assert!(counters.terminal_revalidations >= 2);
        assert_eq!(counters.active_snapshots, 0);
        assert_eq!(counters.max_active_snapshots, 1);
    }
}

fn assert_zero_snapshot_replay(provider: &TestProvider, databases: usize) {
    let state = provider.state.lock().unwrap();
    assert_eq!(state.projections, 0);
    assert_eq!(state.discoveries, 2);
    assert_eq!(state.terminal_callbacks, 1);
    assert_eq!(state.counters.len(), databases);
    assert!(state
        .counters
        .iter()
        .all(|counters| *counters == SqliteInventorySnapshotCounters::default()));
}

fn assert_one_changed_snapshot(provider: &TestProvider, databases: usize) {
    let state = provider.state.lock().unwrap();
    assert_eq!(state.projections, 1);
    assert_eq!(state.discoveries, 2);
    assert_eq!(state.terminal_callbacks, 1);
    assert_eq!(state.counters.len(), databases);
    assert_eq!(
        state
            .counters
            .iter()
            .filter(|counters| counters.copied_snapshot_opens == 1)
            .count(),
        1
    );
    assert_eq!(
        state
            .counters
            .iter()
            .filter(|counters| **counters == SqliteInventorySnapshotCounters::default())
            .count(),
        databases - 1
    );
}

fn assert_no_snapshot_temp_leak(data_root: &Path) {
    let snapshots = data_root.join("tmp/provider-sqlite");
    if snapshots.exists() {
        assert_eq!(fs::read_dir(snapshots).unwrap().count(), 0);
    }
}

fn cleanup_failure_for_test() -> SqliteSourceAccessError {
    SqliteSourceAccessError::CleanupUnavailable {
        operation: "remove test snapshot",
        source: Box::new(SqliteSourceAccessError::SourceChanged),
    }
}

#[test]
fn provider_sink_failure_does_not_mask_snapshot_cleanup_failure() {
    let sink_failure =
        || SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, "staging sink failed");
    let astrbot = astrbot_scan_route_result(
        Some(sink_failure()),
        Err(AstrBotSourceBackedErrorV0::SnapshotCleanup {
            primary: Box::new(AstrBotSourceBackedErrorV0::Capture(
                CaptureError::InvalidPayload("sink sentinel".to_owned()),
            )),
            cleanup: cleanup_failure_for_test(),
        }),
    )
    .unwrap_err();
    let lingma = lingma_scan_route_result(
        Some(sink_failure()),
        Err(LingmaSourceBackedErrorV0::SnapshotCleanup {
            primary: Box::new(LingmaSourceBackedErrorV0::Capture(
                CaptureError::InvalidPayload("sink sentinel".to_owned()),
            )),
            cleanup: cleanup_failure_for_test(),
        }),
    )
    .unwrap_err();

    for error in [astrbot, lingma] {
        assert_eq!(error.kind, SourceBackedRouteErrorKind::Internal);
        assert!(error.detail.contains("staging sink failed"));
        assert!(error
            .detail
            .contains("explicit SQLite snapshot cleanup also failed"));
        assert!(error.detail.contains("ctx-owned SQLite cleanup failed"));
    }
}

#[test]
fn sqlite_inventory_uses_serial_bounded_streaming() {
    for provider in [
        CaptureProvider::AstrBot,
        CaptureProvider::Crush,
        CaptureProvider::Lingma,
        CaptureProvider::Shelley,
    ] {
        assert_eq!(
            sqlite_inventory_leaf_execution_policy(provider),
            DocumentLeafExecutionPolicy::Serial
        );
    }
}

#[test]
fn sqlite_inventory_cold_noop_change_and_delete_use_one_snapshot_per_changed_database() {
    const DATABASES: usize = 8;

    let temp = crate::test_support_paths::tempdir().unwrap();
    let provider_dir = temp.path().join("provider");
    let data_root = temp.path().join("data");
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
    let catalog = Arc::new(Mutex::new(leaves));
    let provider = TestProvider::production(catalog);

    let cold = run_provider(&data_root, provider.clone(), 4, Vec::new()).unwrap();
    let base = cold.lifecycle.sources();
    assert_one_snapshot_per_database(&provider, DATABASES);
    assert_no_snapshot_temp_leak(&data_root);

    provider.reset_run();
    let noop = run_provider(&data_root, provider.clone(), 4, base).unwrap();
    let base = noop.lifecycle.sources();
    assert_zero_snapshot_replay(&provider, DATABASES);

    writers[3]
        .execute(
            "update messages set body = body || '-changed' where id = 1",
            [],
        )
        .unwrap();
    provider.reset_run();
    let changed = run_provider(&data_root, provider.clone(), 4, base).unwrap();
    let base = changed.lifecycle.sources();
    assert_one_changed_snapshot(&provider, DATABASES);

    let deleted = provider.catalog.lock().unwrap().pop().unwrap().source;
    provider.reset_run();
    let after_delete = run_provider(&data_root, provider.clone(), 4, base).unwrap();
    assert_eq!(after_delete.applied_removals.len(), 1);
    assert!(after_delete.applied_removals[0]
        .deletion
        .source()
        .exact_descriptor_eq(&deleted));
    let retained = after_delete.lifecycle.sources();
    assert_eq!(retained.len(), DATABASES - 1);
    assert!(retained.iter().all(|certificate| !certificate
        .observation()
        .source()
        .exact_descriptor_eq(&deleted)));
    assert_zero_snapshot_replay(&provider, DATABASES - 1);
    assert_no_snapshot_temp_leak(&data_root);
    drop(writers);
}

#[test]
fn nonempty_wal_creation_fails_closed_and_clean_retry_succeeds() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let provider_dir = temp.path().join("provider");
    let data_root = temp.path().join("data");
    let database = provider_dir.join("history.sqlite");
    let writer = idle_wal_database(&database, "baseline");
    let provider = TestProvider::production(Arc::new(Mutex::new(vec![TestLeaf {
        source: test_source("history.sqlite"),
        path: database.clone(),
    }])));
    provider.set_after_seal_action(TestAfterSealAction::CreateNonemptyWal);

    let error = run_provider(&data_root, provider.clone(), 1, Vec::new())
        .err()
        .expect("nonempty WAL must fail terminal revalidation");
    assert_eq!(error.kind, SourceBackedRouteErrorKind::SourceChanged);
    let wal = sqlite_component_path(&database, "-wal");
    assert!(fs::metadata(&wal).unwrap().len() > 0);

    fs::remove_file(&wal).unwrap();
    run_provider(&data_root, provider.clone(), 1, Vec::new()).unwrap();
    let outputs = provider.sorted_outputs();
    assert_eq!(outputs.len(), 2);
    assert_eq!(outputs[0], outputs[1]);
    assert_no_snapshot_temp_leak(&data_root);
    drop(writer);
}

#[test]
fn production_shared_inventory_routes_corruption_and_ctx_staging_failure_by_provenance() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("provider/history.sqlite");
    drop(active_wal_database(&database, "corrupt-me"));
    let mut bytes = fs::read(&database).unwrap();
    bytes[..16].copy_from_slice(b"not sqlite data!");
    fs::write(&database, bytes).unwrap();
    let provider = TestProvider::production(Arc::new(Mutex::new(vec![TestLeaf {
        source: test_source("corrupt.sqlite"),
        path: database,
    }])));
    let error = run_provider(&temp.path().join("corrupt-data"), provider, 1, Vec::new())
        .err()
        .expect("provider corruption must fail the production-serial route");
    assert_eq!(error.kind, SourceBackedRouteErrorKind::InvalidSource);
    assert!(
        error.detail.contains("artifact_kind=provider_database"),
        "unexpected corruption detail: {}",
        error.detail
    );

    let healthy = temp.path().join("provider/healthy.sqlite");
    let writer = active_wal_database(&healthy, "healthy");
    let blocked_data_root = temp.path().join("blocked-data-root");
    fs::write(&blocked_data_root, b"not a directory").unwrap();
    let provider = TestProvider::production(Arc::new(Mutex::new(vec![TestLeaf {
        source: test_source("healthy.sqlite"),
        path: healthy,
    }])));
    let error = run_provider(&blocked_data_root, provider, 1, Vec::new())
        .err()
        .expect("blocked ctx staging root must fail the route");
    assert_eq!(error.kind, SourceBackedRouteErrorKind::ResourceUnavailable);
    assert!(!error.detail.contains("artifact_kind=provider_database"));
    drop(writer);
}

fn sqlite_capture_error(error: impl std::fmt::Display) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::InvalidSource, error.to_string())
}
