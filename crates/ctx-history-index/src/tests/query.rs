use super::*;

#[test]
fn pinned_query_api_returns_typed_records_in_deterministic_order() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let first = document(&source, 1, "atomic generation");
    let second = document(&source, 2, "atomic generation");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(second.clone()).unwrap();
    writer.add_core_record(first.clone()).unwrap();
    writer.certify_source(certificate(&source, 1, 2)).unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    let candidates = index
        .search_event_candidates("atomic:generation", 10)
        .unwrap();
    let mut expected_search_ids = vec![first.event_id.as_uuid(), second.event_id.as_uuid()];
    expected_search_ids.sort();
    assert_eq!(
        candidates
            .iter()
            .map(|candidate| candidate.event.event_id.as_uuid())
            .collect::<Vec<_>>(),
        expected_search_ids
    );
    assert_eq!(candidates[0].score, candidates[1].score);

    let exact = index
        .event_by_id(first.event_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(exact.event_id, first.event_id);
    assert_eq!(exact.session_id, first.session_id);
    assert!(exact.source.exact_descriptor_eq(&first.source));
    assert_eq!(exact.provider, "codex");
    assert_eq!(exact.source_format, "codex_session_jsonl");
    assert_eq!(exact.provider_session_id.as_deref(), Some("session"));
    assert_eq!(exact.event_sequence, 1);
    assert_eq!(exact.occurred_at_unix_ms, first.occurred_at_unix_ms);
    assert_eq!(exact.event_type, "message");
    assert_eq!(exact.role.as_deref(), Some("user"));
    assert_eq!(exact.workspace.as_deref(), Some("ctx"));
    assert_eq!(exact.cwd.as_deref(), Some("/work/ctx"));
    assert!(exact.touched_files.is_empty());

    let event_id = first.event_id.to_string();
    let event_prefix = &event_id[..8];
    assert_eq!(
        index.events_by_id_prefix(event_prefix).unwrap()[0].event_id,
        first.event_id
    );

    let ordered = index
        .events_for_session(first.session_id.as_uuid())
        .unwrap();
    assert_eq!(
        ordered
            .iter()
            .map(|event| event.event_sequence)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    let core_ordered = index
        .core_events_for_session(first.session_id.as_uuid())
        .unwrap();
    assert_eq!(
        core_ordered
            .iter()
            .map(|record| record.event_id)
            .collect::<Vec<_>>(),
        ordered
            .iter()
            .map(|event| event.event_id)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        core_ordered[0]
            .core_record
            .content
            .normalized_body
            .as_deref(),
        Some("atomic generation")
    );
    let session = index
        .session_by_id(first.session_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(session.session_id, first.session_id);
    assert_eq!(session.provider, "codex");
    assert_eq!(session.source_format, "codex_session_jsonl");
    assert_eq!(session.provider_session_id.as_deref(), Some("session"));
    assert_eq!(session.first_event_sequence, 1);

    let session_id = first.session_id.to_string();
    let session_prefix = &session_id[..8];
    assert_eq!(
        index.sessions_by_id_prefix(session_prefix).unwrap(),
        vec![session]
    );
}

#[test]
fn core_valid_escape_heavy_query_metadata_indexes_without_a_narrower_json_bound() {
    const ESCAPED_FIELD_BYTES: usize = 17 * 1024;
    const NATIVE_ID_BYTES: usize = 60 * 1024;

    let temp = tempdir().unwrap();
    let source = source("escape-heavy-query-metadata.jsonl");
    let mut record = document(&source, 1, "small searchable body");
    let escaped = "\u{0001}".repeat(ESCAPED_FIELD_BYTES);
    record.provider_session_id = Some(escaped.clone());
    record.branch = Some(escaped.clone());
    record.agent_type = escaped.clone();
    record.event_type = escaped.clone();
    record.role = Some(escaped.clone());
    record.workspace = Some(escaped.clone());
    record.cwd = Some(escaped);
    record.native_event_id = Some(TypedKey::utf8("\u{0002}".repeat(NATIVE_ID_BYTES)).unwrap());
    record.validate_contract().unwrap();
    let encoded_core = record.encode_stored().unwrap();
    let query_metadata =
        crate::index_document::StoredQueryMetadata::encode(&record, encoded_core.len()).unwrap();
    assert!(query_metadata.len() > 1024 * 1024);
    assert!(query_metadata.len() <= encoded_core.len());

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(record.clone()).unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    let indexed = index
        .event_by_id(record.event_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(indexed.provider_session_id, record.provider_session_id);
    assert_eq!(indexed.native_event_id, record.native_event_id);
    assert_eq!(indexed.cwd, record.cwd);
}

#[test]
fn semantic_pairing_many_user_turns_uses_bounded_direct_session_pages() {
    const TURNS: u64 = 256;
    const PAIRING_PAGE_ITEMS: usize = 4;

    let temp = tempdir().unwrap();
    let source = source("many-semantic-turns.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    for turn in 0..TURNS {
        let user_sequence = turn * 2 + 1;
        writer
            .add_core_record(document(
                &source,
                user_sequence,
                &format!("question {turn}"),
            ))
            .unwrap();
        let mut assistant = document(&source, user_sequence + 1, &format!("answer {turn}"));
        assistant.role = Some("assistant".to_owned());
        writer.add_core_record(assistant).unwrap();
    }
    writer
        .certify_source(certificate(&source, 1, TURNS * 2))
        .unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    let anchors = index
        .core_events_for_session(document(&source, 1, "anchor").session_id.as_uuid())
        .unwrap()
        .into_iter()
        .filter(|record| record.role.as_deref() == Some("user"))
        .collect::<Vec<_>>();
    assert_eq!(anchors.len(), TURNS as usize);
    crate::query::reset_session_event_order_term_visits();
    for anchor in &anchors {
        let turn = (anchor.event_sequence - 1) / 2;
        let paired = index
            .semantic_lite_turn_assistant(
                anchor,
                PAIRING_PAGE_ITEMS,
                DEFAULT_CORE_EVENT_PAGE_BUDGET,
            )
            .unwrap()
            .unwrap();
        assert_eq!(paired.0, format!("answer {turn}"));
    }
    let term_visits = crate::query::session_event_order_term_visits();
    assert!(
        term_visits <= TURNS as usize * PAIRING_PAGE_ITEMS * crate::LEXICAL_SEGMENT_MERGE_FAN_IN,
        "direct pairing term visits must stay linear in user turns: {term_visits}"
    );
    assert!(term_visits < (TURNS * TURNS) as usize);
}

#[test]
fn semantic_pairing_crosses_more_than_sixty_four_tool_events_body_free() {
    const TOOL_EVENTS: u64 = 96;

    let temp = tempdir().unwrap();
    let source = source("tool-heavy-semantic-turn.jsonl");
    let user = document(&source, 1, "tool-heavy question");
    let mut assistant = document(&source, TOOL_EVENTS + 2, "answer beyond old window");
    assistant.role = Some("assistant".to_owned());
    let next_user = document(&source, TOOL_EVENTS + 3, "next question");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(user.clone()).unwrap();
    for sequence in 2..=TOOL_EVENTS + 1 {
        let mut tool = document(&source, sequence, "large tool body is not hydrated");
        tool.event_type = "tool_output".to_owned();
        tool.role = Some("tool".to_owned());
        writer.add_core_record(tool).unwrap();
    }
    writer.add_core_record(assistant.clone()).unwrap();
    writer.add_core_record(next_user).unwrap();
    writer
        .certify_source(certificate(&source, 1, TOOL_EVENTS + 3))
        .unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    let anchor = index
        .core_event_by_id(user.event_id.as_uuid())
        .unwrap()
        .unwrap();
    crate::query::reset_stored_core_event_record_materializations();
    crate::query::reset_session_event_order_term_visits();
    let paired = index
        .semantic_lite_turn_assistant(&anchor, 64, DEFAULT_CORE_EVENT_PAGE_BUDGET)
        .unwrap()
        .unwrap();

    assert_eq!(paired.0, "answer beyond old window");
    assert_eq!(paired.1, assistant.occurred_at_unix_ms.unwrap());
    assert_eq!(
        crate::query::stored_core_event_record_materializations(),
        1,
        "tool metadata traversal must hydrate only the paired assistant body"
    );
    let term_visits = crate::query::session_event_order_term_visits();
    assert!(term_visits > 64);
    assert!(
        term_visits <= 2 * 64 * crate::LEXICAL_SEGMENT_MERGE_FAN_IN,
        "tool-heavy pairing must remain page bounded: {term_visits}"
    );
}

#[test]
fn metadata_hot_paths_and_ambiguity_collectors_are_body_free_and_bounded() {
    const EVENT_COUNT: u64 = 64;
    const AMBIGUITY_LIMIT: usize = 2;

    let temp = tempdir().unwrap();
    let source = source("metadata-hot-paths.jsonl");
    let mut event_ids = Vec::new();
    let mut session_ids = Vec::new();
    let mut bodies_by_session = std::collections::BTreeMap::new();
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    for sequence in 1..=EVENT_COUNT {
        let body = format!("ambiguity needle {sequence}");
        let mut event = document_for_session(
            &source,
            &format!("bounded-session-{sequence}"),
            sequence,
            &body,
        );
        event.provider_session_id = Some("shared-provider-session".to_owned());
        event_ids.push(event.event_id.as_uuid());
        session_ids.push(event.session_id.as_uuid());
        bodies_by_session.insert(event.session_id.as_uuid(), body.len());
        writer.add_core_record(event).unwrap();
    }
    writer
        .certify_source(certificate(&source, 1, EVENT_COUNT))
        .unwrap();
    writer.commit(|_| true).unwrap();

    crate::query::reset_stored_event_record_materializations();
    crate::query::reset_stored_core_event_record_materializations();
    let index = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(crate::query::stored_core_event_record_materializations(), 0);

    session_ids.sort();
    session_ids.dedup();
    crate::query::reset_stored_event_record_materializations();
    let provider_sessions = index
        .sessions_by_provider_session_id("shared-provider-session", Some("codex"))
        .unwrap();
    assert_eq!(provider_sessions.len(), AMBIGUITY_LIMIT);
    assert_eq!(
        provider_sessions
            .iter()
            .map(|session| session.session_id.as_uuid())
            .collect::<Vec<_>>(),
        session_ids[..AMBIGUITY_LIMIT]
    );
    assert_eq!(
        crate::query::stored_event_record_materializations(),
        AMBIGUITY_LIMIT,
        "provider-session ambiguity lookup must decode only one metadata record per retained session"
    );

    let session_prefix = session_ids
        .iter()
        .fold(
            std::collections::BTreeMap::<char, Vec<Uuid>>::new(),
            |mut groups, id| {
                groups
                    .entry(id.to_string().chars().next().unwrap())
                    .or_default()
                    .push(*id);
                groups
            },
        )
        .into_iter()
        .find(|(_, ids)| ids.len() > AMBIGUITY_LIMIT)
        .unwrap();
    crate::query::reset_stored_event_record_materializations();
    let prefix_sessions = index
        .sessions_by_id_prefix(&session_prefix.0.to_string())
        .unwrap();
    assert_eq!(
        prefix_sessions
            .iter()
            .map(|session| session.session_id.as_uuid())
            .collect::<Vec<_>>(),
        session_prefix.1[..AMBIGUITY_LIMIT]
    );
    assert_eq!(
        crate::query::stored_event_record_materializations(),
        AMBIGUITY_LIMIT
    );

    event_ids.sort();
    let event_prefix = event_ids
        .iter()
        .fold(
            std::collections::BTreeMap::<char, Vec<Uuid>>::new(),
            |mut groups, id| {
                groups
                    .entry(id.to_string().chars().next().unwrap())
                    .or_default()
                    .push(*id);
                groups
            },
        )
        .into_iter()
        .find(|(_, ids)| ids.len() > AMBIGUITY_LIMIT)
        .unwrap();
    crate::query::reset_stored_event_record_materializations();
    let prefix_events = index
        .events_by_id_prefix(&event_prefix.0.to_string())
        .unwrap();
    assert_eq!(
        prefix_events
            .iter()
            .map(|event| event.event_id.as_uuid())
            .collect::<Vec<_>>(),
        event_prefix.1[..AMBIGUITY_LIMIT]
    );
    assert_eq!(
        crate::query::stored_event_record_materializations(),
        AMBIGUITY_LIMIT
    );

    crate::query::reset_stored_event_record_materializations();
    let candidates = index.search_event_candidates("ambiguity", 5).unwrap();
    assert_eq!(candidates.len(), 5);
    assert_eq!(crate::query::stored_event_record_materializations(), 5);
    assert_eq!(crate::query::stored_core_event_record_materializations(), 0);

    crate::query::reset_stored_event_record_materializations();
    let source_page = index.source_event_page(&source, None, 5).unwrap();
    assert_eq!(source_page.items.len(), 5);
    assert_eq!(crate::query::stored_event_record_materializations(), 5);
    assert_eq!(crate::query::stored_core_event_record_materializations(), 0);

    crate::query::reset_stored_event_record_materializations();
    let semantic_page = index.semantic_event_page(None, 5).unwrap();
    assert_eq!(semantic_page.items.len(), 5);
    assert_eq!(crate::query::stored_event_record_materializations(), 5);
    assert_eq!(crate::query::stored_core_event_record_materializations(), 0);

    crate::query::reset_stored_event_record_materializations();
    let session_id = session_ids[0];
    assert_eq!(
        index
            .core_content_bytes_for_session_if_bounded(session_id, 1)
            .unwrap(),
        Some(bodies_by_session[&session_id])
    );
    assert_eq!(crate::query::stored_event_record_materializations(), 0);
    assert_eq!(crate::query::stored_core_event_record_materializations(), 0);
}

#[test]
fn metadata_lookup_does_not_read_or_validate_the_stored_core_body() {
    use tantivy::schema::Document as _;

    let temp = tempdir().unwrap();
    let source = source("body-free-metadata.jsonl");
    let event = document(&source, 1, "metadata survives an unreadable Core body");
    let event_id = event.event_id;
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(event).unwrap();
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
    let original: TantivyDocument = searcher.doc(address).unwrap();
    let mut malformed = TantivyDocument::default();
    for (field, value) in original.iter_fields_and_values() {
        if field != fields.core_record {
            malformed.add_field_value(field, value);
        }
    }
    malformed.add_bytes(fields.core_record, b"{");
    drop(searcher);

    let directory = DurableMmapDirectory::open(temp.path()).unwrap();
    let index = Index::open(directory).unwrap();
    publish_unchecked_generation(
        temp.path(),
        &index,
        manifest,
        std::slice::from_ref(&source),
        vec![malformed],
    );

    crate::query::reset_stored_core_event_record_materializations();
    let verified = VerifiedIndex::open(temp.path()).unwrap();
    let metadata = verified.event_by_id(event_id.as_uuid()).unwrap().unwrap();
    assert_eq!(metadata.event_id, event_id);
    assert_eq!(
        crate::query::stored_core_event_record_materializations(),
        0,
        "generation verification and metadata lookup must not touch Core bodies"
    );
    assert!(verified.core_event_by_id(event_id.as_uuid()).is_err());
    assert_eq!(crate::query::stored_core_event_record_materializations(), 1);
}

#[test]
fn bounded_core_event_batch_is_complete_and_requested_ordered() {
    let temp = tempdir().unwrap();
    let source = source("bounded-event-batch.jsonl");
    let first = document(&source, 1, "first complete body");
    let second = document(&source, 2, "second complete body");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(first.clone()).unwrap();
    writer.add_core_record(second.clone()).unwrap();
    writer.certify_source(certificate(&source, 1, 2)).unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    crate::query::reset_stored_core_event_record_materializations();
    let coordinates = index
        .session_event_coordinates(first.session_id.as_uuid())
        .unwrap();
    assert_eq!(
        coordinates
            .iter()
            .map(|coordinate| coordinate.event_id)
            .collect::<Vec<_>>(),
        vec![first.event_id.as_uuid(), second.event_id.as_uuid()]
    );
    assert_eq!(
        crate::query::stored_core_event_record_materializations(),
        0,
        "session selection metadata must not decode complete Core bodies"
    );

    let requested = [second.event_id.as_uuid(), first.event_id.as_uuid()];
    crate::query::reset_stored_core_event_record_materializations();
    let bounded_batch = index
        .core_events_by_ids_with_budget(&requested, requested.len(), DEFAULT_CORE_EVENT_PAGE_BUDGET)
        .unwrap()
        .unwrap();
    assert_eq!(
        bounded_batch
            .items
            .iter()
            .map(|record| record.event_id.as_uuid())
            .collect::<Vec<_>>(),
        requested
    );
    assert!(bounded_batch.encoded_core_bytes >= bounded_batch.content_bytes);
    assert_eq!(
        bounded_batch.content_bytes,
        "first complete body".len() + "second complete body".len()
    );

    crate::query::reset_stored_core_event_record_materializations();
    let records = index
        .core_events_by_ids_if_bounded(&requested, requested.len(), usize::MAX)
        .unwrap()
        .unwrap();
    assert_eq!(
        records
            .iter()
            .map(|record| record.event_id.as_uuid())
            .collect::<Vec<_>>(),
        requested
    );
    assert_eq!(
        records
            .iter()
            .map(|record| record.core_record.content.meaningful_text())
            .collect::<Vec<_>>(),
        vec!["second complete body", "first complete body",]
    );
    assert_eq!(
        crate::query::stored_core_event_record_materializations(),
        2,
        "each requested event must materialize exactly one stored Core document"
    );

    crate::query::reset_stored_core_event_record_materializations();
    assert!(index
        .core_events_by_ids_if_bounded(&requested, requested.len() - 1, usize::MAX)
        .unwrap()
        .is_none());
    assert_eq!(
        crate::query::stored_core_event_record_materializations(),
        0,
        "an oversized request must be declined before querying stored documents"
    );

    crate::query::reset_stored_core_event_record_materializations();
    assert!(matches!(
        index.core_events_by_ids_if_bounded(
            &[first.event_id.as_uuid(), first.event_id.as_uuid()],
            2,
            usize::MAX,
        ),
        Err(IndexError::DuplicateEventIdentity(_))
    ));
    assert_eq!(
        crate::query::stored_core_event_record_materializations(),
        0,
        "duplicate requested IDs must be rejected before querying stored documents"
    );

    assert!(index
        .core_events_by_ids_if_bounded(&[first.event_id.as_uuid(), Uuid::nil()], 2, usize::MAX,)
        .unwrap()
        .is_none());
    assert!(index
        .core_events_by_ids_if_bounded(&[], 0, 0)
        .unwrap()
        .unwrap()
        .is_empty());
}

#[test]
fn bounded_session_coordinate_queries_ignore_pathological_nonselected_cardinality() {
    const EVENT_COUNT: u64 = 5_000;
    const SELECTED_SEQUENCE: u64 = 2_500;

    let temp = tempdir().unwrap();
    let source = source("bounded-session-coordinates.jsonl");
    let first = document(&source, 1, "first");
    let session_id = first.session_id.as_uuid();
    let mut selected_event_id = None;
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    for sequence in (1..=EVENT_COUNT).rev() {
        let event = document(&source, sequence, "small body");
        if sequence == SELECTED_SEQUENCE {
            selected_event_id = Some(event.event_id.as_uuid());
        }
        writer.add_core_record(event).unwrap();
    }
    writer
        .certify_source(certificate(&source, 1, EVENT_COUNT))
        .unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    crate::query::reset_stored_core_event_record_materializations();
    let prefix = index
        .session_event_coordinate_prefix(session_id, 6)
        .unwrap();
    assert_eq!(
        prefix
            .iter()
            .map(|coordinate| coordinate.event_sequence)
            .collect::<Vec<_>>(),
        vec![1, 2, 3, 4, 5, 6]
    );
    assert_eq!(crate::query::stored_core_event_record_materializations(), 0);

    let selected_event_id = selected_event_id.unwrap();
    let window = index
        .session_event_coordinate_window(session_id, selected_event_id, 50, 50)
        .unwrap()
        .unwrap();
    assert_eq!(window.len(), MAX_SESSION_EVENT_COORDINATE_WINDOW_ITEMS);
    assert_eq!(window.first().unwrap().event_sequence, 2_450);
    assert_eq!(window[50].event_id, selected_event_id);
    assert_eq!(window.last().unwrap().event_sequence, 2_550);
    assert_eq!(crate::query::stored_core_event_record_materializations(), 0);

    assert!(matches!(
        index.session_event_coordinate_prefix(
            session_id,
            MAX_SESSION_EVENT_COORDINATE_PREFIX_ITEMS + 1,
        ),
        Err(IndexError::InvalidSessionEventCoordinateLimit { .. })
    ));
    assert!(matches!(
        index.session_event_coordinate_window(session_id, selected_event_id, 51, 50),
        Err(IndexError::InvalidSessionEventCoordinateLimit { .. })
    ));
}

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
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
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
    crate::query::reset_stored_core_event_record_materializations();
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
    assert_eq!(
        crate::query::stored_core_event_record_materializations(),
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
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    for document in documents {
        writer.add_core_record(document).unwrap();
    }
    writer.certify_source(certificate(&source, 1, 3)).unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    crate::query::reset_stored_core_event_record_materializations();
    assert!(index
        .core_events_by_ids_if_bounded(&requested, requested.len(), 1)
        .unwrap()
        .is_none());
    assert_eq!(
        crate::query::stored_core_event_record_materializations(),
        1,
        "byte-budget refusal must stop before decoding the remaining large records"
    );
}

#[test]
fn script_aware_analysis_indexes_cjk_and_long_technical_identifiers() {
    let temp = tempdir().unwrap();
    let source = source("script-aware.jsonl");
    let cjk = document(&source, 1, "完成数据库迁移并验证索引");
    let long_component = "CtxSourceBackedGenerationIdentifier".repeat(8);
    let technical_identifier =
        format!("crate::provider::{long_component}::<Result<Vec<ProjectionRecord>>>");
    let identifier = document(
        &source,
        2,
        &format!("failed while resolving {technical_identifier}"),
    );
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(cjk.clone()).unwrap();
    writer.add_core_record(identifier.clone()).unwrap();
    writer.certify_source(certificate(&source, 1, 2)).unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(
        index
            .search_event_candidates("数据库迁移", 10)
            .unwrap()
            .into_iter()
            .map(|candidate| candidate.event.event_id)
            .collect::<Vec<_>>(),
        vec![cjk.event_id]
    );
    assert_eq!(
        index
            .search_event_candidates(&long_component, 10)
            .unwrap()
            .into_iter()
            .map(|candidate| candidate.event.event_id)
            .collect::<Vec<_>>(),
        vec![identifier.event_id]
    );
}

#[test]
fn multi_term_search_ranks_full_coverage_before_one_term_partial_matches() {
    let temp = tempdir().unwrap();
    let source = source("coverage-ranking.jsonl");
    let exact = document(&source, 1, "coveragealpha coveragebeta");
    let partial = document(&source, 2, &"coveragealpha ".repeat(64));
    let unrelated = document(&source, 3, "coveragegamma");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(partial.clone()).unwrap();
    writer.add_core_record(unrelated).unwrap();
    writer.add_core_record(exact.clone()).unwrap();
    writer.certify_source(certificate(&source, 1, 3)).unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    let candidates = index
        .search_event_candidates("coveragealpha coveragebeta", 10)
        .unwrap();
    assert_eq!(
        candidates
            .iter()
            .map(|candidate| candidate.event.event_id)
            .collect::<Vec<_>>(),
        vec![exact.event_id, partial.event_id]
    );
    assert_eq!(
        index
            .search_event_candidates("coveragealpha coveragebeta", 1)
            .unwrap()[0]
            .event
            .event_id,
        exact.event_id
    );
}

#[test]
fn session_event_budget_declines_before_materializing_an_oversized_session() {
    let temp = tempdir().unwrap();
    let source = source("bounded-session.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
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
    assert!(index
        .events_for_session_if_bounded(session_id, 2)
        .unwrap()
        .is_none());
    assert!(index
        .core_events_for_session_if_bounded(session_id, 2)
        .unwrap()
        .is_none());
    assert_eq!(
        index
            .events_for_session_if_bounded(session_id, 3)
            .unwrap()
            .unwrap()
            .len(),
        3
    );
    let core = index
        .core_events_for_session_if_bounded(session_id, 3)
        .unwrap()
        .unwrap();
    assert_eq!(core.len(), 3);
    assert!(core.iter().all(|record| {
        record.core_record.content.normalized_body.as_deref() == Some("bounded body")
    }));
}

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

    let directory = DurableMmapDirectory::open(temp.path()).unwrap();
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

#[test]
fn semantic_event_pages_follow_full_identity_order_and_explicit_eligibility() {
    let temp = tempdir().unwrap();
    let source = source("semantic-pages.jsonl");
    let first = document(&source, 1, "first eligible user message");
    let mut assistant = document(&source, 2, "assistant message");
    assistant.role = Some("assistant".to_owned());
    let mut tool = document(&source, 3, "user-shaped tool call");
    tool.event_type = "tool_call".to_owned();
    let mut control = document(
        &source,
        4,
        "  <environment_context>not a semantic turn</environment_context>  ",
    );
    control.event_type = "notice".to_owned();
    let second = document(&source, 5, "second eligible user message");
    let third = document(&source, 6, "third eligible user message");
    let mut aborted = document(&source, 7, "<turn_aborted>interrupted</turn_aborted>");
    aborted.event_type = "notice".to_owned();
    let mut notification = document(
        &source,
        8,
        "<subagent_notification>completed</subagent_notification>",
    );
    notification.event_type = "notice".to_owned();
    let mut warning = document(
        &source,
        9,
        "Warning: The maximum number of unified exec processes has been reached",
    );
    warning.event_type = "notice".to_owned();
    let discussion = document(
        &source,
        10,
        "How should an embedded <environment_context> marker be rendered?",
    );

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    for document in [
        third.clone(),
        assistant,
        first.clone(),
        control,
        tool,
        second.clone(),
        aborted,
        notification,
        warning,
        discussion.clone(),
    ] {
        writer.add_core_record(document).unwrap();
    }
    writer.certify_source(certificate(&source, 1, 10)).unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    let mut expected = [
        first.event_id,
        second.event_id,
        third.event_id,
        discussion.event_id,
    ];
    expected.sort_by_key(|identity| identity.encode_canonical().unwrap());

    let first_page = index.semantic_event_page(None, 2).unwrap();
    let core_first_page = index.core_semantic_event_page(None, 2).unwrap();
    assert_eq!(first_page.generation_id, index.generation_id());
    assert_eq!(core_first_page.generation_id, first_page.generation_id);
    assert_eq!(
        first_page.eligibility,
        SemanticEligibility::UserMessageCandidateV2
    );
    assert_eq!(core_first_page.eligibility, first_page.eligibility);
    assert_eq!(first_page.eligible_total, 4);
    assert_eq!(core_first_page.eligible_total, first_page.eligible_total);
    assert_eq!(first_page.eligible_count(), 2);
    assert_eq!(
        core_first_page.eligible_count(),
        first_page.eligible_count()
    );
    assert!(!first_page.terminal);
    assert_eq!(core_first_page.terminal, first_page.terminal);
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
    assert_eq!(
        core_first_page
            .next_cursor
            .as_ref()
            .map(SemanticEventCursor::after),
        first_page
            .next_cursor
            .as_ref()
            .map(SemanticEventCursor::after)
    );
    assert!(core_first_page.items.iter().all(|record| record
        .core_record
        .content
        .normalized_body
        .is_some()));
    assert!(first_page.items[0].source.exact_descriptor_eq(&source));
    assert_eq!(
        first_page.items[0].root_session_id,
        first_page.items[0].session_id
    );

    let cursor = first_page.next_cursor.unwrap();
    assert_eq!(cursor.generation_id(), index.generation_id());
    assert_eq!(cursor.eligibility(), SemanticEligibility::CURRENT);
    assert_eq!(cursor.after(), expected[1]);

    let final_page = index.semantic_event_page(Some(&cursor), 2).unwrap();
    let core_final_page = index.core_semantic_event_page(Some(&cursor), 2).unwrap();
    assert_eq!(final_page.eligible_total, 4);
    assert_eq!(core_final_page.eligible_total, final_page.eligible_total);
    assert_eq!(final_page.eligible_count(), 2);
    assert_eq!(
        core_final_page.eligible_count(),
        final_page.eligible_count()
    );
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
    assert!(final_page.terminal);
    assert_eq!(core_final_page.terminal, final_page.terminal);
    assert!(final_page.next_cursor.is_none());
    assert!(core_final_page.next_cursor.is_none());
    assert_eq!(index.semantic_eligible_event_count().unwrap(), 4);
}

#[test]
fn semantic_pages_select_addresses_before_decoding_and_bound_retained_core_bytes() {
    const INELIGIBLE_EVENTS: u64 = 128;
    let temp = tempdir().unwrap();
    let source = source("semantic-address-first.jsonl");
    let large_body = format!("x{}", " ".repeat(512 * 1024 - 1));
    let mut expected = Vec::new();
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    for sequence in 1..=INELIGIBLE_EVENTS {
        let mut event = document(&source, sequence, "ineligible assistant message");
        event.role = Some("assistant".to_owned());
        writer.add_core_record(event).unwrap();
    }
    for sequence in INELIGIBLE_EVENTS + 1..=INELIGIBLE_EVENTS + 3 {
        let event = document(&source, sequence, &large_body);
        expected.push(event.event_id);
        writer.add_core_record(event).unwrap();
    }
    writer
        .certify_source(certificate(&source, 1, INELIGIBLE_EVENTS + 3))
        .unwrap();
    writer.commit(|_| true).unwrap();
    expected.sort_by_key(|identity| identity.encode_canonical().unwrap());

    let index = VerifiedIndex::open(temp.path()).unwrap();
    let budget = CoreEventPageBudget::new(1, 1);
    let mut cursor = None;
    let mut actual = Vec::new();
    loop {
        crate::query::reset_stored_core_event_record_materializations();
        let page = index
            .core_semantic_event_page_with_budget(cursor.as_ref(), 64, budget)
            .unwrap();
        assert_eq!(page.eligible_total, 3);
        assert_eq!(page.items.len(), 1);
        assert!(page.encoded_core_bytes <= ctx_history_core::MAX_ENCODED_CORE_RECORD_BYTES);
        assert!(page.content_bytes <= ctx_history_core::MAX_CORE_CONTENT_BYTES);
        assert!(
            crate::query::stored_core_event_record_materializations() <= 2,
            "semantic byte lookahead may decode only the admitted record and one non-fitting record"
        );
        actual.push(page.items[0].event_id);
        if page.terminal {
            break;
        }
        cursor = page.next_cursor;
    }
    assert_eq!(actual, expected);
}

#[test]
fn semantic_event_pages_handle_empty_final_and_generation_bound_cursors() {
    let temp = tempdir().unwrap();
    GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .commit(|_| true)
        .unwrap();
    let empty = VerifiedIndex::open(temp.path()).unwrap();
    let page = empty.semantic_event_page(None, 1).unwrap();
    assert_eq!(page.eligible_total, 0);
    assert!(page.items.is_empty());
    assert!(page.terminal);
    assert!(page.next_cursor.is_none());

    let source = source("final-page.jsonl");
    let expected = document(&source, 1, "only eligible event");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(expected.clone()).unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open(temp.path()).unwrap();

    let final_page = index.semantic_event_page(None, 1).unwrap();
    assert_eq!(final_page.items.len(), 1);
    assert!(final_page.terminal);
    assert!(final_page.next_cursor.is_none());

    let after_last = SemanticEventCursor::new(index.generation_id(), expected.event_id);
    let empty_final = index.semantic_event_page(Some(&after_last), 1).unwrap();
    assert_eq!(empty_final.eligible_total, 1);
    assert!(empty_final.items.is_empty());
    assert!(empty_final.terminal);
    assert!(empty_final.next_cursor.is_none());

    let foreign = SemanticEventCursor::new("0".repeat(64), expected.event_id);
    assert!(matches!(
        index.semantic_event_page(Some(&foreign), 1),
        Err(IndexError::SemanticEventCursorGenerationMismatch { .. })
    ));
    assert!(matches!(
        index.semantic_event_page(None, 0),
        Err(IndexError::InvalidSemanticEventPageSize { .. })
    ));
    assert!(matches!(
        index.semantic_event_page(None, MAX_SEMANTIC_EVENT_PAGE_ITEMS + 1),
        Err(IndexError::InvalidSemanticEventPageSize { .. })
    ));
}

#[test]
fn semantic_event_pages_keep_old_pins_isolated_from_rewrite_and_deletion() {
    fn page_all(index: &VerifiedIndex) -> Vec<EventRecord> {
        let mut cursor = None;
        let mut records = Vec::new();
        loop {
            let page = index.semantic_event_page(cursor.as_ref(), 1).unwrap();
            records.extend(page.items);
            if page.terminal {
                return records;
            }
            cursor = Some(page.next_cursor.unwrap());
        }
    }

    let temp = tempdir().unwrap();
    let source = source("rewrite-delete.jsonl");
    let old_first = document(&source, 1, "old first event");
    let old_second = document(&source, 2, "old second event");
    let mut first_writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    first_writer.begin_source(source.clone()).unwrap();
    first_writer.add_core_record(old_second.clone()).unwrap();
    first_writer.add_core_record(old_first.clone()).unwrap();
    first_writer
        .certify_source(certificate(&source, 1, 2))
        .unwrap();
    first_writer.commit(|_| true).unwrap();
    let old_pin = VerifiedIndex::open(temp.path()).unwrap();
    let old_cursor = old_pin
        .semantic_event_page(None, 1)
        .unwrap()
        .next_cursor
        .unwrap();

    let mut rewritten_first = document(&source, 1, "rewritten first event");
    rewritten_first.workspace = Some("rewritten-workspace".to_owned());
    let replacement = document(&source, 3, "replacement third event");
    let mut replacement_writer =
        GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    replacement_writer.begin_source(source.clone()).unwrap();
    replacement_writer
        .add_core_record(replacement.clone())
        .unwrap();
    replacement_writer
        .add_core_record(rewritten_first.clone())
        .unwrap();
    replacement_writer
        .certify_source(certificate(&source, 2, 2))
        .unwrap();
    replacement_writer.commit(|_| true).unwrap();
    let rewritten_pin = VerifiedIndex::open(temp.path()).unwrap();

    let old_records = page_all(&old_pin);
    assert_eq!(old_records.len(), 2);
    assert!(old_records
        .iter()
        .any(|event| event.event_id == old_first.event_id));
    assert!(old_records
        .iter()
        .any(|event| event.event_id == old_second.event_id));

    let rewritten_records = page_all(&rewritten_pin);
    assert_eq!(rewritten_records.len(), 2);
    assert!(rewritten_records.iter().any(|event| {
        event.event_id == rewritten_first.event_id
            && event.workspace.as_deref() == Some("rewritten-workspace")
    }));
    assert!(rewritten_records
        .iter()
        .all(|event| event.event_id != old_second.event_id));
    assert!(rewritten_records
        .iter()
        .any(|event| event.event_id == replacement.event_id));
    assert_ne!(old_pin.generation_id(), rewritten_pin.generation_id());
    assert!(matches!(
        rewritten_pin.semantic_event_page(Some(&old_cursor), 1),
        Err(IndexError::SemanticEventCursorGenerationMismatch { .. })
    ));

    let mut deletion_writer =
        GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    let (deletion, inventory) = deletion_evidence(&source, 3);
    deletion_writer.delete_source(deletion, inventory).unwrap();
    deletion_writer.commit(|_| true).unwrap();
    let deleted_pin = VerifiedIndex::open(temp.path()).unwrap();

    assert!(page_all(&deleted_pin).is_empty());
    assert_eq!(page_all(&old_pin).len(), 2);
    assert_eq!(page_all(&rewritten_pin).len(), 2);
}

#[test]
fn filtered_search_covers_relationship_and_public_metadata_contracts() {
    let temp = tempdir().unwrap();
    let codex_root = source("codex-root");
    let codex_child = source("codex-child");
    let claude = source_for_provider(
        "claude_code",
        "claude_projects_jsonl_tree",
        "claude-sessions",
    );

    let mut root = document_for_session(&codex_root, "root-thread", 1, "shared needle");
    root.workspace = Some("Ctx-Rich-Fixture".to_owned());
    root.cwd = Some("/work/ctx-root".to_owned());
    root.occurred_at_unix_ms = Some(100);
    let root_session_id = root.session_id;
    root.root_session_id = root_session_id;

    let mut child = document_for_session(&codex_child, "child-thread", 2, "shared needle");
    child.parent_session_id = Some(root_session_id);
    child.root_session_id = root_session_id;
    child.branch = Some("feature/query-seam".to_owned());
    child.workspace = Some("ChildSpace".to_owned());
    child.cwd = Some("/work/child".to_owned());
    child.agent_type = "subagent".to_owned();
    child.is_primary = false;
    child.event_type = "tool_call".to_owned();
    child.role = Some("assistant".to_owned());
    child.occurred_at_unix_ms = Some(200);
    let child_session_id = child.session_id;

    let mut other = document_for_session(&claude, "other-thread", 3, "shared needle");
    other.workspace = Some("Elsewhere".to_owned());
    other.branch = Some("release".to_owned());
    other.occurred_at_unix_ms = Some(300);
    let other_session_id = other.session_id;
    other.root_session_id = other_session_id;

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(codex_root.clone()).unwrap();
    writer.add_core_record(root).unwrap();
    writer
        .certify_source(certificate(&codex_root, 1, 1))
        .unwrap();
    writer.begin_source(codex_child.clone()).unwrap();
    writer.add_core_record(child).unwrap();
    writer
        .certify_source(certificate(&codex_child, 1, 1))
        .unwrap();
    writer.begin_source(claude.clone()).unwrap();
    writer.add_core_record(other).unwrap();
    writer.certify_source(certificate(&claude, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open(temp.path()).unwrap();

    let all = sorted_uuids(vec![
        root_session_id.as_uuid(),
        child_session_id.as_uuid(),
        other_session_id.as_uuid(),
    ]);
    assert_eq!(
        filtered_session_ids(&index, EventSearchFilters::default()),
        all
    );
    assert_eq!(
        filtered_session_ids(
            &index,
            EventSearchFilters {
                provider: Some("claude_code".to_owned()),
                ..EventSearchFilters::default()
            }
        ),
        vec![other_session_id.as_uuid()]
    );
    assert_eq!(
        filtered_session_ids(
            &index,
            EventSearchFilters {
                workspace: Some("RICH".to_owned()),
                ..EventSearchFilters::default()
            }
        ),
        vec![root_session_id.as_uuid()]
    );
    assert_eq!(
        filtered_session_ids(
            &index,
            EventSearchFilters {
                since_unix_ms: Some(250),
                ..EventSearchFilters::default()
            }
        ),
        vec![other_session_id.as_uuid()]
    );
    assert_eq!(
        filtered_session_ids(
            &index,
            EventSearchFilters {
                event_type: Some("tool_call".to_owned()),
                role: Some("assistant".to_owned()),
                agent_type: Some("subagent".to_owned()),
                ..EventSearchFilters::default()
            }
        ),
        vec![child_session_id.as_uuid()]
    );
    assert_eq!(
        filtered_session_ids(
            &index,
            EventSearchFilters {
                agent_scope: AgentScope::Primary,
                ..EventSearchFilters::default()
            }
        ),
        sorted_uuids(vec![root_session_id.as_uuid(), other_session_id.as_uuid()])
    );
    assert_eq!(
        filtered_session_ids(
            &index,
            EventSearchFilters {
                session_id: Some(child_session_id.as_uuid()),
                agent_scope: AgentScope::Primary,
                ..EventSearchFilters::default()
            }
        ),
        vec![child_session_id.as_uuid()]
    );
    assert_eq!(
        filtered_session_ids(
            &index,
            EventSearchFilters {
                parent_session_id: Some(root_session_id.as_uuid()),
                root_session_id: Some(root_session_id.as_uuid()),
                provider_session_id: Some("child-thread".to_owned()),
                branch: Some("feature/query-seam".to_owned()),
                ..EventSearchFilters::default()
            }
        ),
        vec![child_session_id.as_uuid()]
    );
    assert_eq!(
        filtered_session_ids(
            &index,
            EventSearchFilters {
                exclude_session_tree: Some(ExcludedSessionTree {
                    provider: "codex".to_owned(),
                    provider_session_id: "root-thread".to_owned(),
                    session_id: Some(root_session_id.as_uuid()),
                }),
                ..EventSearchFilters::default()
            }
        ),
        vec![other_session_id.as_uuid()]
    );

    let child = index
        .session_by_id(child_session_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(child.parent_session_id, Some(root_session_id));
    assert_eq!(child.root_session_id, root_session_id);
    assert_eq!(child.provider_session_id.as_deref(), Some("child-thread"));
    assert_eq!(child.branch.as_deref(), Some("feature/query-seam"));
    assert!(child.source_path.is_none());
    assert_eq!(child.agent_type, "subagent");
    assert!(!child.is_primary);
}

#[test]
fn complete_core_body_beyond_16k_round_trips_reopens_and_has_no_stored_preview() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let body = format!("{} tailonlyneedle", "界".repeat(16_384));
    let expected = document(&source, 1, &body);
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(expected.clone()).unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    let record = index
        .event_by_id(expected.event_id.as_uuid())
        .unwrap()
        .unwrap();
    assert!(record.source.exact_descriptor_eq(&expected.source));
    let core_record = index
        .core_record_by_id(expected.event_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(
        core_record.content.normalized_body.as_deref(),
        Some(body.as_str())
    );
    assert!(core_record.repository_bindings.is_empty());
    assert!(core_record.repository_abstentions.is_empty());
    let source_page = index.core_source_event_page(&source, None, 1).unwrap();
    let semantic_page = index.core_semantic_event_page(None, 1).unwrap();
    let session_events = index
        .core_events_for_session(expected.session_id.as_uuid())
        .unwrap();
    let bounded_session_events = index
        .core_events_for_session_if_bounded(expected.session_id.as_uuid(), 1)
        .unwrap()
        .unwrap();
    assert!(source_page.terminal);
    assert!(semantic_page.terminal);
    assert_eq!(semantic_page.eligible_total, 1);
    for record in [
        &source_page.items[0],
        &semantic_page.items[0],
        &session_events[0],
        &bounded_session_events[0],
    ] {
        assert_eq!(record.event_id, expected.event_id);
        assert_eq!(
            record.core_record.content.normalized_body.as_deref(),
            Some(body.as_str())
        );
    }
    assert_eq!(
        index.search_event_candidates("tailonlyneedle", 10).unwrap()[0]
            .event
            .event_id,
        expected.event_id
    );

    let fields = fields_from_schema(index.searcher.schema()).unwrap();
    assert!(index.searcher.schema().get_field("body_preview").is_err());
    assert!(index.searcher.schema().get_field("body").is_err());
    let address = index
        .searcher
        .search(&AllQuery, &DocSetCollector)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let stored: TantivyDocument = index.searcher.doc(address).unwrap();
    assert!(stored.get_first(fields.body_search).is_none());
    assert!(stored.get_first(fields.core_record).is_some());

    drop(index);
    let reopened = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let reopened_records = reopened
        .core_events_for_session(expected.session_id.as_uuid())
        .unwrap();
    assert_eq!(
        reopened_records[0]
            .core_record
            .content
            .normalized_body
            .as_deref(),
        Some(body.as_str())
    );
}

#[test]
fn empty_or_invalid_programmatic_queries_are_safe() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer
        .add_core_record(document(&source, 1, "body"))
        .unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open(temp.path()).unwrap();

    assert!(index.search_event_candidates("", 10).unwrap().is_empty());
    assert!(index.search_event_candidates("body", 0).unwrap().is_empty());
    assert!(matches!(
        index.search_event_candidates_with_filters(
            "body",
            &EventSearchFilters {
                provider: Some("  ".to_owned()),
                ..EventSearchFilters::default()
            },
            10,
        ),
        Err(IndexError::EmptyQueryFilter { field: "provider" })
    ));
    assert!(matches!(
        index.events_by_id_prefix("not-a-uuid"),
        Err(IndexError::InvalidIdPrefix)
    ));
    assert!(matches!(
        index.sessions_by_id_prefix(""),
        Err(IndexError::InvalidIdPrefix)
    ));
}
