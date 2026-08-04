#[test]
fn stored_document_contains_exactly_one_canonical_core_record() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let expected = document(&source, 1, "body");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(expected.clone()).unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    let fields = fields_from_schema(index.searcher.schema()).unwrap();
    let address = index
        .searcher
        .search(&AllQuery, &DocSetCollector)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let stored: TantivyDocument = index.searcher.doc(address).unwrap();
    let values = stored.field_values().collect::<Vec<_>>();

    assert_eq!(values.len(), 1);
    assert_eq!(values[0].0, fields.core_record);
    let encoded = values[0].1.as_bytes().unwrap();
    assert_eq!(CoreRecord::decode_stored(encoded).unwrap(), expected);
    let segment = &index.searcher.segment_readers()[address.segment_ord as usize];
    let encoded_fast_bytes = segment
        .fast_fields()
        .u64("core_record_encoded_bytes")
        .unwrap()
        .first(address.doc_id)
        .unwrap();
    assert_eq!(usize::try_from(encoded_fast_bytes).unwrap(), encoded.len());
    assert_eq!(
        index
            .event_by_id(expected.event_id.as_uuid())
            .unwrap()
            .unwrap()
            .event_id,
        expected.event_id
    );
}

#[test]
fn direct_core_record_is_the_canonical_locator_free_write_path() {
    let temp = tempdir().unwrap();
    let source = source("direct-core.jsonl");
    let expected = document(&source, 1, "direct Core body");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(expected.clone()).unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    let actual = index
        .core_record_by_id(expected.event_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(actual, expected);
    assert!(index
        .event_by_id(expected.event_id.as_uuid())
        .unwrap()
        .unwrap()
        .source
        .exact_descriptor_eq(&source));
}

#[test]
fn direct_core_record_rejects_noncurrent_policy_revisions() {
    let temp = tempdir().unwrap();
    let source = source("direct-core-policy.jsonl");
    let mut record = document(&source, 1, "direct Core body");
    record.normalization_revision += 1;
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source).unwrap();

    assert!(matches!(
        writer.add_core_record(record),
        Err(IndexError::CoreRecordPolicyRevisionMismatch { .. })
    ));
}
