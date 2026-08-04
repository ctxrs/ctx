use super::*;
use ctx_history_core::StableEntityId;

#[test]
fn core_session_pages_traverse_more_than_4096_in_order_without_duplicates() {
    const EVENT_COUNT: u64 = 4_097;
    const PAGE_ITEMS: usize = 1_000;

    let temp = tempdir().unwrap();
    let source = source("huge-session-pages.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for sequence in (1..=EVENT_COUNT).rev() {
        writer
            .add_core_record(document(&source, sequence, "bounded transcript event"))
            .unwrap();
    }
    writer
        .certify_source(certificate(&source, 1, EVENT_COUNT))
        .unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let session_id = document(&source, 1, "unused").session_id;
    let mut cursor = None;
    let mut sequences = Vec::new();
    let mut identities = HashSet::new();
    loop {
        crate::query::reset_stored_core_event_record_materializations();
        let page = index
            .core_session_event_page_with_budget(
                session_id.as_uuid(),
                cursor.as_ref(),
                PAGE_ITEMS,
                DEFAULT_CORE_EVENT_PAGE_BUDGET,
            )
            .unwrap();
        assert_eq!(page.generation_id, index.generation_id());
        assert_eq!(page.session_id, session_id);
        assert!(page.items.len() <= PAGE_ITEMS);
        assert!(
            page.encoded_core_bytes <= DEFAULT_CORE_EVENT_PAGE_BUDGET.maximum_encoded_core_bytes
        );
        assert!(page.content_bytes <= DEFAULT_CORE_EVENT_PAGE_BUDGET.maximum_content_bytes);
        assert_eq!(
            crate::query::stored_core_event_record_materializations(),
            page.items.len(),
            "one page must not decode the unreturned session tail"
        );
        for record in page.items {
            assert!(identities.insert(record.event_id));
            sequences.push(record.event_sequence);
        }
        if page.terminal {
            assert!(page.next_cursor.is_none());
            break;
        }
        let next = page.next_cursor.expect("nonterminal page needs a cursor");
        assert_eq!(next.generation_id(), index.generation_id());
        assert_eq!(next.session_id(), session_id);
        cursor = Some(next);
    }

    assert_eq!(sequences, (1..=EVENT_COUNT).collect::<Vec<_>>());
    assert_eq!(identities.len(), EVENT_COUNT as usize);
}

#[test]
fn core_session_cursor_binds_generation_full_session_and_exact_coordinate() {
    let temp = tempdir().unwrap();
    let source = source("session-cursor-contract.jsonl");
    let alpha_first = document_for_session(&source, "alpha", 1, "same sized body");
    let alpha_second = document_for_session(&source, "alpha", 2, "same sized body");
    let beta_first = document_for_session(&source, "beta", 1, "same sized body");
    let beta_second = document_for_session(&source, "beta", 2, "same sized body");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for record in [
        alpha_second.clone(),
        beta_first.clone(),
        alpha_first.clone(),
        beta_second.clone(),
    ] {
        writer.add_core_record(record).unwrap();
    }
    writer.certify_source(certificate(&source, 1, 4)).unwrap();
    writer.commit(|_| true).unwrap();

    let old_pin = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let first = old_pin
        .core_session_event_page_with_budget(
            alpha_first.session_id.as_uuid(),
            None,
            1,
            DEFAULT_CORE_EVENT_PAGE_BUDGET,
        )
        .unwrap();
    let cursor = first.next_cursor.unwrap();
    let serialized = serde_json::to_vec(&cursor).unwrap();
    let cursor: SessionEventCursor = serde_json::from_slice(&serialized).unwrap();

    assert!(matches!(
        old_pin.core_session_event_page_with_budget(
            beta_first.session_id.as_uuid(),
            Some(&cursor),
            1,
            DEFAULT_CORE_EVENT_PAGE_BUDGET,
        ),
        Err(IndexError::SessionEventCursorSessionMismatch)
    ));

    let mut colliding = alpha_first.session_id.encode_canonical().unwrap();
    colliding[3 + 16] ^= 1;
    let colliding = StableEntityId::decode_canonical(&colliding).unwrap();
    assert_eq!(colliding.as_uuid(), alpha_first.session_id.as_uuid());
    assert_ne!(colliding, alpha_first.session_id);
    let collision_cursor =
        SessionEventCursor::new(old_pin.generation_id(), colliding, cursor.after());
    assert!(matches!(
        old_pin.core_session_event_page_with_budget(
            alpha_first.session_id.as_uuid(),
            Some(&collision_cursor),
            1,
            DEFAULT_CORE_EVENT_PAGE_BUDGET,
        ),
        Err(IndexError::SessionEventCursorSessionMismatch)
    ));

    let mut invalid_after = cursor.after();
    invalid_after.event_sequence += 100;
    let invalid_cursor = SessionEventCursor::new(
        old_pin.generation_id(),
        alpha_first.session_id,
        invalid_after,
    );
    assert!(matches!(
        old_pin.core_session_event_page_with_budget(
            alpha_first.session_id.as_uuid(),
            Some(&invalid_cursor),
            1,
            DEFAULT_CORE_EVENT_PAGE_BUDGET,
        ),
        Err(IndexError::InvalidSessionEventCursorCoordinate)
    ));

    let measured = old_pin
        .core_session_event_page_with_budget(
            alpha_first.session_id.as_uuid(),
            None,
            1,
            DEFAULT_CORE_EVENT_PAGE_BUDGET,
        )
        .unwrap();
    let bounded = old_pin
        .core_session_event_page_with_budget(
            alpha_first.session_id.as_uuid(),
            None,
            MAX_SESSION_EVENT_PAGE_ITEMS,
            CoreEventPageBudget::new(measured.encoded_core_bytes, measured.content_bytes),
        )
        .unwrap();
    assert_eq!(bounded.items.len(), 1);
    assert!(bounded.encoded_core_bytes <= measured.encoded_core_bytes);
    assert!(bounded.content_bytes <= measured.content_bytes);
    assert!(!bounded.terminal);
    assert!(bounded.next_cursor.is_some());

    assert!(matches!(
        old_pin.core_session_event_page_with_budget(
            alpha_first.session_id.as_uuid(),
            None,
            0,
            DEFAULT_CORE_EVENT_PAGE_BUDGET,
        ),
        Err(IndexError::InvalidSessionEventPageSize { .. })
    ));

    let mut replacement = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    replacement.begin_source(source.clone()).unwrap();
    for record in [alpha_first, alpha_second, beta_first, beta_second] {
        replacement.add_core_record(record).unwrap();
    }
    replacement
        .certify_source(certificate(&source, 2, 4))
        .unwrap();
    replacement.commit(|_| true).unwrap();
    let new_pin = VerifiedIndex::open_pinned(temp.path()).unwrap();
    assert!(matches!(
        new_pin.core_session_event_page_with_budget(
            cursor.session_id().as_uuid(),
            Some(&cursor),
            1,
            DEFAULT_CORE_EVENT_PAGE_BUDGET,
        ),
        Err(IndexError::SessionEventCursorGenerationMismatch { .. })
    ));
}

#[test]
fn core_session_page_reports_duplicate_identity_as_typed_error() {
    let temp = tempdir().unwrap();
    let source = source("duplicate-session-page.jsonl");
    let first = document(&source, 1, "first");
    let second = document(&source, 2, "second");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(first.clone()).unwrap();
    writer.add_core_record(second).unwrap();
    writer.certify_source(certificate(&source, 1, 2)).unwrap();
    writer.commit(|_| true).unwrap();

    let (searcher, manifest) = open_unverified_generation(temp.path());
    drop(searcher);
    let directory = DurableMmapDirectory::open(active_generation_path(temp.path())).unwrap();
    let index = Index::open(directory).unwrap();
    publish_unchecked_generation(
        temp.path(),
        &index,
        manifest,
        std::slice::from_ref(&source),
        vec![
            indexed_document(first.clone()),
            indexed_document(first.clone()),
        ],
    );

    let pinned = VerifiedIndex::open_pinned(temp.path()).unwrap();
    assert!(matches!(
        pinned.core_session_event_page_with_budget(
            first.session_id.as_uuid(),
            None,
            1,
            DEFAULT_CORE_EVENT_PAGE_BUDGET,
        ),
        Err(IndexError::DuplicateEventIdentity(_))
    ));
}
