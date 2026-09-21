use std::collections::BTreeSet;
use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::materializer::StatusRequest;
use crate::protocol::CoreProjectionCurrentness;
use ctx_history_core::{
    CertifiedSource, CertifiedSourceDeletion, CertifiedSourceInventory, CoreRecord,
    EventIdentityInput, NativeItemKey, NativeSessionKey, ScannedSourceCounts, SessionIdentityInput,
    SourceAnchor, SourceInventoryObservation, SourceKey, SourceObservation, TypedKey,
    derive_event_id, derive_session_id,
};
use ctx_history_index::{GenerationWriter, WriterOptions};
use ctx_history_platform::platform_security::restrict_private_directory;

use super::*;

#[path = "tests/replacement_regressions.rs"]
mod replacement_regressions;
#[path = "tests/runtime_contracts.rs"]
mod runtime_contracts;
#[path = "tests/writer_wait.rs"]
mod writer_wait;

struct Fixture {
    _temp: tempfile::TempDir,
    data_root: std::path::PathBuf,
    index_root: std::path::PathBuf,
    graph_root: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().to_owned();
        restrict_private_directory(&data_root).unwrap();
        let search = data_root.join("search");
        fs::create_dir(&search).unwrap();
        restrict_private_directory(&search).unwrap();
        let index_root = search.join("lexical");
        fs::create_dir(&index_root).unwrap();
        restrict_private_directory(&index_root).unwrap();
        let graph_root = search.join("attribution");
        Self {
            _temp: temp,
            data_root,
            index_root,
            graph_root,
        }
    }

    fn materializer(&self) -> SegmentMaterializer {
        SegmentMaterializer::open_for_revision(
            &self.graph_root,
            crate::core_materialization::CORE_MATERIALIZER_REVISION,
        )
        .unwrap()
    }

    fn publish(
        &self,
        revision: u8,
        sources: &[(SourceKey, Vec<(u64, String)>)],
        deletion: Option<(&SourceKey, Vec<SourceKey>)>,
    ) -> String {
        let mut writer = GenerationWriter::open(&self.index_root, WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap();
        for (source, records) in sources {
            writer.begin_source(source.clone()).unwrap();
            for (sequence, body) in records {
                writer
                    .add_core_record(record(source, *sequence, body))
                    .unwrap();
            }
            writer
                .certify_source(certificate(source, revision, records.len()))
                .unwrap();
        }
        if let Some((deleted, retained)) = deletion {
            let observation = SourceInventoryObservation::new(
                deleted.provider(),
                "provider-root",
                TypedKey::utf8("root-lineage").unwrap(),
                "tree-inventory-v1",
                vec![revision],
            )
            .unwrap();
            let inventory = CertifiedSourceInventory::certify(
                observation.clone(),
                observation,
                "fixture-discovery-v1",
                retained,
            )
            .unwrap();
            let proof =
                CertifiedSourceDeletion::from_inventory(deleted.clone(), &inventory).unwrap();
            writer.delete_source(proof, inventory).unwrap();
        }
        writer.commit(|_| true).unwrap().generation_id
    }
}

fn source(name: &str) -> SourceKey {
    SourceKey::derive(
        "fixture",
        "fixture_jsonl",
        "fixture-v1",
        1,
        SourceAnchor::provider_native("session-file", TypedKey::utf8(name).unwrap()).unwrap(),
    )
    .unwrap()
}

fn certificate(source: &SourceKey, revision: u8, records: usize) -> CertifiedSource {
    let observation = SourceObservation::new(source.clone(), "file-v1", vec![revision]).unwrap();
    CertifiedSource::certify(
        observation.clone(),
        observation,
        "fixture-parser-v1",
        [revision; 32],
        ScannedSourceCounts {
            complete_records: records as u64,
            retained_records: records as u64,
            indexed_documents: records as u64,
            certified_bytes: records as u64,
            ..ScannedSourceCounts::default()
        },
    )
    .unwrap()
}

fn record(source: &SourceKey, sequence: u64, body: &str) -> CoreRecord {
    let native_session =
        NativeSessionKey::native_id("session", TypedKey::utf8("session").unwrap()).unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: "thread",
        native_session_key: &native_session,
    })
    .unwrap();
    let native_item = NativeItemKey::native_id("message", TypedKey::U64(sequence)).unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &native_item,
        subrecord_selector: None,
    })
    .unwrap();
    CoreRecord::new_selected(
        event_id,
        session_id,
        source.clone(),
        sequence,
        "message",
        "direct-feed-test-v1",
        body.to_owned(),
    )
    .unwrap()
}

fn sync(
    fixture: &Fixture,
    generation: &str,
    materializer: &mut SegmentMaterializer,
) -> CoreMaterializationReceipt {
    let snapshot =
        crate::catch_up::open_exact_core_snapshot(&fixture.data_root, generation).unwrap();
    let CoreMaterializationSyncOutcome::Finished { receipt, did_work } =
        sync_generation_pinned_core(&fixture.data_root, &snapshot, materializer, None).unwrap();
    assert!(did_work);
    receipt
}

#[test]
fn direct_feed_materializes_two_generations_and_current_is_the_only_noop() {
    let fixture = Fixture::new();
    let alpha = source("alpha.jsonl");
    let removed = source("removed.jsonl");
    let first_generation = fixture.publish(
        1,
        &[
            (
                alpha.clone(),
                vec![(1, "old".to_owned()), (2, "tombstone".to_owned())],
            ),
            (removed.clone(), vec![(1, "remove-source".to_owned())]),
        ],
        None,
    );
    let mut materializer = fixture.materializer();
    let first = sync(&fixture, &first_generation, &mut materializer);
    assert_eq!(first.core_generation_id, first_generation);

    let second_generation = fixture.publish(
        2,
        &[(
            alpha.clone(),
            vec![(1, "replacement".to_owned()), (3, "addition".to_owned())],
        )],
        Some((&removed, vec![alpha.clone()])),
    );
    let snapshot =
        crate::catch_up::open_exact_core_snapshot(&fixture.data_root, &second_generation).unwrap();
    let CoreMaterializationSyncOutcome::Finished { receipt, did_work } =
        sync_generation_pinned_core(&fixture.data_root, &snapshot, &mut materializer, None)
            .unwrap();
    assert!(did_work);
    assert_eq!(receipt.core_generation_id, second_generation);
    assert_eq!(receipt.event_count, 2);
    let second_sources = <CoreSnapshot as CoreFeedSnapshot>::source_states(&snapshot).unwrap();
    receipt
        .validate_for_head(&core_generation_head(&snapshot, &second_sources).unwrap())
        .unwrap();

    let CoreMaterializationSyncOutcome::Finished {
        receipt: current,
        did_work,
    } = sync_generation_pinned_core(&fixture.data_root, &snapshot, &mut materializer, None)
        .unwrap();
    assert!(!did_work);
    assert_eq!(current, receipt);
    let status = materializer
        .projection_status(&StatusRequest {
            requested_core_generation_id: Some(second_generation.clone()),
        })
        .unwrap();
    assert_eq!(status.currentness, CoreProjectionCurrentness::Current);
    assert_eq!(status.receipt, Some(receipt));

    let expected_events = [
        record(&alpha, 1, "replacement"),
        record(&alpha, 3, "addition"),
    ]
    .map(|record| record.event_id.digest())
    .into_iter()
    .collect::<BTreeSet<_>>();
    let mut changed_sources = <CoreSnapshot as CoreFeedSnapshot>::source_states(&snapshot).unwrap();
    assert_eq!(changed_sources.len(), 1);
    changed_sources[0].core_record_accumulator = "d".repeat(64);
    let query_generation = "e".repeat(64);
    let query_head = core_generation_head_from_schema(
        &<CoreSnapshot as CoreFeedSnapshot>::schema(&snapshot),
        &query_generation,
        &changed_sources,
    )
    .unwrap();
    let mut session = match materializer
        .start_core_generation(query_head.clone())
        .unwrap()
    {
        CoreGenerationStart::Current(_) => panic!("prospective generation unexpectedly current"),
        CoreGenerationStart::Started(session) => session,
    };
    let reconciliations = session
        .reconcile_source_page(
            CoreSourceDeltaPage::new(
                "0".repeat(64),
                query_generation,
                0,
                true,
                changed_sources
                    .into_iter()
                    .map(CoreSourceDelta::Present)
                    .collect(),
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(reconciliations.len(), 1);
    let (active_states, terminal) = session.event_states(&reconciliations[0], None).unwrap();
    assert!(terminal);
    assert_eq!(
        active_states
            .into_iter()
            .map(|state| state.event_id.digest())
            .collect::<BTreeSet<_>>(),
        expected_events,
    );
    drop(session);
    let cleanup_probe = match materializer.start_core_generation(query_head).unwrap() {
        CoreGenerationStart::Current(_) => panic!("aborted prospective generation became current"),
        CoreGenerationStart::Started(session) => session,
    };
    drop(cleanup_probe);
}

#[test]
fn direct_feed_honors_source_page_batch_and_credit_bounds() {
    let deltas = (0..17)
        .map(|index| {
            CoreSourceDelta::Present(CoreSourceState {
                source: source(&format!("boundary-{index:02}.jsonl")),
                core_record_accumulator: "a".repeat(64),
                event_count: 0,
            })
        })
        .collect::<Vec<_>>();
    let mut deltas = deltas;
    deltas.sort_by_key(|delta| delta.source().identity().digest());
    let one = deltas
        .iter()
        .map(|delta| serde_json::to_vec(delta).unwrap().len())
        .max()
        .unwrap();
    let envelope = (0..=u32::try_from(deltas.len()).unwrap())
        .flat_map(|page_index| [false, true].map(move |terminal| (page_index, terminal)))
        .map(|(page_index, terminal)| {
            empty_source_delta_page_wire_bytes(
                &"1".repeat(64),
                &"2".repeat(64),
                page_index,
                terminal,
            )
            .unwrap()
        })
        .max()
        .unwrap();
    let wire_bound = envelope + one;
    let pages = build_delta_pages_with_wire_bound(
        &"1".repeat(64),
        &"2".repeat(64),
        deltas.clone(),
        wire_bound,
    )
    .unwrap();
    assert_eq!(pages.len(), 17);
    assert!(
        pages
            .iter()
            .all(|page| serde_json::to_vec(page).unwrap().len() <= wire_bound)
    );
    assert!(
        pages
            .iter()
            .all(|page| page.deltas.len() <= MAX_CORE_SOURCE_DELTA_PAGE_ITEMS)
    );

    let mut batch = EventDeltaPageBatchBuilder::new();
    for (index, delta) in deltas.into_iter().enumerate() {
        let reconciliation = CoreSourceReconciliation {
            materialize_index: u32::try_from(index).unwrap(),
            delta,
        };
        let page = event_delta_page(
            &"1".repeat(64),
            &"2".repeat(64),
            &reconciliation,
            0,
            true,
            Vec::new(),
        )
        .unwrap();
        let overflow = batch.try_push(page).unwrap();
        if index < MAX_CORE_EVENT_DELTA_PAGES {
            assert!(overflow.is_none());
        } else {
            assert!(overflow.is_some());
        }
    }
    assert_eq!(batch.pages.len(), MAX_CORE_EVENT_DELTA_PAGES);

    let credits = Arc::new(EncodedPageCredits::new(100));
    let first = credits.acquire(40).unwrap().unwrap();
    let second = credits.acquire(60).unwrap().unwrap();
    assert_eq!(credits.snapshot().unwrap(), (100, 100));
    drop((first, second));
    assert_eq!(credits.snapshot().unwrap(), (0, 100));
}

struct CancellingSnapshot<'a> {
    inner: &'a CoreSnapshot,
    cancelled: &'a AtomicBool,
    calls: AtomicUsize,
}

impl CoreFeedSnapshot for CancellingSnapshot<'_> {
    fn generation_id(&self) -> &str {
        self.inner.generation_id()
    }

    fn schema(&self) -> CoreFeedSchema {
        <CoreSnapshot as CoreFeedSnapshot>::schema(self.inner)
    }

    fn source_states(&self) -> Result<Vec<CoreSourceState>> {
        <CoreSnapshot as CoreFeedSnapshot>::source_states(self.inner)
    }

    fn record_page(
        &self,
        source: &ctx_history_core::SourceKey,
        cursor: Option<&CoreFeedRecordCursor>,
        limit: usize,
        budget: SnapshotPageBudget,
    ) -> Result<CoreFeedRecordPage> {
        let page = <CoreSnapshot as CoreFeedSnapshot>::record_page(
            self.inner, source, cursor, limit, budget,
        )?;
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            assert!(!page.terminal, "fixture must cancel between record pages");
            self.cancelled.store(true, Ordering::SeqCst);
        }
        Ok(page)
    }
}

#[test]
fn cancellation_before_start_and_between_pages_preserves_active_and_cleans_candidate() {
    let fixture = Fixture::new();
    let alpha = source("cancel.jsonl");
    let first_generation =
        fixture.publish(1, &[(alpha.clone(), vec![(1, "active".to_owned())])], None);
    let mut materializer = fixture.materializer();
    let active = sync(&fixture, &first_generation, &mut materializer);

    let many = (0..=MAX_CORE_EVENT_DELTA_PAGE_ITEMS)
        .map(|index| (u64::try_from(index + 1).unwrap(), format!("next-{index}")))
        .collect::<Vec<_>>();
    let next_generation = fixture.publish(2, &[(alpha, many)], None);
    let snapshot =
        crate::catch_up::open_exact_core_snapshot(&fixture.data_root, &next_generation).unwrap();

    let cancelled = || true;
    assert!(
        sync_generation_pinned_core(
            &fixture.data_root,
            &snapshot,
            &mut materializer,
            Some(&cancelled),
        )
        .is_err()
    );

    let cancelled = AtomicBool::new(false);
    let authority = || cancelled.load(Ordering::SeqCst);
    let cancelling = CancellingSnapshot {
        inner: &snapshot,
        cancelled: &cancelled,
        calls: AtomicUsize::new(0),
    };
    assert!(
        sync_core_feed_with_launch(
            &fixture.data_root,
            &cancelling,
            &mut materializer,
            Some(&authority),
            CoreWorkerLaunchSelection::from_runtime(),
        )
        .is_err()
    );
    let status = materializer
        .projection_status(&StatusRequest {
            requested_core_generation_id: Some(first_generation),
        })
        .unwrap();
    assert_eq!(status.currentness, CoreProjectionCurrentness::Current);
    assert_eq!(status.receipt, Some(active));
    let sources = <CoreSnapshot as CoreFeedSnapshot>::source_states(&snapshot).unwrap();
    let cleanup_probe = match materializer
        .start_core_generation(core_generation_head(&snapshot, &sources).unwrap())
        .unwrap()
    {
        CoreGenerationStart::Current(_) => panic!("cancelled generation became active"),
        CoreGenerationStart::Started(session) => session,
    };
    drop(cleanup_probe);
    let retried = sync(&fixture, &next_generation, &mut materializer);
    assert_eq!(retried.core_generation_id, next_generation);
}
