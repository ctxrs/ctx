use super::*;

use ctx_history_core::StableEntityId;
use ctx_history_index_query::SearchContentScope;
use tantivy::{collector::TopDocs, Score};

#[test]
fn copied_events_remain_searchable_and_preserve_exact_copy_claims() {
    let temp = tempdir().unwrap();
    let source = source("copied-search-shaping.jsonl");
    let original = document_for_session(
        &source,
        "original-session",
        1,
        "lineagewindowneedle shared body",
    );
    let mut copied = document_for_session(
        &source,
        "copied-session",
        2,
        "lineagewindowneedle shared body",
    );
    copied.parent_session_id = Some(original.session_id);
    copied.root_session_id = Some(original.session_id);
    copied.session_relationship = Some(ProviderNativeSessionRelationship::Forked);
    copied.event_copy = Some(ProviderNativeEventCopy {
        ancestor_session_id: original.session_id,
        ancestor_event_id: original.event_id,
        proof: ProviderNativeCopyProof::NativeEventIdentity,
    });
    copied.validate_contract().unwrap();
    let unknown = document_for_session(
        &source,
        "unknown-session",
        3,
        "lineagewindowneedle unknown body",
    );
    let unique = document_for_session(
        &source,
        "unique-session",
        4,
        "lineagewindowneedle unique body",
    );

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for record in [
        copied.clone(),
        unique.clone(),
        unknown.clone(),
        original.clone(),
    ] {
        writer.add_core_record(record).unwrap();
    }
    writer.certify_source(certificate(&source, 1, 4)).unwrap();
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();

    let expected = HashSet::from([
        original.event_id,
        copied.event_id,
        unknown.event_id,
        unique.event_id,
    ]);
    let expected_digests = expected
        .iter()
        .map(|event_id| event_id.digest())
        .collect::<HashSet<_>>();
    let lexical = index
        .search_event_candidates("lineagewindowneedle", 4)
        .unwrap();
    assert_eq!(lexical.len(), 4);
    assert_eq!(
        candidate_ids(&lexical).into_iter().collect::<HashSet<_>>(),
        expected_digests
    );
    let listed = index
        .list_event_candidates_with_filters(&EventSearchFilters::default(), 4)
        .unwrap();
    assert_eq!(listed.len(), 4);
    assert_eq!(
        candidate_ids(&listed).into_iter().collect::<HashSet<_>>(),
        expected_digests
    );

    let semantic = index
        .semantic_filter_projection(&EventSearchFilters::default())
        .unwrap();
    assert_eq!(
        semantic.event_ids().collect::<HashSet<_>>(),
        expected
            .iter()
            .map(|event_id| event_id.as_uuid())
            .collect::<HashSet<_>>()
    );
    let semantic_page = index.semantic_event_page(None, 4).unwrap();
    assert_eq!(semantic_page.items.len(), 4);
    assert_eq!(semantic_page.eligible_total, 4);

    let source_page = index.core_source_event_page(&source, None, 4).unwrap();
    assert_eq!(source_page.items.len(), 4);
    let visible_copy = index
        .core_record_by_id(copied.event_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(visible_copy.event_copy, copied.event_copy);
    let copied_session = index
        .core_events_for_session(copied.session_id.as_uuid())
        .unwrap();
    assert_eq!(copied_session.len(), 1);
    assert_eq!(copied_session[0].event_copy, copied.event_copy);
}

#[test]
fn multiple_exact_session_exclusions_filter_lexical_and_semantic_candidates() {
    let temp = tempdir().unwrap();
    let source = source("multiple-session-exclusions.jsonl");
    let first = document_for_session(&source, "first-session", 1, "multiple exclusion needle");
    let second = document_for_session(&source, "second-session", 2, "multiple exclusion needle");
    let retained =
        document_for_session(&source, "retained-session", 3, "multiple exclusion needle");

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for record in [first.clone(), second.clone(), retained.clone()] {
        writer.add_core_record(record).unwrap();
    }
    writer.certify_source(certificate(&source, 1, 3)).unwrap();
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let filters = EventSearchFilters {
        excluded_session_ids: vec![first.session_id.as_uuid(), second.session_id.as_uuid()],
        ..EventSearchFilters::default()
    };

    let lexical = index
        .search_event_candidates_with_filters("multiple exclusion needle", &filters, 10)
        .unwrap();
    assert_eq!(
        lexical
            .iter()
            .map(|candidate| candidate.event.session_id)
            .collect::<Vec<_>>(),
        vec![retained.session_id.as_uuid()]
    );
    let listed = index
        .list_event_candidates_with_filters(&filters, 10)
        .unwrap();
    assert_eq!(
        listed
            .iter()
            .map(|candidate| candidate.event.session_id)
            .collect::<Vec<_>>(),
        vec![retained.session_id.as_uuid()]
    );
    let semantic = index.semantic_filter_projection(&filters).unwrap();
    assert_eq!(
        semantic.event_ids().collect::<Vec<_>>(),
        vec![retained.event_id.as_uuid()]
    );
}

#[test]
fn agent_scope_filter_uses_only_explicit_core_agent_scope() {
    let temp = tempdir().unwrap();
    let source = source("primary-scope-authority.jsonl");
    let primary =
        document_for_session(&source, "primary-session", 1, "primaryauthorityneedle root");
    let related = |session: &str,
                   sequence,
                   relationship: ProviderNativeSessionRelationship,
                   agent_scope: Option<CoreAgentScope>,
                   body: &str| {
        let mut record = document_for_session(&source, session, sequence, body);
        record.parent_session_id = Some(primary.session_id);
        record.root_session_id = Some(primary.session_id);
        record.session_relationship = Some(relationship);
        record.agent_scope = agent_scope;
        record.validate_contract().unwrap();
        record
    };
    let delegated = related(
        "delegated-session",
        2,
        ProviderNativeSessionRelationship::Delegated,
        Some(CoreAgentScope::Primary),
        "primaryauthorityneedle primaryauthorityneedle nonprimaryexplicitneedle",
    );
    let workflow = related(
        "workflow-session",
        3,
        ProviderNativeSessionRelationship::WorkflowChild,
        Some(CoreAgentScope::Primary),
        "primaryauthorityneedle primaryauthorityneedle nonprimaryexplicitneedle",
    );
    let forked = related(
        "forked-session",
        4,
        ProviderNativeSessionRelationship::Forked,
        Some(CoreAgentScope::Subagent),
        "primaryauthorityneedle fork",
    );
    let resumed = related(
        "resumed-session",
        5,
        ProviderNativeSessionRelationship::ResumedFrom,
        None,
        "primaryauthorityneedle resume",
    );

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for record in [
        primary.clone(),
        delegated.clone(),
        workflow.clone(),
        forked.clone(),
        resumed.clone(),
    ] {
        writer.add_core_record(record).unwrap();
    }
    writer.certify_source(certificate(&source, 1, 5)).unwrap();
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let primary_scope = EventSearchFilters {
        agent_scope: AgentScope::Primary,
        ..EventSearchFilters::default()
    };

    let lexical = index
        .search_event_candidates_with_filters("primaryauthorityneedle", &primary_scope, 3)
        .unwrap();
    let expected_primary = HashSet::from([primary.event_id, delegated.event_id, workflow.event_id]);
    let expected_primary_digests = expected_primary
        .iter()
        .map(|event_id| event_id.digest())
        .collect::<HashSet<_>>();
    assert_eq!(lexical.len(), 3);
    assert_eq!(
        candidate_ids(&lexical).into_iter().collect::<HashSet<_>>(),
        expected_primary_digests
    );

    let semantic = index.semantic_filter_projection(&primary_scope).unwrap();
    assert_eq!(
        semantic.event_ids().collect::<HashSet<_>>(),
        expected_primary
            .iter()
            .map(|event_id| event_id.as_uuid())
            .collect()
    );

    let explicit_subagent = index
        .search_event_candidates_with_filters(
            "primaryauthorityneedle",
            &EventSearchFilters {
                agent_scope: AgentScope::Subagent,
                ..EventSearchFilters::default()
            },
            2,
        )
        .unwrap();
    assert_eq!(
        candidate_ids(&explicit_subagent),
        vec![forked.event_id.digest()]
    );

    for expected in [&delegated, &workflow] {
        let explicit_session = index
            .search_event_candidates_with_filters(
                "nonprimaryexplicitneedle",
                &EventSearchFilters {
                    session_id: Some(expected.session_id.as_uuid()),
                    agent_scope: AgentScope::Primary,
                    ..EventSearchFilters::default()
                },
                1,
            )
            .unwrap();
        assert_eq!(
            candidate_ids(&explicit_session),
            vec![expected.event_id.digest()]
        );

        let direct = index
            .core_record_by_id(expected.event_id.as_uuid())
            .unwrap()
            .unwrap();
        assert_eq!(direct.session_relationship, expected.session_relationship);
        assert_eq!(direct.agent_scope, Some(CoreAgentScope::Primary));
    }
    assert!(index
        .search_event_candidates_with_filters(
            "primaryauthorityneedle",
            &EventSearchFilters {
                session_id: Some(resumed.session_id.as_uuid()),
                agent_scope: AgentScope::Primary,
                ..EventSearchFilters::default()
            },
            1,
        )
        .unwrap()
        .is_empty());
}

#[test]
fn copied_bodies_contribute_ordinary_search_postings() {
    const COPIES: u64 = 64;
    const NEEDLE: &str = "copybodystatsneedle";

    fn records_with_copy_body(source: &SourceKey, copy_body: &str) -> Vec<CoreRecord> {
        let first = document_for_session(
            source,
            "body-stats-original-first",
            1,
            "copybodystatsneedle concise",
        );
        let second = document_for_session(
            source,
            "body-stats-original-second",
            2,
            "copybodystatsneedle deliberately longer original body",
        );
        let mut records = vec![first.clone(), second];
        for offset in 0..COPIES {
            let mut copied = document_for_session(
                source,
                &format!("body-stats-copy-{offset}"),
                offset + 3,
                copy_body,
            );
            copied.parent_session_id = Some(first.session_id);
            copied.root_session_id = Some(first.session_id);
            copied.session_relationship = Some(ProviderNativeSessionRelationship::Forked);
            copied.event_copy = Some(ProviderNativeEventCopy {
                ancestor_session_id: first.session_id,
                ancestor_event_id: first.event_id,
                proof: ProviderNativeCopyProof::NativeCopiedFromField,
            });
            copied.validate_contract().unwrap();
            records.push(copied);
        }
        records
    }

    let source = source("copied-body-statistics.jsonl");
    let duplicated = records_with_copy_body(&source, "copybodystatsneedle concise");
    let expected_copy = duplicated[2].clone();
    let (duplicated_temp, duplicated_index) = publish_class_aware_records(duplicated);

    let duplicated_hits = duplicated_index
        .search_event_candidates(NEEDLE, COPIES as usize + 2)
        .unwrap();
    assert_eq!(duplicated_hits.len(), COPIES as usize + 2);
    assert!(duplicated_hits
        .iter()
        .any(|candidate| candidate.event.event_id == expected_copy.event_id.as_uuid()));

    let (searcher, _) = open_unverified_generation(duplicated_temp.path());
    let fields = fields_from_schema(searcher.schema()).unwrap();
    let term = Term::from_field_text(fields.body_search, NEEDLE);
    assert_eq!(searcher.doc_freq(&term).unwrap(), COPIES + 2);
    let raw_matches = searcher
        .search(
            &TermQuery::new(term, IndexRecordOption::WithFreqs),
            &DocSetCollector,
        )
        .unwrap();
    assert_eq!(raw_matches.len(), COPIES as usize + 2);

    let visible_copy = duplicated_index
        .core_record_by_id(expected_copy.event_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(visible_copy.event_copy, expected_copy.event_copy);
    assert_eq!(
        visible_copy.content.normalized_body,
        expected_copy.content.normalized_body
    );
    assert_eq!(
        duplicated_index
            .core_events_for_session(expected_copy.session_id.as_uuid())
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn many_retrieval_derived_bodies_add_no_postings_or_score_order_changes() {
    const EXCLUDED: u64 = 64;
    const NEEDLE: &str = "retrievalbodystatsneedle";

    fn records_with_excluded_body(source: &SourceKey, excluded_body: &str) -> Vec<CoreRecord> {
        let first = document_for_session(
            source,
            "retrieval-stats-original-first",
            1,
            "retrievalbodystatsneedle concise",
        );
        let second = document_for_session(
            source,
            "retrieval-stats-original-second",
            2,
            "retrievalbodystatsneedle deliberately longer original body",
        );
        let mut records = vec![first, second];
        for offset in 0..EXCLUDED {
            records.push(retrieval_excluded(document_for_session(
                source,
                &format!("retrieval-stats-excluded-{offset}"),
                offset + 3,
                excluded_body,
            )));
        }
        records
    }

    let source = source("retrieval-body-statistics.jsonl");
    let duplicated = records_with_excluded_body(&source, "retrievalbodystatsneedle concise");
    let expected_excluded = duplicated[2].clone();
    let control = records_with_excluded_body(&source, "unrelated retrieval body control");
    let (duplicated_temp, duplicated_index) = publish_class_aware_records(duplicated);
    let (_control_temp, control_index) = publish_class_aware_records(control);

    let duplicated_hits = duplicated_index
        .search_event_candidates(NEEDLE, EXCLUDED as usize + 2)
        .unwrap();
    let control_hits = control_index
        .search_event_candidates(NEEDLE, EXCLUDED as usize + 2)
        .unwrap();
    assert_eq!(duplicated_hits.len(), 2);
    assert_eq!(
        candidate_ids(&duplicated_hits),
        candidate_ids(&control_hits)
    );
    assert_eq!(duplicated_hits[0].score, control_hits[0].score);
    assert_eq!(duplicated_hits[1].score, control_hits[1].score);

    let (searcher, _) = open_unverified_generation(duplicated_temp.path());
    let fields = fields_from_schema(searcher.schema()).unwrap();
    let term = Term::from_field_text(fields.body_search, NEEDLE);
    assert_eq!(searcher.doc_freq(&term).unwrap(), 2);
    assert_eq!(
        searcher
            .search(
                &TermQuery::new(term, IndexRecordOption::WithFreqs),
                &DocSetCollector,
            )
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        searcher
            .search(
                &TermQuery::new(
                    Term::from_field_u64(fields.discovery_eligible, 1),
                    IndexRecordOption::Basic,
                ),
                &Count,
            )
            .unwrap(),
        2
    );

    let visible = duplicated_index
        .core_record_by_id(expected_excluded.event_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(visible.content, expected_excluded.content);
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
    replace_literal_fact(&mut root, LiteralFactKind::Workspace, "Ctx-Rich-Fixture");
    replace_literal_fact(&mut root, LiteralFactKind::SessionCwd, "/work/ctx-root");
    root.occurred_at_unix_ms = Some(100);
    let root_session_id = root.session_id;

    let mut child = document_for_session(&codex_child, "child-thread", 2, "shared needle");
    child.parent_session_id = Some(root_session_id);
    child.root_session_id = Some(root_session_id);
    child.session_relationship = Some(ProviderNativeSessionRelationship::Delegated);
    replace_literal_fact(&mut child, LiteralFactKind::Branch, "feature/query-seam");
    replace_literal_fact(&mut child, LiteralFactKind::Workspace, "ChildSpace");
    replace_literal_fact(&mut child, LiteralFactKind::SessionCwd, "/work/child");
    child.agent_scope = Some(CoreAgentScope::Subagent);
    child.event_type = "tool_call".to_owned();
    child.role = Some("assistant".to_owned());
    child.occurred_at_unix_ms = Some(200);
    let child_session_id = child.session_id;

    let mut other = document_for_session(&claude, "other-thread", 3, "shared needle");
    replace_literal_fact(&mut other, LiteralFactKind::Workspace, "Elsewhere");
    replace_literal_fact(&mut other, LiteralFactKind::Branch, "release");
    other.occurred_at_unix_ms = Some(300);
    let other_session_id = other.session_id;

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
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
                agent_scope: AgentScope::Subagent,
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
        Vec::<Uuid>::new()
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
                    session_ids: vec![root_session_id.as_uuid(), child_session_id.as_uuid()],
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
    assert_eq!(child.root_session_id, Some(root_session_id));
    assert_eq!(child.provider_session_id.as_deref(), Some("child-thread"));
    assert_eq!(child.agent_scope, Some(CoreAgentScope::Subagent));
}

#[test]
fn exact_session_tree_exclusion_does_not_cross_duplicate_provider_session_roots() {
    let temp = tempdir().unwrap();
    let first_root_source = source("exact-tree-first-root.jsonl");
    let first_child_source = source("exact-tree-first-child.jsonl");
    let second_root_source = source("exact-tree-second-root.jsonl");

    let first_root = document_for_session(
        &first_root_source,
        "duplicate-provider-session",
        1,
        "shared needle first root",
    );
    let mut first_child = document_for_session(
        &first_child_source,
        "first-child",
        1,
        "shared needle first child",
    );
    first_child.parent_session_id = Some(first_root.session_id);
    first_child.root_session_id = Some(first_root.session_id);
    first_child.session_relationship = Some(ProviderNativeSessionRelationship::Delegated);
    first_child.agent_scope = Some(CoreAgentScope::Subagent);
    let second_root = document_for_session(
        &second_root_source,
        "duplicate-provider-session",
        1,
        "shared needle second root",
    );
    assert_ne!(first_root.session_id, second_root.session_id);

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    for (source, record) in [
        (first_root_source, first_root.clone()),
        (first_child_source, first_child.clone()),
        (second_root_source, second_root.clone()),
    ] {
        writer.begin_source(source.clone()).unwrap();
        writer.add_core_record(record).unwrap();
        writer.certify_source(certificate(&source, 1, 1)).unwrap();
    }
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();

    let all_session_ids = sorted_uuids(vec![
        first_root.session_id.as_uuid(),
        first_child.session_id.as_uuid(),
        second_root.session_id.as_uuid(),
    ]);
    assert_eq!(
        filtered_session_ids(
            &index,
            EventSearchFilters {
                exclude_session_tree: Some(ExcludedSessionTree {
                    session_ids: Vec::new(),
                }),
                ..EventSearchFilters::default()
            },
        ),
        all_session_ids
    );

    let filters = EventSearchFilters {
        exclude_session_tree: Some(ExcludedSessionTree {
            session_ids: vec![
                first_root.session_id.as_uuid(),
                first_child.session_id.as_uuid(),
            ],
        }),
        ..EventSearchFilters::default()
    };
    assert_eq!(
        filtered_session_ids(&index, filters.clone()),
        vec![second_root.session_id.as_uuid()]
    );
    let semantic = index.semantic_filter_projection(&filters).unwrap();
    assert_eq!(
        semantic.event_ids().collect::<Vec<_>>(),
        vec![second_root.event_id.as_uuid()]
    );
}

#[test]
fn complete_core_body_beyond_16k_round_trips_reopens_and_has_no_stored_preview() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let body = format!("{} tailonlyneedle", "界".repeat(16_384));
    let expected = document(&source, 1, &body);
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
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
        expected.event_id.as_uuid()
    );

    let (searcher, _) = open_unverified_generation(temp.path());
    let fields = fields_from_schema(searcher.schema()).unwrap();
    assert!(searcher.schema().get_field("body_preview").is_err());
    assert!(searcher.schema().get_field("body").is_err());
    let address = searcher
        .search(&AllQuery, &DocSetCollector)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let stored: TantivyDocument = searcher.doc(address).unwrap();
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
fn retrieval_derived_records_are_absent_from_discovery_but_present_in_core_enumeration() {
    let temp = tempdir().unwrap();
    let source = source("retrieval-derived-search.jsonl");
    let ordinary = document(&source, 1, "ordinary searchable canary");

    let mut excluded_call = document(&source, 2, "retrievalderivedcanary call payload");
    excluded_call.event_type = "tool_call".to_owned();
    excluded_call.role = Some("assistant".to_owned());
    add_literal_fact(
        &mut excluded_call,
        LiteralFactKind::File,
        "src/RetrievalDerivedCanary.rs",
    );
    excluded_call.validate_contract().unwrap();
    let excluded_call = retrieval_excluded(excluded_call);

    let mut excluded_output = document(&source, 3, "retrievalderivedcanary output payload");
    excluded_output.event_type = "tool_output".to_owned();
    excluded_output.role = Some("tool".to_owned());
    excluded_output.validate_contract().unwrap();
    let excluded_output = retrieval_excluded(excluded_output);

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for record in [
        ordinary.clone(),
        excluded_call.clone(),
        excluded_output.clone(),
    ] {
        writer.add_core_record(record).unwrap();
    }
    writer.certify_source(certificate(&source, 1, 3)).unwrap();
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();

    assert!(index
        .search_event_candidates("retrievalderivedcanary", 10)
        .unwrap()
        .is_empty());
    for scope in [SearchContentScope::Calls, SearchContentScope::Outputs] {
        let filters = EventSearchFilters {
            content_scope: scope,
            ..EventSearchFilters::default()
        };
        assert!(index
            .search_event_candidates_with_filters("retrievalderivedcanary", &filters, 10)
            .unwrap()
            .is_empty());
        assert!(index
            .list_event_candidates_with_filters(&filters, 10)
            .unwrap()
            .is_empty());
    }
    assert!(index
        .list_event_candidates_with_filters(
            &EventSearchFilters {
                file: Some("retrievalderivedcanary.rs".to_owned()),
                ..EventSearchFilters::default()
            },
            10,
        )
        .unwrap()
        .is_empty());
    assert_eq!(
        candidate_ids(
            &index
                .list_event_candidates_with_filters(&EventSearchFilters::default(), 10)
                .unwrap()
        ),
        vec![ordinary.event_id.digest()]
    );

    for expected in [&excluded_call, &excluded_output] {
        assert_eq!(
            index
                .core_record_by_id(expected.event_id.as_uuid())
                .unwrap()
                .unwrap(),
            *expected
        );
        assert_eq!(
            index
                .event_by_id(expected.event_id.as_uuid())
                .unwrap()
                .unwrap()
                .event_id,
            expected.event_id
        );
    }
    assert_eq!(
        index
            .core_events_for_session(ordinary.session_id.as_uuid())
            .unwrap()
            .len(),
        3
    );
    let source_page = index.core_source_event_page(&source, None, 10).unwrap();
    assert!(source_page.terminal);
    assert_eq!(source_page.items.len(), 3);
}

#[test]
fn empty_or_invalid_programmatic_queries_are_safe() {
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
        Err(ctx_history_index_query::LexicalSearchError::Index(
            IndexError::EmptyQueryFilter { field: "provider" }
        ))
    ));
    for (query, limit) in [("", 10), ("body", 0)] {
        assert!(matches!(
            index.search_event_candidates_with_filters(
                query,
                &EventSearchFilters {
                    provider: Some("  ".to_owned()),
                    ..EventSearchFilters::default()
                },
                limit,
            ),
            Err(ctx_history_index_query::LexicalSearchError::Index(
                IndexError::EmptyQueryFilter { field: "provider" }
            ))
        ));
    }
    assert!(matches!(
        index.list_event_candidates_with_filters(
            &EventSearchFilters {
                file: Some("  ".to_owned()),
                ..EventSearchFilters::default()
            },
            0,
        ),
        Err(ctx_history_index_query::LexicalSearchError::Index(
            IndexError::EmptyQueryFilter { field: "file" }
        ))
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

fn publish_class_aware_records(records: Vec<CoreRecord>) -> (TempDir, VerifiedIndex) {
    let temp = tempdir().unwrap();
    let source = records.first().unwrap().source.clone();
    let document_count = u64::try_from(records.len()).unwrap();
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for record in records {
        writer.add_core_record(record).unwrap();
    }
    writer
        .certify_source(certificate(&source, 1, document_count))
        .unwrap();
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open(temp.path()).unwrap();
    (temp, index)
}

fn class_aware_record(
    source: &SourceKey,
    sequence: u64,
    event_type: &str,
    body: &str,
) -> CoreRecord {
    let mut record = document(source, sequence, body);
    record.event_type = event_type.to_owned();
    record.validate_contract().unwrap();
    record
}

fn candidate_ids(candidates: &[EventSearchCandidate]) -> Vec<[u8; 32]> {
    candidates
        .iter()
        .map(|candidate| candidate.event.event_identity_digest)
        .collect()
}

fn candidate_score(candidates: &[EventSearchCandidate], event_id: StableEntityId) -> Score {
    candidates
        .iter()
        .find(|candidate| candidate.event.event_identity_digest == event_id.digest())
        .unwrap()
        .score
}

fn assert_score_ratio(actual: Score, reference: Score, expected: Score) {
    let ratio = actual / reference;
    assert!(
        (ratio - expected).abs() < 0.000_01,
        "expected score ratio {expected}, got {ratio} ({actual}/{reference})"
    );
}

#[test]
fn all_scope_applies_class_weights_and_retains_unknown_types_with_stable_ties() {
    let source = source("class-weight-ordering.jsonl");
    let event_types = [
        "message",
        "summary",
        "tool_call",
        "command_started",
        "tool_output",
        "command_output",
        "command_finished",
        "notice",
        "future_searchable_type",
    ];
    let records = event_types
        .iter()
        .enumerate()
        .map(|(index, event_type)| {
            class_aware_record(
                &source,
                u64::try_from(index + 1).unwrap(),
                event_type,
                "classweightneedle",
            )
        })
        .collect::<Vec<_>>();
    let ids_by_type = records
        .iter()
        .map(|record| (record.event_type.clone(), record.event_id))
        .collect::<std::collections::HashMap<_, _>>();
    let (_temp, index) = publish_class_aware_records(records);

    let omitted = index
        .search_event_candidates("classweightneedle", 20)
        .unwrap();
    let explicit_all = index
        .search_event_candidates_with_filters(
            "classweightneedle",
            &EventSearchFilters {
                content_scope: SearchContentScope::All,
                ..EventSearchFilters::default()
            },
            20,
        )
        .unwrap();
    assert_eq!(omitted, explicit_all, "omitted and explicit all must agree");
    assert_eq!(omitted.len(), event_types.len());

    let message_id = ids_by_type["message"];
    let message_score = candidate_score(&omitted, message_id);
    assert_eq!(omitted[0].event.event_identity_digest, message_id.digest());
    assert_score_ratio(
        candidate_score(&omitted, ids_by_type["summary"]),
        message_score,
        0.9,
    );
    for event_type in [
        "tool_call",
        "command_started",
        "notice",
        "future_searchable_type",
    ] {
        assert_score_ratio(
            candidate_score(&omitted, ids_by_type[event_type]),
            message_score,
            0.8,
        );
    }
    for event_type in ["tool_output", "command_output", "command_finished"] {
        assert_score_ratio(
            candidate_score(&omitted, ids_by_type[event_type]),
            message_score,
            0.6,
        );
    }

    let mut expected_fallback_tie = [
        ids_by_type["tool_call"].digest(),
        ids_by_type["command_started"].digest(),
        ids_by_type["notice"].digest(),
        ids_by_type["future_searchable_type"].digest(),
    ];
    expected_fallback_tie.sort();
    assert_eq!(candidate_ids(&omitted)[2..6], expected_fallback_tie);
    assert!(candidate_ids(&omitted).contains(&ids_by_type["future_searchable_type"].digest()));
}

#[test]
fn explicit_scopes_filter_search_list_and_semantic_projection_with_ordinary_io_weight() {
    let source = source("content-scopes.jsonl");
    let event_types = [
        "message",
        "summary",
        "tool_call",
        "command_started",
        "tool_output",
        "command_output",
        "command_finished",
        "notice",
    ];
    let records = event_types
        .iter()
        .enumerate()
        .map(|(index, event_type)| {
            class_aware_record(
                &source,
                u64::try_from(index + 1).unwrap(),
                event_type,
                "scopeneedle",
            )
        })
        .collect::<Vec<_>>();
    let message_id = records[0].event_id;
    let call_id = records[2].event_id;
    let output_id = records[4].event_id;
    let record_ids_by_type = records
        .iter()
        .map(|record| (record.event_type.clone(), record.event_id.digest()))
        .collect::<Vec<_>>();
    let (_temp, index) = publish_class_aware_records(records);

    let all = index.search_event_candidates("scopeneedle", 20).unwrap();
    for (scope, expected_types) in [
        (
            SearchContentScope::Transcript,
            HashSet::from(["message", "summary"]),
        ),
        (
            SearchContentScope::Calls,
            HashSet::from(["tool_call", "command_started"]),
        ),
        (
            SearchContentScope::Outputs,
            HashSet::from(["tool_output", "command_output", "command_finished"]),
        ),
    ] {
        let filters = EventSearchFilters {
            content_scope: scope,
            ..EventSearchFilters::default()
        };
        let searched = index
            .search_event_candidates_with_filters("scopeneedle", &filters, 20)
            .unwrap();
        let listed = index
            .list_event_candidates_with_filters(&filters, 20)
            .unwrap();
        let expected_ids = record_ids_by_type
            .iter()
            .filter(|(event_type, _)| expected_types.contains(event_type.as_str()))
            .map(|(_, event_id)| *event_id)
            .collect::<HashSet<_>>();
        assert_eq!(
            candidate_ids(&searched).into_iter().collect::<HashSet<_>>(),
            expected_ids
        );
        assert_eq!(
            candidate_ids(&listed).into_iter().collect::<HashSet<_>>(),
            expected_ids
        );

        let semantic = index.semantic_filter_projection(&filters).unwrap();
        match scope {
            SearchContentScope::Transcript => {
                assert_eq!(
                    semantic.event_ids().collect::<Vec<_>>(),
                    vec![message_id.as_uuid()]
                );
            }
            SearchContentScope::Calls | SearchContentScope::Outputs => {
                assert_eq!(semantic.event_ids().count(), 0);
            }
            SearchContentScope::All => unreachable!(),
        }
    }

    let calls = index
        .search_event_candidates_with_filters(
            "scopeneedle",
            &EventSearchFilters {
                content_scope: SearchContentScope::Calls,
                ..EventSearchFilters::default()
            },
            20,
        )
        .unwrap();
    let outputs = index
        .search_event_candidates_with_filters(
            "scopeneedle",
            &EventSearchFilters {
                content_scope: SearchContentScope::Outputs,
                ..EventSearchFilters::default()
            },
            20,
        )
        .unwrap();
    assert_score_ratio(
        candidate_score(&all, call_id),
        candidate_score(&calls, call_id),
        0.8,
    );
    assert_score_ratio(
        candidate_score(&all, output_id),
        candidate_score(&outputs, output_id),
        0.6,
    );

    let semantic_all = index
        .semantic_filter_projection(&EventSearchFilters::default())
        .unwrap();
    assert_eq!(
        semantic_all.event_ids().collect::<Vec<_>>(),
        vec![message_id.as_uuid()]
    );
}

#[test]
fn output_heavy_candidates_do_not_starve_the_stronger_transcript_before_top_docs() {
    const OUTPUTS: u64 = 32;
    let source = source("output-saturation.jsonl");
    let transcript =
        class_aware_record(&source, 1, "message", "saturationneedle concise transcript");
    let transcript_id = transcript.event_id;
    let mut records = vec![transcript];
    for sequence in 2..=OUTPUTS + 1 {
        records.push(class_aware_record(
            &source,
            sequence,
            "tool_output",
            &"saturationneedle ".repeat(128),
        ));
    }
    let (temp, index) = publish_class_aware_records(records);

    let (searcher, _) = open_unverified_generation(temp.path());
    let fields = fields_from_schema(searcher.schema()).unwrap();
    let raw_query = TermQuery::new(
        Term::from_field_text(fields.body_search, "saturationneedle"),
        IndexRecordOption::WithFreqs,
    );
    let raw_hits = searcher
        .search(&raw_query, &TopDocs::with_limit(1).order_by_score())
        .unwrap();
    let raw_top = decoded_stored_core(&searcher, raw_hits[0].1);
    assert_eq!(
        raw_top.event_type, "tool_output",
        "the fixture must put an output first without class weighting"
    );

    let weighted = index
        .search_event_candidates("saturationneedle", 1)
        .unwrap();
    assert_eq!(weighted[0].event.event_id, transcript_id.as_uuid());
}

#[test]
fn exact_event_type_conflicts_with_every_explicit_content_scope_at_the_index_boundary() {
    let source = source("scope-conflict.jsonl");
    let record = class_aware_record(&source, 1, "message", "conflictneedle");
    let (_temp, index) = publish_class_aware_records(vec![record]);

    for scope in [
        SearchContentScope::Transcript,
        SearchContentScope::Calls,
        SearchContentScope::Outputs,
    ] {
        let filters = EventSearchFilters {
            content_scope: scope,
            event_type: Some("message".to_owned()),
            ..EventSearchFilters::default()
        };
        for error in [
            index
                .search_event_candidates_with_filters("conflictneedle", &filters, 0)
                .unwrap_err(),
            index
                .list_event_candidates_with_filters(&filters, 0)
                .unwrap_err(),
        ] {
            assert!(matches!(
                error,
                ctx_history_index_query::LexicalSearchError::Index(
                    IndexError::ContentScopeEventTypeConflict { scope: actual }
                )
                    if actual == scope.as_str()
            ));
        }
        assert!(matches!(
            index.semantic_filter_projection(&filters).unwrap_err(),
            IndexError::ContentScopeEventTypeConflict { scope: actual }
                if actual == scope.as_str()
        ));
    }
}
