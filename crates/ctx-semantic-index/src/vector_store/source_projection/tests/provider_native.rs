use super::*;

#[test]
fn provider_native_copies_enter_semantic_projection_without_target_resolution() -> Result<()> {
    let fixture = Fixture::new(4)?;
    let original = fixture.record(0, 1, "semantic copy canary")?;
    let mut copied = fixture.record(1, 1, "semantic copy canary")?;
    copied.parent_session_id = Some(original.session_id);
    copied.root_session_id = Some(original.session_id);
    copied.session_relationship = Some(ProviderNativeSessionRelationship::Forked);
    copied.event_copy = Some(ProviderNativeEventCopy {
        ancestor_session_id: original.session_id,
        ancestor_event_id: original.event_id,
        proof: ProviderNativeCopyProof::NativeEventIdentity,
    });
    copied.validate_contract()?;

    let missing_target = fixture.record(3, 1, "absent semantic copy target")?;
    let mut copied_with_missing_target = fixture.record(2, 1, "missing-target copy canary")?;
    copied_with_missing_target.parent_session_id = Some(missing_target.session_id);
    copied_with_missing_target.root_session_id = Some(missing_target.session_id);
    copied_with_missing_target.session_relationship =
        Some(ProviderNativeSessionRelationship::Forked);
    copied_with_missing_target.event_copy = Some(ProviderNativeEventCopy {
        ancestor_session_id: missing_target.session_id,
        ancestor_event_id: missing_target.event_id,
        proof: ProviderNativeCopyProof::NativeCopiedFromField,
    });
    copied_with_missing_target.validate_contract()?;

    let root = fixture.data_root.join("index-copied-semantic-inclusion");
    let mut writer = GenerationWriter::open(&root, WriterOptions::default())?
        .into_writer()
        .map_err(crate::committed_generation_recovery_error)?;
    for (source_index, record) in [
        (0_usize, original.clone()),
        (1, copied.clone()),
        (2, copied_with_missing_target.clone()),
    ] {
        let fixture_source = &fixture.sources[source_index];
        writer.begin_source(fixture_source.source.clone())?;
        writer.add_core_record(record)?;
        let observation = SourceObservation::new(
            fixture_source.source.clone(),
            "fixture-copied-semantic-inclusion",
            vec![u8::try_from(source_index + 1)?],
        )?;
        writer.certify_source(CertifiedSource::certify(
            observation.clone(),
            observation,
            "fixture-parser-v1",
            [u8::try_from(source_index + 1)?; 32],
            ScannedSourceCounts {
                complete_records: 1,
                retained_records: 1,
                indexed_documents: 1,
                certified_bytes: 50,
                ..ScannedSourceCounts::default()
            },
        )?)?;
    }
    writer.commit(|_| true)?;
    let index = VerifiedIndex::open_pinned(root)?;

    assert_eq!(index.manifest().indexed_documents, 3);
    assert!(index
        .core_source_event_page(&fixture.sources[0].source, None, 1)?
        .items[0]
        .event_copy
        .is_none());
    assert_eq!(
        index
            .core_source_event_page(&fixture.sources[1].source, None, 1)?
            .items[0]
            .event_copy,
        copied.event_copy
    );
    assert_eq!(
        index
            .core_source_event_page(&fixture.sources[2].source, None, 1)?
            .items[0]
            .event_copy,
        copied_with_missing_target.event_copy
    );

    let mut store = SemanticVectorStore::open(&fixture.semantic_path, semantic_model_contract())?;
    let mut builder = CoreBuilder::default();
    let mut embedder = MarkerEmbedder::default();
    let outcome = reconcile_all(&mut store, &index, &mut builder, &mut embedder)?;
    assert_eq!(outcome.records_decoded, 3);
    assert_eq!(outcome.records_embedded, 3);
    assert_eq!(outcome.records_filtered, 0);
    assert_eq!(builder.calls.len(), 3);
    assert_eq!(
        builder.calls.iter().copied().collect::<HashSet<_>>(),
        HashSet::from([
            original.event_id.as_uuid(),
            copied.event_id.as_uuid(),
            copied_with_missing_target.event_id.as_uuid(),
        ])
    );
    assert_eq!(active_events(&store)?, 3);
    Ok(())
}
