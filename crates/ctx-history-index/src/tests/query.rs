use super::*;

#[test]
fn pinned_query_api_returns_typed_records_in_deterministic_order() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let first = document(&source, 1, "atomic generation");
    let second = document(&source, 2, "atomic generation");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_document(second.clone()).unwrap();
    writer.add_document(first.clone()).unwrap();
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
    assert_eq!(exact.locator, first.locator);
    assert_eq!(exact.provider, "codex");
    assert_eq!(exact.source_format, "codex_session_jsonl");
    assert_eq!(exact.provider_session_id.as_deref(), Some("session"));
    assert_eq!(exact.event_sequence, 1);
    assert_eq!(exact.occurred_at_unix_ms, first.occurred_at_unix_ms);
    assert_eq!(exact.event_type, "message");
    assert_eq!(exact.role.as_deref(), Some("user"));
    assert_eq!(exact.workspace.as_deref(), Some("ctx"));
    assert_eq!(exact.cwd.as_deref(), Some("/work/ctx"));
    assert_eq!(exact.touched_files, vec!["src/lib.rs"]);

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
    writer.add_document(cjk.clone()).unwrap();
    writer.add_document(identifier.clone()).unwrap();
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
    writer.add_document(partial.clone()).unwrap();
    writer.add_document(unrelated).unwrap();
    writer.add_document(exact.clone()).unwrap();
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
    first.add_document(target_fourth.clone()).unwrap();
    first.add_document(target_first.clone()).unwrap();
    first
        .certify_source(appendable_certificate(&target, 1, 2, 20))
        .unwrap();
    first.begin_source(other.clone()).unwrap();
    first.add_document(other_second.clone()).unwrap();
    first.add_document(other_first.clone()).unwrap();
    first.certify_source(certificate(&other, 1, 2)).unwrap();
    first.commit(|_| true).unwrap();

    let mut append = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    append
        .writer_mut()
        .unwrap()
        .set_merge_policy(Box::<NoMergePolicy>::default());
    let base = append.begin_source_append(target.clone()).unwrap().clone();
    append.add_document(target_third.clone()).unwrap();
    append.add_document(target_second.clone()).unwrap();
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
    assert_eq!(first_page.generation_id, index.generation_id());
    assert!(first_page.source.exact_descriptor_eq(&target));
    assert!(!first_page.terminal);
    assert_eq!(
        first_page
            .items
            .iter()
            .map(|event| event.event_id)
            .collect::<Vec<_>>(),
        expected[..2]
    );
    assert!(first_page
        .items
        .iter()
        .all(|event| event.locator.source().exact_descriptor_eq(&target)));

    let serialized = serde_json::to_vec(first_page.next_cursor.as_ref().unwrap()).unwrap();
    let cursor: SourceEventCursor = serde_json::from_slice(&serialized).unwrap();
    assert_eq!(cursor.generation_id(), index.generation_id());
    assert!(cursor.source().exact_descriptor_eq(&target));
    assert_eq!(cursor.after(), expected[1]);
    let final_page = index.source_event_page(&target, Some(&cursor), 2).unwrap();
    assert!(final_page.terminal);
    assert!(final_page.next_cursor.is_none());
    assert_eq!(
        final_page
            .items
            .iter()
            .map(|event| event.event_id)
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
        .all(|event| event.locator.source().exact_descriptor_eq(&target)));

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
fn source_event_pages_bind_generation_descriptor_and_bounds() {
    const { assert!(MAX_SOURCE_EVENT_PAGE_ITEMS <= 4_096) };
    let temp = tempdir().unwrap();
    let source = source("rewrite-delete-pages.jsonl");
    let old_first = document(&source, 1, "old first");
    let old_second = document(&source, 2, "old second");
    let mut first = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    first.begin_source(source.clone()).unwrap();
    first.add_document(old_second.clone()).unwrap();
    first.add_document(old_first.clone()).unwrap();
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
    rewriting.add_document(replacement.clone()).unwrap();
    rewriting.add_document(rewritten_first.clone()).unwrap();
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
        writer.add_document(document).unwrap();
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
    assert_eq!(first_page.generation_id, index.generation_id());
    assert_eq!(
        first_page.eligibility,
        SemanticEligibility::UserMessageCandidateV2
    );
    assert_eq!(first_page.eligible_total, 4);
    assert_eq!(first_page.eligible_count(), 2);
    assert!(!first_page.terminal);
    assert_eq!(
        first_page
            .items
            .iter()
            .map(|event| event.event_id)
            .collect::<Vec<_>>(),
        expected[..2]
    );
    assert_eq!(first_page.items[0].locator.source(), &source);
    assert_eq!(
        first_page.items[0].root_session_id,
        first_page.items[0].session_id
    );

    let cursor = first_page.next_cursor.unwrap();
    assert_eq!(cursor.generation_id(), index.generation_id());
    assert_eq!(cursor.eligibility(), SemanticEligibility::CURRENT);
    assert_eq!(cursor.after(), expected[1]);

    let final_page = index.semantic_event_page(Some(&cursor), 2).unwrap();
    assert_eq!(final_page.eligible_total, 4);
    assert_eq!(final_page.eligible_count(), 2);
    assert_eq!(
        final_page
            .items
            .iter()
            .map(|event| event.event_id)
            .collect::<Vec<_>>(),
        expected[2..]
    );
    assert!(final_page.terminal);
    assert!(final_page.next_cursor.is_none());
    assert_eq!(index.semantic_eligible_event_count().unwrap(), 4);
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
    writer.add_document(expected.clone()).unwrap();
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
    first_writer.add_document(old_second.clone()).unwrap();
    first_writer.add_document(old_first.clone()).unwrap();
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
        .add_document(replacement.clone())
        .unwrap();
    replacement_writer
        .add_document(rewritten_first.clone())
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
    root.source_path = Some("/history/ctx[root].jsonl".to_owned());
    root.occurred_at_unix_ms = Some(100);
    let root_session_id = root.session_id;
    root.root_session_id = root_session_id;

    let mut child = document_for_session(&codex_child, "child-thread", 2, "shared needle");
    child.parent_session_id = Some(root_session_id);
    child.root_session_id = root_session_id;
    child.branch = Some("feature/query-seam".to_owned());
    child.workspace = Some("ChildSpace".to_owned());
    child.cwd = Some("/work/child".to_owned());
    child.source_path = Some("/history/child.jsonl".to_owned());
    child.agent_type = "subagent".to_owned();
    child.is_primary = false;
    child.event_type = "tool_call".to_owned();
    child.role = Some("assistant".to_owned());
    child.occurred_at_unix_ms = Some(200);
    child.touched_files = vec!["crates/Query.rs".to_owned()];
    let child_session_id = child.session_id;

    let mut other = document_for_session(&claude, "other-thread", 3, "shared needle");
    other.workspace = Some("Elsewhere".to_owned());
    other.branch = Some("release".to_owned());
    other.occurred_at_unix_ms = Some(300);
    let other_session_id = other.session_id;
    other.root_session_id = other_session_id;

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(codex_root.clone()).unwrap();
    writer.add_document(root).unwrap();
    writer
        .certify_source(certificate(&codex_root, 1, 1))
        .unwrap();
    writer.begin_source(codex_child.clone()).unwrap();
    writer.add_document(child).unwrap();
    writer
        .certify_source(certificate(&codex_child, 1, 1))
        .unwrap();
    writer.begin_source(claude.clone()).unwrap();
    writer.add_document(other).unwrap();
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
                workspace: Some("CTX[ROOT]".to_owned()),
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
                file: Some("QUERY.RS".to_owned()),
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
    assert_eq!(child.source_path.as_deref(), Some("/history/child.jsonl"));
    assert_eq!(child.agent_type, "subagent");
    assert!(!child.is_primary);
}

#[test]
fn full_body_is_searchable_but_never_stored_or_returned() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let body = format!("{} tailonlyneedle", "界".repeat(16_384));
    let expected = document(&source, 1, &body);
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_document(expected.clone()).unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    let record = index
        .event_by_id(expected.event_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(record.locator, expected.locator);
    assert_eq!(
        index.search_event_candidates("tailonlyneedle", 10).unwrap()[0]
            .event
            .event_id,
        expected.event_id
    );

    let fields = fields_from_schema(index.searcher.schema()).unwrap();
    let address = index
        .searcher
        .search(&AllQuery, &DocSetCollector)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let stored: TantivyDocument = index.searcher.doc(address).unwrap();
    assert!(stored.get_first(fields.body_search).is_none());
}

#[test]
fn empty_or_invalid_programmatic_queries_are_safe() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_document(document(&source, 1, "body")).unwrap();
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
