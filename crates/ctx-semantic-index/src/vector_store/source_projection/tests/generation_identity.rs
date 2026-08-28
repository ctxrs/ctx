use super::*;

#[test]
fn incremental_delta_reopens_catches_up_and_acknowledges_verified_generation() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let root = fixture.data_root.join("index-delta-manifest-identity");
    let initial = fixture.publish_to_root(&root, "delta-base", &[(0, bodies("base", 1))])?;
    let initial_generation_id = initial.generation_id().to_owned();
    assert_eq!(initial.manifest().generation_id()?, initial_generation_id);

    let mut store = SemanticVectorStore::open(&fixture.semantic_path, semantic_model_contract())?;
    let mut builder = CoreBuilder::default();
    let mut embedder = MarkerEmbedder::default();
    assert!(reconcile_all(&mut store, &initial, &mut builder, &mut embedder)?.ready);
    drop(initial);

    let mut appended = bodies("base", 1);
    appended.push("appended event".to_owned());
    let published = fixture.publish_to_root(&root, "delta-next", &[(0, appended)])?;
    let published_generation_id = published.generation_id().to_owned();
    assert_ne!(
        published.manifest().generation_id()?,
        published_generation_id
    );
    drop(published);

    let reopened = VerifiedIndex::open_pinned(&root)?;
    assert_eq!(reopened.generation_id(), published_generation_id);
    assert_ne!(
        reopened.manifest().generation_id()?,
        reopened.generation_id()
    );
    let generation =
        SourceBackedSemanticGeneration::from_verified_index(&reopened, semantic_model_contract())?;
    assert_eq!(generation.core_generation_id, reopened.generation_id());

    let mut stale = generation.clone();
    stale.core_generation_id = initial_generation_id.clone();
    let error = store
        .reconcile_source_backed_generation(&reopened, &stale, &mut builder, &mut embedder)
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("source-backed semantic target does not match its pinned Core index"));

    let outcome = reconcile_generation(
        &mut store,
        &reopened,
        &generation,
        &mut builder,
        &mut embedder,
    )?;
    assert!(outcome.ready);
    assert_eq!(outcome.records_decoded, 2);
    assert_eq!(outcome.records_reused, 1);
    assert_eq!(outcome.records_embedded, 1);
    assert_eq!(
        store
            .source_acknowledgement()?
            .expect("incremental semantic acknowledgement")
            .core_generation_id,
        published_generation_id
    );
    assert!(matches!(
        store.source_backed_generation_pin_exact(&initial_generation_id, 1)?,
        SourceBackedGenerationPin::NotReady
    ));
    assert!(matches!(
        store.source_backed_generation_pin_exact(&published_generation_id, 2)?,
        SourceBackedGenerationPin::Ready(_)
    ));
    Ok(())
}
