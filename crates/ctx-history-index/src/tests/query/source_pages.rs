use super::*;

#[test]
fn source_event_pages_order_across_segments_isolate_and_do_not_duplicate() {
    let temp = tempdir().unwrap();
    let target = source("paged-source.jsonl");
    let other = source("other-source.jsonl");
    let target_first = document(&target, 1, "target first");
    let target_second = document(&target, 2, "target second");
    let target_third = document(&target, 3, "target third");
    let target_fourth = document(&target, 4, "target fourth");
    let other_first = document(&other, 1, "other first");
    let other_second = document(&other, 2, "other second");

    let mut first = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    first
        .writer_mut()
        .unwrap()
        .set_merge_policy(Box::<NoMergePolicy>::default());
    first.begin_source(target.clone()).unwrap();
    first.add_core_record(target_fourth.clone()).unwrap();
    first.add_core_record(target_first.clone()).unwrap();
    first
        .certify_source(appendable_certificate(&target, 1, 2, 20))
        .unwrap();
    first.begin_source(other.clone()).unwrap();
    first.add_core_record(other_second.clone()).unwrap();
    first.add_core_record(other_first.clone()).unwrap();
    first.certify_source(certificate(&other, 1, 2)).unwrap();
    first.commit(|_| true).unwrap();

    let mut append = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    append
        .writer_mut()
        .unwrap()
        .set_merge_policy(Box::<NoMergePolicy>::default());
    let base = append.begin_source_append(target.clone()).unwrap().clone();
    append.add_core_record(target_third.clone()).unwrap();
    append.add_core_record(target_second.clone()).unwrap();
    let proof = CertifiedSourceAppend::certify(
        &base,
        appendable_certificate(&target, 2, 4, 40),
        20,
        [1; 32],
    )
    .unwrap();
    append.certify_source_append(proof).unwrap();
    append.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    assert!(
        index.searcher.segment_readers().len() >= 2,
        "test requires a multi-segment generation"
    );
    let mut expected = vec![
        target_first.event_id,
        target_second.event_id,
        target_third.event_id,
        target_fourth.event_id,
    ];
    expected.sort_by_key(|identity| identity.encode_canonical().unwrap());

    let first_page = index.source_event_page(&target, None, 2).unwrap();
    let core_first_page = index.core_source_event_page(&target, None, 2).unwrap();
    assert_eq!(first_page.generation_id, index.generation_id());
    assert_eq!(core_first_page.generation_id, first_page.generation_id);
    assert_eq!(core_first_page.terminal, first_page.terminal);
    assert_eq!(
        core_first_page
            .next_cursor
            .as_ref()
            .map(SourceEventCursor::after),
        first_page
            .next_cursor
            .as_ref()
            .map(SourceEventCursor::after)
    );
    assert!(first_page.source.exact_descriptor_eq(&target));
    assert!(core_first_page.source.exact_descriptor_eq(&target));
    assert!(!first_page.terminal);
    assert_eq!(
        first_page
            .items
            .iter()
            .map(|event| event.event_id)
            .collect::<Vec<_>>(),
        expected[..2]
    );
    assert_eq!(
        core_first_page
            .items
            .iter()
            .map(|record| record.event_id)
            .collect::<Vec<_>>(),
        expected[..2]
    );
    assert!(core_first_page.items.iter().all(|record| record
        .core_record
        .content
        .normalized_body
        .is_some()));
    assert!(first_page
        .items
        .iter()
        .all(|event| event.source.exact_descriptor_eq(&target)));

    let serialized = serde_json::to_vec(first_page.next_cursor.as_ref().unwrap()).unwrap();
    let cursor: SourceEventCursor = serde_json::from_slice(&serialized).unwrap();
    assert_eq!(cursor.generation_id(), index.generation_id());
    assert!(cursor.source().exact_descriptor_eq(&target));
    assert_eq!(cursor.after(), expected[1]);
    let final_page = index.source_event_page(&target, Some(&cursor), 2).unwrap();
    let core_final_page = index
        .core_source_event_page(&target, Some(&cursor), 2)
        .unwrap();
    assert!(final_page.terminal);
    assert_eq!(core_final_page.terminal, final_page.terminal);
    assert!(final_page.next_cursor.is_none());
    assert!(core_final_page.next_cursor.is_none());
    assert_eq!(
        final_page
            .items
            .iter()
            .map(|event| event.event_id)
            .collect::<Vec<_>>(),
        expected[2..]
    );
    assert_eq!(
        core_final_page
            .items
            .iter()
            .map(|record| record.event_id)
            .collect::<Vec<_>>(),
        expected[2..]
    );

    let all = collect_source_pages(&index, &target, 1);
    assert_eq!(
        all.iter().map(|event| event.event_id).collect::<Vec<_>>(),
        expected
    );
    let unique = all
        .iter()
        .map(|event| event.event_id)
        .collect::<HashSet<_>>();
    assert_eq!(unique.len(), expected.len());
    assert!(all
        .iter()
        .all(|event| event.source.exact_descriptor_eq(&target)));

    let other_page = index.source_event_page(&other, None, 10).unwrap();
    let mut expected_other = vec![other_first.event_id, other_second.event_id];
    expected_other.sort_by_key(|identity| identity.encode_canonical().unwrap());
    assert_eq!(
        other_page
            .items
            .iter()
            .map(|event| event.event_id)
            .collect::<Vec<_>>(),
        expected_other
    );
    assert!(matches!(
        index.source_event_page(&other, Some(&cursor), 1),
        Err(IndexError::SourceEventCursorSourceMismatch)
    ));
    let invalid_identity =
        SourceEventCursor::new(index.generation_id(), target.clone(), other_first.event_id);
    assert!(matches!(
        index.source_event_page(&target, Some(&invalid_identity), 1),
        Err(IndexError::InvalidSourceEventCursorIdentity)
    ));
}

#[test]
fn source_event_second_page_reopens_without_materializing_the_remaining_source() {
    const EVENT_COUNT: u64 = 1_024;
    const PAGE_LIMIT: usize = 17;

    let temp = tempdir().unwrap();
    let source = source("large-paged-source.jsonl");
    let mut expected = Vec::with_capacity(EVENT_COUNT as usize);
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    for sequence in 1..=EVENT_COUNT {
        let event = document(&source, sequence, "large source body");
        expected.push(event.event_id);
        writer.add_core_record(event).unwrap();
    }
    writer
        .certify_source(certificate(&source, 1, EVENT_COUNT))
        .unwrap();
    writer.commit(|_| true).unwrap();
    expected.sort_by_key(|identity| identity.encode_canonical().unwrap());

    let initial = VerifiedIndex::open(temp.path()).unwrap();
    let first = initial
        .core_source_event_page(&source, None, PAGE_LIMIT)
        .unwrap();
    let serialized_cursor = serde_json::to_vec(first.next_cursor.as_ref().unwrap()).unwrap();
    drop(initial);

    let reopened = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let cursor: SourceEventCursor = serde_json::from_slice(&serialized_cursor).unwrap();
    crate::query::reset_stored_core_event_record_materializations();
    let second = reopened
        .core_source_event_page(&source, Some(&cursor), PAGE_LIMIT)
        .unwrap();
    let materializations = crate::query::stored_core_event_record_materializations();
    let segment_bound = (PAGE_LIMIT + 1) * reopened.searcher.segment_readers().len();
    assert!(
        materializations <= segment_bound,
        "stored Core materializations {materializations} exceeded page/lookahead × segment bound {segment_bound}"
    );
    assert!(
        materializations < EVENT_COUNT as usize / 2,
        "a second page materialized the large remaining source"
    );
    assert!(!second.terminal);
    assert_eq!(
        second
            .items
            .iter()
            .map(|record| record.event_id)
            .collect::<Vec<_>>(),
        expected[PAGE_LIMIT..PAGE_LIMIT * 2]
    );
    assert_eq!(
        second.next_cursor.as_ref().map(SourceEventCursor::after),
        Some(expected[PAGE_LIMIT * 2 - 1])
    );

    crate::query::reset_stored_core_event_record_materializations();
    let legacy = reopened
        .source_event_page(&source, Some(&cursor), PAGE_LIMIT)
        .unwrap();
    assert_eq!(
        crate::query::stored_core_event_record_materializations(),
        0,
        "metadata-only source pages must not decode selected or lookahead Core bodies"
    );
    assert_eq!(legacy.generation_id, second.generation_id);
    assert!(legacy.source.exact_descriptor_eq(&second.source));
    assert_eq!(legacy.terminal, second.terminal);
    assert_eq!(
        legacy.next_cursor.as_ref().map(SourceEventCursor::after),
        second.next_cursor.as_ref().map(SourceEventCursor::after)
    );
    assert_eq!(
        legacy.items,
        second
            .items
            .into_iter()
            .map(|record| record.event)
            .collect::<Vec<_>>()
    );
}

#[test]
fn sparse_source_pages_visit_only_exact_source_terms_across_segments_and_reopen() {
    const UNRELATED_SOURCES: u64 = 32;
    const EVENTS_PER_UNRELATED_SOURCE: u64 = 32;
    const PAGE_LIMIT: usize = 2;

    let temp = tempdir().unwrap();
    let target = source("sparse-target.jsonl");
    let mut expected = Vec::new();
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer
        .writer_mut()
        .unwrap()
        .set_merge_policy(Box::<NoMergePolicy>::default());
    writer.begin_source(target.clone()).unwrap();
    for sequence in 1..=2 {
        let event = document(&target, sequence, "sparse target first segment");
        expected.push(event.event_id);
        writer.add_core_record(event).unwrap();
    }
    writer
        .certify_source(appendable_certificate(&target, 1, 2, 20))
        .unwrap();
    for source_index in 0..UNRELATED_SOURCES {
        let unrelated = source(&format!("unrelated-{source_index}.jsonl"));
        writer.begin_source(unrelated.clone()).unwrap();
        for sequence in 1..=EVENTS_PER_UNRELATED_SOURCE {
            writer
                .add_core_record(document(&unrelated, sequence, "unrelated corpus event"))
                .unwrap();
        }
        writer
            .certify_source(certificate(&unrelated, 1, EVENTS_PER_UNRELATED_SOURCE))
            .unwrap();
    }
    writer.commit(|_| true).unwrap();

    let mut append = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    append
        .writer_mut()
        .unwrap()
        .set_merge_policy(Box::<NoMergePolicy>::default());
    let base = append.begin_source_append(target.clone()).unwrap().clone();
    for sequence in 3..=4 {
        let event = document(&target, sequence, "sparse target second segment");
        expected.push(event.event_id);
        append.add_core_record(event).unwrap();
    }
    append
        .certify_source_append(
            CertifiedSourceAppend::certify(
                &base,
                appendable_certificate(&target, 2, 4, 40),
                20,
                [1; 32],
            )
            .unwrap(),
        )
        .unwrap();
    append.commit(|_| true).unwrap();
    expected.sort_by_key(|identity| identity.encode_canonical().unwrap());

    let first_pin = VerifiedIndex::open(temp.path()).unwrap();
    let segment_count = first_pin.searcher.segment_readers().len();
    assert!(segment_count >= 2, "test requires multiple live segments");
    crate::query::reset_source_event_order_term_visits();
    crate::query::reset_stored_core_event_record_materializations();
    let first = first_pin
        .core_source_event_page(&target, None, PAGE_LIMIT)
        .unwrap();
    let first_term_visits = crate::query::source_event_order_term_visits();
    assert!(
        first_term_visits <= (PAGE_LIMIT + 1) * segment_count,
        "exact-source term visits {first_term_visits} exceeded page/lookahead × segments"
    );
    assert_eq!(
        crate::query::stored_core_event_record_materializations(),
        PAGE_LIMIT
    );
    assert!(
        first_term_visits < (UNRELATED_SOURCES * EVENTS_PER_UNRELATED_SOURCE) as usize / 8,
        "sparse-source page work followed the unrelated corpus"
    );
    assert_eq!(
        first
            .items
            .iter()
            .map(|record| record.event_id)
            .collect::<Vec<_>>(),
        expected[..PAGE_LIMIT]
    );
    let serialized_cursor = serde_json::to_vec(first.next_cursor.as_ref().unwrap()).unwrap();
    drop(first_pin);

    let reopened = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let cursor: SourceEventCursor = serde_json::from_slice(&serialized_cursor).unwrap();
    crate::query::reset_source_event_order_term_visits();
    crate::query::reset_stored_core_event_record_materializations();
    let second = reopened
        .core_source_event_page(&target, Some(&cursor), PAGE_LIMIT)
        .unwrap();
    assert!(second.terminal);
    assert_eq!(
        second
            .items
            .iter()
            .map(|record| record.event_id)
            .collect::<Vec<_>>(),
        expected[PAGE_LIMIT..]
    );
    assert!(crate::query::source_event_order_term_visits() <= (PAGE_LIMIT + 1) * segment_count);
    assert_eq!(
        crate::query::stored_core_event_record_materializations(),
        PAGE_LIMIT
    );
}

#[test]
fn source_event_byte_budget_returns_large_singletons_without_skips_or_extra_decodes() {
    let temp = tempdir().unwrap();
    let source = source("large-source-page.jsonl");
    let maximum_body = format!(
        "x{}",
        " ".repeat(ctx_history_core::MAX_CORE_CONTENT_BYTES - 1)
    );
    let large_body = format!("x{}", " ".repeat(1024 * 1024 - 1));
    let bodies = [
        maximum_body.as_str(),
        large_body.as_str(),
        large_body.as_str(),
    ];
    let mut expected_content_bytes = HashMap::new();
    let mut expected_ids = Vec::new();
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    for (index, body) in bodies.into_iter().enumerate() {
        let event = document(&source, index as u64 + 1, body);
        expected_content_bytes.insert(event.event_id, body.len());
        expected_ids.push(event.event_id);
        writer.add_core_record(event).unwrap();
    }
    writer.certify_source(certificate(&source, 1, 3)).unwrap();
    writer.commit(|_| true).unwrap();
    expected_ids.sort_by_key(|identity| identity.encode_canonical().unwrap());

    let index = VerifiedIndex::open(temp.path()).unwrap();
    let budget = CoreEventPageBudget::new(1, 1);
    let mut cursor = None;
    let mut actual_ids = Vec::new();
    loop {
        crate::query::reset_stored_core_event_record_materializations();
        let page = index
            .core_source_event_page_with_budget(
                &source,
                cursor.as_ref(),
                MAX_SOURCE_EVENT_PAGE_ITEMS,
                budget,
            )
            .unwrap();
        assert_eq!(page.items.len(), 1, "oversized valid records must progress");
        assert_eq!(
            crate::query::stored_core_event_record_materializations(),
            1,
            "size suffixes must stop before decoding the next record"
        );
        let event_id = page.items[0].event_id;
        assert_eq!(
            page.content_bytes,
            *expected_content_bytes.get(&event_id).unwrap()
        );
        assert!(page.content_bytes <= ctx_history_core::MAX_CORE_CONTENT_BYTES);
        assert!(page.encoded_core_bytes <= ctx_history_core::MAX_ENCODED_CORE_RECORD_BYTES);
        actual_ids.push(event_id);
        if page.terminal {
            assert!(page.next_cursor.is_none());
            break;
        }
        cursor = page.next_cursor;
    }
    assert_eq!(actual_ids, expected_ids);
}

#[test]
fn source_event_size_suffix_counts_structured_core_content() {
    let temp = tempdir().unwrap();
    let source = source("structured-source-page.jsonl");
    let event = document(&source, 1, "normalized body");
    let structured_content = serde_json::json!({
        "command": "cargo test",
        "output": ["first", "second", "third"],
    });
    let expected_content_bytes = event.content.normalized_body.as_ref().unwrap().len()
        + serde_json::to_vec(&structured_content).unwrap().len();
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer
        .add_core_record(with_annotation(
            event,
            CoreRecordAnnotation {
                structured_content: Some(structured_content),
                ..CoreRecordAnnotation::default()
            },
        ))
        .unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    let page = index.core_source_event_page(&source, None, 1).unwrap();
    assert!(page.terminal);
    assert_eq!(page.content_bytes, expected_content_bytes);
    assert_eq!(
        crate::index_document::core_content_bytes(&page.items[0].core_record.content).unwrap(),
        expected_content_bytes
    );
}

#[test]
fn source_event_page_rejects_a_forged_order_size_suffix_before_returning_record() {
    let temp = tempdir().unwrap();
    let source = source("forged-source-order.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer
        .add_core_record(document(&source, 1, "forged order body"))
        .unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();

    let (searcher, manifest) = open_unverified_generation(temp.path());
    let fields = fields_from_schema(searcher.schema()).unwrap();
    let address = searcher
        .search(&AllQuery, &DocSetCollector)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let mut forged: TantivyDocument = searcher.doc(address).unwrap();
    let encoded_core = forged
        .get_first(fields.core_record)
        .and_then(|value| value.as_bytes())
        .unwrap()
        .to_vec();
    let core_record = ctx_history_core::CoreRecord::decode_stored(&encoded_core).unwrap();
    let mut forged_order = crate::index_document::SourceEventOrderKey::for_core_record(
        &core_record,
        encoded_core.len(),
    )
    .unwrap()
    .into_bytes();
    let last = forged_order.last_mut().unwrap();
    *last ^= 1;
    forged.add_bytes(fields.source_event_order, &forged_order);
    drop(searcher);

    let directory = DurableMmapDirectory::open(active_generation_path(temp.path())).unwrap();
    let index = Index::open(directory).unwrap();
    publish_unchecked_generation(
        temp.path(),
        &index,
        manifest,
        std::slice::from_ref(&source),
        vec![forged],
    );
    let verified = VerifiedIndex::open(temp.path()).unwrap();
    assert!(matches!(
        verified.core_source_event_page(&source, None, 1),
        Err(IndexError::InvalidStoredDocumentField("source_event_order"))
    ));
}

#[test]
fn source_event_pages_bind_generation_descriptor_and_bounds() {
    const { assert!(MAX_SOURCE_EVENT_PAGE_ITEMS <= 4_096) };
    let temp = tempdir().unwrap();
    let source = source("rewrite-delete-pages.jsonl");
    let old_first = document(&source, 1, "old first");
    let old_second = document(&source, 2, "old second");
    let mut first = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    first.begin_source(source.clone()).unwrap();
    first.add_core_record(old_second.clone()).unwrap();
    first.add_core_record(old_first.clone()).unwrap();
    first.certify_source(certificate(&source, 1, 2)).unwrap();
    first.commit(|_| true).unwrap();
    let old_pin = VerifiedIndex::open(temp.path()).unwrap();
    let old_cursor = old_pin
        .source_event_page(&source, None, 1)
        .unwrap()
        .next_cursor
        .unwrap();

    assert!(matches!(
        old_pin.source_event_page(&source, None, 0),
        Err(IndexError::InvalidSourceEventPageSize { .. })
    ));
    assert!(matches!(
        old_pin.source_event_page(&source, None, MAX_SOURCE_EVENT_PAGE_ITEMS + 1),
        Err(IndexError::InvalidSourceEventPageSize { .. })
    ));
    assert!(matches!(
        old_pin.core_source_event_page_with_budget(
            &source,
            None,
            1,
            CoreEventPageBudget::new(0, 1),
        ),
        Err(IndexError::InvalidCoreEventPageByteLimit { .. })
    ));
    assert!(matches!(
        old_pin.core_source_event_page_with_budget(
            &source,
            None,
            1,
            CoreEventPageBudget::new(
                ctx_history_core::MAX_ENCODED_CORE_RECORD_BYTES + 1,
                ctx_history_core::MAX_CORE_CONTENT_BYTES,
            ),
        ),
        Err(IndexError::InvalidCoreEventPageByteLimit { .. })
    ));
    assert!(
        old_pin
            .source_event_page(&source, None, MAX_SOURCE_EVENT_PAGE_ITEMS)
            .unwrap()
            .terminal
    );

    let mut rewritten_first = document(&source, 1, "rewritten first");
    rewritten_first.workspace = Some("rewritten".to_owned());
    let replacement = document(&source, 3, "replacement");
    let mut rewriting = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    rewriting.begin_source(source.clone()).unwrap();
    rewriting.add_core_record(replacement.clone()).unwrap();
    rewriting.add_core_record(rewritten_first.clone()).unwrap();
    rewriting
        .certify_source(certificate(&source, 2, 2))
        .unwrap();
    rewriting.commit(|_| true).unwrap();
    let rewritten_pin = VerifiedIndex::open(temp.path()).unwrap();

    assert!(matches!(
        rewritten_pin.source_event_page(&source, Some(&old_cursor), 1),
        Err(IndexError::SourceEventCursorGenerationMismatch { .. })
    ));
    let rewritten = collect_source_pages(&rewritten_pin, &source, 1);
    assert_eq!(rewritten.len(), 2);
    assert!(rewritten.iter().any(|event| {
        event.event_id == rewritten_first.event_id
            && event.workspace.as_deref() == Some("rewritten")
    }));
    assert!(rewritten
        .iter()
        .any(|event| event.event_id == replacement.event_id));
    assert!(rewritten
        .iter()
        .all(|event| event.event_id != old_second.event_id));
    let old = collect_source_pages(&old_pin, &source, 1);
    assert_eq!(old.len(), 2);
    assert!(old.iter().any(|event| event.event_id == old_first.event_id));

    let changed_descriptor = source_for_provider(
        "codex",
        "codex_prompt_history_jsonl",
        "rewrite-delete-pages.jsonl",
    );
    assert_eq!(changed_descriptor, source);
    assert!(!changed_descriptor.exact_descriptor_eq(&source));
    assert!(matches!(
        rewritten_pin.source_event_page(&changed_descriptor, None, 1),
        Err(IndexError::SourceEventSourceDescriptorMismatch(_))
    ));

    let (deletion, inventory) = deletion_evidence(&source, 3);
    let mut deleting = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    deleting.delete_source(deletion, inventory).unwrap();
    deleting.commit(|_| true).unwrap();
    let deleted_pin = VerifiedIndex::open(temp.path()).unwrap();
    assert!(matches!(
        deleted_pin.source_event_page(&source, Some(&old_cursor), 1),
        Err(IndexError::SourceEventCursorGenerationMismatch { .. })
    ));
    assert!(matches!(
        deleted_pin.source_event_page(&source, None, 1),
        Err(IndexError::SourceEventSourceNotRetained(_))
    ));
    assert_eq!(collect_source_pages(&old_pin, &source, 1).len(), 2);
    assert_eq!(collect_source_pages(&rewritten_pin, &source, 1).len(), 2);
}
