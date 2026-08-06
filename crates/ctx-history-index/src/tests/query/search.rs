use super::*;

use crate::query::SearchContentScope;
use ctx_history_core::StableEntityId;
use tantivy::{collector::TopDocs, Score};

#[test]
fn copied_events_are_excluded_before_search_windows_and_unknowns_remain_searchable() {
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
    copied
        .set_session_relationship(
            SessionRelationshipKind::Forked,
            Some(original.session_id),
            original.session_id,
        )
        .unwrap();
    copied.event_origin = EventOrigin::CopiedFromAncestor {
        ancestor_session_id: original.session_id,
        ancestor_event_id: original.event_id,
        proof: EventCopyProofKind::NativeEventIdentity,
    };
    copied.validate_contract().unwrap();
    let unknown = document_for_session(
        &source,
        "unknown-session",
        3,
        "lineagewindowneedle unknown body",
    );
    let mut unique = document_for_session(
        &source,
        "unique-session",
        4,
        "lineagewindowneedle unique body",
    );
    unique.event_origin = EventOrigin::UniqueToSession;
    unique.validate_contract().unwrap();

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

    let expected = HashSet::from([original.event_id, unknown.event_id, unique.event_id]);
    let lexical = index
        .search_event_candidates("lineagewindowneedle", 3)
        .unwrap();
    assert_eq!(lexical.len(), 3, "copied rows must not consume the window");
    assert_eq!(
        candidate_ids(&lexical).into_iter().collect::<HashSet<_>>(),
        expected
    );
    assert!(!lexical
        .iter()
        .any(|candidate| candidate.event.event_id == copied.event_id));

    let listed = index
        .list_event_candidates_with_filters(&EventSearchFilters::default(), 3)
        .unwrap();
    assert_eq!(listed.len(), 3);
    assert_eq!(
        candidate_ids(&listed).into_iter().collect::<HashSet<_>>(),
        expected
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
    let semantic_page = index.semantic_event_page(None, 3).unwrap();
    assert_eq!(semantic_page.items.len(), 3);
    assert_eq!(semantic_page.eligible_total, 3);

    let source_page = index.core_source_event_page(&source, None, 4).unwrap();
    assert_eq!(source_page.items.len(), 4);
    let visible_copy = index
        .core_record_by_id(copied.event_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(visible_copy.event_origin, copied.event_origin);
    let copied_session = index
        .core_events_for_session(copied.session_id.as_uuid())
        .unwrap();
    assert_eq!(copied_session.len(), 1);
    assert_eq!(copied_session[0].event_origin, copied.event_origin);
}

#[test]
fn many_copied_bodies_add_no_postings_or_score_order_changes() {
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
            copied
                .set_session_relationship(
                    SessionRelationshipKind::Forked,
                    Some(first.session_id),
                    first.session_id,
                )
                .unwrap();
            copied.event_origin = EventOrigin::CopiedFromAncestor {
                ancestor_session_id: first.session_id,
                ancestor_event_id: first.event_id,
                proof: EventCopyProofKind::NativeCopiedFromField,
            };
            copied.validate_contract().unwrap();
            records.push(copied);
        }
        records
    }

    let source = source("copied-body-statistics.jsonl");
    let duplicated = records_with_copy_body(&source, "copybodystatsneedle concise");
    let expected_copy = duplicated[2].clone();
    let control = records_with_copy_body(&source, "unrelated copied body control");
    let (_duplicated_temp, duplicated_index) = publish_class_aware_records(duplicated);
    let (_control_temp, control_index) = publish_class_aware_records(control);

    let duplicated_hits = duplicated_index
        .search_event_candidates(NEEDLE, COPIES as usize + 2)
        .unwrap();
    let control_hits = control_index
        .search_event_candidates(NEEDLE, COPIES as usize + 2)
        .unwrap();
    assert_eq!(duplicated_hits.len(), 2);
    assert_eq!(
        candidate_ids(&duplicated_hits),
        candidate_ids(&control_hits)
    );
    assert_eq!(duplicated_hits[0].score, control_hits[0].score);
    assert_eq!(duplicated_hits[1].score, control_hits[1].score);

    let fields = fields_from_schema(duplicated_index.searcher.schema()).unwrap();
    let term = Term::from_field_text(fields.body_search, NEEDLE);
    assert_eq!(duplicated_index.searcher.doc_freq(&term).unwrap(), 2);
    let raw_matches = duplicated_index
        .searcher
        .search(
            &TermQuery::new(term, IndexRecordOption::WithFreqs),
            &DocSetCollector,
        )
        .unwrap();
    assert_eq!(raw_matches.len(), 2);

    let visible_copy = duplicated_index
        .core_record_by_id(expected_copy.event_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(visible_copy.event_origin, expected_copy.event_origin);
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

    let mut child = document_for_session(&codex_child, "child-thread", 2, "shared needle");
    child
        .set_session_relationship(
            SessionRelationshipKind::Delegated,
            Some(root_session_id),
            root_session_id,
        )
        .unwrap();
    child.branch = Some("feature/query-seam".to_owned());
    child.workspace = Some("ChildSpace".to_owned());
    child.cwd = Some("/work/child".to_owned());
    child.agent_type = "subagent".to_owned();
    child.event_type = "tool_call".to_owned();
    child.role = Some("assistant".to_owned());
    child.occurred_at_unix_ms = Some(200);
    let child_session_id = child.session_id;

    let mut other = document_for_session(&claude, "other-thread", 3, "shared needle");
    other.workspace = Some("Elsewhere".to_owned());
    other.branch = Some("release".to_owned());
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
    assert_eq!(child.agent_type, "subagent");
    assert!(!child.is_primary);
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

fn candidate_ids(candidates: &[EventSearchCandidate]) -> Vec<StableEntityId> {
    candidates
        .iter()
        .map(|candidate| candidate.event.event_id)
        .collect()
}

fn candidate_event_types(candidates: &[EventSearchCandidate]) -> HashSet<&str> {
    candidates
        .iter()
        .map(|candidate| candidate.event.event_type.as_str())
        .collect()
}

fn candidate_score(candidates: &[EventSearchCandidate], event_id: StableEntityId) -> Score {
    candidates
        .iter()
        .find(|candidate| candidate.event.event_id == event_id)
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
    assert_eq!(omitted[0].event.event_id, message_id);
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
        ids_by_type["tool_call"],
        ids_by_type["command_started"],
        ids_by_type["notice"],
        ids_by_type["future_searchable_type"],
    ];
    expected_fallback_tie.sort_by_key(|event_id| event_id.as_uuid());
    assert_eq!(candidate_ids(&omitted)[2..6], expected_fallback_tie);
    assert!(omitted
        .iter()
        .any(|candidate| candidate.event.event_type == "future_searchable_type"));
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
        assert_eq!(candidate_event_types(&searched), expected_types);
        assert_eq!(candidate_event_types(&listed), expected_types);

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
    let (_temp, index) = publish_class_aware_records(records);

    let fields = fields_from_schema(index.searcher.schema()).unwrap();
    let raw_query = TermQuery::new(
        Term::from_field_text(fields.body_search, "saturationneedle"),
        IndexRecordOption::WithFreqs,
    );
    let raw_hits = index
        .searcher
        .search(&raw_query, &TopDocs::with_limit(1).order_by_score())
        .unwrap();
    let raw_top = decoded_stored_core(&index.searcher, raw_hits[0].1);
    assert_eq!(
        raw_top.event_type, "tool_output",
        "the fixture must put an output first without class weighting"
    );

    let weighted = index
        .search_event_candidates("saturationneedle", 1)
        .unwrap();
    assert_eq!(weighted[0].event.event_id, transcript_id);
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
            index.semantic_filter_projection(&filters).unwrap_err(),
        ] {
            assert!(matches!(
                error,
                IndexError::ContentScopeEventTypeConflict { scope: actual }
                    if actual == scope.as_str()
            ));
        }
    }
}
