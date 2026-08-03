#[test]
fn complete_verifier_decodes_once_with_bounded_parallel_segment_state() {
    const SOURCE_COUNT: usize = 6;
    const DOCUMENTS_PER_SOURCE: u64 = 24;

    let (temp, sources) = multisegment_fixture(SOURCE_COUNT, DOCUMENTS_PER_SOURCE);
    let (searcher, manifest) = open_unverified_generation(temp.path());
    assert_eq!(sources.len(), SOURCE_COUNT);
    assert_eq!(searcher.segment_readers().len(), SOURCE_COUNT);

    let metrics =
        crate::publication::verify_searcher_with_metrics(&searcher, &manifest, 2, true).unwrap();
    let expected_documents = SOURCE_COUNT * DOCUMENTS_PER_SOURCE as usize;

    assert_eq!(metrics.worker_budget, 2);
    assert_eq!(metrics.segment_tasks, SOURCE_COUNT);
    assert_eq!(metrics.document_decodes, expected_documents);
    assert_eq!(metrics.source_terms, SOURCE_COUNT);
    assert_eq!(metrics.max_active_workers, 2);
    assert_eq!(metrics.max_buffered_segments, metrics.worker_budget);
    assert_eq!(metrics.max_buffered_event_identities, 0);
    assert_eq!(metrics.max_buffered_session_identities, 0);
    assert!(metrics.stored_core_bytes > 0);
    assert!(metrics.body_tokens >= expected_documents as u64);
    assert_eq!(
        metrics.verification_spill_bytes,
        expected_documents as u64 * 133
    );
    assert!(metrics.verification_heap_payload_bound_bytes < 32 * 1024);
}

#[test]
fn complete_verifier_rejects_identity_digest_corruption() {
    let temp = tempdir().unwrap();
    let source = source("digest-corruption.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer
        .add_core_record(document(&source, 1, "body"))
        .unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();

    let pinned = VerifiedIndex::open(temp.path()).unwrap();
    let fields = fields_from_schema(pinned.searcher.schema()).unwrap();
    let address = pinned
        .searcher
        .search(&AllQuery, &DocSetCollector)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let document = indexed_document(decoded_stored_core(&pinned.searcher, address));
    let mut forged = TantivyDocument::default();
    for (field, value) in document.field_values() {
        if field != fields.event_identity_digest {
            forged.add_field_value(field, value);
        }
    }
    forged.add_text(fields.event_identity_digest, "00");
    let index = pinned.searcher.index().clone();
    publish_unchecked_generation(
        temp.path(),
        &index,
        GenerationManifest::from_sources(vec![certificate(&source, 2, 1)]).unwrap(),
        std::slice::from_ref(&source),
        vec![forged],
    );

    let (searcher, manifest) = open_unverified_generation(temp.path());
    let error = verify_searcher(&searcher, &manifest).unwrap_err();
    assert!(matches!(
        error,
        IndexError::InvalidStoredDocumentField("core_record")
    ));
}

#[test]
fn complete_verifier_rejects_source_count_corruption() {
    let temp = tempdir().unwrap();
    let first = source("count-first.jsonl");
    let second = source("count-second.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(first.clone()).unwrap();
    writer
        .add_core_record(document(&first, 1, "first"))
        .unwrap();
    writer.certify_source(certificate(&first, 1, 1)).unwrap();
    writer.begin_source(second.clone()).unwrap();
    writer
        .add_core_record(document(&second, 1, "second"))
        .unwrap();
    writer
        .add_core_record(document(&second, 2, "second"))
        .unwrap();
    writer.certify_source(certificate(&second, 1, 2)).unwrap();
    writer.commit(|_| true).unwrap();

    let pinned = VerifiedIndex::open(temp.path()).unwrap();
    let index = pinned.searcher.index().clone();
    publish_unchecked_generation(
        temp.path(),
        &index,
        GenerationManifest::from_sources(vec![
            certificate(&first, 2, 2),
            certificate(&second, 2, 1),
        ])
        .unwrap(),
        &[],
        Vec::new(),
    );

    let (searcher, manifest) = open_unverified_generation(temp.path());
    let error = verify_searcher(&searcher, &manifest).unwrap_err();
    assert!(matches!(
        error,
        IndexError::CoreRecordAggregateCountMismatch { .. }
    ));
}

#[test]
fn complete_verifier_rejects_total_count_corruption() {
    let temp = tempdir().unwrap();
    let source = source("total-count.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer
        .add_core_record(document(&source, 1, "body"))
        .unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();

    let pinned = VerifiedIndex::open(temp.path()).unwrap();
    let index = pinned.searcher.index().clone();
    publish_unchecked_generation(
        temp.path(),
        &index,
        GenerationManifest::from_sources(vec![certificate(&source, 2, 2)]).unwrap(),
        &[],
        Vec::new(),
    );

    let (searcher, manifest) = open_unverified_generation(temp.path());
    let error = verify_searcher(&searcher, &manifest).unwrap_err();
    assert!(matches!(
        error,
        IndexError::DocumentCountMismatch {
            manifest: 2,
            index: 1
        }
    ));
}
