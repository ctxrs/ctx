use super::*;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

struct SparseSources {
    generation: String,
    schema: CoreFeedSchema,
    order: Vec<([u8; 32], u64)>,
    records: BTreeMap<[u8; 32], CoreRecord>,
    largest_page: AtomicUsize,
}

fn inventory_source(index: u64) -> SourceKey {
    source(&format!("inventory-{index}"))
}

impl SparseSources {
    fn state(&self, index: u64) -> CoreSourceState {
        let source = inventory_source(index);
        let record = self.records.get(&source.identity().digest());
        let accumulator = record
            .map(|record| {
                let leaf = ctx_history_core::core_record_leaf_digest(
                    record.event_id,
                    &record.encode_stored().unwrap(),
                )
                .unwrap();
                hex::encode(
                    ctx_history_core::core_record_accumulator_leaf_digest(record.event_id, &leaf)
                        .unwrap(),
                )
            })
            .unwrap_or_else(|| "0".repeat(64));
        CoreSourceState {
            source,
            core_record_accumulator: accumulator,
            event_count: u64::from(record.is_some()),
        }
    }
}

impl CoreFeedSnapshot for SparseSources {
    fn generation_id(&self) -> &str {
        &self.generation
    }
    fn schema(&self) -> CoreFeedSchema {
        self.schema.clone()
    }
    fn visit_source_pages(
        &self,
        visit: &mut dyn FnMut(Vec<CoreSourceState>, bool) -> Result<()>,
    ) -> Result<()> {
        if self.order.is_empty() {
            return visit(Vec::new(), true);
        }
        for (ordinal, page) in self.order.chunks(127).enumerate() {
            self.largest_page.fetch_max(page.len(), Ordering::Relaxed);
            let page = page.iter().map(|(_, index)| self.state(*index)).collect();
            visit(page, (ordinal + 1) * 127 >= self.order.len())?;
        }
        Ok(())
    }
    fn record_page(
        &self,
        source: &SourceKey,
        cursor: Option<&CoreFeedRecordCursor>,
        _limit: usize,
        _budget: SnapshotPageBudget,
    ) -> Result<CoreFeedRecordPage> {
        assert!(cursor.is_none());
        let mut encoded_core_bytes = 0;
        let mut content_bytes = 0;
        let items = self
            .records
            .get(&source.identity().digest())
            .map(|record| {
                let encoded = record.encode_stored().unwrap();
                encoded_core_bytes = encoded.len();
                content_bytes = record.content.encoded_content_bytes().unwrap();
                CoreFeedRecordPageItem {
                    core_record: record.clone(),
                    core_record_sha256: hex::encode(Sha256::digest(&encoded)),
                }
            })
            .into_iter()
            .collect();
        Ok(CoreFeedRecordPage {
            generation_id: self.generation.clone(),
            source: source.clone(),
            items,
            encoded_core_bytes,
            content_bytes,
            next_cursor: None,
            terminal: true,
        })
    }
}

#[test]
fn paged_feed_above_16k_preserves_restart_replacement_deletion_and_noop() {
    materialize_inventory(20_003, true);
}

#[test]
#[ignore = "large inventory resource qualification; run explicitly with --ignored --exact"]
fn paged_feed_580225_sources_publishes_reopens_and_noops() {
    materialize_inventory(580_225, false);
}

fn materialize_inventory(count: u64, followup: bool) {
    let started = std::time::Instant::now();
    let fixture = Fixture::new();
    let empty_generation = fixture.publish(1, &[], None);
    let template =
        crate::catch_up::open_exact_core_snapshot(&fixture.data_root, &empty_generation).unwrap();
    // The fixture retains ordering keys only. Descriptors and zero-event source
    // states are generated on demand so its memory cannot hide a feed-sized map.
    let mut order = (0..count)
        .map(|index| (inventory_source(index).identity().digest(), index))
        .collect::<Vec<_>>();
    order.sort_unstable();
    let mut records = BTreeMap::new();
    for ordinal in [0, 16_384, count as usize - 1] {
        let (identity, index) = order[ordinal];
        records.insert(
            identity,
            record(&inventory_source(index), 1, "sparse original"),
        );
    }
    let mut snapshot = SparseSources {
        generation: "a".repeat(64),
        schema: <CoreSnapshot as CoreFeedSnapshot>::schema(&template),
        order,
        records,
        largest_page: AtomicUsize::new(0),
    };
    let sync = |snapshot: &SparseSources, materializer: &mut SegmentMaterializer| {
        sync_core_feed_with_launch(
            &fixture.data_root,
            snapshot,
            materializer,
            None,
            CoreWorkerLaunchSelection::from_runtime(),
        )
        .unwrap()
    };
    let mut materializer = fixture.materializer();
    let (first, did_work) = sync(&snapshot, &mut materializer);
    assert!(did_work);
    assert_eq!(u64::from(first.source_count), count);
    assert_eq!(first.event_count, 3);
    assert_eq!(snapshot.largest_page.load(Ordering::Relaxed), 127);
    let manifest = crate::graph::segment::SegmentStore::new(&fixture.graph_root)
        .load_active()
        .unwrap()
        .unwrap();
    assert_eq!(
        manifest
            .segments
            .iter()
            .filter(|segment| segment.role == crate::materializer::model::MATERIALIZER_SOURCE_ROLE)
            .count(),
        count.div_ceil(100_000) as usize
    );
    assert_eq!(
        manifest.segments.len(),
        count.div_ceil(100_000) as usize + 2,
        "metadata segments plus sparse Flat/event indexes"
    );
    assert!(serde_json::to_vec(&manifest).unwrap().len() < 32 * 1024);
    let index = manifest
        .segments
        .iter()
        .find(|reference| reference.role == ctx_attribution_index::EVENT_STATE_INDEX_ROLE)
        .unwrap();
    let mut generation = [0; 32];
    hex::decode_to_slice(&index.generation_id, &mut generation).unwrap();
    let segment = ctx_attribution_index::SegmentFile::open(
        &fixture.graph_root.join(&index.file_name),
        generation,
        index.role,
    )
    .unwrap();
    let mut reader = ctx_attribution_index::EventIndexReader::open(segment).unwrap();
    for record in snapshot.records.values() {
        let source = ctx_attribution_index::EventIndexSource::new(record.source.clone()).unwrap();
        match reader.lookup(&source, record.event_id).unwrap().unwrap() {
            ctx_attribution_index::EventIndexEntry::State { state, .. } => {
                assert_eq!(
                    state.core_record_sha256,
                    hex::encode(Sha256::digest(record.encode_stored().unwrap()))
                );
                assert_eq!(state.lineage.session_id, record.session_id);
            }
            _ => panic!("published sparse event became a tombstone"),
        }
    }
    drop(reader);
    eprintln!(
        "inventory qualification: sources={count}, metadata_segments={}, total_segments={}, manifest_bytes={}, segment_plaintext_bytes={}, publish_elapsed={:?}",
        count.div_ceil(100_000),
        manifest.segments.len(),
        serde_json::to_vec(&manifest).unwrap().len(),
        manifest
            .segments
            .iter()
            .map(|segment| segment.plaintext_bytes)
            .sum::<u64>(),
        started.elapsed()
    );
    drop(materializer);
    let mut materializer = fixture.materializer();
    assert_eq!(sync(&snapshot, &mut materializer), (first, false));
    if !followup {
        eprintln!(
            "inventory qualification: reopen/no-op elapsed={:?}",
            started.elapsed()
        );
        return;
    }
    snapshot.generation = "b".repeat(64);
    // Keep the first sparse source for a real event replacement; remove roughly
    // half the inventory, including a sparse source, through terminal deletions.
    let keep = snapshot.order[0].0;
    let remove = snapshot.order[count as usize - 1].0;
    snapshot
        .order
        .retain(|(identity, _)| *identity == keep || (*identity != remove && identity[0] % 2 == 0));
    snapshot
        .records
        .retain(|identity, _| *identity == keep || (*identity != remove && identity[0] % 2 == 0));
    let replacement_source = snapshot.records[&keep].source.clone();
    snapshot
        .records
        .insert(keep, record(&replacement_source, 1, "sparse replacement"));
    let (second, did_work) = sync(&snapshot, &mut materializer);
    assert!(did_work);
    assert_eq!(second.source_count as usize, snapshot.order.len());
    assert_eq!(second.event_count as usize, snapshot.records.len());
    drop(materializer);
    assert_eq!(
        sync(&snapshot, &mut fixture.materializer()),
        (second, false)
    );
}
