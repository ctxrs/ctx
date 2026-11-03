use super::*;
use ctx_history_index_query::{LexicalWorkBudget, LEXICAL_WORK_BUDGET_V1};

#[test]
fn dense_top_k_skips_blocks_beyond_a_posting_cap() {
    dense_measurement(4096, false);
}

#[test]
#[ignore = "large synthetic lexical latency and work measurement"]
fn dense_top_k_measurement() {
    dense_measurement(800_000, true);
    dense_measurement(3_000_000, true);
}

fn dense_measurement(count: u64, measure: bool) {
    const QUERY: &str = "and from the with";
    let temp = tempdir().unwrap();
    let source = source("review-dense-common-words.jsonl");
    let noise = format!("{QUERY} {}", "ballast ".repeat(27));
    let mut writer = GenerationWriter::open(
        temp.path(),
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 4_000_000_000,
        },
    )
    .unwrap()
    .into_writer()
    .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for sequence in 1..count {
        let body = if sequence % 10 == 0 {
            "ballast"
        } else {
            &noise
        };
        writer
            .add_core_record(document(&source, sequence, body))
            .unwrap();
    }
    let best = document_for_session(
        &source,
        "final-target",
        count,
        &format!("{QUERY} {QUERY} {QUERY}"),
    );
    let expected = best.event_id.as_uuid();
    let target_session = best.session_id.as_uuid();
    writer.add_core_record(best).unwrap();
    writer
        .certify_source(certificate(&source, 1, count))
        .unwrap();
    writer.commit(|_| true).unwrap();
    let (searcher, _) = open_unverified_generation(temp.path());
    assert_eq!(searcher.segment_readers().len(), 1);
    assert_eq!(u64::from(searcher.segment_readers()[0].num_docs()), count);
    let index = VerifiedIndex::open(temp.path()).unwrap();
    for query in ["and from the", QUERY] {
        let budget = LexicalWorkBudget {
            maximum_body_posting_advances: if measure {
                LEXICAL_WORK_BUDGET_V1.maximum_body_posting_advances
            } else {
                4096
            },
            ..LEXICAL_WORK_BUDGET_V1
        };
        let run = || {
            lexical_search_batch_with_budget(
                &index,
                &[query],
                &EventSearchFilters::default(),
                if measure { 10 } else { 1 },
                budget,
            )
            .unwrap()
        };
        for _ in 0..if measure { 5 } else { 0 } {
            run();
        }
        let mut samples = Vec::new();
        for _ in 0..if measure { 30 } else { 1 } {
            let start = std::time::Instant::now();
            run();
            samples.push(start.elapsed().as_micros());
        }
        samples.sort_unstable();
        let batch = run();
        eprintln!(
            "dense_top_k events={count} matching_events={} terms={} median_us={} p95_us={} best_found={} complete={} exhaustive={} counters={:?} exhaustion={:?}",
            count - (count - 1) / 10,
            query.split_whitespace().count(),
            samples[samples.len() / 2],
            samples[samples.len() * 95 / 100],
            batch.candidates[0].event.event_id == expected,
            batch.complete,
            batch.candidate_set_exhaustive,
            batch.counters,
            batch.exhaustion
        );
        assert!(batch.complete, "{:?}", batch.exhaustion);
        assert!(!batch.candidate_set_exhaustive);
        assert_eq!(batch.candidates[0].event.event_id, expected);
        assert!(batch.counters.candidate_docs < 1000);
        assert!(batch.counters.body_posting_advances < count);
        // A narrow positive filter reaches the final row using charged seeks,
        // proving it exists and has a strictly higher score in the same corpus.
        let final_only = lexical_search_batch(
            &index,
            &[query],
            &EventSearchFilters {
                session_id: Some(target_session),
                ..EventSearchFilters::default()
            },
            10,
        )
        .unwrap();
        assert!(final_only.complete && final_only.candidate_set_exhaustive);
        assert_eq!(final_only.candidates[0].event.event_id, expected);
        assert_eq!(final_only.candidates[0], batch.candidates[0]);
        eprintln!(
            "dense_top_k target_score={} returned_best_score={} target_filtered_counters={:?}",
            final_only.candidates[0].score, batch.candidates[0].score, final_only.counters
        );
    }
}
