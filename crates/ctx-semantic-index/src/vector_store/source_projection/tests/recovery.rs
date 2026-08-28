use std::{fs, path::Path};

use super::*;

fn source_stage_entries(root: &Path) -> Result<Vec<String>> {
    let directory = root.join("flat_source_stage");
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut entries = fs::read_dir(directory)?
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
        .collect::<std::io::Result<Vec<_>>>()?;
    entries.sort();
    Ok(entries)
}

#[derive(Clone, Copy, Debug)]
enum InvalidEmbeddingBatch {
    Error,
    Short,
    Long,
    WrongDimensions,
    Nan,
    Infinity,
    ZeroNorm,
    OutsideNormalizationTolerance,
    InsideNormalizationTolerance,
}

struct InvalidEmbedder(InvalidEmbeddingBatch);

impl SemanticBatchEmbedder for InvalidEmbedder {
    fn embed_chunks(&mut self, chunks: &[SemanticChunkDocument]) -> Result<Vec<Vec<f32>>> {
        let dimensions = semantic_model_contract().dimensions();
        let unit = || {
            let mut vector = vec![0.0; dimensions];
            vector[0] = 1.0;
            vector
        };
        match self.0 {
            InvalidEmbeddingBatch::Error => Err(anyhow!("injected executor failure")),
            InvalidEmbeddingBatch::Short => Ok((0..chunks.len().saturating_sub(1))
                .map(|_| unit())
                .collect()),
            InvalidEmbeddingBatch::Long => Ok((0..=chunks.len()).map(|_| unit()).collect()),
            InvalidEmbeddingBatch::WrongDimensions => {
                Ok(chunks.iter().map(|_| vec![1.0; dimensions - 1]).collect())
            }
            InvalidEmbeddingBatch::Nan => Ok(chunks
                .iter()
                .map(|_| {
                    let mut vector = unit();
                    vector[0] = f32::NAN;
                    vector
                })
                .collect()),
            InvalidEmbeddingBatch::Infinity => Ok(chunks
                .iter()
                .map(|_| {
                    let mut vector = unit();
                    vector[0] = f32::INFINITY;
                    vector
                })
                .collect()),
            InvalidEmbeddingBatch::ZeroNorm => {
                Ok(chunks.iter().map(|_| vec![0.0; dimensions]).collect())
            }
            InvalidEmbeddingBatch::OutsideNormalizationTolerance => Ok(chunks
                .iter()
                .map(|_| {
                    let mut vector = vec![0.0; dimensions];
                    vector[0] = 1.001_f32;
                    vector
                })
                .collect()),
            InvalidEmbeddingBatch::InsideNormalizationTolerance => Ok(chunks
                .iter()
                .map(|_| {
                    let mut vector = vec![0.0; dimensions];
                    vector[0] = 1.000_4_f32;
                    vector
                })
                .collect()),
        }
    }
}

#[test]
fn malformed_executor_pages_never_publish_and_retry_cleanly_after_restart() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let index = fixture.publish("invalid-executor", &[(0, bodies("invalid", 1))])?;
    let mut store = open_store(&fixture.semantic_path)?;
    let baseline = store
        .flat
        .active_publication_token()
        .map_err(anyhow::Error::new)?;
    let mut durable_frontier = None;

    for mode in [
        InvalidEmbeddingBatch::Error,
        InvalidEmbeddingBatch::Short,
        InvalidEmbeddingBatch::Long,
        InvalidEmbeddingBatch::WrongDimensions,
        InvalidEmbeddingBatch::Nan,
        InvalidEmbeddingBatch::Infinity,
        InvalidEmbeddingBatch::ZeroNorm,
        InvalidEmbeddingBatch::OutsideNormalizationTolerance,
    ] {
        let error = store
            .reconcile_source_backed_index(
                &index,
                &mut CoreBuilder::default(),
                &mut InvalidEmbedder(mode),
            )
            .expect_err("malformed executor output must fail closed");
        assert!(!error.to_string().is_empty(), "missing error for {mode:?}");
        assert_eq!(
            store
                .flat
                .active_publication_token()
                .map_err(anyhow::Error::new)?,
            baseline,
            "{mode:?} changed active Flat authority"
        );
        assert!(store.source_acknowledgement()?.is_none());
        let frontier = store
            .source_frontier()?
            .ok_or_else(|| anyhow!("{mode:?} lost its retry frontier"))?;
        if let Some(expected) = durable_frontier.as_ref() {
            assert_eq!(&frontier, expected, "{mode:?} advanced the frontier");
        } else {
            durable_frontier = Some(frontier);
        }
        drop(store);
        store = open_store(&fixture.semantic_path)?;
    }

    let rebuilt = reconcile_all(
        &mut store,
        &index,
        &mut CoreBuilder::default(),
        &mut InvalidEmbedder(InvalidEmbeddingBatch::InsideNormalizationTolerance),
    )?;
    assert_eq!(rebuilt.records_embedded, 1);
    assert!(store.source_acknowledgement()?.is_some());
    Ok(())
}

#[test]
fn final_changed_source_commit_restart_replays_durable_stage_cleanup() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let initial = fixture.publish("final-commit-initial", &[(0, bodies("initial", 2))])?;
    let target = fixture.publish("final-commit-target", &[(0, bodies("changed", 3))])?;
    let mut clean = SemanticVectorStore::open(
        &fixture.data_root.join("semantic-clean-final"),
        semantic_model_contract(),
    )?;
    reconcile_all(
        &mut clean,
        &initial,
        &mut CoreBuilder::default(),
        &mut MarkerEmbedder::default(),
    )?;
    reconcile_all(
        &mut clean,
        &target,
        &mut CoreBuilder::default(),
        &mut MarkerEmbedder::default(),
    )?;
    let expected = projection_snapshot(&clean)?;

    let mut store = SemanticVectorStore::open(&fixture.semantic_path, semantic_model_contract())?;
    let mut builder = CoreBuilder::default();
    let mut embedder = MarkerEmbedder::default();
    reconcile_all(&mut store, &initial, &mut builder, &mut embedder)?;
    builder.calls.clear();
    store.flat.fail_after_source_publication_commit_once();
    let error = store
        .reconcile_source_backed_index(&target, &mut builder, &mut embedder)
        .unwrap_err();
    assert!(error.to_string().contains(
        "injected failure after published semantic source frontier commit before staging acknowledgement"
    ));
    let frontier = store
        .source_frontier()?
        .ok_or_else(|| anyhow!("final source commit lost its durable frontier"))?;
    assert!(frontier.active_source_identity_digest.is_none());
    assert_eq!(
        store
            .flat
            .active_publication_token()
            .map_err(anyhow::Error::new)?,
        frontier.flat_publication
    );
    assert_eq!(projection_snapshot(&store)?, expected);
    let retained = source_stage_entries(&fixture.semantic_path)?;
    assert!(retained.iter().any(|entry| entry == "final.json"));
    drop(store);

    builder.calls.clear();
    let mut restarted =
        SemanticVectorStore::open(&fixture.semantic_path, semantic_model_contract())?;
    let restarted_outcome = reconcile_all(&mut restarted, &target, &mut builder, &mut embedder)?;
    assert_eq!(restarted_outcome.records_decoded, 0);
    assert!(
        builder.calls.is_empty(),
        "final changed source replay unexpectedly staged a later source"
    );
    assert!(source_stage_entries(&fixture.semantic_path)?.is_empty());
    assert_eq!(projection_snapshot(&restarted)?, expected);
    Ok(())
}

#[test]
fn tampered_final_candidate_cannot_acknowledge_or_delete_staging() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let initial = fixture.publish("tampered-ack-initial", &[(0, bodies("initial", 2))])?;
    let target = fixture.publish("tampered-ack-target", &[(0, bodies("changed", 3))])?;
    let mut store = SemanticVectorStore::open(&fixture.semantic_path, semantic_model_contract())?;
    let mut builder = CoreBuilder::default();
    let mut embedder = MarkerEmbedder::default();
    reconcile_all(&mut store, &initial, &mut builder, &mut embedder)?;
    store.flat.fail_after_source_publication_commit_once();
    let error = store
        .reconcile_source_backed_index(&target, &mut builder, &mut embedder)
        .unwrap_err();
    assert!(error.to_string().contains("before staging acknowledgement"));
    store
        .flat
        .corrupt_retained_source_candidate_hash()
        .map_err(anyhow::Error::new)?;
    let retained = source_stage_entries(&fixture.semantic_path)?;
    let active = projection_snapshot(&store)?;
    drop(store);

    let mut restarted =
        SemanticVectorStore::open(&fixture.semantic_path, semantic_model_contract())?;
    let error = restarted
        .reconcile_source_backed_index(&target, &mut builder, &mut embedder)
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("candidate disagrees with active Flat authority"));
    assert_eq!(source_stage_entries(&fixture.semantic_path)?, retained);
    assert_eq!(projection_snapshot(&restarted)?, active);
    Ok(())
}
