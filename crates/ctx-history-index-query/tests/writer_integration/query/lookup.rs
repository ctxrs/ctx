use super::*;

#[test]
fn pinned_query_api_returns_typed_records_in_deterministic_order() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let first = document(&source, 1, "atomic generation");
    let second = document(&source, 2, "atomic generation");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(second.clone()).unwrap();
    writer.add_core_record(first.clone()).unwrap();
    writer.certify_source(certificate(&source, 1, 2)).unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(index.session_count().unwrap(), 1);
    assert_eq!(index.event_type_count("message").unwrap(), 2);
    assert_eq!(index.event_type_count("tool_call").unwrap(), 0);
    let candidates = index
        .search_event_candidates("atomic:generation", 10)
        .unwrap();
    let mut expected_search_ids = vec![second.event_id.as_uuid(), first.event_id.as_uuid()];
    expected_search_ids.sort();
    assert_eq!(
        candidates
            .iter()
            .map(|candidate| candidate.event.event_id)
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
    assert_eq!(exact.agent_scope, Some(CoreAgentScope::Primary));

    let event_id = first.event_id.to_string();
    let event_prefix = &event_id[..8];
    ctx_history_index_query::reset_stored_event_record_materializations();
    assert_eq!(
        index.event_ids_by_id_prefix(event_prefix).unwrap(),
        vec![first.event_id.as_uuid()]
    );
    assert_eq!(
        ctx_history_index_query::stored_event_record_materializations(),
        0,
        "IDs-only event prefix resolution must not load Core"
    );
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
    ctx_history_index_query::reset_stored_event_record_materializations();
    assert_eq!(
        index.session_ids_by_id_prefix(session_prefix).unwrap(),
        vec![first.session_id.as_uuid()]
    );
    assert_eq!(
        ctx_history_index_query::stored_event_record_materializations(),
        0,
        "IDs-only session prefix resolution must not load Core"
    );
    assert_eq!(
        index.sessions_by_id_prefix(session_prefix).unwrap(),
        vec![session]
    );
}

#[test]
fn session_count_excludes_sessions_removed_by_source_replacement() {
    let temp = tempdir().unwrap();
    let source = source("replaced-session.jsonl");

    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_core_record(document_for_session(&source, "retired", 1, "retired"))
        .unwrap();
    initial.certify_source(certificate(&source, 1, 1)).unwrap();
    initial.commit(|_| true).unwrap();

    let mut replacement = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    replacement.begin_source(source.clone()).unwrap();
    replacement
        .add_core_record(document_for_session(&source, "current", 1, "current"))
        .unwrap();
    replacement
        .certify_source(certificate(&source, 2, 1))
        .unwrap();
    replacement.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(index.session_count().unwrap(), 1);
    assert_eq!(index.event_type_count("message").unwrap(), 1);
}

#[test]
fn decoded_core_event_preserves_searchable_literal_files_in_provider_order() {
    let temp = tempdir().unwrap();
    let source = source("repository-files.jsonl");
    let mut expected = document(&source, 1, "repository file activity");
    for file in ["src/lib.rs", "src/lib.rs", "src/new.rs", "src/old.rs"] {
        add_literal_fact(&mut expected, LiteralFactKind::File, file);
    }
    expected.validate_contract().unwrap();

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(expected.clone()).unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    let decoded = index
        .core_event_by_id(expected.event_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(decoded.core_record, expected);
    let files = decoded
        .core_record
        .content
        .activity
        .as_ref()
        .unwrap()
        .facts
        .iter()
        .filter(|fact| fact.kind == LiteralFactKind::File)
        .map(|fact| fact.value.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        files,
        ["src/lib.rs", "src/lib.rs", "src/new.rs", "src/old.rs"]
    );
    for file in ["SRC/LIB.RS", "src/new.rs", "src/old.rs"] {
        let matches = index
            .search_event_candidates_with_filters(
                "repository:file:activity",
                &EventSearchFilters {
                    file: Some(file.to_owned()),
                    ..EventSearchFilters::default()
                },
                10,
            )
            .unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].event.event_id, expected.event_id.as_uuid());
    }
}

#[test]
fn literal_file_fact_is_searchable_without_repository_attribution() {
    let temp = tempdir().unwrap();
    let source = source("unknown-repository-files.jsonl");
    let mut unknown = document(&source, 1, "unknownoriginsearchneedle");
    add_literal_fact(&mut unknown, LiteralFactKind::File, "src/unknown.rs");
    unknown.validate_contract().unwrap();

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(unknown.clone()).unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open(temp.path()).unwrap();

    assert_eq!(
        index
            .search_event_candidates("unknownoriginsearchneedle", 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        index
            .search_event_candidates_with_filters(
                "unknownoriginsearchneedle",
                &EventSearchFilters {
                    file: Some("SRC/UNKNOWN.RS".to_owned()),
                    ..EventSearchFilters::default()
                },
                10,
            )
            .unwrap()
            .into_iter()
            .map(|candidate| candidate.event.event_id)
            .collect::<Vec<_>>(),
        vec![unknown.event_id.as_uuid()]
    );
}

#[test]
fn escape_heavy_metadata_round_trips_from_the_single_stored_core_record() {
    const ESCAPED_FIELD_BYTES: usize = 17 * 1024;
    const NATIVE_ID_BYTES: usize = 60 * 1024;

    let temp = tempdir().unwrap();
    let source = source("escape-heavy-core-metadata.jsonl");
    let mut record = document(&source, 1, "small searchable body");
    let escaped = "\u{0001}".repeat(ESCAPED_FIELD_BYTES);
    record.provider_session_id = Some(escaped.clone());
    record.event_type = escaped.clone();
    record.role = Some(escaped.clone());
    record.parser_revision = escaped.clone();
    replace_literal_fact(&mut record, LiteralFactKind::Branch, escaped.clone());
    replace_literal_fact(&mut record, LiteralFactKind::Workspace, escaped.clone());
    replace_literal_fact(&mut record, LiteralFactKind::SessionCwd, escaped);
    record.native_event_id = Some(TypedKey::utf8("\u{0002}".repeat(NATIVE_ID_BYTES)).unwrap());
    record.validate_contract().unwrap();
    assert!(record.encode_stored().unwrap().len() > 1024 * 1024);

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(record.clone()).unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    let indexed = index
        .event_by_id(record.event_id.as_uuid())
        .unwrap()
        .unwrap();
    let core = index
        .core_event_by_id(record.event_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(indexed, core.event);
    assert_eq!(core.core_record, record);
    assert_eq!(indexed.provider_session_id, record.provider_session_id);
    assert_eq!(indexed.native_event_id, record.native_event_id);
    let (searcher, _) = open_unverified_generation(temp.path());
    assert!(searcher.schema().get_field("query_metadata").is_err());
}

#[test]
fn semantic_pairing_many_user_turns_uses_bounded_direct_session_pages() {
    const TURNS: u64 = 256;
    const PAIRING_PAGE_ITEMS: usize = 4;

    let temp = tempdir().unwrap();
    let source = source("many-semantic-turns.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
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
    ctx_history_index_query::reset_session_event_order_term_visits();
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
    let term_visits = ctx_history_index_query::session_event_order_term_visits();
    assert!(
        term_visits <= TURNS as usize * PAIRING_PAGE_ITEMS * crate::LEXICAL_SEGMENT_MERGE_FAN_IN,
        "direct pairing term visits must stay linear in user turns: {term_visits}"
    );
    assert!(term_visits < (TURNS * TURNS) as usize);
}

#[test]
fn semantic_pairing_rejects_excluded_anchor_and_skips_excluded_assistant_content() {
    let temp = tempdir().unwrap();
    let source = source("semantic-pairing-retrieval-derived.jsonl");
    let user = document(&source, 1, "question");
    let mut ordinary_assistant = document(&source, 2, "ordinary assistant answer");
    ordinary_assistant.role = Some("assistant".to_owned());
    ordinary_assistant.validate_contract().unwrap();
    let mut excluded_assistant = document(&source, 3, "retrieval payload must not pair");
    excluded_assistant.role = Some("assistant".to_owned());
    excluded_assistant.validate_contract().unwrap();
    let excluded_assistant = retrieval_excluded(excluded_assistant);
    let next_user = document(&source, 4, "next question");
    let excluded_anchor = retrieval_excluded(document(&source, 5, "excluded question"));

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for record in [
        user.clone(),
        ordinary_assistant.clone(),
        excluded_assistant.clone(),
        next_user,
        excluded_anchor.clone(),
    ] {
        writer.add_core_record(record).unwrap();
    }
    writer.certify_source(certificate(&source, 1, 5)).unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let anchor = index
        .core_event_by_id(user.event_id.as_uuid())
        .unwrap()
        .unwrap();
    let paired = index
        .semantic_lite_turn_assistant(&anchor, 4, DEFAULT_CORE_EVENT_PAGE_BUDGET)
        .unwrap()
        .unwrap();
    assert_eq!(paired.0, "ordinary assistant answer");
    assert_eq!(paired.1, ordinary_assistant.occurred_at_unix_ms.unwrap());

    let excluded_anchor = index
        .core_event_by_id(excluded_anchor.event_id.as_uuid())
        .unwrap()
        .unwrap();
    assert!(matches!(
        index.semantic_lite_turn_assistant(&excluded_anchor, 4, DEFAULT_CORE_EVENT_PAGE_BUDGET),
        Err(IndexError::InvalidStoredDocumentField(_))
    ));
    assert_eq!(
        index
            .core_record_by_id(excluded_assistant.event_id.as_uuid())
            .unwrap()
            .unwrap(),
        excluded_assistant
    );
}

#[test]
fn semantic_pairing_preserves_copied_assistant_content() {
    let temp = tempdir().unwrap();
    let source = source("semantic-pairing-copied-assistant.jsonl");
    let mut ancestor = document_for_session(&source, "ancestor", 1, "copied answer must not pair");
    ancestor.role = Some("assistant".to_owned());
    let mut user = document_for_session(&source, "child", 1, "question");
    let mut ordinary_assistant =
        document_for_session(&source, "child", 2, "ordinary assistant answer");
    ordinary_assistant.role = Some("assistant".to_owned());
    let mut copied_assistant = document_for_session(&source, "child", 3, "copied answer must pair");
    copied_assistant.role = Some("assistant".to_owned());
    let mut next_user = document_for_session(&source, "child", 4, "next question");

    for record in [
        &mut user,
        &mut ordinary_assistant,
        &mut copied_assistant,
        &mut next_user,
    ] {
        record.parent_session_id = Some(ancestor.session_id);
        record.root_session_id = Some(ancestor.session_id);
        record.session_relationship = Some(ProviderNativeSessionRelationship::Forked);
    }
    copied_assistant.event_copy = Some(ProviderNativeEventCopy {
        ancestor_session_id: ancestor.session_id,
        ancestor_event_id: ancestor.event_id,
        proof: ProviderNativeCopyProof::NativeEventIdentity,
    });
    for record in [
        &ancestor,
        &user,
        &ordinary_assistant,
        &copied_assistant,
        &next_user,
    ] {
        record.validate_contract().unwrap();
    }

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for record in [
        ancestor,
        user.clone(),
        ordinary_assistant.clone(),
        copied_assistant.clone(),
        next_user,
    ] {
        writer.add_core_record(record).unwrap();
    }
    writer.certify_source(certificate(&source, 1, 5)).unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let anchor = index
        .core_event_by_id(user.event_id.as_uuid())
        .unwrap()
        .unwrap();
    let paired = index
        .semantic_lite_turn_assistant(&anchor, 4, DEFAULT_CORE_EVENT_PAGE_BUDGET)
        .unwrap()
        .unwrap();

    assert_eq!(paired.0, "copied answer must pair");
    assert_eq!(paired.1, copied_assistant.occurred_at_unix_ms.unwrap());
    assert_eq!(
        index
            .core_record_by_id(copied_assistant.event_id.as_uuid())
            .unwrap()
            .unwrap(),
        copied_assistant
    );
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
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
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
    ctx_history_index_query::reset_stored_core_event_record_materializations();
    ctx_history_index_query::reset_session_event_order_term_visits();
    let paired = index
        .semantic_lite_turn_assistant(&anchor, 64, DEFAULT_CORE_EVENT_PAGE_BUDGET)
        .unwrap()
        .unwrap();

    assert_eq!(paired.0, "answer beyond old window");
    assert_eq!(paired.1, assistant.occurred_at_unix_ms.unwrap());
    assert_eq!(
        ctx_history_index_query::stored_core_event_record_materializations(),
        1,
        "tool metadata traversal must hydrate only the paired assistant body"
    );
    let term_visits = ctx_history_index_query::session_event_order_term_visits();
    assert!(term_visits > 64);
    assert!(
        term_visits <= 2 * 64 * crate::LEXICAL_SEGMENT_MERGE_FAN_IN,
        "tool-heavy pairing must remain page bounded: {term_visits}"
    );
}

#[test]
fn semantic_pairing_many_segments_merges_each_order_term_once_across_pages_and_reopen() {
    const SEGMENTS: u64 = 6;
    const EVENTS_PER_SEGMENT: u64 = 6;
    const TOTAL_EVENTS: u64 = SEGMENTS * EVENTS_PER_SEGMENT;
    const FIRST_ASSISTANT_SEQUENCE: u64 = TOTAL_EVENTS - EVENTS_PER_SEGMENT * 2 + 1;
    const LAST_ASSISTANT_SEQUENCE: u64 = TOTAL_EVENTS - EVENTS_PER_SEGMENT;
    const PAGE_ITEMS: usize = 3;
    const ASSISTANT_EVENTS: usize = SEGMENTS as usize;

    let temp = tempdir().unwrap();
    let source = source("many-segment-semantic-turn.jsonl");
    let mut anchor_id = None;
    let mut latest_assistant = None;
    for segment_index in 0..SEGMENTS {
        let revision = (segment_index + 1) as u8;
        let retained_events = (segment_index + 1) * EVENTS_PER_SEGMENT;
        let retained_bytes = retained_events * 10;
        let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap();
        writer.test_disable_merges().unwrap();
        let append_base = if segment_index == 0 {
            writer.begin_source(source.clone()).unwrap();
            None
        } else {
            Some(writer.begin_source_append(source.clone()).unwrap().clone())
        };

        // Interleave every segment's sequence range and insert each run in
        // reverse so only the indexed key can define global traversal order.
        for sequence in (0..EVENTS_PER_SEGMENT)
            .rev()
            .map(|event_index| segment_index + 1 + event_index * SEGMENTS)
        {
            let is_next_user = sequence == TOTAL_EVENTS;
            let is_assistant =
                (FIRST_ASSISTANT_SEQUENCE..=LAST_ASSISTANT_SEQUENCE).contains(&sequence);
            let mut event = document(
                &source,
                sequence,
                if sequence == 1 {
                    "many-segment question".to_owned()
                } else if is_next_user {
                    "next question".to_owned()
                } else if is_assistant {
                    format!("answer {sequence}")
                } else {
                    format!("tool body {sequence}")
                }
                .as_str(),
            );
            if sequence == 1 {
                anchor_id = Some(event.event_id.as_uuid());
            } else if is_assistant {
                event.role = Some("assistant".to_owned());
                if sequence == LAST_ASSISTANT_SEQUENCE {
                    latest_assistant = Some((
                        format!("answer {sequence}"),
                        event.occurred_at_unix_ms.unwrap(),
                    ));
                }
            } else if !is_next_user {
                event.event_type = "tool_output".to_owned();
                event.role = Some("tool".to_owned());
            }
            writer.add_core_record(event).unwrap();
        }

        let certified = appendable_certificate(&source, revision, retained_events, retained_bytes);
        if let Some(base) = append_base {
            writer
                .certify_source_append(
                    CertifiedSourceAppend::certify(
                        &base,
                        certified,
                        retained_bytes - EVENTS_PER_SEGMENT * 10,
                        [revision - 1; 32],
                    )
                    .unwrap(),
                )
                .unwrap();
        } else {
            writer.certify_source(certified).unwrap();
        }
        writer.commit(|_| true).unwrap();
    }

    let anchor_id = anchor_id.unwrap();
    let expected_latest = latest_assistant.unwrap();
    let active_segment_count = open_unverified_generation(temp.path())
        .0
        .segment_readers()
        .len();
    let assert_traversal = |index: &VerifiedIndex| {
        assert!(
            active_segment_count >= SEGMENTS as usize,
            "test requires one live segment per append"
        );
        let anchor = index.core_event_by_id(anchor_id).unwrap().unwrap();
        ctx_history_index_query::reset_stored_core_event_record_materializations();
        ctx_history_index_query::reset_session_event_order_term_visits();
        let paired = index
            .semantic_lite_turn_assistant(&anchor, PAGE_ITEMS, DEFAULT_CORE_EVENT_PAGE_BUDGET)
            .unwrap()
            .unwrap();

        assert_eq!(paired, expected_latest);
        assert_eq!(
            ctx_history_index_query::stored_core_event_record_materializations(),
            ASSISTANT_EVENTS,
            "tool records must remain body-free and no assistant may be skipped or decoded twice"
        );
        assert_eq!(
            ctx_history_index_query::session_event_order_term_visits(),
            (TOTAL_EVENTS - 1) as usize,
            "each globally considered order term must be decoded exactly once"
        );
        assert_eq!(
            ctx_history_index_query::session_event_order_visited_sequences(),
            (2..=TOTAL_EVENTS).collect::<Vec<_>>(),
            "merged pages must preserve exact order without skips or duplicates"
        );
    };

    let first_pin = VerifiedIndex::open(temp.path()).unwrap();
    assert_traversal(&first_pin);
    drop(first_pin);
    let reopened = VerifiedIndex::open_pinned(temp.path()).unwrap();
    assert_traversal(&reopened);
}

#[test]
fn lexical_candidates_remain_thin_while_other_collectors_decode_only_selected_results() {
    const EVENT_COUNT: u64 = 64;
    const AMBIGUITY_LIMIT: usize = 2;

    let temp = tempdir().unwrap();
    let source = source("metadata-hot-paths.jsonl");
    let mut event_ids = Vec::new();
    let mut session_ids = Vec::new();
    let mut bodies_by_session = std::collections::BTreeMap::new();
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
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

    ctx_history_index_query::reset_stored_event_record_materializations();
    ctx_history_index_query::reset_stored_core_event_record_materializations();
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    assert_eq!(
        ctx_history_index_query::stored_core_event_record_materializations(),
        0
    );

    session_ids.sort();
    session_ids.dedup();
    ctx_history_index_query::reset_stored_event_record_materializations();
    let provider_sessions = index
        .sessions_by_provider_session_id("shared-provider-session", Some("codex"), None, None)
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
        ctx_history_index_query::stored_event_record_materializations(),
        AMBIGUITY_LIMIT * 2,
        "provider-session ambiguity lookup must decode only one identity bootstrap and one selected event per retained session"
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
    ctx_history_index_query::reset_stored_event_record_materializations();
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
        ctx_history_index_query::stored_event_record_materializations(),
        AMBIGUITY_LIMIT * 2
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
    ctx_history_index_query::reset_stored_event_record_materializations();
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
        ctx_history_index_query::stored_event_record_materializations(),
        AMBIGUITY_LIMIT
    );

    ctx_history_index_query::reset_stored_event_record_materializations();
    let candidates = index.search_event_candidates("ambiguity", 5).unwrap();
    assert_eq!(candidates.len(), 5);
    assert_eq!(
        ctx_history_index_query::stored_event_record_materializations(),
        0,
        "lexical ranking must return thin references without decoding Core"
    );
    assert_eq!(
        ctx_history_index_query::stored_core_event_record_materializations(),
        0
    );

    ctx_history_index_query::reset_stored_event_record_materializations();
    let source_page = index.source_event_page(&source, None, 5).unwrap();
    assert_eq!(source_page.items.len(), 5);
    assert_eq!(
        ctx_history_index_query::stored_event_record_materializations(),
        5
    );
    assert_eq!(
        ctx_history_index_query::stored_core_event_record_materializations(),
        0
    );

    ctx_history_index_query::reset_stored_event_record_materializations();
    let semantic_page = index.semantic_event_page(None, 5).unwrap();
    assert_eq!(semantic_page.items.len(), 5);
    assert_eq!(
        ctx_history_index_query::stored_event_record_materializations(),
        5
    );
    assert_eq!(
        ctx_history_index_query::stored_core_event_record_materializations(),
        0
    );

    ctx_history_index_query::reset_stored_event_record_materializations();
    let session_id = session_ids[0];
    assert_eq!(
        index
            .core_content_bytes_for_session_if_bounded(session_id, 1)
            .unwrap(),
        Some(bodies_by_session[&session_id])
    );
    assert_eq!(
        ctx_history_index_query::stored_event_record_materializations(),
        0
    );
    assert_eq!(
        ctx_history_index_query::stored_core_event_record_materializations(),
        0
    );
}

#[test]
fn corrupt_stored_core_fails_closed_during_exhaustive_open() {
    use tantivy::schema::Document as _;

    let temp = tempdir().unwrap();
    let source = source("corrupt-stored-core.jsonl");
    let event = document(&source, 1, "complete Core body");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
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
        if field != fields.core_record && field != fields.core_record_encoded_bytes {
            malformed.add_field_value(field, value);
        }
    }
    malformed.add_u64(fields.core_record_encoded_bytes, 1);
    malformed.add_bytes(fields.core_record, b"{");
    drop(searcher);

    let directory = DurableMmapDirectory::open(active_generation_path(temp.path())).unwrap();
    let index = Index::open(directory).unwrap();
    publish_unchecked_generation(
        temp.path(),
        &index,
        manifest,
        std::slice::from_ref(&source),
        vec![malformed],
    );

    assert!(matches!(
        VerifiedIndex::open(temp.path()),
        Err(IndexError::CoreRecord(_))
    ));
}

#[path = "lookup/additional.rs"]
mod additional;
