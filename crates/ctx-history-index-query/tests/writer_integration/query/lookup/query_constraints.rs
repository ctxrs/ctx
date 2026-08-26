#[test]
fn custom_source_filters_use_the_core_native_event_identity() {
    let temp = tempdir().unwrap();
    let source = SourceKey::derive(
        "custom",
        "ctx_history_jsonl",
        "catalog",
        1,
        SourceAnchor::CatalogLineage([42; 32]),
    )
    .unwrap();
    let mut record = document(&source, 1, "custom identity needle");
    let native_event_id = TypedKey::composite(vec![
        TypedKey::utf8("fixture-provider").unwrap(),
        TypedKey::utf8("fixture-source").unwrap(),
        TypedKey::utf8("fixture-session").unwrap(),
    ])
    .unwrap();
    record.native_event_id = Some(native_event_id.clone());
    assert_eq!(record.native_event_id.as_ref(), Some(&native_event_id));

    let event_id = record.event_id;
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(record).unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    let filters = EventSearchFilters {
        history_source: Some("fixture-provider/fixture-source".to_owned()),
        provider_key: Some("fixture-provider".to_owned()),
        source_id: Some("fixture-source".to_owned()),
        ..EventSearchFilters::default()
    };
    ctx_history_index_query::reset_stored_core_event_record_materializations();
    let hits = index
        .search_event_candidates_with_filters("identity", &filters, 10)
        .unwrap();
    assert_eq!(
        hits.iter()
            .map(|candidate| candidate.event.event_id)
            .collect::<Vec<_>>(),
        vec![event_id]
    );
    assert_eq!(
        hits[0].event.native_event_id.as_ref(),
        Some(&native_event_id)
    );
    assert!(filters.matches_source_identity(&hits[0].event));
    let listed = index
        .list_event_candidates_with_filters_batch(&filters, 10)
        .unwrap();
    assert!(listed.complete);
    assert!(listed.candidate_set_exhaustive);
    assert_eq!(listed.candidates.len(), 1);
    assert_eq!(listed.candidates[0].event.event_id, event_id);
    let semantic = index.semantic_filter_projection(&filters).unwrap();
    assert_eq!(
        semantic.event_ids().collect::<Vec<_>>(),
        vec![event_id.as_uuid()]
    );
    assert_eq!(
        ctx_history_index_query::stored_core_event_record_materializations(),
        0,
        "custom source filtering must use bounded indexed identity metadata"
    );

    let misses = index
        .search_event_candidates_with_filters(
            "identity",
            &EventSearchFilters {
                source_id: Some("another-source".to_owned()),
                ..EventSearchFilters::default()
            },
            10,
        )
        .unwrap();
    assert!(misses.is_empty());
}

#[test]
fn allowed_source_keys_select_exact_physical_sources_in_one_index() {
    let temp = tempdir().unwrap();
    let personal = source("personal-root.jsonl");
    let work = source("work-root.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    for (sequence, source) in [(1, &personal), (2, &work)] {
        writer.begin_source(source.clone()).unwrap();
        writer
            .add_core_record(document(source, sequence, "shared root needle"))
            .unwrap();
        writer
            .certify_source(certificate(source, sequence as u8, 1))
            .unwrap();
    }
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    let personal_only = EventSearchFilters {
        allowed_source_keys: Some(vec![source_token(&personal)]),
        ..EventSearchFilters::default()
    };
    let hits = index
        .search_event_candidates_with_filters("needle", &personal_only, 10)
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].event.source.exact_descriptor_eq(&personal));

    let none = index
        .search_event_candidates_with_filters(
            "needle",
            &EventSearchFilters {
                allowed_source_keys: Some(Vec::new()),
                ..EventSearchFilters::default()
            },
            10,
        )
        .unwrap();
    assert!(none.is_empty());
}

#[test]
fn bounded_core_event_batch_stops_after_one_large_record_exceeds_byte_budget() {
    let temp = tempdir().unwrap();
    let source = source("large-bounded-event-batch.jsonl");
    let large_body = "large-core-body".repeat(128 * 1024);
    let documents = (1..=3)
        .map(|sequence| document(&source, sequence, &large_body))
        .collect::<Vec<_>>();
    let requested = documents
        .iter()
        .map(|document| document.event_id.as_uuid())
        .collect::<Vec<_>>();
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for document in documents {
        writer.add_core_record(document).unwrap();
    }
    writer.certify_source(certificate(&source, 1, 3)).unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    ctx_history_index_query::reset_stored_core_event_record_materializations();
    assert!(index
        .core_events_by_ids_if_bounded(&requested, requested.len(), 1)
        .unwrap()
        .is_none());
    assert_eq!(
        ctx_history_index_query::stored_core_event_record_materializations(),
        1,
        "byte-budget refusal must stop before decoding the remaining large records"
    );
}

#[test]
fn session_event_budget_declines_before_materializing_an_oversized_session() {
    let temp = tempdir().unwrap();
    let source = source("bounded-session.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for sequence in 1..=3 {
        writer
            .add_core_record(document(&source, sequence, "bounded body"))
            .unwrap();
    }
    writer.certify_source(certificate(&source, 1, 3)).unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    let session_id = document(&source, 1, "bounded body").session_id.as_uuid();
    ctx_history_index_query::reset_stored_event_record_materializations();
    ctx_history_index_query::reset_stored_core_event_record_materializations();
    assert!(index
        .events_for_session_if_bounded(session_id, 2)
        .unwrap()
        .is_none());
    assert!(index
        .core_events_for_session_if_bounded(session_id, 2)
        .unwrap()
        .is_none());
    assert_eq!(
        ctx_history_index_query::stored_event_record_materializations(),
        0
    );
    assert_eq!(
        ctx_history_index_query::stored_core_event_record_materializations(),
        0
    );
    assert_eq!(
        index
            .events_for_session_if_bounded(session_id, 3)
            .unwrap()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        ctx_history_index_query::stored_event_record_materializations(),
        3
    );
    let core = index
        .core_events_for_session_if_bounded(session_id, 3)
        .unwrap()
        .unwrap();
    assert_eq!(core.len(), 3);
    assert_eq!(
        ctx_history_index_query::stored_core_event_record_materializations(),
        3
    );
    assert!(core.iter().all(|record| {
        record.core_record.content.normalized_body.as_deref() == Some("bounded body")
    }));
}
