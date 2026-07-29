use std::{
    fs,
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::SystemTime,
};

use super::*;

#[derive(Clone)]
struct CapturedRouteLeaf {
    source: SourceKey,
    certificate: CertifiedSource,
    document: LexicalDocument,
}

struct CapturedRouteFixture {
    registry: SourceBackedProviderRegistry,
    live_leaves: Arc<Mutex<Vec<CapturedRouteLeaf>>>,
    capture_calls: Arc<AtomicUsize>,
    leaf_scans: Arc<AtomicUsize>,
    route_available: Arc<AtomicBool>,
    source_a: SourceKey,
    source_b: SourceKey,
    source_c: SourceKey,
    cold_b: CertifiedSource,
}

impl CapturedRouteFixture {
    fn new() -> Self {
        let leaves = vec![
            captured_route_leaf(1, 1),
            captured_route_leaf(2, 1),
            captured_route_leaf(3, 1),
        ];
        let source_a = leaves[0].source.clone();
        let source_b = leaves[1].source.clone();
        let source_c = leaves[2].source.clone();
        let cold_b = leaves[1].certificate.clone();
        let owned_sources = Arc::new(
            leaves
                .iter()
                .map(|leaf| leaf.source.clone())
                .collect::<Vec<_>>(),
        );
        let live_leaves = Arc::new(Mutex::new(leaves));
        let capture_calls = Arc::new(AtomicUsize::new(0));
        let leaf_scans = Arc::new(AtomicUsize::new(0));
        let route_available = Arc::new(AtomicBool::new(true));
        let route_source = fixture_provider_source(
            CaptureProvider::Gemini,
            GEMINI_CLI_SOURCE_FORMAT,
            ProviderImportSupport::Native,
        );
        let driver = captured_route_driver(
            &route_source,
            {
                let live_leaves = Arc::clone(&live_leaves);
                let capture_calls = Arc::clone(&capture_calls);
                let leaf_scans = Arc::clone(&leaf_scans);
                let route_available = Arc::clone(&route_available);
                move |sink| {
                    capture_calls.fetch_add(1, Ordering::SeqCst);
                    if !route_available.load(Ordering::SeqCst) {
                        return Err(SourceBackedRouteError::new(
                            SourceBackedRouteErrorKind::Unavailable,
                            "fixture captured route is temporarily unavailable",
                        ));
                    }
                    let leaves = live_leaves.lock().unwrap().clone();
                    for leaf in leaves {
                        leaf_scans.fetch_add(1, Ordering::SeqCst);
                        sink.begin(leaf.source)?;
                        sink.document(leaf.document)?;
                        sink.certify(leaf.certificate)?;
                    }
                    Ok(())
                }
            },
            move |candidate| {
                owned_sources
                    .iter()
                    .any(|source| source.exact_descriptor_eq(candidate))
            },
            |request| {
                Ok(HydratedProviderRecord {
                    event_id: request.event_id(),
                    provider_bytes: request
                        .locator()
                        .source()
                        .identity()
                        .to_string()
                        .into_bytes(),
                })
            },
        );
        let mut registry = SourceBackedProviderRegistry::new();
        registry.register(fixture_executable_route(
            CaptureProvider::Gemini,
            GEMINI_CLI_SOURCE_FORMAT,
            driver,
        ));
        Self {
            registry,
            live_leaves,
            capture_calls,
            leaf_scans,
            route_available,
            source_a,
            source_b,
            source_c,
            cold_b,
        }
    }

    fn reset_capture_calls(&self) {
        self.capture_calls.store(0, Ordering::SeqCst);
        self.leaf_scans.store(0, Ordering::SeqCst);
    }

    fn capture_calls(&self) -> usize {
        self.capture_calls.load(Ordering::SeqCst)
    }

    fn leaf_scans(&self) -> usize {
        self.leaf_scans.load(Ordering::SeqCst)
    }

    fn remove_a_and_mutate_b(&self) {
        *self.live_leaves.lock().unwrap() =
            vec![captured_route_leaf(2, 2), captured_route_leaf(3, 1)];
    }

    fn make_unavailable(&self) {
        self.route_available.store(false, Ordering::SeqCst);
    }
}

#[derive(Debug, PartialEq, Eq)]
struct PublishedSnapshot {
    generation_id: String,
    opstamp: u64,
    meta_json: Vec<u8>,
    meta_modified: SystemTime,
    #[cfg(unix)]
    meta_inode: u64,
    active_segment_inventory: Vec<PathBuf>,
}

fn captured_route_leaf(lineage: u8, revision: u8) -> CapturedRouteLeaf {
    let source = fixture_source(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, lineage);
    let session_id = fixture_session_id(&source);
    let item_key = NativeItemKey::native_id("message", TypedKey::U64(1)).unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source: &source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &item_key,
        subrecord_selector: None,
    })
    .unwrap();
    let revision_digest = [lineage.wrapping_mul(16).wrapping_add(revision); 32];
    let locator = SourceRecordLocator::new(
        source.clone(),
        NativeRecordCoordinate::ProviderNative {
            namespace: "captured-route-lifecycle".to_owned(),
            coordinate: TypedKey::U64(1),
        },
        LocatorRevisionPolicy::ExactSourceRevision,
        Some(revision_digest),
        [lineage.wrapping_mul(32).wrapping_add(revision); 32],
    )
    .unwrap();
    let body = format!("captured route leaf {lineage} revision {revision}");
    let document = LexicalDocument {
        event_id,
        session_id,
        parent_session_id: None,
        root_session_id: session_id,
        source: source.clone(),
        locator,
        provider_session_id: Some(format!("leaf-{lineage}")),
        branch: None,
        source_path: Some(format!("/fixture/captured-route/leaf-{lineage}")),
        agent_type: "primary".to_owned(),
        is_primary: true,
        event_sequence: 1,
        occurred_at_unix_ms: Some(1),
        event_type: "message".to_owned(),
        role: Some("user".to_owned()),
        body,
        workspace: None,
        cwd: None,
        touched_files: Vec::new(),
    };
    let observation = SourceObservation::new(
        source.clone(),
        "captured-route-lifecycle-observation-v1",
        vec![lineage, revision],
    )
    .unwrap();
    let certificate = CertifiedSource::certify(
        observation.clone(),
        observation,
        "ordered-batch-test-v1",
        revision_digest,
        ScannedSourceCounts {
            complete_records: 1,
            retained_records: 1,
            indexed_documents: 1,
            certified_bytes: 1,
            ..ScannedSourceCounts::default()
        },
    )
    .unwrap();
    CapturedRouteLeaf {
        source,
        certificate,
        document,
    }
}

fn publish_captured_route(
    root: &Path,
    fixture: &CapturedRouteFixture,
) -> SourceBackedRefreshReceipt {
    refresh_source_backed_generation(root, &fixture.registry, lifecycle_writer_options()).unwrap()
}

fn lifecycle_writer_options() -> WriterOptions {
    WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    }
}

fn published_snapshot(root: &Path, receipt: &SourceBackedRefreshReceipt) -> PublishedSnapshot {
    let index = VerifiedIndex::open(root).unwrap();
    assert_eq!(index.generation_id(), receipt.commit.generation_id);
    let mut active_segment_inventory = index
        .validate_checksums()
        .unwrap()
        .into_iter()
        .collect::<Vec<_>>();
    active_segment_inventory.sort();
    let meta_path = root.join("meta.json");
    let meta_metadata = fs::metadata(&meta_path).unwrap();
    PublishedSnapshot {
        generation_id: receipt.commit.generation_id.clone(),
        opstamp: receipt.commit.opstamp,
        meta_json: fs::read(meta_path).unwrap(),
        meta_modified: meta_metadata.modified().unwrap(),
        #[cfg(unix)]
        meta_inode: {
            use std::os::unix::fs::MetadataExt as _;

            meta_metadata.ino()
        },
        active_segment_inventory,
    }
}

fn assert_every_published_source_resolves(root: &Path, registry: &SourceBackedProviderRegistry) {
    let index = VerifiedIndex::open(root).unwrap();
    let resolver = registry.resolver_registry();
    for certificate in &index.manifest().sources {
        let source = certificate.observation().source();
        let events = index.source_event_page(source, None, 10).unwrap().items;
        assert_eq!(
            events.len(),
            1,
            "fixture source {} did not retain its one indexed event",
            source.identity()
        );
        let event = &events[0];
        let request = EventHydrationRequest::new(event.event_id, event.locator.clone()).unwrap();
        resolver.hydrate_event(&request).unwrap();
    }
}

#[test]
fn captured_route_unchanged_refresh_uses_one_route_wide_terminal_capture() {
    let temp = tempdir().unwrap();
    let fixture = CapturedRouteFixture::new();
    let cold = publish_captured_route(temp.path(), &fixture);
    assert_eq!(cold.sources.len(), 3);

    fixture.reset_capture_calls();
    let unchanged = publish_captured_route(temp.path(), &fixture);

    assert_every_published_source_resolves(temp.path(), &fixture.registry);
    assert_eq!(unchanged.sources, cold.sources);
    assert_eq!(
        fixture.capture_calls(),
        2,
        "unchanged refresh must perform one staging capture and one route-wide terminal capture"
    );
    assert_eq!(
        fixture.leaf_scans(),
        6,
        "three leaves must be scanned once during staging and once during terminal revalidation"
    );
}

#[test]
fn captured_route_unchanged_refresh_preserves_physical_publication() {
    let temp = tempdir().unwrap();
    let fixture = CapturedRouteFixture::new();
    let cold = publish_captured_route(temp.path(), &fixture);
    let before = published_snapshot(temp.path(), &cold);

    fixture.reset_capture_calls();
    let unchanged = publish_captured_route(temp.path(), &fixture);
    let after = published_snapshot(temp.path(), &unchanged);

    assert_every_published_source_resolves(temp.path(), &fixture.registry);
    assert_eq!(after.generation_id, before.generation_id);
    assert_eq!(after.opstamp, before.opstamp);
    assert_eq!(after.meta_json, before.meta_json);
    assert_eq!(after.meta_modified, before.meta_modified);
    #[cfg(unix)]
    assert_eq!(after.meta_inode, before.meta_inode);
    assert_eq!(
        after.active_segment_inventory,
        before.active_segment_inventory
    );
}

#[test]
fn captured_route_removal_is_certified_or_old_generation_is_retained() {
    let temp = tempdir().unwrap();
    let fixture = CapturedRouteFixture::new();
    let cold = publish_captured_route(temp.path(), &fixture);
    let before = published_snapshot(temp.path(), &cold);
    fixture.remove_a_and_mutate_b();
    fixture.reset_capture_calls();

    match refresh_source_backed_generation(
        temp.path(),
        &fixture.registry,
        lifecycle_writer_options(),
    ) {
        Ok(published) => {
            assert_every_published_source_resolves(temp.path(), &fixture.registry);
            assert!(
                published.sources.iter().all(|certificate| {
                    !certificate
                        .observation()
                        .source()
                        .exact_descriptor_eq(&fixture.source_a)
                }),
                "removed source A was silently carried into a new generation"
            );
            assert!(
                published.removals.iter().any(|removal| {
                    removal
                        .deletion
                        .source()
                        .exact_descriptor_eq(&fixture.source_a)
                }),
                "successful refresh did not carry certified removal evidence for source A"
            );
            let published_b = published
                .sources
                .iter()
                .find(|certificate| {
                    certificate
                        .observation()
                        .source()
                        .exact_descriptor_eq(&fixture.source_b)
                })
                .expect("mutated source B was not retained");
            assert_ne!(published_b, &fixture.cold_b);
            assert!(published.sources.iter().any(|certificate| {
                certificate
                    .observation()
                    .source()
                    .exact_descriptor_eq(&fixture.source_c)
            }));
        }
        Err(_) => {
            let retained = VerifiedIndex::open(temp.path()).unwrap();
            assert_every_published_source_resolves(temp.path(), &fixture.registry);
            assert_eq!(retained.generation_id(), before.generation_id);
            assert_eq!(
                fs::read(temp.path().join("meta.json")).unwrap(),
                before.meta_json
            );
            let mut retained_inventory = retained
                .validate_checksums()
                .unwrap()
                .into_iter()
                .collect::<Vec<_>>();
            retained_inventory.sort();
            assert_eq!(retained_inventory, before.active_segment_inventory);
        }
    }
}

#[test]
fn captured_route_unavailability_retains_the_complete_old_generation() {
    let temp = tempdir().unwrap();
    let fixture = CapturedRouteFixture::new();
    let cold = publish_captured_route(temp.path(), &fixture);
    let before = published_snapshot(temp.path(), &cold);
    fixture.make_unavailable();
    fixture.reset_capture_calls();

    let error = refresh_source_backed_generation(
        temp.path(),
        &fixture.registry,
        lifecycle_writer_options(),
    )
    .unwrap_err();
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
    let retained = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(retained.generation_id(), before.generation_id);
    assert_eq!(
        fs::read(temp.path().join("meta.json")).unwrap(),
        before.meta_json
    );
    assert_eq!(fixture.capture_calls(), 1);
}
