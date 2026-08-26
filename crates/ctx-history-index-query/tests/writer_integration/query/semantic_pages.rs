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

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
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
    let receipt = writer.commit(|_| true).unwrap();
    assert_eq!(receipt.indexed_documents, 10);

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
        SemanticEligibility::UserMessageCandidateV4
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
    assert_eq!(first_page.items[0].root_session_id, None);

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
fn semantic_filter_projection_matches_lexical_filter_semantics_without_core_decode() {
    let temp = tempdir().unwrap();
    let source = source("semantic-filter-parity.jsonl");
    let mut target = document_for_session(&source, "target-session", 1, "shared parity needle");
    replace_literal_fact(
        &mut target,
        LiteralFactKind::SessionCwd,
        "/Work/CwdOnlyTarget",
    );
    add_literal_fact(&mut target, LiteralFactKind::Branch, "main");
    add_literal_fact(&mut target, LiteralFactKind::File, "src/ParityTarget.rs");
    target.validate_contract().unwrap();
    let other = document_for_session(&source, "other-session", 2, "shared parity needle");
    let mut subagent = document_for_session(&source, "subagent-session", 3, "shared parity needle");
    subagent.agent_scope = Some(CoreAgentScope::Subagent);
    subagent.parent_session_id = Some(target.session_id);
    subagent.root_session_id = Some(target.session_id);
    subagent.session_relationship = Some(ProviderNativeSessionRelationship::Delegated);
    subagent.validate_contract().unwrap();

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for record in [target.clone(), other.clone(), subagent.clone()] {
        writer.add_core_record(record).unwrap();
    }
    writer.certify_source(certificate(&source, 1, 3)).unwrap();
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();

    let parity_filters = [
        EventSearchFilters::default(),
        EventSearchFilters {
            allowed_source_keys: Some(vec![source_token(&source)]),
            ..EventSearchFilters::default()
        },
        EventSearchFilters {
            allowed_source_keys: Some(Vec::new()),
            ..EventSearchFilters::default()
        },
        EventSearchFilters {
            session_id: Some(target.session_id.as_uuid()),
            provider: Some("codex".to_owned()),
            source_format: Some("codex_session_jsonl".to_owned()),
            branch: Some("main".to_owned()),
            workspace: Some("cwdonlyTARGET".to_owned()),
            since_unix_ms: target.occurred_at_unix_ms,
            event_type: Some("message".to_owned()),
            role: Some("user".to_owned()),
            agent_scope: AgentScope::Primary,
            file: Some("paritytarget.RS".to_owned()),
            ..EventSearchFilters::default()
        },
        EventSearchFilters {
            agent_scope: AgentScope::Primary,
            ..EventSearchFilters::default()
        },
        EventSearchFilters {
            parent_session_id: Some(target.session_id.as_uuid()),
            root_session_id: Some(target.session_id.as_uuid()),
            provider_session_id: Some("subagent-session".to_owned()),
            agent_scope: AgentScope::Subagent,
            ..EventSearchFilters::default()
        },
        EventSearchFilters {
            exclude_session_tree: Some(ExcludedSessionTree {
                session_ids: vec![target.session_id.as_uuid()],
            }),
            ..EventSearchFilters::default()
        },
        EventSearchFilters {
            excluded_session_ids: vec![target.session_id.as_uuid(), subagent.session_id.as_uuid()],
            ..EventSearchFilters::default()
        },
        EventSearchFilters {
            excluded_session_ids: vec![other.session_id.as_uuid()],
            exclude_session_tree: Some(ExcludedSessionTree {
                session_ids: vec![target.session_id.as_uuid()],
            }),
            ..EventSearchFilters::default()
        },
    ];
    for filters in parity_filters {
        let lexical_batch =
            lexical_search_batch(&index, &["shared parity needle"], &filters, 10).unwrap();
        assert!(lexical_batch.complete);
        if filters.workspace.is_some() || filters.file.is_some() {
            assert!(lexical_batch.counters.term_expansions > 0);
        } else {
            assert_eq!(lexical_batch.counters.term_expansions, 0);
        }
        let lexical = lexical_batch
            .candidates
            .into_iter()
            .map(|candidate| candidate.event.event_id)
            .collect::<HashSet<_>>();
        let listed = lexical_list_batch(&index, &filters, 10).unwrap();
        assert!(listed.complete);
        assert_eq!(
            listed
                .candidates
                .into_iter()
                .map(|candidate| candidate.event.event_id)
                .collect::<HashSet<_>>(),
            lexical,
            "manual body and list candidates must share exact filter semantics"
        );
        ctx_history_index_query::reset_core_record_decodes();
        let semantic = semantic_projection(&index, &filters).unwrap();
        assert_eq!(semantic.generation_id(), index.generation_id());
        assert_eq!(semantic.event_ids().collect::<HashSet<_>>(), lexical);
        assert_eq!(
            ctx_history_index_query::core_record_decodes(),
            0,
            "semantic eligibility must use indexed metadata without decoding Core bodies"
        );
    }

    let invalid = EventSearchFilters {
        provider: Some("  ".to_owned()),
        ..EventSearchFilters::default()
    };
    assert!(matches!(
        semantic_projection(&index, &invalid),
        Err(IndexError::EmptyQueryFilter { field: "provider" })
    ));
}

#[test]
fn retrieval_derived_user_message_is_not_a_semantic_candidate() {
    let temp = tempdir().unwrap();
    let source = source("semantic-retrieval-derived.jsonl");
    let ordinary = document(&source, 1, "ordinary semantic candidate");
    let excluded = retrieval_excluded(document(
        &source,
        2,
        "retrieval derived semantic bypass canary",
    ));

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(ordinary.clone()).unwrap();
    writer.add_core_record(excluded.clone()).unwrap();
    writer.certify_source(certificate(&source, 1, 2)).unwrap();
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();

    assert_eq!(index.semantic_eligible_event_count().unwrap(), 1);
    let metadata_page = index.semantic_event_page(None, 10).unwrap();
    assert!(metadata_page.terminal);
    assert_eq!(metadata_page.eligible_total, 1);
    assert_eq!(metadata_page.items.len(), 1);
    assert_eq!(metadata_page.items[0].event_id, ordinary.event_id);

    let core_page = index.core_semantic_event_page(None, 10).unwrap();
    assert!(core_page.terminal);
    assert_eq!(core_page.eligible_total, 1);
    assert_eq!(core_page.items.len(), 1);
    assert_eq!(core_page.items[0].event_id, ordinary.event_id);

    let projection = semantic_projection(&index, &EventSearchFilters::default()).unwrap();
    assert_eq!(
        projection.event_ids().collect::<Vec<_>>(),
        vec![ordinary.event_id.as_uuid()]
    );
    assert_eq!(
        index
            .core_record_by_id(excluded.event_id.as_uuid())
            .unwrap()
            .unwrap(),
        excluded
    );
}

#[test]
fn semantic_first_pages_filter_the_neutral_core_order() {
    const INELIGIBLE_EVENTS: u64 = 2_048;
    let temp = tempdir().unwrap();
    let first_source = source("semantic-bounded-first-page.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(first_source.clone()).unwrap();
    for sequence in 1..=INELIGIBLE_EVENTS {
        let mut event = document(&first_source, sequence, "irrelevant assistant message");
        event.role = Some("assistant".to_owned());
        writer.add_core_record(event).unwrap();
    }
    for sequence in INELIGIBLE_EVENTS + 1..=INELIGIBLE_EVENTS + 2 {
        writer
            .add_core_record(document(&first_source, sequence, "eligible user message"))
            .unwrap();
    }
    writer
        .certify_source(certificate(&first_source, 1, INELIGIBLE_EVENTS + 2))
        .unwrap();
    let receipt = writer.commit(|_| true).unwrap();
    assert_eq!(receipt.indexed_documents, INELIGIBLE_EVENTS + 2);

    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    ctx_history_index_query::reset_semantic_event_order_term_visits();
    ctx_history_index_query::reset_stored_core_event_record_materializations();
    let page = index.core_semantic_event_page(None, 1).unwrap();
    assert_eq!(page.eligible_total, 2);
    assert_eq!(page.items.len(), 1);
    assert!(!page.terminal);
    assert!(ctx_history_index_query::semantic_event_order_term_visits() > 0);
    assert_eq!(
        ctx_history_index_query::stored_core_event_record_materializations(),
        1
    );

    let empty_temp = tempdir().unwrap();
    let empty_source = source("semantic-bounded-empty-page.jsonl");
    let mut empty_writer = GenerationWriter::open(empty_temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    empty_writer.begin_source(empty_source.clone()).unwrap();
    for sequence in 1..=INELIGIBLE_EVENTS {
        let mut event = document(&empty_source, sequence, "irrelevant assistant message");
        event.role = Some("assistant".to_owned());
        empty_writer.add_core_record(event).unwrap();
    }
    empty_writer
        .certify_source(certificate(&empty_source, 1, INELIGIBLE_EVENTS))
        .unwrap();
    let empty_receipt = empty_writer.commit(|_| true).unwrap();
    assert_eq!(empty_receipt.indexed_documents, INELIGIBLE_EVENTS);

    let empty = VerifiedIndex::open_pinned(empty_temp.path()).unwrap();
    ctx_history_index_query::reset_semantic_event_order_term_visits();
    ctx_history_index_query::reset_stored_core_event_record_materializations();
    let page = empty.core_semantic_event_page(None, 1).unwrap();
    assert_eq!(page.eligible_total, 0);
    assert!(page.items.is_empty());
    assert!(page.terminal);
    assert_eq!(
        ctx_history_index_query::stored_core_event_record_materializations(),
        0
    );
}

#[test]
fn semantic_policy_count_tracks_unchanged_core_across_append_retain_and_delete() {
    let temp = tempdir().unwrap();
    let source = source("semantic-manifest-count.jsonl");
    let first_user = document(&source, 1, "first user message");
    let mut first_assistant = document(&source, 2, "first assistant message");
    first_assistant.role = Some("assistant".to_owned());

    let mut first = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    first.begin_source(source.clone()).unwrap();
    first.add_core_record(first_user).unwrap();
    first.add_core_record(first_assistant).unwrap();
    first
        .certify_source(appendable_certificate(&source, 1, 2, 20))
        .unwrap();
    let _first_receipt = first.commit(|_| true).unwrap();
    assert_eq!(
        VerifiedIndex::open(temp.path())
            .unwrap()
            .semantic_eligible_event_count()
            .unwrap(),
        1
    );

    let mut second_user = document(&source, 3, "second user message");
    replace_literal_fact(&mut second_user, LiteralFactKind::Workspace, "append");
    let mut second_assistant = document(&source, 4, "second assistant message");
    second_assistant.role = Some("assistant".to_owned());
    let mut append = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let base = append.begin_source_append(source.clone()).unwrap().clone();
    append.add_core_record(second_assistant).unwrap();
    append.add_core_record(second_user).unwrap();
    append
        .certify_source_append(
            CertifiedSourceAppend::certify(
                &base,
                appendable_certificate(&source, 2, 4, 40),
                20,
                [1; 32],
            )
            .unwrap(),
        )
        .unwrap();
    let append_receipt = append.commit(|_| true).unwrap();
    assert_eq!(
        VerifiedIndex::open(temp.path())
            .unwrap()
            .semantic_eligible_event_count()
            .unwrap(),
        2
    );

    let retained_certificate = append_receipt.manifest().sources[0].clone();
    let mut retain = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    retain.retain_source(retained_certificate).unwrap();
    let retain_receipt = retain.commit(|_| true).unwrap();
    assert_eq!(retain_receipt.generation_id, append_receipt.generation_id);

    let mut deletion = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let (proof, inventory) = deletion_evidence(&source, 3);
    deletion.delete_source(proof, inventory).unwrap();
    let _deletion_receipt = deletion.commit(|_| true).unwrap();
    assert_eq!(
        VerifiedIndex::open(temp.path())
            .unwrap()
            .semantic_eligible_event_count()
            .unwrap(),
        0
    );
}

#[test]
fn semantic_pages_select_addresses_before_decoding_and_bound_retained_core_bytes() {
    const INELIGIBLE_EVENTS: u64 = 2_048;
    let temp = tempdir().unwrap();
    let source = source("semantic-address-first.jsonl");
    let large_body = format!("x{}", " ".repeat(512 * 1024 - 1));
    let mut expected = Vec::new();
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
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
    ctx_history_index_query::reset_stored_event_record_materializations();
    let metadata_page = index.semantic_event_page(None, 1).unwrap();
    assert_eq!(metadata_page.eligible_total, 3);
    assert_eq!(metadata_page.items.len(), 1);
    assert_eq!(
        ctx_history_index_query::stored_event_record_materializations(),
        1,
        "the exact indexed total and eligibility filter must not decode the corpus"
    );
    let budget = CoreEventPageBudget::new(1, 1);
    let mut cursor = None;
    let mut actual = Vec::new();
    loop {
        ctx_history_index_query::reset_stored_core_event_record_materializations();
        let page = index
            .core_semantic_event_page_with_budget(cursor.as_ref(), 64, budget)
            .unwrap();
        assert_eq!(page.eligible_total, 3);
        assert_eq!(page.items.len(), 1);
        assert!(page.encoded_core_bytes <= ctx_history_core::MAX_ENCODED_CORE_RECORD_BYTES);
        assert!(page.content_bytes <= ctx_history_core::MAX_CORE_CONTENT_BYTES);
        assert_eq!(
            ctx_history_index_query::stored_core_event_record_materializations(),
            1,
            "a one-item semantic page decodes only its returned Core record"
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
        .into_writer()
        .unwrap()
        .commit(|_| true)
        .unwrap();
    let empty = VerifiedIndex::open(temp.path()).unwrap();
    ctx_history_index_query::reset_stored_event_record_materializations();
    let page = empty.semantic_event_page(None, 1).unwrap();
    assert_eq!(page.eligible_total, 0);
    assert!(page.items.is_empty());
    assert!(page.terminal);
    assert!(page.next_cursor.is_none());
    assert_eq!(
        ctx_history_index_query::stored_event_record_materializations(),
        0
    );

    let source = source("final-page.jsonl");
    let expected = document(&source, 1, "only eligible event");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
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
    let mut first_writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
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
    replace_literal_fact(
        &mut rewritten_first,
        LiteralFactKind::Workspace,
        "rewritten-workspace",
    );
    let replacement = document(&source, 3, "replacement third event");
    let mut replacement_writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
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
    assert!(rewritten_records
        .iter()
        .any(|event| event.event_id == rewritten_first.event_id));
    assert!(
        rewritten_pin
            .core_record_by_id(rewritten_first.event_id.as_uuid())
            .unwrap()
            .unwrap()
            .content
            .activity
            .as_ref()
            .unwrap()
            .facts
            .iter()
            .any(|fact| fact.kind == LiteralFactKind::Workspace
                && fact.value == "rewritten-workspace")
    );
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

    let mut deletion_writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let (deletion, inventory) = deletion_evidence(&source, 3);
    deletion_writer.delete_source(deletion, inventory).unwrap();
    deletion_writer.commit(|_| true).unwrap();
    let deleted_pin = VerifiedIndex::open(temp.path()).unwrap();

    assert!(page_all(&deleted_pin).is_empty());
    assert_eq!(page_all(&old_pin).len(), 2);
    assert_eq!(page_all(&rewritten_pin).len(), 2);
}
