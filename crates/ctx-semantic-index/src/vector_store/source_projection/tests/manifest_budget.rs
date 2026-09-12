use super::*;
use crate::vector_store::flat_segments::{FlatSegmentStore, PinnedFlatGeneration};
use anyhow::Context as _;

const MANIFEST_BUDGET: u64 = 64 * 1024;
const SOURCE_COUNT: usize = 48;

fn drain_bounded_maintenance(store: &mut SemanticVectorStore, index: &VerifiedIndex) -> Result<()> {
    let mut builder = CoreBuilder::default();
    let mut embedder = MarkerEmbedder::default();
    for boundary in 0..512 {
        let outcome = store
            .reconcile_source_backed_index_one_durable_boundary_with_checkpoint_and_progress(
                index,
                &mut builder,
                &mut embedder,
                &mut || Ok(()),
                &mut |_| Ok(()),
            )
            .with_context(|| format!("bounded semantic maintenance boundary {boundary}"))?;
        if outcome.ready() {
            return Ok(());
        }
        assert!(outcome.work_remaining());
    }
    Err(anyhow!("bounded semantic maintenance did not converge"))
}

fn ready_pin(
    store: &SemanticVectorStore,
    index: &VerifiedIndex,
    count: usize,
) -> Result<PinnedFlatGeneration> {
    match store.source_backed_generation_pin_exact(index.generation_id(), u64::try_from(count)?)? {
        SourceBackedGenerationPin::Ready(pin) => Ok(pin),
        _ => Err(anyhow!("complete semantic projection is not ready")),
    }
}

fn assert_semantic_query(pin: &PinnedFlatGeneration, expected_events: usize) -> Result<()> {
    assert_eq!(pin.stats().active_events, expected_events);
    let mut query = vec![0.0; semantic_model_contract().dimensions()];
    query[1] = 1.0;
    let identity = |event: Uuid| {
        let mut digest = [0; 32];
        digest[..16].copy_from_slice(event.as_bytes());
        digest[16..].copy_from_slice(event.as_bytes());
        Some(digest)
    };
    let result = scan_exact_generation(pin, &[query], 1, &identity, Instant::now())?;
    assert_eq!(result.hits.len(), 1);
    Ok(())
}

fn assert_manifest_bound(root: &Path) -> Result<()> {
    let mut manifests = 0;
    for entry in fs::read_dir(root.join("flat_manifests"))? {
        let path = entry?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            manifests += 1;
            assert!(fs::metadata(path)?.len() <= MANIFEST_BUDGET);
        }
    }
    assert!(manifests > 0);
    Ok(())
}

#[test]
fn bounded_manifest_backfill_and_incremental_generations_remain_queryable() -> Result<()> {
    FlatSegmentStore::with_test_manifest_byte_limit(MANIFEST_BUDGET, || {
        let fixture = Fixture::new(SOURCE_COUNT)?;
        let mut specs = (0..SOURCE_COUNT)
            .map(|source| (source, bodies(&format!("source-{source}"), 1)))
            .collect::<Vec<_>>();
        let initial = fixture.publish("manifest-backfill", &specs)?;
        let mut store = open_store(&fixture.semantic_path)?;
        drain_bounded_maintenance(&mut store, &initial)?;
        let retained = ready_pin(&store, &initial, SOURCE_COUNT)?;
        assert_semantic_query(&retained, SOURCE_COUNT)?;
        assert_manifest_bound(&fixture.semantic_path)?;

        for revision in 1..=6 {
            specs[0].1.push(format!("incremental document {revision}"));
            let target = fixture.publish(&format!("manifest-incremental-{revision}"), &specs)?;
            drain_bounded_maintenance(&mut store, &target)?;
            assert_semantic_query(
                &ready_pin(&store, &target, SOURCE_COUNT + revision)?,
                SOURCE_COUNT + revision,
            )?;
            assert_manifest_bound(&fixture.semantic_path)?;
            assert_semantic_query(&retained, SOURCE_COUNT)?;
            drop(store);
            store = open_store(&fixture.semantic_path)?;
            assert_semantic_query(
                &ready_pin(&store, &target, SOURCE_COUNT + revision)?,
                SOURCE_COUNT + revision,
            )?;
        }
        Ok(())
    })
}
