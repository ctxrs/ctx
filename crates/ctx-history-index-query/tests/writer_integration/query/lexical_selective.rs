use super::*;

#[test]
fn selective_session_seeks_past_unrelated_body_matches_with_a_small_candidate_budget() {
    let temp = tempdir().unwrap();
    let source = source("selective-session.jsonl");
    let mut records = (1..=16)
        .map(|sequence| document_for_session(&source, "unrelated", sequence, "scopeneedle"))
        .collect::<Vec<_>>();
    let selected = document_for_session(&source, "selected", 17, "scopeneedle");
    let expected = selected.event_id.as_uuid();
    let filters = EventSearchFilters {
        session_id: Some(selected.session_id.as_uuid()),
        ..EventSearchFilters::default()
    };
    records.push(selected);
    let index = publish_records_in_one_segment(&temp, &source, records);
    let (searcher, _) = open_unverified_generation(temp.path());
    assert_eq!(searcher.segment_readers().len(), 1);
    assert_eq!(searcher.segment_readers()[0].max_doc(), 17);
    assert_eq!(
        decoded_stored_core(&searcher, tantivy::DocAddress::new(0, 16))
            .event_id
            .as_uuid(),
        expected,
        "the only scoped match must follow the unrelated body matches"
    );

    let broad =
        lexical_search_batch(&index, &["scopeneedle"], &EventSearchFilters::default(), 20).unwrap();
    assert!(broad.complete && broad.candidate_set_exhaustive);
    assert_eq!(broad.candidates.len(), 17);
    assert_eq!(broad.counters.candidate_docs, 17);
    assert_eq!(broad.counters.body_posting_advances, 17);
    let full = lexical_search_batch(&index, &["scopeneedle"], &filters, 20).unwrap();
    assert!(full.complete && full.candidate_set_exhaustive);
    assert_eq!(full.candidates.len(), 1);
    assert_eq!(full.candidates[0].event.event_id, expected);

    let budget = ctx_history_index_query::LexicalWorkBudget {
        maximum_candidate_docs: 2,
        ..ctx_history_index_query::LEXICAL_WORK_BUDGET_V1
    };
    let small =
        lexical_search_batch_with_budget(&index, &["scopeneedle"], &filters, 20, budget).unwrap();
    eprintln!(
        "broad={:?}; selective full={:?}; small={:?}; exhaustion={:?}",
        broad.counters, full.counters, small.counters, small.exhaustion
    );
    assert!(small.complete, "{:?}", small.exhaustion);
    assert!(small.candidate_set_exhaustive);
    assert_eq!(small.candidates.len(), 1);
    assert_eq!(small.candidates[0].event.event_id, expected);
    assert_eq!(small.candidates, full.candidates);
    assert_eq!(small.counters.candidate_docs, 1);
    assert_eq!(small.counters.body_posting_advances, 2);
}

#[test]
fn selective_positive_or_groups_preserve_exclusions_substrings_and_ranking() {
    let temp = tempdir().unwrap();
    let source = source("selective-or-groups.jsonl");
    // Independently authored membership: 1, 2, 3, 9 and 10 satisfy every
    // predicate. Each other row violates the indicated predicate.
    let rows = [
        ("tool_call", "alpha", "user"), // class
        ("summary", "beta", "user"),
        ("message", "alpha beta", "user"),
        ("summary", "beta", "user"),
        ("message", "alpha", "user"),   // excluded session
        ("summary", "beta", "user"),    // workspace
        ("message", "alpha", "user"),   // file
        ("summary", "beta", "user"),    // timestamp
        ("message", "neither", "user"), // body
        ("message", "alpha beta", "user"),
        ("summary", "beta", "user"),
        ("tool_output", "alpha beta", "user"), // class
        ("summary", "beta", "assistant"),      // role
    ];
    let records = rows
        .iter()
        .enumerate()
        .map(|(index, (event_type, body, role))| {
            let mut record =
                document_for_session(&source, &format!("session-{index}"), index as u64 + 1, body);
            record.event_type = (*event_type).to_owned();
            record.role = Some((*role).to_owned());
            record.occurred_at_unix_ms = Some(if index == 7 { 1 } else { 100 });
            add_literal_fact(
                &mut record,
                LiteralFactKind::Workspace,
                if index == 5 {
                    "/other"
                } else {
                    "/Work/Selected"
                },
            );
            add_literal_fact(
                &mut record,
                LiteralFactKind::File,
                if index == 6 {
                    "other.rs"
                } else {
                    "src/Selected.rs"
                },
            );
            record.validate_contract().unwrap();
            record
        })
        .collect::<Vec<_>>();
    let expected = [1, 2, 3, 9, 10].map(|index| records[index].event_id.as_uuid());
    let filter = EventSearchFilters {
        allowed_source_keys: Some(vec![source_token(&source), "absent-source".to_owned()]),
        provider: Some("codex".to_owned()),
        content_scope: SearchContentScope::Transcript,
        role: Some("user".to_owned()),
        excluded_session_ids: vec![records[4].session_id.as_uuid()],
        workspace: Some("SELECTED".to_owned()),
        file: Some("selected.RS".to_owned()),
        since_unix_ms: Some(50),
        ..EventSearchFilters::default()
    };
    let index = publish_records_in_one_segment(&temp, &source, records);
    let broad = lexical_search_batch(
        &index,
        &["alpha", "beta"],
        &EventSearchFilters::default(),
        20,
    )
    .unwrap();
    let filtered = lexical_search_batch(&index, &["alpha", "beta"], &filter, 20).unwrap();
    assert!(broad.complete && broad.candidate_set_exhaustive);
    assert!(filtered.complete && filtered.candidate_set_exhaustive);
    assert_eq!(
        filtered
            .candidates
            .iter()
            .map(|candidate| candidate.event.event_id)
            .collect::<HashSet<_>>(),
        HashSet::from(expected)
    );
    // Broad retrieval supplies only score/order parity, never membership.
    let expected_ranked = broad
        .candidates
        .into_iter()
        .filter(|candidate| expected.contains(&candidate.event.event_id))
        .collect::<Vec<_>>();
    assert_eq!(filtered.candidates, expected_ranked);
    assert!(filtered.candidates[..2]
        .iter()
        .all(|candidate| candidate.coverage.matched_terms == 2));
}

#[test]
fn selective_traversal_precharges_posting_work_and_preserves_negative_candidate_bounds() {
    use ctx_history_index_query::{LexicalWorkBudget, LexicalWorkCounter, LEXICAL_WORK_BUDGET_V1};

    let temp = tempdir().unwrap();
    let source = source("selective-work-bounds.jsonl");
    let mut records = (1..=16)
        .map(|sequence| document_for_session(&source, "unrelated", sequence, "scopeneedle"))
        .collect::<Vec<_>>();
    let unrelated_session = records[0].session_id.as_uuid();
    let mut selected = document_for_session(&source, "selected", 17, "scopeneedle");
    let expected = selected.event_id.as_uuid();
    let selected_session = selected.session_id.as_uuid();
    add_literal_fact(&mut selected, LiteralFactKind::Workspace, "/selected");
    records.push(selected);
    let index = publish_records_in_one_segment(&temp, &source, records);
    let positive = EventSearchFilters {
        session_id: Some(selected_session),
        ..EventSearchFilters::default()
    };
    let run = |filters: &EventSearchFilters, budget| {
        lexical_search_batch_with_budget(&index, &["scopeneedle"], filters, 20, budget).unwrap()
    };
    for (budget, counter) in [
        (
            LexicalWorkBudget {
                maximum_body_posting_advances: 0,
                ..LEXICAL_WORK_BUDGET_V1
            },
            LexicalWorkCounter::BodyPostingAdvances,
        ),
        (
            LexicalWorkBudget {
                maximum_filter_probes: 0,
                ..LEXICAL_WORK_BUDGET_V1
            },
            LexicalWorkCounter::FilterProbes,
        ),
        (
            LexicalWorkBudget {
                maximum_filter_seeks: 0,
                ..LEXICAL_WORK_BUDGET_V1
            },
            LexicalWorkCounter::FilterSeeks,
        ),
        (
            LexicalWorkBudget {
                maximum_candidate_docs: 0,
                ..LEXICAL_WORK_BUDGET_V1
            },
            LexicalWorkCounter::CandidateDocs,
        ),
    ] {
        let batch = run(&positive, budget);
        assert_exhausted_at(&batch, counter, 0, 0);
        assert_eq!(batch.counters.candidate_docs, 0);
        assert_eq!(batch.counters.retained_candidates, 0);
        assert!(batch.candidates.is_empty());
        assert_eq!(
            batch,
            run(&positive, budget),
            "first blocked operation is deterministic"
        );
    }
    let one_movement = run(
        &positive,
        LexicalWorkBudget {
            maximum_body_posting_advances: 1,
            ..LEXICAL_WORK_BUDGET_V1
        },
    );
    assert_exhausted_at(&one_movement, LexicalWorkCounter::BodyPostingAdvances, 1, 1);
    assert_eq!(one_movement.counters.candidate_docs, 1);
    assert_eq!(one_movement.candidates.len(), 1);
    assert_eq!(one_movement.candidates[0].event.event_id, expected);

    let small = LexicalWorkBudget {
        maximum_candidate_docs: 2,
        ..LEXICAL_WORK_BUDGET_V1
    };
    for filters in [
        EventSearchFilters {
            excluded_session_ids: vec![unrelated_session],
            ..EventSearchFilters::default()
        },
        EventSearchFilters {
            workspace: Some("selected".to_owned()),
            ..EventSearchFilters::default()
        },
    ] {
        let batch = run(&filters, small);
        assert_exhausted_at(&batch, LexicalWorkCounter::CandidateDocs, 2, 2);
        assert!(
            batch.candidates.is_empty(),
            "arbitrary rejection cannot hide candidate work"
        );
    }
    let excluded = run(
        &EventSearchFilters {
            excluded_session_ids: vec![selected_session],
            ..positive
        },
        small,
    );
    assert!(excluded.complete && excluded.candidate_set_exhaustive);
    assert_eq!(excluded.counters.candidate_docs, 1);
    assert!(excluded.candidates.is_empty());
}

#[test]
fn selective_or_and_bounds_converge_or_exhaust_before_candidate_admission() {
    use ctx_history_index_query::{LexicalWorkCounter, LEXICAL_WORK_BUDGET_V1};

    let temp = tempdir().unwrap();
    let source = source("selective-convergence.jsonl");
    // The class OR admits doc 0, but role advances to 1. Class then advances
    // to 2, and role to 3, forcing the earlier OR group to be checked again.
    let records = [
        ("message", "assistant"),
        ("tool_call", "user"),
        ("summary", "assistant"),
        ("message", "user"),
        ("summary", "user"),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (event_type, role))| {
        let mut record = document(&source, index as u64 + 1, "convergenceneedle");
        record.event_type = event_type.to_owned();
        record.role = Some(role.to_owned());
        record
    })
    .collect::<Vec<_>>();
    let expected = [records[3].event_id.as_uuid(), records[4].event_id.as_uuid()];
    let index = publish_records_in_one_segment(&temp, &source, records);
    let filters = EventSearchFilters {
        content_scope: SearchContentScope::Transcript,
        role: Some("user".to_owned()),
        ..EventSearchFilters::default()
    };
    let run = |budget| {
        lexical_search_batch_with_budget(&index, &["convergenceneedle"], &filters, 10, budget)
            .unwrap()
    };
    let complete = run(LEXICAL_WORK_BUDGET_V1);
    assert!(complete.complete && complete.candidate_set_exhaustive);
    assert_eq!(
        complete
            .candidates
            .iter()
            .map(|candidate| candidate.event.event_id)
            .collect::<HashSet<_>>(),
        HashSet::from(expected)
    );
    assert_eq!(complete.counters.candidate_docs, 2);
    assert_eq!(complete.counters.body_posting_advances, 5);

    for (counter, limit) in [
        (LexicalWorkCounter::FilterProbes, 6),
        (LexicalWorkCounter::FilterSeeks, 1),
    ] {
        let mut budget = LEXICAL_WORK_BUDGET_V1;
        match counter {
            LexicalWorkCounter::FilterProbes => budget.maximum_filter_probes = limit,
            LexicalWorkCounter::FilterSeeks => budget.maximum_filter_seeks = limit,
            _ => unreachable!(),
        }
        let partial = run(budget);
        assert_exhausted_at(&partial, counter, limit, limit);
        assert!(partial.candidates.is_empty());
        assert_eq!(partial.counters.candidate_docs, 0);
        assert_eq!(partial.counters.retained_candidates, 0);
        assert_eq!(partial.counters.body_posting_advances, 2);
        assert_eq!(partial.counters.filter_seeks, 1);
        let exhaustion = partial.exhaustion.as_ref().unwrap();
        assert_eq!(exhaustion.next_doc, Some(2));
        assert_eq!(exhaustion.segment.as_ref().unwrap().stable_segment_index, 0);
    }
}

#[test]
fn selective_disjoint_and_missing_positive_groups_finish_without_candidates() {
    let temp = tempdir().unwrap();
    let source = source("selective-empty-intersection.jsonl");
    let records = [
        ("message", "assistant"),
        ("tool_call", "user"),
        ("summary", "assistant"),
        ("tool_call", "user"),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (event_type, role))| {
        let mut record = document(&source, index as u64 + 1, "disjointneedle");
        record.event_type = event_type.to_owned();
        record.role = Some(role.to_owned());
        record
    })
    .collect::<Vec<_>>();
    let expected_class = [records[0].event_id.as_uuid(), records[2].event_id.as_uuid()];
    let expected_role = [records[1].event_id.as_uuid(), records[3].event_id.as_uuid()];
    let index = publish_records_in_one_segment(&temp, &source, records);
    let class = EventSearchFilters {
        content_scope: SearchContentScope::Transcript,
        ..EventSearchFilters::default()
    };
    let role = EventSearchFilters {
        role: Some("user".to_owned()),
        ..EventSearchFilters::default()
    };
    for (filters, expected) in [(&class, expected_class), (&role, expected_role)] {
        let group = lexical_search_batch(&index, &["disjointneedle"], filters, 10).unwrap();
        assert!(group.complete && group.candidate_set_exhaustive);
        assert_eq!(
            group
                .candidates
                .iter()
                .map(|candidate| candidate.event.event_id)
                .collect::<HashSet<_>>(),
            HashSet::from(expected)
        );
    }
    let disjoint = EventSearchFilters {
        role: role.role,
        ..class
    };
    let empty = lexical_search_batch(&index, &["disjointneedle"], &disjoint, 10).unwrap();
    assert!(empty.complete && empty.candidate_set_exhaustive);
    assert!(empty.candidates.is_empty());
    assert_eq!(empty.counters.candidate_docs, 0);
    assert_eq!(empty.counters.body_posting_advances, 3);
    assert_eq!(empty.counters.filter_seeks, 3);

    let missing = EventSearchFilters {
        allowed_source_keys: Some(vec!["missing-a".to_owned(), "missing-b".to_owned()]),
        ..disjoint
    };
    let empty = lexical_search_batch(&index, &["disjointneedle"], &missing, 10).unwrap();
    assert!(empty.complete && empty.candidate_set_exhaustive);
    assert!(empty.candidates.is_empty());
    assert_eq!(empty.counters.candidate_docs, 0);
    assert_eq!(empty.counters.body_posting_advances, 0);
    assert_eq!(empty.counters.filter_probes, 0);
    assert_eq!(empty.counters.filter_seeks, 0);
}
