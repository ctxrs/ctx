use super::*;
use ctx_history_index_query::LEXICAL_WORK_BUDGET_V1;

const QUERY: &str = "budgetalpha budgetbeta budgetgamma";

fn broad_corpus(count: u64) -> (TempDir, VerifiedIndex, Uuid) {
    let temp = tempdir().unwrap();
    let source = source("broad-top-k.jsonl");
    let noise = format!("{QUERY} {}", "ballast ".repeat(27));
    let mut writer = GenerationWriter::open(
        temp.path(),
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 1024 * 1024 * 1024,
        },
    )
    .unwrap()
    .into_writer()
    .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for sequence in 1..count {
        writer
            .add_core_record(document(&source, sequence, &noise))
            .unwrap();
    }
    let best = document(&source, count, &format!("{QUERY} {QUERY} {QUERY}"));
    let expected = best.event_id.as_uuid();
    writer.add_core_record(best).unwrap();
    writer
        .certify_source(certificate(&source, 1, count))
        .unwrap();
    writer.commit(|_| true).unwrap();
    // One segment makes the target's position after the old scan cap exact,
    // independent of Tantivy's flush scheduling or random segment IDs.
    let (searcher, _) = open_unverified_generation(temp.path());
    assert_eq!(searcher.segment_readers().len(), 1);
    assert_eq!(u64::from(searcher.segment_readers()[0].num_docs()), count);
    let index = VerifiedIndex::open(temp.path()).unwrap();
    (temp, index, expected)
}

#[test]
fn best_match_after_candidate_cap_is_not_lost_to_scan_order() {
    let (_temp, index, expected) =
        broad_corpus(LEXICAL_WORK_BUDGET_V1.maximum_candidate_docs + 128);
    for limit in [10, 256, MAX_LEXICAL_QUERY_RESULTS] {
        let batch = observed_candidates(&index, QUERY, limit).unwrap().batch;
        assert_eq!(batch.candidates[0].event.event_id, expected);
        assert!(batch.complete, "{:?}", batch.exhaustion);
        assert!(!batch.candidate_set_exhaustive);
        assert!(
            batch.counters.candidate_docs < 16 * limit as u64,
            "{:?}",
            batch.counters
        );
        assert_eq!(
            batch,
            observed_candidates(&index, QUERY, limit).unwrap().batch
        );
    }
}

#[test]
fn rank_pruning_preserves_scores_citations_and_filters() {
    let temp = tempdir().unwrap();
    let source = source("rank-bound-parity.jsonl");
    let mut records = Vec::new();
    for sequence in 1..=192 {
        let body = match sequence % 4 {
            0 => "budgetalpha ",
            1 => "budgetalpha budgetbeta ",
            _ => "budgetalpha budgetbeta budgetgamma ",
        };
        let mut record = document(
            &source,
            sequence,
            &format!("{} ballast", body.repeat((sequence % 7 + 1) as usize)),
        );
        record.event_type = ["message", "summary", "tool_call", "tool_output"]
            [(sequence / 4 % 4) as usize]
            .to_owned();
        if sequence % 3 == 0 {
            add_literal_fact(&mut record, LiteralFactKind::File, "selected.rs");
        }
        records.push(record);
    }
    let index = publish_records(&temp, &source, records);
    for query in [QUERY, "budgetalpha budgetbeta budgetgamma absentterm"] {
        for scope in [
            SearchContentScope::All,
            SearchContentScope::Transcript,
            SearchContentScope::Calls,
            SearchContentScope::Outputs,
        ] {
            for file in [None, Some("selected.rs".to_owned())] {
                let filters = EventSearchFilters {
                    content_scope: scope,
                    file,
                    since_unix_ms: Some(1_700_000_000_040),
                    ..EventSearchFilters::default()
                };
                // A heap larger than the corpus never invokes rank pruning. Full
                // candidate equality covers score bits, coverage and citation data.
                let exhaustive = lexical_search_batch(&index, &[query], &filters, 192).unwrap();
                assert!(exhaustive.complete && exhaustive.candidate_set_exhaustive);
                for limit in [1, 10, exhaustive.candidates.len()] {
                    let bounded = lexical_search_batch(&index, &[query], &filters, limit).unwrap();
                    assert!(bounded.complete);
                    assert_eq!(
                        bounded.candidates,
                        exhaustive.candidates[..limit.min(exhaustive.candidates.len())]
                    );
                    assert_eq!(
                        bounded.candidate_set_exhaustive,
                        exhaustive.candidates.len() <= limit
                    );
                }
            }
        }
    }
}

#[test]
fn pruned_traversal_and_competitive_candidates_still_exhaust_truthfully() {
    use ctx_history_index_query::{LexicalWorkBudget, LexicalWorkCounter};
    let temp = tempdir().unwrap();
    let source = source("rank-bound-work.jsonl");
    let initial = document(&source, 1, &format!("{QUERY} ballast"));
    let mut records = vec![initial];
    for sequence in 2..=20 {
        let mut output = document(&source, sequence, &format!("{QUERY} ballast"));
        output.event_type = "tool_output".to_owned();
        records.push(output);
    }
    let best = document(&source, 21, &format!("{QUERY} {QUERY} {QUERY}"));
    let expected = best.event_id.as_uuid();
    records.push(best);
    let index = publish_records_in_one_segment(&temp, &source, records);
    let run = |budget| {
        lexical_search_batch_with_budget(
            &index,
            &[QUERY],
            &EventSearchFilters::default(),
            1,
            budget,
        )
        .unwrap()
    };
    let complete = run(LexicalWorkBudget {
        maximum_candidate_docs: 2,
        ..LEXICAL_WORK_BUDGET_V1
    });
    assert!(complete.complete && !complete.candidate_set_exhaustive);
    assert_eq!(complete.candidates[0].event.event_id, expected);
    assert_eq!(complete.counters.candidate_docs, 2);
    assert_eq!(complete.counters.body_posting_advances, 63);
    for (budget, counter, limit) in [
        (
            LexicalWorkBudget {
                maximum_candidate_docs: 1,
                ..LEXICAL_WORK_BUDGET_V1
            },
            LexicalWorkCounter::CandidateDocs,
            1,
        ),
        (
            LexicalWorkBudget {
                maximum_body_posting_advances: 12,
                ..LEXICAL_WORK_BUDGET_V1
            },
            LexicalWorkCounter::BodyPostingAdvances,
            12,
        ),
        (
            LexicalWorkBudget {
                maximum_filter_probes: 5,
                ..LEXICAL_WORK_BUDGET_V1
            },
            LexicalWorkCounter::FilterProbes,
            5,
        ),
    ] {
        let partial = run(budget);
        assert_exhausted_at(&partial, counter, limit, limit);
        assert!(!partial.candidate_set_exhaustive);
        assert_eq!(partial, run(budget));
    }
}

#[test]
fn pruned_but_filtered_documents_do_not_report_result_truncation() {
    let temp = tempdir().unwrap();
    let source = source("rank-bound-filtered.jsonl");
    let mut first = document(&source, 1, QUERY);
    add_literal_fact(&mut first, LiteralFactKind::File, "selected.rs");
    let mut rejected = document(&source, 2, "budgetalpha");
    rejected.occurred_at_unix_ms = Some(1);
    let index = publish_records_in_one_segment(&temp, &source, vec![first, rejected]);
    for filters in [
        EventSearchFilters {
            file: Some("selected.rs".to_owned()),
            ..EventSearchFilters::default()
        },
        EventSearchFilters {
            since_unix_ms: Some(100),
            ..EventSearchFilters::default()
        },
    ] {
        let batch = lexical_search_batch(&index, &[QUERY], &filters, 1).unwrap();
        assert!(batch.complete && batch.candidate_set_exhaustive);
        assert_eq!(batch.candidates.len(), 1);
    }
}

#[test]
fn coverage_bound_seeks_over_partial_matches_with_charged_work() {
    let temp = tempdir().unwrap();
    let source = source("coverage-pivot.jsonl");
    let mut records = vec![document(&source, 1, &format!("{QUERY} ballast ballast"))];
    for sequence in 2..=101 {
        records.push(document(&source, sequence, "budgetalpha"));
    }
    let best = document(&source, 102, &format!("{QUERY} {QUERY} {QUERY}"));
    let expected = best.event_id.as_uuid();
    records.push(best);
    let index = publish_records_in_one_segment(&temp, &source, records);
    let budget = ctx_history_index_query::LexicalWorkBudget {
        maximum_candidate_docs: 2,
        maximum_body_posting_advances: 8,
        ..LEXICAL_WORK_BUDGET_V1
    };
    let batch = lexical_search_batch_with_budget(
        &index,
        &[QUERY],
        &EventSearchFilters::default(),
        1,
        budget,
    )
    .unwrap();
    assert!(
        batch.complete && !batch.candidate_set_exhaustive,
        "{:?}",
        batch.exhaustion
    );
    assert_eq!(batch.candidates[0].event.event_id, expected);
    assert_eq!(batch.counters.candidate_docs, 2);
    assert_eq!(batch.counters.body_posting_advances, 8);
    let partial = lexical_search_batch_with_budget(
        &index,
        &[QUERY],
        &EventSearchFilters::default(),
        1,
        ctx_history_index_query::LexicalWorkBudget {
            maximum_body_posting_advances: 4,
            ..budget
        },
    )
    .unwrap();
    assert_exhausted_at(
        &partial,
        ctx_history_index_query::LexicalWorkCounter::BodyPostingAdvances,
        4,
        4,
    );
    assert_eq!(partial.candidates.len(), 1);
    assert_eq!(partial.exhaustion.unwrap().next_doc, Some(101));
}

#[test]
#[ignore = "large synthetic lexical latency and work measurement"]
fn broad_top_k_measurement() {
    let (_temp, index, expected) = broad_corpus(300_000);
    for _ in 0..5 {
        observed_candidates(&index, QUERY, 10).unwrap();
    }
    let mut elapsed = Vec::new();
    for _ in 0..30 {
        let start = std::time::Instant::now();
        observed_candidates(&index, QUERY, 10).unwrap();
        elapsed.push(start.elapsed().as_micros());
    }
    elapsed.sort_unstable();
    let batch = observed_candidates(&index, QUERY, 10).unwrap().batch;
    eprintln!(
        "events=300000 warm_samples=30 median_us={} p95_us={} best_found={} complete={} counters={:?} exhaustion={:?}",
        elapsed[15], elapsed[28], batch.candidates[0].event.event_id == expected,
        batch.complete, batch.counters, batch.exhaustion
    );
}
