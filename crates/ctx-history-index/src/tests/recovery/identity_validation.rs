#[test]
fn certificate_count_mismatch_is_rejected_before_commit() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer
        .add_core_record(document(&source, 1, "body"))
        .unwrap();
    let error = writer
        .certify_source(certificate(&source, 1, 2))
        .unwrap_err();
    assert!(matches!(
        error,
        IndexError::SourceDocumentCountMismatch { .. }
    ));
}

#[test]
fn duplicate_event_identity_is_rejected_by_prepublication_term_audit() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    let duplicate = document(&source, 1, "first");
    writer.add_core_record(duplicate.clone()).unwrap();
    writer.add_core_record(duplicate).unwrap();
    writer.certify_source(certificate(&source, 1, 2)).unwrap();
    let error = writer.commit(|_| true).unwrap_err();
    assert!(matches!(error, IndexError::DuplicateEventIdentity(_)));
    assert!(load_active_generation_pointer(temp.path())
        .unwrap()
        .is_none());
}

#[test]
fn verified_generation_rejects_a_forged_duplicate_event_identity() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer
        .add_core_record(document(&source, 1, "body"))
        .unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();

    let pinned = VerifiedIndex::open(temp.path()).unwrap();
    let addresses = pinned.searcher.search(&AllQuery, &DocSetCollector).unwrap();
    let address = addresses.into_iter().next().unwrap();
    let duplicate = indexed_document(decoded_stored_core(&pinned.searcher, address));
    let index = pinned.searcher.index().clone();
    publish_unchecked_generation(
        temp.path(),
        &index,
        GenerationManifest::from_sources(vec![certificate(&source, 2, 2)]).unwrap(),
        &[],
        vec![duplicate],
    );

    let (searcher, manifest) = open_unverified_generation(temp.path());
    assert!(matches!(
        verify_searcher(&searcher, &manifest),
        Err(IndexError::DuplicateEventIdentity(_))
    ));
    let error = match VerifiedIndex::open(temp.path()) {
        Ok(_) => panic!("duplicate event generation unexpectedly opened"),
        Err(error) => error,
    };
    assert!(matches!(error, IndexError::DuplicateEventIdentity(_)));
}

#[test]
fn verified_generation_rejects_forged_source_ownership() {
    let temp = tempdir().unwrap();
    let first = source("first.jsonl");
    let second = source("second.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(first.clone()).unwrap();
    writer.add_core_record(document(&first, 1, "body")).unwrap();
    writer.certify_source(certificate(&first, 1, 1)).unwrap();
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
        if field != fields.source_key {
            forged.add_field_value(field, value);
        }
    }
    forged.add_text(fields.source_key, source_token(&second));
    let index = pinned.searcher.index().clone();
    publish_unchecked_generation(
        temp.path(),
        &index,
        GenerationManifest::from_sources(vec![certificate(&second, 2, 1)]).unwrap(),
        std::slice::from_ref(&first),
        vec![forged],
    );

    let (searcher, manifest) = open_unverified_generation(temp.path());
    assert!(matches!(
        verify_searcher(&searcher, &manifest),
        Err(IndexError::InvalidStoredDocumentField("core_record"))
    ));
    let error = match VerifiedIndex::open(temp.path()) {
        Ok(_) => panic!("source ownership mismatch unexpectedly opened"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        IndexError::InvalidStoredDocumentField("core_record")
    ));
}

#[test]
fn verified_generation_rejects_malformed_stored_core_during_exhaustive_audit() {
    let temp = tempdir().unwrap();
    let source = source("malformed-core.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    let event = document(&source, 1, "complete body");
    writer.add_core_record(event).unwrap();
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
    let document = pinned.searcher.doc::<TantivyDocument>(address).unwrap();
    let mut forged = TantivyDocument::default();
    for (field, value) in document.field_values() {
        if field != fields.core_record && field != fields.core_record_encoded_bytes {
            forged.add_field_value(field, value);
        }
    }
    forged.add_u64(fields.core_record_encoded_bytes, 1);
    forged.add_bytes(fields.core_record, b"{");
    let index = pinned.searcher.index().clone();
    publish_unchecked_generation(
        temp.path(),
        &index,
        GenerationManifest::from_sources(vec![certificate(&source, 2, 1)]).unwrap(),
        std::slice::from_ref(&source),
        vec![forged],
    );

    assert!(matches!(
        VerifiedIndex::open(temp.path()),
        Err(IndexError::CoreRecord(_))
    ));
}

#[test]
fn document_identity_kinds_are_checked() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    let mut invalid = document(&source, 1, "body");
    invalid.event_id = invalid.session_id;
    let error = writer.add_core_record(invalid).unwrap_err();
    assert!(matches!(error, IndexError::CoreRecord(_)));
}

#[test]
fn document_identities_must_belong_to_the_document_source() {
    let temp = tempdir().unwrap();
    let first = source("first");
    let second = source("second");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(second.clone()).unwrap();
    let mut invalid = document(&first, 1, "body");
    invalid.source = second;
    let error = writer.add_core_record(invalid).unwrap_err();
    assert!(matches!(error, IndexError::CoreRecord(_)));
}

#[test]
fn empty_core_body_is_rejected_by_the_canonical_writer_validation() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    let mut invalid = document(&source, 1, "body");
    invalid.content.normalized_body = Some(String::new());
    let error = writer.add_core_record(invalid).unwrap_err();
    assert!(matches!(error, IndexError::CoreRecord(_)));
}

#[test]
fn invalid_memory_budget_has_no_filesystem_side_effect() {
    let parent = tempdir().unwrap();
    let root = parent.path().join("not-created");
    let error = match GenerationWriter::open(
        &root,
        WriterOptions {
            indexer_threads: 2,
            memory_bytes: 1,
        },
    ) {
        Ok(_) => panic!("invalid memory budget unexpectedly opened an index"),
        Err(error) => error,
    };
    assert!(matches!(error, IndexError::IndexMemoryTooSmall { .. }));
    assert!(!root.exists());
}
