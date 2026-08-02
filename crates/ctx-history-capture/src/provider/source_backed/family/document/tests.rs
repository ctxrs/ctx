use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Barrier, Mutex},
};

use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, CoreRecord, EventIdentityInput,
    NativeItemKey, NativeSessionKey, SessionIdentityInput, SourceAnchor,
};
use ctx_history_index::{VerifiedIndex, WriterOptions};
use sha2::{Digest, Sha256};

use super::*;
use crate::{
    provider::source_backed::{
        refresh_source_backed_generation, source_backed_leaf_worker_budget,
        SourceBackedCoordinatorError, SourceBackedProviderRegistry, SourceBackedRefreshOutcome,
        SourceBackedRefreshReceipt, SourceBackedSourceFailureClass,
        AUTOMATIC_SOURCE_DELETION_MISSING_INVENTORIES,
    },
    ProviderCatalogSupport, ProviderImportSupport, ProviderSourceKind, ProviderSourceStatus,
    AUGGIE_SESSION_JSON_SOURCE_FORMAT,
};

const SYNTHETIC_SCHEMA_V1: &str = "synthetic-document-family-v1";
const SYNTHETIC_SCHEMA_V2: &str = "synthetic-document-family-v2";

#[derive(Debug, Clone, PartialEq, Eq)]
struct SyntheticLeaf {
    physical_id: u8,
    logical_id: u8,
    revision: u8,
    body: String,
}

impl SyntheticLeaf {
    fn source(&self) -> SourceKey {
        self.source_with_schema(SYNTHETIC_SCHEMA_V1)
    }

    fn source_with_schema(&self, schema_variant: &str) -> SourceKey {
        SourceKey::derive(
            CaptureProvider::Auggie.as_str(),
            AUGGIE_SESSION_JSON_SOURCE_FORMAT,
            schema_variant,
            1,
            SourceAnchor::CatalogLineage([self.logical_id; 32]),
        )
        .unwrap()
    }

    fn fingerprint(&self) -> DocumentLeafFingerprint {
        let mut digest = Sha256::new();
        digest.update(b"ctx.synthetic-document-leaf-v1\0");
        digest.update([self.physical_id, self.revision]);
        digest.update((self.body.len() as u64).to_be_bytes());
        digest.update(self.body.as_bytes());
        DocumentLeafFingerprint::new(digest.finalize().into())
    }

    fn content_digest(&self) -> [u8; 32] {
        Sha256::digest(self.body.as_bytes()).into()
    }
}

#[derive(Default)]
struct SyntheticState {
    available: bool,
    leaves: Vec<SyntheticLeaf>,
    durable_replay: bool,
    bind_durable_replay_descriptor: bool,
    parser_v2: bool,
    schema_v2: bool,
    scan_counts: HashMap<u8, usize>,
    discovery_calls: usize,
    mutate_before_scan: Option<u8>,
    mutate_on_revalidate: bool,
    leaf_execution_policy: DocumentLeafExecutionPolicy,
    scan_barrier: Option<Arc<Barrier>>,
    active_scans: usize,
    peak_scans: usize,
}

#[derive(Clone)]
struct SyntheticAdapter {
    state: Arc<Mutex<SyntheticState>>,
}

impl SyntheticAdapter {
    fn new(leaves: Vec<SyntheticLeaf>) -> Self {
        Self {
            state: Arc::new(Mutex::new(SyntheticState {
                available: true,
                leaves,
                durable_replay: true,
                ..SyntheticState::default()
            })),
        }
    }

    fn tree(state: &SyntheticState) -> CompleteDocumentTree<SyntheticLeaf, [u8; 32]> {
        let leaves = state
            .leaves
            .iter()
            .cloned()
            .map(|leaf| {
                let fingerprint = leaf.fingerprint();
                if state.durable_replay {
                    ObservedDocumentLeaf::new(fingerprint, leaf)
                } else {
                    ObservedDocumentLeaf::with_durable_replay(fingerprint, leaf, false)
                }
            })
            .collect::<Vec<_>>();
        let tree_fingerprint = synthetic_tree_fingerprint(&leaves);
        CompleteDocumentTree::new(tree_fingerprint, leaves, tree_fingerprint)
    }

    fn source(&self, logical_id: u8) -> SourceKey {
        let state = self.state.lock().unwrap();
        let leaf = state
            .leaves
            .iter()
            .find(|leaf| leaf.logical_id == logical_id)
            .unwrap();
        leaf.source_with_schema(if state.schema_v2 {
            SYNTHETIC_SCHEMA_V2
        } else {
            SYNTHETIC_SCHEMA_V1
        })
    }

    fn reset_scan_counts(&self) {
        let mut state = self.state.lock().unwrap();
        assert_eq!(state.active_scans, 0);
        state.scan_counts.clear();
        state.peak_scans = 0;
    }

    fn scan_count(&self, physical_id: u8) -> usize {
        self.state
            .lock()
            .unwrap()
            .scan_counts
            .get(&physical_id)
            .copied()
            .unwrap_or_default()
    }

    fn replace(&self, physical_id: u8, revision: u8, body: &str) {
        let mut state = self.state.lock().unwrap();
        let leaf = state
            .leaves
            .iter_mut()
            .find(|leaf| leaf.physical_id == physical_id)
            .unwrap();
        leaf.revision = revision;
        leaf.body = body.to_owned();
    }

    fn use_logical_snapshot_scans(&self) {
        self.state.lock().unwrap().durable_replay = false;
    }

    fn use_schema_v2(&self) {
        self.state.lock().unwrap().schema_v2 = true;
    }

    fn bind_durable_replay_descriptor(&self) {
        self.state.lock().unwrap().bind_durable_replay_descriptor = true;
    }

    fn touch_physical_revision(&self, physical_id: u8, revision: u8) {
        let mut state = self.state.lock().unwrap();
        let leaf = state
            .leaves
            .iter_mut()
            .find(|leaf| leaf.physical_id == physical_id)
            .unwrap();
        leaf.revision = revision;
    }

    fn use_independent_leaf_scans(&self, requested_workers: usize) -> usize {
        let mut state = self.state.lock().unwrap();
        let worker_count = effective_test_leaf_worker_count(requested_workers, state.leaves.len());
        state.leaf_execution_policy =
            DocumentLeafExecutionPolicy::IndependentCapped(requested_workers);
        state.scan_barrier = Some(Arc::new(Barrier::new(worker_count)));
        worker_count
    }

    fn peak_scans(&self) -> usize {
        self.state.lock().unwrap().peak_scans
    }

    fn clear_scan_barrier(&self) {
        self.state.lock().unwrap().scan_barrier = None;
    }

    fn total_scans(&self) -> usize {
        self.state.lock().unwrap().scan_counts.values().sum()
    }
}

struct SyntheticScanActivity {
    state: Arc<Mutex<SyntheticState>>,
}

impl Drop for SyntheticScanActivity {
    fn drop(&mut self) {
        let mut state = self.state.lock().unwrap();
        state.active_scans = state.active_scans.saturating_sub(1);
    }
}

impl ReplacementDocumentTree for SyntheticAdapter {
    type Leaf = SyntheticLeaf;
    type TreeAuthority = [u8; 32];

    fn parser_revision(&self) -> &'static str {
        if self.state.lock().unwrap().parser_v2 {
            "synthetic-document-parser-v2"
        } else {
            "synthetic-document-parser-v1"
        }
    }

    fn owns_source(&self, source: &SourceKey) -> bool {
        source.provider() == CaptureProvider::Auggie.as_str()
            && source.source_format() == AUGGIE_SESSION_JSON_SOURCE_FORMAT
            && matches!(
                source.schema_variant(),
                SYNTHETIC_SCHEMA_V1 | SYNTHETIC_SCHEMA_V2
            )
    }

    fn leaf_execution_policy(&self) -> DocumentLeafExecutionPolicy {
        self.state.lock().unwrap().leaf_execution_policy
    }

    fn independent_leaf_source(
        &self,
        _authority: &Self::TreeAuthority,
        leaf: &Self::Leaf,
    ) -> SourceBackedRouteResult<SourceKey> {
        let schema_v2 = self.state.lock().unwrap().schema_v2;
        Ok(leaf.source_with_schema(if schema_v2 {
            SYNTHETIC_SCHEMA_V2
        } else {
            SYNTHETIC_SCHEMA_V1
        }))
    }

    fn durable_replay_source(
        &self,
        authority: &Self::TreeAuthority,
        leaf: &Self::Leaf,
    ) -> SourceBackedRouteResult<Option<SourceKey>> {
        let bind_descriptor = self.state.lock().unwrap().bind_durable_replay_descriptor;
        if bind_descriptor {
            self.independent_leaf_source(authority, leaf).map(Some)
        } else {
            Ok(None)
        }
    }

    fn discover_complete(
        &self,
    ) -> SourceBackedRouteResult<CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>> {
        let mut state = self.state.lock().unwrap();
        state.discovery_calls += 1;
        if !state.available {
            return Err(SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::Unavailable,
                "synthetic document tree unavailable",
            ));
        }
        Ok(Self::tree(&state))
    }

    fn scan_changed(
        &self,
        _authority: &Self::TreeAuthority,
        leaf: &Self::Leaf,
        sink: &mut ChangedDocumentSink<'_, '_>,
    ) -> SourceBackedRouteResult<DocumentSourceTerminal> {
        let barrier = {
            let mut state = self.state.lock().unwrap();
            state.active_scans += 1;
            state.peak_scans = state.peak_scans.max(state.active_scans);
            state.scan_barrier.clone()
        };
        let _activity = SyntheticScanActivity {
            state: Arc::clone(&self.state),
        };
        if let Some(barrier) = barrier {
            barrier.wait();
        }
        let mut state = self.state.lock().unwrap();
        if state.mutate_before_scan == Some(leaf.physical_id) {
            let live = state
                .leaves
                .iter_mut()
                .find(|candidate| candidate.physical_id == leaf.physical_id)
                .unwrap();
            live.revision = live.revision.saturating_add(1);
            state.mutate_before_scan = None;
        }
        let live = state
            .leaves
            .iter()
            .find(|candidate| candidate.physical_id == leaf.physical_id)
            .cloned()
            .ok_or_else(|| document_changed("synthetic leaf disappeared before scan"))?;
        if live != *leaf {
            return Err(document_changed(
                "synthetic leaf changed between observation and scan",
            ));
        }
        *state.scan_counts.entry(leaf.physical_id).or_default() += 1;
        let durable_replay = state.durable_replay;
        let schema_v2 = state.schema_v2;
        drop(state);

        let source = leaf.source_with_schema(if schema_v2 {
            SYNTHETIC_SCHEMA_V2
        } else {
            SYNTHETIC_SCHEMA_V1
        });
        let record = synthetic_core_record(leaf, source.clone());
        sink.begin_source(source.clone())?;
        sink.emit_core_record(record)?;
        let revision = if durable_replay {
            vec![leaf.physical_id, leaf.revision]
        } else {
            leaf.content_digest().to_vec()
        };
        let observation = SourceObservation::new(
            source.clone(),
            "synthetic-document-observation-v1",
            revision,
        )
        .map_err(document_contract_error)?;
        Ok(DocumentSourceTerminal {
            source,
            opening: observation.clone(),
            closing: observation,
            parser_revision: self.parser_revision(),
            content_digest: leaf.content_digest(),
            counts: ScannedSourceCounts {
                complete_records: 1,
                retained_records: 1,
                rejected_records: 0,
                ignored_records: 0,
                indexed_documents: 1,
                certified_bytes: leaf.body.len() as u64,
            },
        })
    }

    fn revalidate_complete(
        &self,
        tree: &CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>,
    ) -> SourceBackedRouteResult<[u8; 32]> {
        let mut state = self.state.lock().unwrap();
        if state.mutate_on_revalidate {
            state.leaves[0].revision = state.leaves[0].revision.saturating_add(1);
            state.mutate_on_revalidate = false;
        }
        Ok(Self::tree(&state).tree_fingerprint).and_then(|current| {
            if current == tree.authority {
                Ok(current)
            } else {
                Err(document_changed(
                    "synthetic tree changed before terminal revalidation",
                ))
            }
        })
    }
}

fn synthetic_tree_fingerprint(leaves: &[ObservedDocumentLeaf<SyntheticLeaf>]) -> [u8; 32] {
    let mut fingerprints = leaves
        .iter()
        .map(|leaf| leaf.fingerprint)
        .collect::<Vec<_>>();
    fingerprints.sort_unstable();
    let mut digest = Sha256::new();
    digest.update(b"ctx.synthetic-document-tree-v1\0");
    for fingerprint in fingerprints {
        digest.update(fingerprint.as_bytes());
    }
    digest.finalize().into()
}

fn synthetic_core_record(leaf: &SyntheticLeaf, source: SourceKey) -> CoreRecord {
    let native_session_key =
        NativeSessionKey::native_id("synthetic.session", TypedKey::U64(leaf.logical_id as u64))
            .unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source: &source,
        logical_session_kind: "synthetic-session",
        native_session_key: &native_session_key,
    })
    .unwrap();
    let native_item_key = NativeItemKey::native_id("synthetic.message", TypedKey::U64(1)).unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source: &source,
        session_id,
        logical_item_kind: "synthetic-message",
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .unwrap();
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        session_id,
        source,
        1,
        "message",
        "primary",
        true,
        "synthetic-document-parser-v1",
        leaf.body.clone(),
    )
    .unwrap();
    record.provider_session_id = Some(format!("synthetic-{}", leaf.logical_id));
    record.native_event_id = Some(TypedKey::U64(1));
    record.occurred_at_unix_ms = Some(1);
    record.role = Some("user".to_owned());
    record
}

fn fixture_registry(root: &Path, adapter: SyntheticAdapter) -> SourceBackedProviderRegistry {
    let source = fixture_source(root);
    let mut registry = SourceBackedProviderRegistry::new();
    register_replacement_document_tree_route(
        &mut registry,
        source,
        SourceBackedRouteSelection::Automatic,
        adapter,
    )
    .unwrap();
    registry
}

fn fixture_source(root: &Path) -> ProviderSource {
    ProviderSource {
        provider: CaptureProvider::Auggie,
        path: root.to_path_buf(),
        exists: true,
        source_format: AUGGIE_SESSION_JSON_SOURCE_FORMAT,
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        status: ProviderSourceStatus::Available,
        unsupported_reason: None,
    }
}

fn writer_options() -> WriterOptions {
    WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    }
}

fn effective_test_leaf_worker_count(requested_workers: usize, leaf_count: usize) -> usize {
    requested_workers
        .min(leaf_count)
        .min(source_backed_leaf_worker_budget(
            writer_options().indexer_threads,
        ))
}

fn publish(
    root: &Path,
    registry: &SourceBackedProviderRegistry,
) -> crate::provider::source_backed::SourceBackedRefreshReceipt {
    refresh_source_backed_generation(root, registry, writer_options()).unwrap()
}

fn assert_document_source_failure(
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
    assert_eq!(failure.provider, CaptureProvider::Auggie);
    assert_eq!(failure.class, class);
    assert!(failure.carried_forward);
    assert_eq!(failure.source_selector, selector.display().to_string());
    assert!(!failure.detail.is_empty());
    assert_eq!(failure.source_identity.len(), 64);
}

fn assert_cold_document_source_failure(error: SourceBackedCoordinatorError, selector: &Path) {
    let SourceBackedCoordinatorError::NoUsableSourceRoutes { failures } = error else {
        panic!("expected a cold source-scoped failure, got {error:?}");
    };
    assert_eq!(failures.total(), 1);
    assert_eq!(failures.omitted(), 0);
    let failure = &failures.failures()[0];
    assert_eq!(failure.provider, CaptureProvider::Auggie);
    assert_eq!(failure.class, SourceBackedSourceFailureClass::SourceChanged);
    assert!(!failure.carried_forward);
    assert_eq!(failure.source_selector, selector.display().to_string());
    assert!(!failure.detail.is_empty());
    assert_eq!(failure.source_identity.len(), 64);
}

fn publish_with_reopened_route(
    index_root: &Path,
    selected_root: &Path,
    adapter: SyntheticAdapter,
) -> crate::provider::source_backed::SourceBackedRefreshReceipt {
    let registry = fixture_registry(selected_root, adapter);
    publish(index_root, &registry)
}

fn membership_source(logical_id: u64, schema_variant: &str) -> SourceKey {
    let mut lineage = [0; 32];
    lineage[..size_of::<u64>()].copy_from_slice(&logical_id.to_be_bytes());
    SourceKey::derive(
        CaptureProvider::Auggie.as_str(),
        AUGGIE_SESSION_JSON_SOURCE_FORMAT,
        schema_variant,
        1,
        SourceAnchor::CatalogLineage(lineage),
    )
    .unwrap()
}

#[test]
fn document_membership_indexes_are_linear_exact_and_source_ordered() {
    const BASE_SOURCE_COUNT: usize = 1_000;
    const SCHEMA: &str = "synthetic-document-membership-v1";

    let sources = (0..BASE_SOURCE_COUNT)
        .filter(|logical_id| logical_id % 2 == 0)
        .map(|logical_id| membership_source(logical_id as u64, SCHEMA))
        .collect::<Vec<_>>();
    let mut current = CurrentDocumentSources::with_capacity(sources.len());
    for source in &sources {
        assert!(!current.contains_canonical(source));
        assert!(current.insert(source.clone()));
    }

    assert!(current
        .ordered_inventory_sources()
        .iter()
        .zip(&sources)
        .all(|(actual, expected)| actual.exact_descriptor_eq(expected)));
    assert_eq!(
        current.operations(),
        DocumentMembershipOperations {
            source_insertions: sources.len(),
            canonical_lookups: sources.len(),
            exact_lookups: 0,
            exact_comparisons: 0,
        }
    );

    current.reset_operations();
    let retained = (0..BASE_SOURCE_COUNT)
        .map(|logical_id| membership_source(logical_id as u64, SCHEMA))
        .filter(|source| current.contains_exact(source))
        .count();
    assert_eq!(retained, sources.len());

    let changed_descriptor = membership_source(0, "synthetic-document-membership-schema-change");
    assert!(current.contains_canonical(&changed_descriptor));
    assert!(!current.contains_exact(&changed_descriptor));
    assert_eq!(
        current.operations(),
        DocumentMembershipOperations {
            source_insertions: 0,
            canonical_lookups: 1,
            exact_lookups: BASE_SOURCE_COUNT + 1,
            exact_comparisons: sources.len(),
        }
    );
}

#[test]
fn independent_leaf_runner_has_one_vs_four_parity_and_bounded_peak_work() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let leaves = (0_u8..8)
        .map(|id| SyntheticLeaf {
            physical_id: id,
            logical_id: id,
            revision: 1,
            body: format!("independent leaf {id}"),
        })
        .collect::<Vec<_>>();
    let serial_runner = SyntheticAdapter::new(leaves.clone());
    serial_runner.use_independent_leaf_scans(1);
    let parallel_runner = SyntheticAdapter::new(leaves);
    let parallel_worker_count = parallel_runner.use_independent_leaf_scans(4);
    let serial_registry = fixture_registry(temp.path(), serial_runner.clone());
    let parallel_registry = fixture_registry(temp.path(), parallel_runner.clone());
    let serial_root = temp.path().join("serial-runner");
    let parallel_root = temp.path().join("parallel-runner");

    let serial_cold = publish(&serial_root, &serial_registry);
    let parallel_cold = publish(&parallel_root, &parallel_registry);
    assert_eq!(
        parallel_cold.commit.generation_id,
        serial_cold.commit.generation_id
    );
    assert_eq!(parallel_cold.sources, serial_cold.sources);
    assert_eq!(serial_runner.peak_scans(), 1);
    assert_eq!(parallel_runner.peak_scans(), parallel_worker_count);
    assert_eq!(serial_runner.total_scans(), 8);
    assert_eq!(parallel_runner.total_scans(), 8);

    serial_runner.reset_scan_counts();
    parallel_runner.reset_scan_counts();
    let serial_noop = publish(&serial_root, &serial_registry);
    let parallel_noop = publish(&parallel_root, &parallel_registry);
    assert_eq!(
        serial_noop.commit.generation_id,
        serial_cold.commit.generation_id
    );
    assert_eq!(
        parallel_noop.commit.generation_id,
        parallel_cold.commit.generation_id
    );
    assert_eq!(parallel_noop.sources, serial_noop.sources);
    assert_eq!(serial_runner.total_scans(), 0);
    assert_eq!(parallel_runner.total_scans(), 0);

    serial_runner.clear_scan_barrier();
    parallel_runner.clear_scan_barrier();
    serial_runner.replace(3, 2, "independent leaf 3 changed");
    parallel_runner.replace(3, 2, "independent leaf 3 changed");
    let serial_changed = publish(&serial_root, &serial_registry);
    let parallel_changed = publish(&parallel_root, &parallel_registry);
    assert_eq!(
        parallel_changed.commit.generation_id,
        serial_changed.commit.generation_id
    );
    assert_eq!(parallel_changed.sources, serial_changed.sources);
    assert_eq!(serial_runner.scan_count(3), 1);
    assert_eq!(parallel_runner.scan_count(3), 1);

    let deleted_source = serial_runner.source(0);
    serial_runner
        .state
        .lock()
        .unwrap()
        .leaves
        .retain(|leaf| leaf.logical_id != 0);
    parallel_runner
        .state
        .lock()
        .unwrap()
        .leaves
        .retain(|leaf| leaf.logical_id != 0);
    serial_runner.reset_scan_counts();
    parallel_runner.reset_scan_counts();
    for expected_missing in 1..AUTOMATIC_SOURCE_DELETION_MISSING_INVENTORIES {
        let serial_grace = publish(&serial_root, &serial_registry);
        let parallel_grace = publish(&parallel_root, &parallel_registry);
        assert!(serial_grace.removals.is_empty());
        assert!(parallel_grace.removals.is_empty());
        assert_eq!(serial_grace.sources.len(), 8);
        assert_eq!(parallel_grace.sources.len(), 8);
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
    serial_runner.reset_scan_counts();
    parallel_runner.reset_scan_counts();
    let serial_deleted = publish(&serial_root, &serial_registry);
    let parallel_deleted = publish(&parallel_root, &parallel_registry);
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
    assert_eq!(serial_runner.total_scans(), 0);
    assert_eq!(parallel_runner.total_scans(), 0);

    let retained_generation = parallel_deleted.commit.generation_id.clone();
    parallel_runner.replace(1, 2, "terminal tree race");
    parallel_runner.state.lock().unwrap().mutate_on_revalidate = true;
    let failed =
        refresh_source_backed_generation(&parallel_root, &parallel_registry, writer_options())
            .unwrap();
    assert_document_source_failure(
        &failed,
        SourceBackedSourceFailureClass::SourceChanged,
        temp.path(),
    );
    assert_eq!(failed.commit.generation_id, retained_generation);
    assert_eq!(failed.sources, parallel_deleted.sources);
    assert_eq!(
        VerifiedIndex::open(&parallel_root).unwrap().generation_id(),
        retained_generation
    );
    assert_eq!(
        VerifiedIndex::open(&parallel_root)
            .unwrap()
            .manifest()
            .sources,
        parallel_deleted.sources
    );

    let retried = publish(&parallel_root, &parallel_registry);
    assert_eq!(retried.outcome, SourceBackedRefreshOutcome::Completed);
    assert_eq!(retried.successful_routes, 1);
    assert!(retried.source_failures.is_empty());
    assert_ne!(retried.commit.generation_id, retained_generation);
}

#[test]
fn durable_replay_rederives_exact_descriptor_before_reusing_unchanged_fingerprint() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let index_root = temp.path().join("index");
    let leaf = SyntheticLeaf {
        physical_id: 1,
        logical_id: 1,
        revision: 1,
        body: "unchanged descriptor migration document".to_owned(),
    };
    let fingerprint = leaf.fingerprint();
    let adapter = SyntheticAdapter::new(vec![leaf]);
    adapter.bind_durable_replay_descriptor();
    let registry = fixture_registry(temp.path(), adapter.clone());
    let original_tree = SyntheticAdapter::tree(&adapter.state.lock().unwrap());
    assert_eq!(original_tree.leaves[0].fingerprint, fingerprint);

    let descriptor_a = adapter.source(1);
    let bound_descriptor_a = adapter
        .durable_replay_source(
            &original_tree.authority,
            &original_tree.leaves[0].provider_leaf,
        )
        .unwrap()
        .unwrap();
    assert!(bound_descriptor_a.exact_descriptor_eq(&descriptor_a));
    let cold = publish(&index_root, &registry);
    assert_eq!(adapter.scan_count(1), 1);

    adapter.reset_scan_counts();
    adapter.use_schema_v2();
    let descriptor_b = adapter.source(1);
    let current_tree = SyntheticAdapter::tree(&adapter.state.lock().unwrap());
    assert_eq!(current_tree.leaves[0].fingerprint, fingerprint);
    assert!(adapter.state.lock().unwrap().durable_replay);
    assert_eq!(
        current_tree.tree_fingerprint,
        original_tree.tree_fingerprint
    );
    assert_eq!(descriptor_a.identity(), descriptor_b.identity());
    assert!(!descriptor_a.exact_descriptor_eq(&descriptor_b));
    assert_eq!(cold.sources[0].parser_revision(), adapter.parser_revision());

    let replaced = publish(&index_root, &registry);
    assert_eq!(adapter.scan_count(1), 1);
    assert_ne!(replaced.commit.generation_id, cold.commit.generation_id);
    assert_eq!(replaced.sources.len(), 1);
    assert!(replaced.sources[0]
        .observation()
        .source()
        .exact_descriptor_eq(&descriptor_b));
    assert!(replaced.removals.is_empty());

    let index = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(index.document_count(), 1);
    assert!(matches!(
        index.source_event_page(&descriptor_a, None, 8),
        Err(ctx_history_index::IndexError::SourceEventSourceDescriptorMismatch(_))
    ));
    let event = index
        .source_event_page(&descriptor_b, None, 8)
        .unwrap()
        .items
        .remove(0);
    let record = index
        .core_record_by_id(event.event_id.as_uuid())
        .unwrap()
        .unwrap();
    assert!(record.source.exact_descriptor_eq(&descriptor_b));
    assert_eq!(
        record.content.normalized_body.as_deref(),
        Some("unchanged descriptor migration document")
    );
}

#[test]
fn same_lineage_schema_descriptor_replacement_is_atomic_and_removes_stale_documents() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let index_root = temp.path().join("index");
    let adapter = SyntheticAdapter::new(vec![SyntheticLeaf {
        physical_id: 1,
        logical_id: 1,
        revision: 1,
        body: "stale descriptor A document".to_owned(),
    }]);
    adapter.use_logical_snapshot_scans();
    let registry = fixture_registry(temp.path(), adapter.clone());

    let descriptor_a = adapter.source(1);
    let cold = publish(&index_root, &registry);
    assert_eq!(cold.sources.len(), 1);
    assert!(cold.sources[0]
        .observation()
        .source()
        .exact_descriptor_eq(&descriptor_a));

    adapter.replace(1, 2, "replacement descriptor B document");
    adapter.use_schema_v2();
    let descriptor_b = adapter.source(1);
    assert_eq!(descriptor_a.identity(), descriptor_b.identity());
    assert!(!descriptor_a.exact_descriptor_eq(&descriptor_b));
    assert_eq!(descriptor_b.schema_variant(), SYNTHETIC_SCHEMA_V2);

    let replaced = publish(&index_root, &registry);
    assert_eq!(replaced.sources.len(), 1);
    assert!(replaced.sources[0]
        .observation()
        .source()
        .exact_descriptor_eq(&descriptor_b));
    assert!(replaced.removals.is_empty());

    let index = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(index.document_count(), 1);
    assert!(matches!(
        index.source_event_page(&descriptor_a, None, 8),
        Err(ctx_history_index::IndexError::SourceEventSourceDescriptorMismatch(_))
    ));
    let event = index
        .source_event_page(&descriptor_b, None, 8)
        .unwrap()
        .items
        .remove(0);
    let record = index
        .core_record_by_id(event.event_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(
        record.content.normalized_body.as_deref(),
        Some("replacement descriptor B document")
    );
}

#[test]
fn active_source_family_contract_document_replacement_lifecycle_is_exact() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let index_root = temp.path().join("index");
    let adapter = SyntheticAdapter::new(vec![
        SyntheticLeaf {
            physical_id: 1,
            logical_id: 1,
            revision: 1,
            body: "alpha".to_owned(),
        },
        SyntheticLeaf {
            physical_id: 2,
            logical_id: 2,
            revision: 1,
            body: "bravo".to_owned(),
        },
    ]);
    let unbound_tree = SyntheticAdapter::tree(&adapter.state.lock().unwrap());
    assert_eq!(
        adapter.leaf_execution_policy(),
        DocumentLeafExecutionPolicy::Serial
    );
    assert!(adapter
        .durable_replay_source(
            &unbound_tree.authority,
            &unbound_tree.leaves[0].provider_leaf,
        )
        .unwrap()
        .is_none());
    let registry = fixture_registry(temp.path(), adapter.clone());

    let cold = publish(&index_root, &registry);
    assert_eq!(cold.sources.len(), 2);
    assert_eq!(adapter.scan_count(1), 1);
    assert_eq!(adapter.scan_count(2), 1);
    assert_eq!(adapter.peak_scans(), 1);

    adapter.reset_scan_counts();
    let unchanged = publish(&index_root, &registry);
    assert_eq!(unchanged.commit.generation_id, cold.commit.generation_id);
    assert_eq!(adapter.scan_count(1), 0);
    assert_eq!(adapter.scan_count(2), 0);

    for (revision, body) in [(2, "bravo grows"), (3, "charlie!!!!"), (4, "cut")] {
        adapter.replace(2, revision, body);
        adapter.reset_scan_counts();
        let changed = publish(&index_root, &registry);
        assert_eq!(changed.sources.len(), 2);
        assert_eq!(adapter.scan_count(1), 0);
        assert_eq!(adapter.scan_count(2), 1);
        let source = adapter.source(2);
        let items = VerifiedIndex::open(&index_root)
            .unwrap()
            .source_event_page(&source, None, 8)
            .unwrap()
            .items;
        assert_eq!(items.len(), 1);
        let record = VerifiedIndex::open(&index_root)
            .unwrap()
            .core_record_by_id(items[0].event_id.as_uuid())
            .unwrap()
            .unwrap();
        assert_eq!(record.content.normalized_body.as_deref(), Some(body));
    }

    adapter.state.lock().unwrap().leaves.push(SyntheticLeaf {
        physical_id: 3,
        logical_id: 3,
        revision: 1,
        body: "new leaf".to_owned(),
    });
    adapter.reset_scan_counts();
    let added = publish(&index_root, &registry);
    assert_eq!(added.sources.len(), 3);
    assert_eq!(adapter.scan_count(1), 0);
    assert_eq!(adapter.scan_count(2), 0);
    assert_eq!(adapter.scan_count(3), 1);

    let deleted_source = adapter.source(1);
    adapter
        .state
        .lock()
        .unwrap()
        .leaves
        .retain(|leaf| leaf.logical_id != 1);
    adapter.reset_scan_counts();
    for expected_missing in 1..AUTOMATIC_SOURCE_DELETION_MISSING_INVENTORIES {
        let grace = publish(&index_root, &registry);
        assert_eq!(grace.sources.len(), 3);
        assert!(grace.removals.is_empty());
        assert_eq!(
            grace
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
            VerifiedIndex::open(&index_root)
                .unwrap()
                .source_event_page(&deleted_source, None, 8)
                .unwrap()
                .items
                .len(),
            1
        );
    }
    adapter.reset_scan_counts();
    let deleted = publish(&index_root, &registry);
    assert_eq!(deleted.sources.len(), 2);
    assert!(deleted.removals.iter().any(|removal| removal
        .deletion
        .source()
        .exact_descriptor_eq(&deleted_source)));
    assert_eq!(adapter.scan_count(2), 0);
    assert_eq!(adapter.scan_count(3), 0);
    assert!(matches!(
        VerifiedIndex::open(&index_root)
            .unwrap()
            .source_event_page(&deleted_source, None, 8),
        Err(ctx_history_index::IndexError::SourceEventSourceNotRetained(
            _
        ))
    ));

    let retained_generation = deleted.commit.generation_id.clone();
    adapter.state.lock().unwrap().available = false;
    let failed =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_document_source_failure(
        &failed,
        SourceBackedSourceFailureClass::Unavailable,
        temp.path(),
    );
    assert_eq!(failed.commit.generation_id, retained_generation);
    assert_eq!(failed.sources, deleted.sources);
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().generation_id(),
        retained_generation
    );
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().manifest().sources,
        deleted.sources
    );

    adapter.state.lock().unwrap().available = true;
    let retried = publish(&index_root, &registry);
    assert_eq!(retried.outcome, SourceBackedRefreshOutcome::Completed);
    assert_eq!(retried.successful_routes, 1);
    assert!(retried.source_failures.is_empty());
    assert_eq!(retried.commit.generation_id, retained_generation);
    assert_eq!(retried.sources, deleted.sources);
}

#[test]
fn automatic_document_missing_grace_survives_route_reopen_and_reappearance() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let index_root = temp.path().join("index");
    let selected_root = temp.path().join("selected");
    let leaf = SyntheticLeaf {
        physical_id: 1,
        logical_id: 1,
        revision: 1,
        body: "durable last-good leaf".to_owned(),
    };
    let source = leaf.source();
    let adapter = SyntheticAdapter::new(vec![leaf.clone()]);

    let cold = publish_with_reopened_route(&index_root, &selected_root, adapter.clone());
    let noop = publish_with_reopened_route(&index_root, &selected_root, adapter.clone());
    assert_eq!(noop.commit.generation_id, cold.commit.generation_id);
    assert!(noop.commit.manifest().source_catalog().is_empty());

    adapter.state.lock().unwrap().leaves.clear();
    for expected_missing in 1..AUTOMATIC_SOURCE_DELETION_MISSING_INVENTORIES {
        let grace = publish_with_reopened_route(&index_root, &selected_root, adapter.clone());
        assert_eq!(grace.sources.len(), 1);
        assert!(grace.removals.is_empty());
        assert_eq!(
            grace
                .commit
                .manifest()
                .source_catalog()
                .missing_source(&source)
                .unwrap()
                .consecutive_missing()
                .get(),
            expected_missing
        );
        assert_eq!(
            VerifiedIndex::open(&index_root).unwrap().document_count(),
            1
        );
    }

    adapter.state.lock().unwrap().leaves.push(leaf.clone());
    let reappeared = publish_with_reopened_route(&index_root, &selected_root, adapter.clone());
    assert_eq!(reappeared.sources.len(), 1);
    assert!(reappeared.removals.is_empty());
    assert!(reappeared.commit.manifest().source_catalog().is_empty());
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().document_count(),
        1
    );

    adapter.state.lock().unwrap().leaves.clear();
    for _ in 1..AUTOMATIC_SOURCE_DELETION_MISSING_INVENTORIES {
        let grace = publish_with_reopened_route(&index_root, &selected_root, adapter.clone());
        assert!(grace.removals.is_empty());
        assert_eq!(grace.sources.len(), 1);
    }
    let deleted = publish_with_reopened_route(&index_root, &selected_root, adapter);
    assert!(deleted.commit.manifest().source_catalog().is_empty());
    assert_eq!(deleted.sources.len(), 0);
    assert_eq!(deleted.removals.len(), 1);
    assert!(deleted.removals[0]
        .deletion
        .source()
        .exact_descriptor_eq(&source));
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().document_count(),
        0
    );
}

#[test]
fn logical_snapshot_leaf_scans_once_and_discards_identical_staging() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let index_root = temp.path().join("index");
    let adapter = SyntheticAdapter::new(vec![SyntheticLeaf {
        physical_id: 1,
        logical_id: 1,
        revision: 1,
        body: "logical rows".to_owned(),
    }]);
    adapter.use_logical_snapshot_scans();
    let registry = fixture_registry(temp.path(), adapter.clone());

    let cold = publish(&index_root, &registry);
    assert_eq!(adapter.scan_count(1), 1);

    adapter.reset_scan_counts();
    adapter.touch_physical_revision(1, 2);
    let physical_only = publish(&index_root, &registry);
    assert_eq!(adapter.scan_count(1), 1);
    assert_eq!(
        physical_only.commit.generation_id,
        cold.commit.generation_id
    );
    assert_eq!(physical_only.commit.opstamp, cold.commit.opstamp);

    adapter.reset_scan_counts();
    adapter.replace(1, 3, "changed logical rows");
    let logical_change = publish(&index_root, &registry);
    assert_eq!(adapter.scan_count(1), 1);
    assert_ne!(
        logical_change.commit.generation_id,
        cold.commit.generation_id
    );
}

#[test]
fn logical_snapshot_four_worker_noop_retains_and_changed_leaf_replays() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let index_root = temp.path().join("index");
    let adapter = SyntheticAdapter::new(
        (0_u8..8)
            .map(|id| SyntheticLeaf {
                physical_id: id,
                logical_id: id,
                revision: 1,
                body: format!("logical leaf {id}"),
            })
            .collect(),
    );
    adapter.use_logical_snapshot_scans();
    let worker_count = adapter.use_independent_leaf_scans(4);
    let registry = fixture_registry(temp.path(), adapter.clone());

    let cold = publish(&index_root, &registry);
    assert_eq!(adapter.total_scans(), 8);
    assert_eq!(adapter.peak_scans(), worker_count);

    for id in 0_u8..8 {
        adapter.touch_physical_revision(id, 2);
    }
    adapter.reset_scan_counts();
    let logical_noop = publish(&index_root, &registry);
    assert_eq!(logical_noop.commit.generation_id, cold.commit.generation_id);
    assert_eq!(logical_noop.commit.opstamp, cold.commit.opstamp);
    assert_eq!(adapter.total_scans(), 8);
    assert_eq!(adapter.peak_scans(), worker_count);

    adapter.replace(3, 3, "logical leaf 3 changed");
    adapter.reset_scan_counts();
    let changed = publish(&index_root, &registry);
    assert_ne!(changed.commit.generation_id, cold.commit.generation_id);
    assert_eq!(adapter.total_scans(), 8);
    assert_eq!(adapter.peak_scans(), worker_count);

    let source = adapter.source(3);
    let item = VerifiedIndex::open(&index_root)
        .unwrap()
        .source_event_page(&source, None, 8)
        .unwrap()
        .items
        .remove(0);
    let record = VerifiedIndex::open(&index_root)
        .unwrap()
        .core_record_by_id(item.event_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(
        record.content.normalized_body.as_deref(),
        Some("logical leaf 3 changed")
    );
}

#[test]
fn active_source_family_contract_document_replacement_tree_rejects_races_duplicates_and_replaces_on_parser_change(
) {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let index_root = temp.path().join("index");
    let adapter = SyntheticAdapter::new(vec![SyntheticLeaf {
        physical_id: 1,
        logical_id: 1,
        revision: 1,
        body: "alpha".to_owned(),
    }]);
    let registry = fixture_registry(temp.path(), adapter.clone());
    let cold = publish(&index_root, &registry);

    adapter.reset_scan_counts();
    adapter.state.lock().unwrap().parser_v2 = true;
    let parser_changed = publish(&index_root, &registry);
    assert_eq!(adapter.scan_count(1), 1);

    adapter.replace(1, 2, "observation race");
    adapter.state.lock().unwrap().mutate_before_scan = Some(1);
    let before = parser_changed.commit.generation_id.clone();
    let observation_failure =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_document_source_failure(
        &observation_failure,
        SourceBackedSourceFailureClass::SourceChanged,
        temp.path(),
    );
    assert_eq!(observation_failure.commit.generation_id, before);
    assert_eq!(observation_failure.sources, parser_changed.sources);
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().generation_id(),
        before
    );

    adapter.state.lock().unwrap().mutate_on_revalidate = true;
    let terminal_failure =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_document_source_failure(
        &terminal_failure,
        SourceBackedSourceFailureClass::SourceChanged,
        temp.path(),
    );
    assert_eq!(terminal_failure.commit.generation_id, before);
    assert_eq!(terminal_failure.sources, parser_changed.sources);
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().generation_id(),
        before
    );

    let retried = publish(&index_root, &registry);
    assert_eq!(retried.outcome, SourceBackedRefreshOutcome::Completed);
    assert_eq!(retried.successful_routes, 1);
    assert!(retried.source_failures.is_empty());
    assert_ne!(retried.commit.generation_id, before);

    let duplicate_physical = SyntheticAdapter::new(vec![
        SyntheticLeaf {
            physical_id: 8,
            logical_id: 8,
            revision: 1,
            body: "duplicate".to_owned(),
        },
        SyntheticLeaf {
            physical_id: 8,
            logical_id: 9,
            revision: 1,
            body: "duplicate".to_owned(),
        },
    ]);
    let duplicate_registry = fixture_registry(temp.path(), duplicate_physical.clone());
    let duplicate_physical_error = refresh_source_backed_generation(
        temp.path().join("duplicate-physical"),
        &duplicate_registry,
        writer_options(),
    )
    .unwrap_err();
    assert_cold_document_source_failure(duplicate_physical_error, temp.path());
    assert_eq!(duplicate_physical.scan_count(8), 0);

    let duplicate_source = SyntheticAdapter::new(vec![
        SyntheticLeaf {
            physical_id: 10,
            logical_id: 10,
            revision: 1,
            body: "first".to_owned(),
        },
        SyntheticLeaf {
            physical_id: 11,
            logical_id: 10,
            revision: 1,
            body: "second".to_owned(),
        },
    ]);
    let duplicate_registry = fixture_registry(temp.path(), duplicate_source);
    let duplicate_source_error = refresh_source_backed_generation(
        temp.path().join("duplicate-source"),
        &duplicate_registry,
        writer_options(),
    )
    .unwrap_err();
    match duplicate_source_error {
        SourceBackedCoordinatorError::RouteScan { provider, source } => {
            assert_eq!(provider, CaptureProvider::Auggie);
            assert_eq!(source.kind, SourceBackedRouteErrorKind::Internal);
            assert!(source
                .detail
                .contains("source replacement has already started"));
        }
        error => panic!("expected a duplicate-source writer failure, got {error:?}"),
    }
    assert_eq!(cold.sources.len(), 1);
}

#[test]
fn terminal_tree_witness_rejects_mutation_and_reappearance_between_callbacks() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let retained = SyntheticLeaf {
        physical_id: 1,
        logical_id: 1,
        revision: 1,
        body: "retained".to_owned(),
    };
    let adapter = SyntheticAdapter::new(vec![retained.clone()]);
    adapter.use_logical_snapshot_scans();
    let tree = SyntheticAdapter::tree(&adapter.state.lock().unwrap());
    let source = retained.source();
    let observation = SourceObservation::new(
        source.clone(),
        "synthetic-document-observation-v1",
        retained.content_digest().to_vec(),
    )
    .unwrap();
    let certificate = CertifiedSource::certify(
        observation.clone(),
        observation,
        adapter.parser_revision(),
        retained.content_digest(),
        ScannedSourceCounts {
            complete_records: 1,
            retained_records: 1,
            rejected_records: 0,
            ignored_records: 0,
            indexed_documents: 1,
            certified_bytes: retained.body.len() as u64,
        },
    )
    .unwrap();
    let inventory = DocumentInventoryAuthority::new(&fixture_source(temp.path()))
        .certify(tree.tree_fingerprint, vec![source.clone()])
        .unwrap();
    let state = Mutex::new(DocumentCommitState {
        expected: Some(ExpectedDocumentRoute::new(
            tree,
            vec![certificate.clone()],
            inventory.clone(),
        )),
    });

    assert!(revalidate_document_target(
        &state,
        SourceBackedRevalidationTarget::Source(&certificate),
    ));
    adapter.replace(1, 2, "mutated between source and inventory callbacks");
    assert!(!revalidate_document_inventory(&adapter, &state, &inventory,));

    let tree = SyntheticAdapter::tree(&adapter.state.lock().unwrap());
    let retained = adapter.state.lock().unwrap().leaves[0].clone();
    let source = retained.source();
    let observation = SourceObservation::new(
        source.clone(),
        "synthetic-document-observation-v1",
        retained.content_digest().to_vec(),
    )
    .unwrap();
    let certificate = CertifiedSource::certify(
        observation.clone(),
        observation,
        adapter.parser_revision(),
        retained.content_digest(),
        ScannedSourceCounts {
            complete_records: 1,
            retained_records: 1,
            rejected_records: 0,
            ignored_records: 0,
            indexed_documents: 1,
            certified_bytes: retained.body.len() as u64,
        },
    )
    .unwrap();
    let inventory = DocumentInventoryAuthority::new(&fixture_source(temp.path()))
        .certify(tree.tree_fingerprint, vec![source])
        .unwrap();
    let deleted_leaf = SyntheticLeaf {
        physical_id: 2,
        logical_id: 2,
        revision: 1,
        body: "deleted".to_owned(),
    };
    let deletion =
        CertifiedSourceDeletion::from_inventory(deleted_leaf.source(), &inventory).unwrap();
    let state = Mutex::new(DocumentCommitState {
        expected: Some(ExpectedDocumentRoute::new(
            tree,
            vec![certificate],
            inventory.clone(),
        )),
    });
    assert!(revalidate_document_target(
        &state,
        SourceBackedRevalidationTarget::Deletion(&deletion),
    ));
    adapter.state.lock().unwrap().leaves.push(deleted_leaf);
    assert!(!revalidate_document_inventory(&adapter, &state, &inventory,));
}
