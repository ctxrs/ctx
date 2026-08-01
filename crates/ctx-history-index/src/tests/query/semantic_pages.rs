use super::*;

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
