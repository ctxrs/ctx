use super::*;

#[test]
fn retrieval_excluded_events_never_enter_the_source_backed_semantic_projection() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let mut excluded = fixture.record(0, 1, "retrieval payload must not embed")?;
    excluded.content.discovery_exclusion = Some(CoreDiscoveryExclusion::CtxRetrievalDerived);
    excluded.validate_contract()?;

    let root = fixture.data_root.join("index-retrieval-semantic-exclusion");
    let fixture_source = &fixture.sources[0];
    let mut writer = GenerationWriter::open(&root, WriterOptions::default())?
        .into_writer()
        .map_err(crate::committed_generation_recovery_error)?;
    writer.begin_source(fixture_source.source.clone())?;
    writer.add_core_record(excluded.clone())?;
    let observation = SourceObservation::new(
        fixture_source.source.clone(),
        "fixture-retrieval-semantic-exclusion",
        b"retrieval-semantic-exclusion".to_vec(),
    )?;
    writer.certify_source(CertifiedSource::certify(
        observation.clone(),
        observation,
        "fixture-parser-v1",
        [1; 32],
        ScannedSourceCounts {
            complete_records: 1,
            retained_records: 1,
            indexed_documents: 1,
            certified_bytes: 50,
            ..ScannedSourceCounts::default()
        },
    )?)?;
    writer.commit(|_| true)?;
    let index = VerifiedIndex::open_pinned(root)?;
    let source_digest = index.manifest().core_record_aggregates[0]
        .source_identity_digest()
        .to_owned();

    assert_eq!(index.manifest().indexed_documents, 1);
    assert_eq!(index.semantic_eligible_event_count()?, 0);
    let semantic = index.core_semantic_event_page(None, 1)?;
    assert!(semantic.terminal);
    assert!(semantic.items.is_empty());

    let mut store = SemanticVectorStore::open(&fixture.semantic_path, semantic_model_contract())?;
    let mut builder = CoreBuilder::default();
    let mut embedder = MarkerEmbedder::default();
    let outcome = reconcile_all(&mut store, &index, &mut builder, &mut embedder)?;
    assert_eq!(outcome.records_decoded, 1);
    assert_eq!(outcome.records_embedded, 0);
    assert!(builder.calls.is_empty());
    assert_eq!(embedder.chunks, 0);
    assert_eq!(active_events(&store)?, 0);
    assert!(source_rows(&store, &source_digest)?.is_empty());
    Ok(())
}
