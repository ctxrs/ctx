use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Barrier, Mutex},
};

use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, ContentSourceResolver,
    EventHydrationRequest, EventIdentityInput, LocatorRevisionPolicy, NativeItemKey,
    NativeRecordCoordinate, NativeSessionKey, SessionIdentityInput, SourceAnchor,
    SourceRecordLocator,
};
use ctx_history_index::{VerifiedIndex, WriterOptions};

use super::*;
use crate::{
    provider::source_backed::{
        refresh_source_backed_generation, SourceBackedCoordinatorError,
        SourceBackedProviderRegistry,
    },
    ProviderCatalogSupport, ProviderImportSupport, ProviderSourceKind, ProviderSourceStatus,
    AUGGIE_SESSION_JSON_SOURCE_FORMAT,
};

#[derive(Debug, Clone, PartialEq, Eq)]
struct SyntheticLeaf {
    physical_id: u8,
    logical_id: u8,
    revision: u8,
    body: String,
}

impl SyntheticLeaf {
    fn source(&self) -> SourceKey {
        SourceKey::derive(
            CaptureProvider::Auggie.as_str(),
            AUGGIE_SESSION_JSON_SOURCE_FORMAT,
            "synthetic-document-family-v1",
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
    parser_v2: bool,
    scan_counts: HashMap<u8, usize>,
    hydration_parses: usize,
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
        self.state
            .lock()
            .unwrap()
            .leaves
            .iter()
            .find(|leaf| leaf.logical_id == logical_id)
            .unwrap()
            .source()
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

    fn touch_physical_revision(&self, physical_id: u8, revision: u8) {
        let mut state = self.state.lock().unwrap();
        let leaf = state
            .leaves
            .iter_mut()
            .find(|leaf| leaf.physical_id == physical_id)
            .unwrap();
        leaf.revision = revision;
    }

    fn use_independent_leaf_scans(&self, barrier_participants: usize) {
        let mut state = self.state.lock().unwrap();
        state.leaf_execution_policy =
            DocumentLeafExecutionPolicy::IndependentWithWorkers(barrier_participants);
        state.scan_barrier = Some(Arc::new(Barrier::new(barrier_participants)));
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
            && source.schema_variant() == "synthetic-document-family-v1"
    }

    fn leaf_execution_policy(&self) -> DocumentLeafExecutionPolicy {
        self.state.lock().unwrap().leaf_execution_policy
    }

    fn independent_leaf_source(
        &self,
        _authority: &Self::TreeAuthority,
        leaf: &Self::Leaf,
    ) -> SourceBackedRouteResult<SourceKey> {
        Ok(leaf.source())
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
        drop(state);

        let source = leaf.source();
        let document = synthetic_document(leaf);
        sink.begin_source(source.clone())?;
        sink.emit_document(document)?;
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

    fn hydrate_group(
        &self,
        request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure> {
        let mut state = self.state.lock().unwrap();
        state.hydration_parses += 1;
        let source = request
            .events()
            .first()
            .map(|event| event.locator().source())
            .ok_or_else(|| {
                hydration_failure(
                    HydrationFailureKind::InvalidLocator,
                    "synthetic hydration group is empty",
                )
            })?;
        let leaf = state
            .leaves
            .iter()
            .find(|leaf| leaf.source().exact_descriptor_eq(source))
            .ok_or_else(|| {
                hydration_failure(
                    HydrationFailureKind::MissingRecord,
                    "synthetic source is missing",
                )
            })?;
        let digest = leaf.content_digest();
        let records = request
            .events()
            .iter()
            .map(|event| {
                if event.locator().certified_source_revision_digest() != Some(&digest)
                    || event.locator().record_digest() != &digest
                {
                    return Err(hydration_failure(
                        HydrationFailureKind::StaleRecordEvidence,
                        "synthetic locator is stale",
                    ));
                }
                Ok(HydratedProviderRecord {
                    event_id: event.event_id(),
                    provider_bytes: leaf.body.as_bytes().to_vec(),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        BatchHydrationResult::new(records)
            .map_err(|error| hydration_failure(HydrationFailureKind::InvalidLocator, error))
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

fn synthetic_document(leaf: &SyntheticLeaf) -> LexicalDocument {
    let source = leaf.source();
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
    let digest = leaf.content_digest();
    let locator = SourceRecordLocator::new(
        source.clone(),
        NativeRecordCoordinate::Document {
            object_key: TypedKey::U64(1),
            json_pointer: Some("/message".to_owned()),
        },
        LocatorRevisionPolicy::ExactSourceRevision,
        Some(digest),
        digest,
    )
    .unwrap();
    LexicalDocument {
        event_id,
        session_id,
        parent_session_id: None,
        root_session_id: session_id,
        source,
        locator,
        provider_session_id: Some(format!("synthetic-{}", leaf.logical_id)),
        branch: None,
        source_path: Some(format!("/synthetic/{}.json", leaf.physical_id)),
        agent_type: "primary".to_owned(),
        is_primary: true,
        event_sequence: 1,
        occurred_at_unix_ms: Some(1),
        event_type: "message".to_owned(),
        role: Some("user".to_owned()),
        body: leaf.body.clone(),
        workspace: None,
        cwd: None,
        touched_files: Vec::new(),
    }
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

fn publish(
    root: &Path,
    registry: &SourceBackedProviderRegistry,
) -> crate::provider::source_backed::SourceBackedRefreshReceipt {
    refresh_source_backed_generation(root, registry, writer_options()).unwrap()
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
    parallel_runner.use_independent_leaf_scans(4);
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
    assert_eq!(parallel_runner.peak_scans(), 4);
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

    let retained_source = serial_runner.source(3);
    let retained_event = VerifiedIndex::open(&serial_root)
        .unwrap()
        .source_event_page(&retained_source, None, 8)
        .unwrap()
        .items
        .remove(0);
    let retained_hydration =
        EventHydrationRequest::new(retained_event.event_id, retained_event.locator).unwrap();
    assert_eq!(
        serial_registry
            .resolver_registry()
            .hydrate_event(&retained_hydration)
            .unwrap(),
        parallel_registry
            .resolver_registry()
            .hydrate_event(&retained_hydration)
            .unwrap()
    );

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

    let retained_generation = parallel_deleted.commit.generation_id;
    parallel_runner.replace(1, 2, "terminal tree race");
    parallel_runner.state.lock().unwrap().mutate_on_revalidate = true;
    assert!(
        refresh_source_backed_generation(&parallel_root, &parallel_registry, writer_options())
            .is_err()
    );
    assert_eq!(
        VerifiedIndex::open(&parallel_root).unwrap().generation_id(),
        retained_generation
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
        let hydration =
            EventHydrationRequest::new(items[0].event_id, items[0].locator.clone()).unwrap();
        assert_eq!(
            registry
                .resolver_registry()
                .hydrate_event(&hydration)
                .unwrap()
                .provider_bytes,
            body.as_bytes()
        );
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

    let retained_generation = deleted.commit.generation_id;
    adapter.state.lock().unwrap().available = false;
    let error =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap_err();
    assert!(matches!(
        error,
        SourceBackedCoordinatorError::RouteScan {
            source: SourceBackedRouteError {
                kind: SourceBackedRouteErrorKind::Unavailable,
                ..
            },
            ..
        }
    ));
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().generation_id(),
        retained_generation
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
    publish(&index_root, &registry);
    assert_eq!(adapter.scan_count(1), 1);

    adapter.replace(1, 2, "observation race");
    adapter.state.lock().unwrap().mutate_before_scan = Some(1);
    let before = VerifiedIndex::open(&index_root)
        .unwrap()
        .generation_id()
        .to_owned();
    assert!(refresh_source_backed_generation(&index_root, &registry, writer_options()).is_err());
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().generation_id(),
        before
    );

    adapter.state.lock().unwrap().mutate_on_revalidate = true;
    assert!(refresh_source_backed_generation(&index_root, &registry, writer_options()).is_err());
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().generation_id(),
        before
    );

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
    assert!(refresh_source_backed_generation(
        temp.path().join("duplicate-physical"),
        &duplicate_registry,
        writer_options()
    )
    .is_err());
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
    assert!(refresh_source_backed_generation(
        temp.path().join("duplicate-source"),
        &duplicate_registry,
        writer_options()
    )
    .is_err());
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

#[test]
fn replacement_tree_group_hydration_parses_once_preserves_order_and_fails_atomically() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let index_root = temp.path().join("index");
    let adapter = SyntheticAdapter::new(vec![
        SyntheticLeaf {
            physical_id: 1,
            logical_id: 1,
            revision: 1,
            body: "first source".to_owned(),
        },
        SyntheticLeaf {
            physical_id: 2,
            logical_id: 2,
            revision: 1,
            body: "second source".to_owned(),
        },
    ]);
    let registry = fixture_registry(temp.path(), adapter.clone());
    publish(&index_root, &registry);
    let source = adapter.source(1);
    let event = VerifiedIndex::open(&index_root)
        .unwrap()
        .source_event_page(&source, None, 8)
        .unwrap()
        .items
        .remove(0);
    let first = EventHydrationRequest::new(event.event_id, event.locator.clone()).unwrap();
    let second_document = synthetic_document(&SyntheticLeaf {
        physical_id: 1,
        logical_id: 1,
        revision: 1,
        body: "first source".to_owned(),
    });
    let second_locator = SourceRecordLocator::new(
        source,
        NativeRecordCoordinate::Document {
            object_key: TypedKey::U64(2),
            json_pointer: Some("/message".to_owned()),
        },
        LocatorRevisionPolicy::ExactSourceRevision,
        second_document
            .locator
            .certified_source_revision_digest()
            .copied(),
        *second_document.locator.record_digest(),
    )
    .unwrap();
    let second_item = NativeItemKey::native_id("synthetic.message", TypedKey::U64(2)).unwrap();
    let second_id = derive_event_id(EventIdentityInput {
        source: second_locator.source(),
        session_id: second_document.session_id,
        logical_item_kind: "synthetic-message",
        native_item_key: &second_item,
        subrecord_selector: None,
    })
    .unwrap();
    let second = EventHydrationRequest::new(second_id, second_locator).unwrap();
    let batch = BatchHydrationRequest::new(vec![second.clone(), first.clone()]).unwrap();
    let result = registry.resolver_registry().hydrate_batch(&batch).unwrap();
    assert_eq!(
        result
            .records()
            .iter()
            .map(|record| record.event_id)
            .collect::<Vec<_>>(),
        vec![second.event_id(), first.event_id()]
    );
    assert_eq!(adapter.state.lock().unwrap().hydration_parses, 1);

    adapter.replace(1, 2, "changed after indexing");
    let stale = registry
        .resolver_registry()
        .hydrate_batch(&batch)
        .unwrap_err();
    assert_eq!(stale.kind, HydrationFailureKind::StaleRecordEvidence);
    assert_eq!(adapter.state.lock().unwrap().hydration_parses, 2);
}
