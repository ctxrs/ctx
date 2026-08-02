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

#[test]
fn final_changed_source_commit_restart_replays_durable_stage_cleanup() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let initial = fixture.publish("final-commit-initial", &[(0, bodies("initial", 2))])?;
    let target = fixture.publish("final-commit-target", &[(0, bodies("changed", 3))])?;
    let mut clean = SemanticVectorStore::open(&fixture.data_root.join("semantic-clean-final"))?;
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

    let mut store = SemanticVectorStore::open(&fixture.semantic_path)?;
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
    let mut restarted = SemanticVectorStore::open(&fixture.semantic_path)?;
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
    let mut store = SemanticVectorStore::open(&fixture.semantic_path)?;
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

    let mut restarted = SemanticVectorStore::open(&fixture.semantic_path)?;
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
