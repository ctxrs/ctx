use super::top_k_oracle::{exhaustive, publish_model, random, ModelRow, TERMS};
use super::*;

fn adversarial_rows(source: &SourceKey, seed: u64) -> Vec<ModelRow> {
    let mut state = seed * 0x9e3779b9;
    (0..1154)
        .map(|offset| {
            let deleted = offset % 7 == 5;
            let segment = match offset {
                0 => 0,
                1..=127 => 1,
                128..=255 => 2,
                256..=384 => 3,
                _ => 4,
            };
            let mut frequencies = [0; 4];
            for (term, frequency) in frequencies.iter_mut().enumerate() {
                *frequency = if (segment + seed as usize) % 5 == term {
                    0
                } else {
                    random(&mut state, 12) as u32
                };
            }
            let mut filler = random(&mut state, [1, 4, 64, 256, 2048][segment]) as u32;
            if offset % 5 == 0 {
                frequencies = [1, 0, 0, 0];
                filler = 0;
            }
            if offset % 17 == 0 {
                frequencies = [0; 4];
                filler = 0;
            }
            if offset % 11 == 0 {
                frequencies = [3; 4];
                filler = 0;
            }
            if frequencies.iter().sum::<u32>() + filler == 0 {
                filler = 1;
            }
            let selected = offset % 3 == 0;
            let mut body = String::new();
            for (term, frequency) in TERMS.iter().zip(frequencies) {
                body.push_str(&format!("{term} ").repeat(frequency as usize));
            }
            body.push_str(&"ballast ".repeat(filler as usize));
            if deleted {
                body.push_str("graveyard");
            }
            let mut record = document_for_session(
                source,
                &format!("session-{}", offset % 9),
                (offset + 1) as u64,
                &body,
            );
            record.event_type = [
                "message",
                "summary",
                "tool_call",
                "command_started",
                "tool_output",
                "command_output",
                "command_finished",
            ][random(&mut state, 7) as usize]
                .to_owned();
            record.occurred_at_unix_ms = if offset % 13 == 0 {
                None
            } else {
                Some(random(&mut state, 200) as i64)
            };
            if selected {
                add_literal_fact(&mut record, LiteralFactKind::File, "selected");
            }
            let mut digest = [0x40; 32];
            if seed.is_multiple_of(2) {
                digest[..6].copy_from_slice(&random(&mut state, u64::MAX).to_be_bytes()[..6]);
            }
            digest[12..16].copy_from_slice(&(2000_u32 - offset as u32).to_be_bytes());
            digest[24..].copy_from_slice(&(offset as u64 + 1).to_be_bytes());
            record = with_event_identity_digest(record, digest);
            ModelRow {
                record,
                frequencies,
                length: frequencies.iter().sum::<u32>()
                    + filler
                    + u32::from(selected)
                    + u32::from(deleted),
                selected,
                deleted,
            }
        })
        .collect()
}

#[test]
fn top_k_skewed_segments_deletes_and_all_classes_match_oracle() {
    let mut checked = 0;
    for seed in 1..=8 {
        let temp = tempdir().unwrap();
        let source = source(&format!("synthetic-adversarial-{seed}.jsonl"));
        let rows = adversarial_rows(&source, seed);
        let index = publish_model(&temp, &source, &rows, &[1, 127, 128, 129, 769], false);
        let (searcher, _) = open_unverified_generation(temp.path());
        assert_eq!(
            searcher
                .segment_readers()
                .iter()
                .map(|s| s.max_doc() as usize)
                .sum::<usize>(),
            rows.len()
        );
        assert!(searcher.segment_readers().iter().any(|s| s.has_deletes()));
        for query in [
            vec![0],
            vec![0, 1],
            vec![0, 1, 2],
            vec![0, 1, 2, 3],
            vec![0, 1, 2, 3, 4],
        ] {
            let text = query
                .iter()
                .map(|&t| if t < 4 { TERMS[t] } else { "missingterm" })
                .collect::<Vec<_>>()
                .join(" ");
            for scope in [
                SearchContentScope::All,
                SearchContentScope::Transcript,
                SearchContentScope::Calls,
                SearchContentScope::Outputs,
            ] {
                for case in 0..4 {
                    let filters = EventSearchFilters {
                        content_scope: scope,
                        file: (case & 1 != 0).then(|| "selected".to_owned()),
                        since_unix_ms: (case & 2 != 0).then_some(100),
                        excluded_session_ids: if case == 3 {
                            vec![rows[0].record.session_id.as_uuid()]
                        } else {
                            vec![]
                        },
                        ..EventSearchFilters::default()
                    };
                    let expected = exhaustive(&rows, &query, &filters);
                    for limit in [1, 3, 10, 127, 4096] {
                        let batch =
                            lexical_search_batch(&index, &[&text], &filters, limit).unwrap();
                        assert!(batch.complete, "seed={seed} {:?}", batch.exhaustion);
                        let actual = batch
                            .candidates
                            .iter()
                            .map(|c| {
                                (
                                    c.event.event_identity_digest,
                                    c.score.to_bits(),
                                    c.coverage.matched_terms,
                                )
                            })
                            .collect::<Vec<_>>();
                        assert_eq!(
                            actual,
                            expected[..expected.len().min(limit)],
                            "seed={seed} query={text} scope={scope:?} case={case} limit={limit}"
                        );
                        assert_eq!(batch.candidate_set_exhaustive, expected.len() <= limit);
                        if limit == 1 {
                            assert_eq!(
                                batch,
                                lexical_search_batch(&index, &[&text], &filters, limit).unwrap()
                            );
                        }
                        checked += 1;
                    }
                }
            }
        }
    }
    eprintln!("top_k_adversarial comparisons={checked} exact_score_bits=true segment_sizes=1,127,128,129,769 tombstones=true classes=7");
}

#[test]
fn top_k_verified_malformed_tombstones_do_not_break_live_search() {
    let temp = tempdir().unwrap();
    let source = source("synthetic-deleted-fastfield.jsonl");
    let rows = adversarial_rows(&source, 2);
    let index = publish_model(&temp, &source, &rows, &[1154], true);
    let filters = EventSearchFilters::default();
    let expected = exhaustive(&rows, &[0, 1, 2, 3], &filters);
    let batch = lexical_search_batch(&index, &["alpha beta delta gamma"], &filters, 1).unwrap();
    assert!(batch.complete);
    assert_eq!(
        batch.candidates[0].event.event_identity_digest,
        expected[0].0
    );
    assert_eq!(batch.candidates[0].score.to_bits(), expected[0].1);
}

#[test]
fn top_k_production_competitive_limit_is_reported() {
    use ctx_history_index_query::{LexicalWorkCounter, LEXICAL_WORK_BUDGET_V1};
    let temp = tempdir().unwrap();
    let source = source("synthetic-competitive-identity.jsonl");
    let count = LEXICAL_WORK_BUDGET_V1.maximum_candidate_docs + 128;
    let records = (1..=count)
        .map(|sequence| {
            let mut digest = [0x40; 32];
            digest[12..16].copy_from_slice(&((count + 1 - sequence) as u32).to_be_bytes());
            with_event_identity_digest(document(&source, sequence, "alpha beta gamma"), digest)
        })
        .collect::<Vec<_>>();
    let expected_partial = records[65535].event_id.digest();
    let true_best = records.last().unwrap().event_id.digest();
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
    for record in records {
        writer.add_core_record(record).unwrap();
    }
    writer
        .certify_source(certificate(&source, 1, count))
        .unwrap();
    writer.commit(|_| true).unwrap();
    let (searcher, _) = open_unverified_generation(temp.path());
    assert_eq!(searcher.segment_readers().len(), 1);
    let index = VerifiedIndex::open(temp.path()).unwrap();
    let run = || {
        lexical_search_batch(
            &index,
            &["alpha beta gamma"],
            &EventSearchFilters::default(),
            1,
        )
        .unwrap()
    };
    let batch = run();
    assert_exhausted_at(&batch, LexicalWorkCounter::CandidateDocs, 65536, 65536);
    assert!(!batch.candidate_set_exhaustive);
    assert_eq!(
        batch.candidates[0].event.event_identity_digest,
        expected_partial
    );
    assert_ne!(batch.candidates[0].event.event_identity_digest, true_best);
    assert_eq!(batch, run());
}

#[test]
fn top_k_deleted_rows_must_not_consume_competitive_candidate_budget() {
    let temp = tempdir().unwrap();
    let source = source("synthetic-deleted-candidate-budget.jsonl");
    let deleted_count = 65_537_usize;
    let count = deleted_count * 4;
    let rows = (0..count)
        .map(|offset| {
            let deleted = (2..2 + deleted_count).contains(&offset);
            let (body, frequencies, length) = if deleted {
                (
                    "alpha beta delta alpha beta delta alpha beta delta alpha beta delta graveyard",
                    [4, 4, 4, 0],
                    13,
                )
            } else if offset == count - 1 {
                (
                    "alpha beta delta alpha beta delta alpha beta delta",
                    [3, 3, 3, 0],
                    9,
                )
            } else if offset < 2 {
                (
                    "alpha beta delta ballast ballast ballast ballast ballast ballast",
                    [1, 1, 1, 0],
                    9,
                )
            } else {
                ("ballast", [0; 4], 1)
            };
            ModelRow {
                record: document(&source, offset as u64 + 1, body),
                frequencies,
                length,
                selected: false,
                deleted,
            }
        })
        .collect::<Vec<_>>();
    let expected = rows.last().unwrap().record.event_id.digest();
    let index = publish_model(&temp, &source, &rows, &[count], false);
    let (searcher, _) = open_unverified_generation(temp.path());
    assert_eq!(searcher.segment_readers()[0].max_doc() as usize, count);
    assert_eq!(
        searcher.segment_readers()[0].num_deleted_docs() as usize,
        deleted_count
    );
    let batch = lexical_search_batch(
        &index,
        &["alpha beta delta"],
        &EventSearchFilters::default(),
        1,
    )
    .unwrap();
    eprintln!("top_k_deleted physical={count} deleted={deleted_count} best_found={} complete={} counters={:?} exhaustion={:?}", batch.candidates[0].event.event_identity_digest == expected, batch.complete, batch.counters, batch.exhaustion);
    assert!(
        batch.complete,
        "deleted rows consumed the competitive-candidate budget"
    );
    assert_eq!(batch.candidates[0].event.event_identity_digest, expected);
    assert!(batch.counters.candidate_docs <= 3);

    // Without a full heap there is no block pruning. Tombstone traversal is
    // still bounded, but only the three live rows are candidate examinations.
    let exhaustive = lexical_search_batch(
        &index,
        &["alpha beta delta"],
        &EventSearchFilters::default(),
        10,
    )
    .unwrap();
    assert!(exhaustive.complete && exhaustive.candidate_set_exhaustive);
    assert_eq!(exhaustive.candidates.len(), 3);
    assert_eq!(exhaustive.counters.candidate_docs, 3);
    assert_eq!(
        exhaustive.counters.body_posting_advances,
        (deleted_count as u64 + 3) * 3
    );
    assert_eq!(exhaustive.candidates[0], batch.candidates[0]);
}
