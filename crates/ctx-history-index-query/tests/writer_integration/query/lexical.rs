use super::*;

fn publish_records(temp: &TempDir, source: &SourceKey, records: Vec<CoreRecord>) -> VerifiedIndex {
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
        .certify_source(certificate(source, 1, document_count))
        .unwrap();
    writer.commit(|_| true).unwrap();
    VerifiedIndex::open(temp.path()).unwrap()
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
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(cjk.clone()).unwrap();
    writer.add_core_record(identifier.clone()).unwrap();
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
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(partial.clone()).unwrap();
    writer.add_core_record(unrelated).unwrap();
    writer.add_core_record(exact.clone()).unwrap();
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
    let batch = index
        .search_event_candidates_batch("coveragealpha coveragebeta", 10)
        .unwrap();
    assert!(batch.complete);
    assert_eq!(batch.exhaustion, None);
    assert_eq!(
        batch
            .candidates
            .iter()
            .map(|candidate| candidate.coverage)
            .collect::<Vec<_>>(),
        vec![
            ctx_history_index_query::LexicalTermCoverage {
                matched_terms: 2,
                query_terms: 2,
            },
            ctx_history_index_query::LexicalTermCoverage {
                matched_terms: 1,
                query_terms: 2,
            },
        ]
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
fn coverage_ranking_executes_once_and_materializes_each_ranked_candidate_once() {
    let temp = tempdir().unwrap();
    let source = source("coverage-decode-count.jsonl");
    let full = document(&source, 1, "decodealpha decodebeta decodegamma");
    let two_terms = document(
        &source,
        2,
        &format!("{} {}", "decodealpha ".repeat(32), "decodebeta ".repeat(32)),
    );
    let one_term = document(&source, 3, &"decodealpha ".repeat(96));
    let expected = vec![full.event_id, two_terms.event_id, one_term.event_id];
    let expected_encoded_core_bytes = [&full, &two_terms, &one_term]
        .into_iter()
        .map(|record| u64::try_from(record.encode_stored().unwrap().len()).unwrap())
        .sum::<u64>();
    let index = publish_records(&temp, &source, vec![one_term, two_terms, full]);

    ctx_history_index_query::reset_stored_event_record_materializations();
    let observed = observed_candidates(&index, "decodealpha decodebeta decodegamma", 3).unwrap();
    let candidates = observed.candidates;

    assert_eq!(
        candidates
            .iter()
            .map(|candidate| candidate.event.event_id)
            .collect::<Vec<_>>(),
        expected
    );
    assert_eq!(
        ctx_history_index_query::stored_event_record_materializations(),
        candidates.len(),
        "the manual pass decodes each retained Core record exactly once"
    );
    assert_eq!(observed.receipt.query_executions, 1);
    assert_eq!(observed.receipt.collector_hits, 3);
    assert_eq!(observed.receipt.records_decoded, 3);
    assert_eq!(
        observed.receipt.encoded_core_bytes_decoded,
        expected_encoded_core_bytes
    );
}

#[test]
fn empty_and_no_match_queries_distinguish_unattempted_from_exact_zero_work() {
    let (_temp, index) = lexical_query_limit_fixture();

    let empty = observed_candidates(&index, "", 10).unwrap();
    let no_match = observed_candidates(&index, "uniquenonexistentreceiptneedle", 10).unwrap();

    assert_eq!(
        empty.receipt,
        ctx_history_index_query::EventCandidateQueryReceipt::default()
    );
    assert_eq!(no_match.receipt.query_executions, 1);
    assert_eq!(no_match.receipt.collector_hits, 0);
    assert_eq!(no_match.receipt.records_decoded, 0);
    assert_eq!(no_match.receipt.encoded_core_bytes_decoded, 0);
}

#[test]
fn candidate_query_receipt_needs_no_drop() {
    assert!(!std::mem::needs_drop::<
        ctx_history_index_query::EventCandidateQueryReceipt,
    >());
}

#[test]
fn candidate_decode_failure_preserves_completed_low_level_work() {
    let temp = tempdir().unwrap();
    let source = source("partial-failure-receipt.jsonl");
    let first = document(&source, 1, "partialfailurereceiptneedle first");
    let second = document(&source, 2, "partialfailurereceiptneedle second");
    let encoded_sizes = [&first, &second]
        .into_iter()
        .map(|record| {
            (
                record.event_id,
                u64::try_from(record.encode_stored().unwrap().len()).unwrap(),
            )
        })
        .collect::<std::collections::HashMap<_, _>>();
    let index = publish_records(&temp, &source, vec![first, second]);
    let successful = observed_candidates(&index, "partialfailurereceiptneedle", 2).unwrap();
    let first_decoded_bytes = encoded_sizes[&successful.candidates[0].event.event_id];

    ctx_history_index_query::fail_lexical_candidate_materialization_after(1);
    let failure = observed_candidates(&index, "partialfailurereceiptneedle", 2).unwrap_err();

    assert!(matches!(
        failure.error,
        IndexError::InvalidStoredDocumentField("test_lexical_candidate_materialization_failure")
    ));
    assert_eq!(failure.receipt.query_executions, 1);
    assert_eq!(failure.receipt.collector_hits, 2);
    assert_eq!(failure.receipt.records_decoded, 1);
    assert_eq!(
        failure.receipt.encoded_core_bytes_decoded,
        first_decoded_bytes
    );
}

#[test]
fn candidate_decode_failure_injection_is_cleared_after_each_query() {
    let temp = tempdir().unwrap();
    let source = source("failure-injection-reset.jsonl");
    let index = publish_records(
        &temp,
        &source,
        vec![
            document(&source, 1, "failureinjectionresetneedle first"),
            document(&source, 2, "failureinjectionresetneedle second"),
        ],
    );

    ctx_history_index_query::fail_lexical_candidate_materialization_after(2);
    observed_candidates(&index, "failureinjectionresetneedle", 1).unwrap();

    observed_candidates(&index, "failureinjectionresetneedle", 2)
        .expect("unused failure injection state must not leak into the next query");
}

fn observed_candidates(
    index: &VerifiedIndex,
    query: &str,
    limit: usize,
) -> ctx_history_index_query::DiagnosedEventCandidateQueryResult {
    index.search_event_candidates_any_with_filters_diagnosed(
        &[query],
        &EventSearchFilters::default(),
        limit,
    )
}

fn lexical_query_limit_fixture() -> (TempDir, VerifiedIndex) {
    let temp = tempdir().unwrap();
    let source = source("query-limits.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer
        .add_core_record(document(&source, 1, "bounded lexical query"))
        .unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open(temp.path()).unwrap();
    (temp, index)
}

fn assert_no_lexical_query_was_constructed_or_executed() {
    assert_eq!(ctx_history_index_query::lexical_query_constructions(), 0);
    assert_eq!(ctx_history_index_query::lexical_query_executions(), 0);
}

#[test]
fn lexical_result_limits_reject_oversized_and_usize_max_before_query_work() {
    let (_temp, index) = lexical_query_limit_fixture();
    for requested in [MAX_LEXICAL_QUERY_RESULTS + 1, usize::MAX] {
        ctx_history_index_query::reset_lexical_query_work();
        let error = index
            .search_event_candidates("bounded", requested)
            .unwrap_err();
        assert!(matches!(
            error,
            ctx_history_index_query::LexicalSearchError::Index(
                IndexError::InvalidLexicalResultLimit {
                requested: actual,
                maximum
            })
                if actual == requested && maximum == MAX_LEXICAL_QUERY_RESULTS
        ));
        assert_no_lexical_query_was_constructed_or_executed();

        ctx_history_index_query::reset_lexical_query_work();
        let error = index
            .list_event_candidates_with_filters(&EventSearchFilters::default(), requested)
            .unwrap_err();
        assert!(matches!(
            error,
            ctx_history_index_query::LexicalSearchError::Index(
                IndexError::InvalidLexicalResultLimit {
                requested: actual,
                maximum
            })
                if actual == requested && maximum == MAX_LEXICAL_QUERY_RESULTS
        ));
        assert_no_lexical_query_was_constructed_or_executed();
    }
}

#[test]
fn timestamps_never_break_equal_relevance_ties() {
    let temp = tempdir().unwrap();
    let source = source("stable-identity-tie.jsonl");
    let mut first = document_for_session(&source, "first", 1, "stable tie needle");
    let mut second = document_for_session(&source, "second", 1, "stable tie needle");
    add_literal_fact(&mut first, LiteralFactKind::File, "src/stable-tie.rs");
    add_literal_fact(&mut second, LiteralFactKind::File, "src/stable-tie.rs");
    let (expected, newer) = if first.event_id.digest() < second.event_id.digest() {
        (first.event_id, second.event_id)
    } else {
        (second.event_id, first.event_id)
    };
    if first.event_id == expected {
        first.occurred_at_unix_ms = Some(100);
        second.occurred_at_unix_ms = Some(200);
    } else {
        first.occurred_at_unix_ms = Some(200);
        second.occurred_at_unix_ms = Some(100);
    }
    let index = publish_records(&temp, &source, vec![first, second]);

    let lexical = index
        .search_event_candidates_batch("stable tie needle", 10)
        .unwrap();
    assert_eq!(lexical.candidates[0].event.event_id, expected);
    assert_eq!(lexical.candidates[1].event.event_id, newer);

    let listed = index
        .list_event_candidates_with_filters_batch(
            &EventSearchFilters {
                file: Some("stable-tie.rs".to_owned()),
                ..EventSearchFilters::default()
            },
            10,
        )
        .unwrap();
    assert_eq!(listed.candidates[0].event.event_id, expected);
    assert_eq!(listed.candidates[1].event.event_id, newer);
}

#[test]
fn oversized_single_query_is_rejected_before_query_construction() {
    let (_temp, index) = lexical_query_limit_fixture();
    let oversized = "x".repeat(LEXICAL_QUERY_LIMITS.maximum_aggregate_bytes + 1);
    ctx_history_index_query::reset_lexical_query_work();

    let error = index.search_event_candidates(&oversized, 10).unwrap_err();

    assert!(matches!(
        error,
        ctx_history_index_query::LexicalSearchError::Index(IndexError::LexicalQueryBytesTooLarge {
            actual,
            maximum,
        }) if actual == LEXICAL_QUERY_LIMITS.maximum_aggregate_bytes + 1
            && maximum == LEXICAL_QUERY_LIMITS.maximum_aggregate_bytes
    ));
    assert_no_lexical_query_was_constructed_or_executed();
}

#[test]
fn repeated_terms_are_rejected_before_query_construction() {
    let (_temp, index) = lexical_query_limit_fixture();
    let alternatives = vec!["bounded"; LEXICAL_QUERY_LIMITS.maximum_alternatives + 1];
    ctx_history_index_query::reset_lexical_query_work();

    let error = index
        .search_event_candidates_any_with_filters(&alternatives, &EventSearchFilters::default(), 10)
        .unwrap_err();

    assert!(matches!(
        error,
        ctx_history_index_query::LexicalSearchError::Index(IndexError::LexicalQueryAlternativesTooMany {
            observed,
            maximum
        })
            if observed == LEXICAL_QUERY_LIMITS.maximum_alternatives + 1
                && maximum == LEXICAL_QUERY_LIMITS.maximum_alternatives
    ));
    assert_no_lexical_query_was_constructed_or_executed();
}

#[test]
fn analyzed_tokens_are_rejected_before_deduplication_or_query_construction() {
    let (_temp, index) = lexical_query_limit_fixture();
    let query = (0..=LEXICAL_QUERY_LIMITS.maximum_unique_tokens)
        .map(|index| format!("uniquetoken{index}"))
        .collect::<Vec<_>>()
        .join(" ");
    ctx_history_index_query::reset_lexical_query_work();

    let error = index.search_event_candidates(&query, 10).unwrap_err();

    assert!(matches!(
        error,
        ctx_history_index_query::LexicalSearchError::Index(IndexError::LexicalQueryTokensTooMany {
            observed,
            maximum
        })
            if observed == LEXICAL_QUERY_LIMITS.maximum_unique_tokens + 1
                && maximum == LEXICAL_QUERY_LIMITS.maximum_unique_tokens
    ));
    assert_no_lexical_query_was_constructed_or_executed();

    let repeated = std::iter::repeat_n("bounded", LEXICAL_QUERY_LIMITS.maximum_unique_tokens + 1)
        .collect::<Vec<_>>()
        .join(" ");
    let error = index.search_event_candidates(&repeated, 10).unwrap_err();
    assert!(matches!(
        error,
        ctx_history_index_query::LexicalSearchError::Index(
            IndexError::LexicalQueryTokensTooMany { observed, maximum }
        ) if observed == LEXICAL_QUERY_LIMITS.maximum_unique_tokens + 1
            && maximum == LEXICAL_QUERY_LIMITS.maximum_unique_tokens
    ));
    assert_no_lexical_query_was_constructed_or_executed();
}

fn manual_budget_fixture() -> (
    TempDir,
    VerifiedIndex,
    ctx_history_core::StableEntityId,
    ctx_history_core::StableEntityId,
) {
    let temp = tempdir().unwrap();
    let source = source("manual-budget.jsonl");
    let decoy = document(&source, 1, "decoyterm");
    let mut first = document(&source, 2, "manualbudgetneedle");
    let mut second = document(&source, 3, "manualbudgetneedle decoyterm");
    for record in [&mut first, &mut second] {
        add_literal_fact(record, LiteralFactKind::Workspace, "/Work/ManualBudget");
        add_literal_fact(record, LiteralFactKind::File, "src/ManualBudget.rs");
        record.validate_contract().unwrap();
    }
    let first_id = first.event_id;
    let second_id = second.event_id;
    let records = vec![decoy, first, second];
    let index = publish_records(&temp, &source, records.clone());
    drop(index);

    // GenerationWriter deliberately emits independently replaceable source
    // segments. Rewrite the same verified documents together so the seek
    // boundary exercises a posting cursor behind a later candidate doc.
    let (searcher, manifest) = open_unverified_generation(temp.path());
    drop(searcher);
    let directory = DurableMmapDirectory::open(active_generation_path(temp.path())).unwrap();
    let tantivy_index = Index::open(directory).unwrap();
    publish_unchecked_generation(
        temp.path(),
        &tantivy_index,
        manifest,
        std::slice::from_ref(&source),
        records
            .into_iter()
            .enumerate()
            .map(|(index, record)| {
                let authority = ctx_history_index_format::SessionAuthorityKey::exact(
                    record.session_id,
                    record.source.identity(),
                )
                .unwrap();
                let mut document = indexed_document(record);
                if index == 0 {
                    let fields = fields_from_schema(&lexical_schema()).unwrap();
                    document.add_bytes(fields.session_authority, authority.as_bytes());
                }
                document
            })
            .collect(),
    );
    let index = VerifiedIndex::open(temp.path()).unwrap();
    (temp, index, first_id, second_id)
}

fn counter_value(
    counters: &ctx_history_index_query::LexicalWorkCounters,
    counter: ctx_history_index_query::LexicalWorkCounter,
) -> u64 {
    use ctx_history_index_query::LexicalWorkCounter;

    match counter {
        LexicalWorkCounter::Segments => counters.segments,
        LexicalWorkCounter::CandidateDocs => counters.candidate_docs,
        LexicalWorkCounter::BodyPostingAdvances => counters.body_posting_advances,
        LexicalWorkCounter::ExactFilterTerms => counters.exact_filter_terms,
        LexicalWorkCounter::FilterInputBytes => counters.filter_input_bytes,
        LexicalWorkCounter::DictionaryLookups => counters.dictionary_lookups,
        LexicalWorkCounter::PostingOpens => counters.posting_opens,
        LexicalWorkCounter::FilterProbes => counters.filter_probes,
        LexicalWorkCounter::FilterSeeks => counters.filter_seeks,
        LexicalWorkCounter::SubstringDictionarySteps => counters.substring_dictionary_steps,
        LexicalWorkCounter::SubstringDictionaryBytes => counters.substring_dictionary_bytes,
        LexicalWorkCounter::SubstringPostingDocs => counters.substring_posting_docs,
        LexicalWorkCounter::SubstringBitmapBytes => counters.substring_bitmap_bytes,
        LexicalWorkCounter::RetainedCandidates => counters.retained_candidates,
        LexicalWorkCounter::FinalMaterializations => counters.final_materializations,
        LexicalWorkCounter::FinalMaterializationBytes => counters.final_materialization_bytes,
        LexicalWorkCounter::TermExpansions => counters.term_expansions,
    }
}

fn assert_exhausted_at(
    batch: &ctx_history_index_query::LexicalSearchBatch,
    counter: ctx_history_index_query::LexicalWorkCounter,
    used: u64,
    limit: u64,
) {
    assert!(
        !batch.complete,
        "expected {counter:?} exhaustion, got complete counters {:?}",
        batch.counters
    );
    assert!(!batch.candidate_set_exhaustive);
    let exhaustion = batch.exhaustion.as_ref().unwrap();
    assert_eq!(exhaustion.counter, counter);
    assert_eq!(exhaustion.used, used);
    assert_eq!(exhaustion.limit, limit);
    assert_eq!(counter_value(&batch.counters, counter), used);
    assert!(used <= limit, "a rejected operation must never be charged");
}

#[test]
fn alternatives_execute_one_manual_pass_and_report_deduplicated_coverage() {
    let (_temp, index, first_id, second_id) = manual_budget_fixture();
    ctx_history_index_query::reset_lexical_query_work();

    let batch = index
        .search_event_candidates_any_with_filters_batch(
            &["manualbudgetneedle", "decoyterm", "manualbudgetneedle"],
            &EventSearchFilters::default(),
            10,
        )
        .unwrap();

    assert!(batch.complete);
    assert!(batch.candidate_set_exhaustive);
    assert_eq!(batch.exhaustion, None);
    assert_eq!(ctx_history_index_query::lexical_query_constructions(), 0);
    assert_eq!(ctx_history_index_query::lexical_query_executions(), 1);
    assert_eq!(batch.counters.analyzed_tokens, 3);
    assert_eq!(batch.counters.candidate_docs, 3);
    assert_eq!(batch.counters.body_posting_advances, 4);
    assert_eq!(batch.counters.term_expansions, 0);
    assert_eq!(batch.candidates[0].event.event_id, second_id);
    assert_eq!(
        batch.candidates[0].coverage,
        ctx_history_index_query::LexicalTermCoverage {
            matched_terms: 2,
            query_terms: 2,
        }
    );
    let first = batch
        .candidates
        .iter()
        .find(|candidate| candidate.event.event_id == first_id)
        .unwrap();
    assert_eq!(
        first.coverage,
        ctx_history_index_query::LexicalTermCoverage {
            matched_terms: 1,
            query_terms: 2,
        }
    );
}

#[test]
fn manual_executor_charges_each_material_budget_before_work() {
    use ctx_history_index_query::LexicalWorkCounter;

    let (_temp, index, _, _) = manual_budget_fixture();
    let filters = EventSearchFilters::default();
    let run = |budget| {
        index
            .search_event_candidates_any_with_filters_batch_with_budget_for_test(
                &["manualbudgetneedle"],
                &filters,
                10,
                budget,
            )
            .unwrap()
    };

    let mut budget = ctx_history_index_query::LEXICAL_WORK_BUDGET_V1;
    budget.maximum_exact_filter_terms = 0;
    let batch = run(budget);
    assert_exhausted_at(&batch, LexicalWorkCounter::ExactFilterTerms, 0, 0);
    assert_eq!(batch.counters.segments, 0);

    let provider_filters = EventSearchFilters {
        provider: Some("codex".to_owned()),
        ..EventSearchFilters::default()
    };
    let mut budget = ctx_history_index_query::LEXICAL_WORK_BUDGET_V1;
    budget.maximum_filter_input_bytes = 4;
    let batch = index
        .search_event_candidates_any_with_filters_batch_with_budget_for_test(
            &["manualbudgetneedle"],
            &provider_filters,
            10,
            budget,
        )
        .unwrap();
    assert_exhausted_at(&batch, LexicalWorkCounter::FilterInputBytes, 0, 4);
    assert_eq!(batch.counters.segments, 0);

    let mut budget = ctx_history_index_query::LEXICAL_WORK_BUDGET_V1;
    budget.maximum_segments = 0;
    let batch = run(budget);
    assert_exhausted_at(&batch, LexicalWorkCounter::Segments, 0, 0);
    assert!(batch.exhaustion.as_ref().unwrap().segment.is_none());
    assert_eq!(batch.counters.dictionary_lookups, 0);

    let mut budget = ctx_history_index_query::LEXICAL_WORK_BUDGET_V1;
    budget.maximum_dictionary_lookups = 0;
    index.reset_manual_lexical_io_observability_for_test();
    let batch = run(budget);
    assert_exhausted_at(&batch, LexicalWorkCounter::DictionaryLookups, 0, 0);
    assert_eq!(batch.counters.posting_opens, 0);
    assert_eq!(
        index.manual_inverted_index_acquisitions_for_test(),
        0,
        "zero body dictionary budget must precede inverted-index acquisition"
    );

    index.reset_manual_lexical_io_observability_for_test();
    let batch = index
        .list_event_candidates_with_filters_batch_with_budget_for_test(&filters, 10, budget)
        .unwrap();
    assert_exhausted_at(&batch, LexicalWorkCounter::DictionaryLookups, 0, 0);
    assert_eq!(
        index.manual_inverted_index_acquisitions_for_test(),
        0,
        "zero exact-filter dictionary budget must precede inverted-index acquisition"
    );

    let mut budget = ctx_history_index_query::LEXICAL_WORK_BUDGET_V1;
    budget.maximum_posting_opens = 0;
    index.reset_manual_lexical_io_observability_for_test();
    let batch = run(budget);
    assert_exhausted_at(&batch, LexicalWorkCounter::PostingOpens, 0, 0);
    assert_eq!(batch.counters.candidate_docs, 0);
    assert_eq!(
        index.manual_posting_reads_for_test(),
        0,
        "zero posting-open budget must precede every posting-list read"
    );

    let mut budget = ctx_history_index_query::LEXICAL_WORK_BUDGET_V1;
    budget.maximum_candidate_docs = 0;
    let batch = run(budget);
    assert_exhausted_at(&batch, LexicalWorkCounter::CandidateDocs, 0, 0);
    assert_eq!(batch.counters.filter_probes, 0);

    let mut budget = ctx_history_index_query::LEXICAL_WORK_BUDGET_V1;
    budget.maximum_filter_probes = 0;
    let batch = run(budget);
    assert_exhausted_at(&batch, LexicalWorkCounter::FilterProbes, 0, 0);
    assert_eq!(batch.counters.filter_seeks, 0);
    assert_eq!(batch.counters.retained_candidates, 0);

    let mut budget = ctx_history_index_query::LEXICAL_WORK_BUDGET_V1;
    budget.maximum_filter_seeks = 0;
    let batch = run(budget);
    assert_exhausted_at(&batch, LexicalWorkCounter::FilterSeeks, 0, 0);
    assert_eq!(batch.counters.retained_candidates, 0);

    let mut budget = ctx_history_index_query::LEXICAL_WORK_BUDGET_V1;
    budget.maximum_retained_candidates = 0;
    let batch = run(budget);
    assert_exhausted_at(&batch, LexicalWorkCounter::RetainedCandidates, 0, 0);
    assert_eq!(batch.counters.final_materializations, 0);

    let mut budget = ctx_history_index_query::LEXICAL_WORK_BUDGET_V1;
    budget.maximum_body_posting_advances = 0;
    let batch = run(budget);
    assert_exhausted_at(&batch, LexicalWorkCounter::BodyPostingAdvances, 0, 0);
    assert_eq!(batch.counters.candidate_docs, 1);
    assert_eq!(batch.counters.retained_candidates, 1);
    assert_eq!(batch.candidates.len(), 1);

    let mut budget = ctx_history_index_query::LEXICAL_WORK_BUDGET_V1;
    budget.maximum_final_materializations = 0;
    ctx_history_index_query::reset_stored_event_record_materializations();
    let batch = run(budget);
    assert_exhausted_at(&batch, LexicalWorkCounter::FinalMaterializations, 0, 0);
    assert!(batch.candidates.is_empty());
    assert_eq!(
        ctx_history_index_query::stored_event_record_materializations(),
        0
    );

    let mut budget = ctx_history_index_query::LEXICAL_WORK_BUDGET_V1;
    budget.maximum_final_materialization_bytes = 0;
    ctx_history_index_query::reset_stored_event_record_materializations();
    let batch = run(budget);
    assert_exhausted_at(&batch, LexicalWorkCounter::FinalMaterializationBytes, 0, 0);
    assert_eq!(batch.counters.final_materializations, 0);
    assert_eq!(
        ctx_history_index_query::stored_event_record_materializations(),
        0
    );
    assert_eq!(batch.counters.term_expansions, 0);
}

#[test]
fn substring_filters_use_bounded_literal_fact_bitmaps_without_core_confirmation() {
    use ctx_history_index_query::LexicalWorkCounter;

    let (_temp, index, _, _) = manual_budget_fixture();
    let plain = index
        .search_event_candidates_batch("manualbudgetneedle", 10)
        .unwrap();
    let filters = EventSearchFilters {
        workspace: Some("manualBUDGET".to_owned()),
        file: Some("MANUALbudget.RS".to_owned()),
        ..EventSearchFilters::default()
    };
    let filtered = index
        .search_event_candidates_with_filters_batch("manualbudgetneedle", &filters, 10)
        .unwrap();
    assert!(filtered.complete);
    assert_eq!(filtered.candidates.len(), 2);
    assert_eq!(filtered.counters.substring_dictionary_steps, 7);
    assert!(filtered.counters.substring_dictionary_bytes > 0);
    assert_eq!(filtered.counters.substring_posting_docs, 4);
    assert_eq!(filtered.counters.substring_bitmap_bytes, 16);
    assert_eq!(filtered.counters.term_expansions, 2);
    assert_eq!(
        filtered.counters.posting_opens,
        plain.counters.posting_opens + 2,
        "each matching literal term is consumed and dropped immediately"
    );

    let mut budget = ctx_history_index_query::LEXICAL_WORK_BUDGET_V1;
    budget.maximum_substring_bitmap_bytes = 0;
    ctx_history_index_query::reset_core_record_decodes();
    let batch = index
        .search_event_candidates_any_with_filters_batch_with_budget_for_test(
            &["manualbudgetneedle"],
            &filters,
            10,
            budget,
        )
        .unwrap();
    assert_exhausted_at(&batch, LexicalWorkCounter::SubstringBitmapBytes, 0, 0);
    assert_eq!(batch.counters.substring_dictionary_steps, 0);
    assert_eq!(ctx_history_index_query::core_record_decodes(), 0);

    let mut budget = ctx_history_index_query::LEXICAL_WORK_BUDGET_V1;
    budget.maximum_substring_dictionary_steps = 0;
    ctx_history_index_query::reset_core_record_decodes();
    let batch = index
        .search_event_candidates_any_with_filters_batch_with_budget_for_test(
            &["manualbudgetneedle"],
            &filters,
            10,
            budget,
        )
        .unwrap();
    assert_exhausted_at(&batch, LexicalWorkCounter::SubstringDictionarySteps, 0, 0);
    assert_eq!(batch.counters.term_expansions, 0);
    assert_eq!(ctx_history_index_query::core_record_decodes(), 0);

    let mut budget = ctx_history_index_query::LEXICAL_WORK_BUDGET_V1;
    budget.maximum_substring_dictionary_bytes = 0;
    ctx_history_index_query::reset_core_record_decodes();
    let batch = index
        .search_event_candidates_any_with_filters_batch_with_budget_for_test(
            &["manualbudgetneedle"],
            &filters,
            10,
            budget,
        )
        .unwrap();
    assert_eq!(
        batch.exhaustion.as_ref().unwrap().counter,
        LexicalWorkCounter::SubstringDictionaryBytes
    );
    assert_eq!(batch.counters.substring_dictionary_bytes, 0);
    assert_eq!(batch.counters.term_expansions, 0);
    assert_eq!(ctx_history_index_query::core_record_decodes(), 0);

    let mut budget = ctx_history_index_query::LEXICAL_WORK_BUDGET_V1;
    budget.maximum_term_expansions = 0;
    let batch = index
        .search_event_candidates_any_with_filters_batch_with_budget_for_test(
            &["manualbudgetneedle"],
            &filters,
            10,
            budget,
        )
        .unwrap();
    assert_exhausted_at(&batch, LexicalWorkCounter::TermExpansions, 0, 0);
    assert_eq!(batch.counters.substring_posting_docs, 0);

    let mut budget = ctx_history_index_query::LEXICAL_WORK_BUDGET_V1;
    budget.maximum_substring_posting_docs = 0;
    let batch = index
        .search_event_candidates_any_with_filters_batch_with_budget_for_test(
            &["manualbudgetneedle"],
            &filters,
            10,
            budget,
        )
        .unwrap();
    assert_exhausted_at(&batch, LexicalWorkCounter::SubstringPostingDocs, 0, 0);
    assert_eq!(batch.counters.term_expansions, 1);
}

#[test]
fn heap_truncation_and_work_exhaustion_have_distinct_complete_signals() {
    use ctx_history_index_query::LexicalWorkCounter;

    let (_temp, index, _, _) = manual_budget_fixture();
    let complete = index
        .search_event_candidates_batch("manualbudgetneedle", 10)
        .unwrap();
    assert_eq!(complete.candidates.len(), 2);
    assert!(complete.complete);
    assert!(complete.candidate_set_exhaustive);

    let exactly_retained = index
        .search_event_candidates_batch("manualbudgetneedle", 2)
        .unwrap();
    assert!(exactly_retained.complete);
    assert!(
        exactly_retained.candidate_set_exhaustive,
        "filling the heap is exhaustive when no admissible match is discarded"
    );
    let relevance_truncated = index
        .search_event_candidates_batch("manualbudgetneedle", 1)
        .unwrap();
    assert!(relevance_truncated.complete);
    assert!(!relevance_truncated.candidate_set_exhaustive);
    assert_eq!(relevance_truncated.exhaustion, None);
    assert_eq!(relevance_truncated.candidates.len(), 1);
    let zero_limit = index
        .search_event_candidates_batch("manualbudgetneedle", 0)
        .unwrap();
    assert!(zero_limit.complete);
    assert!(!zero_limit.candidate_set_exhaustive);
    let no_terms = index.search_event_candidates_batch("", 10).unwrap();
    assert!(no_terms.complete);
    assert!(no_terms.candidate_set_exhaustive);

    let mut budget = ctx_history_index_query::LEXICAL_WORK_BUDGET_V1;
    budget.maximum_final_materializations = 1;
    let partial = index
        .search_event_candidates_any_with_filters_batch_with_budget_for_test(
            &["manualbudgetneedle"],
            &EventSearchFilters::default(),
            10,
            budget,
        )
        .unwrap();
    assert_exhausted_at(&partial, LexicalWorkCounter::FinalMaterializations, 1, 1);
    assert_eq!(partial.candidates.len(), 1);
    assert_eq!(
        partial.candidates[0].event.event_id, complete.candidates[0].event.event_id,
        "final exhaustion returns only the materialized leading prefix"
    );

    let leading_encoded_bytes = u64::try_from(
        index
            .core_record_by_id(complete.candidates[0].event.event_id.as_uuid())
            .unwrap()
            .unwrap()
            .encode_stored()
            .unwrap()
            .len(),
    )
    .unwrap();
    let mut budget = ctx_history_index_query::LEXICAL_WORK_BUDGET_V1;
    budget.maximum_final_materialization_bytes = leading_encoded_bytes;
    let byte_partial = index
        .search_event_candidates_any_with_filters_batch_with_budget_for_test(
            &["manualbudgetneedle"],
            &EventSearchFilters::default(),
            10,
            budget,
        )
        .unwrap();
    assert_exhausted_at(
        &byte_partial,
        LexicalWorkCounter::FinalMaterializationBytes,
        leading_encoded_bytes,
        leading_encoded_bytes,
    );
    assert_eq!(byte_partial.counters.final_materializations, 1);
    assert_eq!(byte_partial.candidates.len(), 1);
    assert_eq!(
        byte_partial.candidates[0].event.event_id, complete.candidates[0].event.event_id,
        "an oversized next item must not cause the leading result to be skipped"
    );

    let mut budget = ctx_history_index_query::LEXICAL_WORK_BUDGET_V1;
    budget.maximum_candidate_docs = 0;
    let work_exhausted = index
        .search_event_candidates_any_with_filters_batch_with_budget_for_test(
            &["manualbudgetneedle"],
            &EventSearchFilters::default(),
            10,
            budget,
        )
        .unwrap();
    assert!(!work_exhausted.complete);
    assert!(!work_exhausted.candidate_set_exhaustive);
    assert_eq!(
        work_exhausted.exhaustion.as_ref().unwrap().counter,
        LexicalWorkCounter::CandidateDocs
    );
    let error = index
        .search_event_candidates_any_with_filters_with_budget_for_test(
            &["manualbudgetneedle"],
            &EventSearchFilters::default(),
            10,
            budget,
        )
        .unwrap_err();
    assert!(matches!(
        error,
        ctx_history_index_query::LexicalSearchError::WorkExhausted(exhaustion)
            if exhaustion.counter == LexicalWorkCounter::CandidateDocs
                && exhaustion.used == 0
                && exhaustion.limit == 0
                && exhaustion.segment.is_some()
                && exhaustion.next_doc.is_some()
    ));
}

#[test]
fn thirty_two_term_fanout_streams_one_stable_segment_at_a_time() {
    const SEGMENTS: usize = 8;
    let temp = tempdir().unwrap();
    let terms = (0..LEXICAL_QUERY_LIMITS.maximum_unique_tokens)
        .map(|index| format!("fanoutterm{index}"))
        .collect::<Vec<_>>();
    let query = terms.join(" ");
    let mut expected = Vec::with_capacity(SEGMENTS);
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.test_disable_merges().unwrap();
    for segment_index in 0..SEGMENTS {
        let source = source(&format!("fanout-segment-{segment_index}.jsonl"));
        let record = document(&source, 1, &query);
        expected.push(record.event_id);
        writer.begin_source(source.clone()).unwrap();
        writer.add_core_record(record).unwrap();
        writer.certify_source(certificate(&source, 1, 1)).unwrap();
    }
    writer.commit(|_| true).unwrap();
    expected.sort_by_key(|event_id| event_id.digest());

    let index = VerifiedIndex::open(temp.path()).unwrap();
    index.reset_manual_lexical_io_observability_for_test();
    let batch = index
        .search_event_candidates_batch(&query, SEGMENTS)
        .unwrap();

    assert!(batch.complete);
    assert!(batch.candidate_set_exhaustive);
    assert_eq!(batch.counters.segments, SEGMENTS as u64);
    assert_eq!(batch.counters.candidate_docs, SEGMENTS as u64);
    assert_eq!(
        batch.counters.body_posting_advances,
        (SEGMENTS * LEXICAL_QUERY_LIMITS.maximum_unique_tokens) as u64
    );
    assert_eq!(
        batch
            .candidates
            .iter()
            .map(|candidate| candidate.event.event_id)
            .collect::<Vec<_>>(),
        expected,
        "full rank ties must use the stable identity key, not segment order"
    );
    assert_eq!(
        index.manual_event_range_order_decodes_for_test(),
        SEGMENTS,
        "an exhaustive result decodes one order key per retained finalist"
    );
    let simultaneous = index.maximum_simultaneous_manual_postings_for_test();
    assert_eq!(
        simultaneous,
        LEXICAL_QUERY_LIMITS.maximum_unique_tokens + 2,
        "one segment retains 32 body, one discovery, and one class posting"
    );
    assert_eq!(
        batch.counters.posting_opens,
        (simultaneous * SEGMENTS) as u64,
        "cumulative opens prove the working-set observation spans every segment"
    );

    index.reset_manual_lexical_io_observability_for_test();
    let top_three = index.search_event_candidates_batch(&query, 3).unwrap();
    assert!(top_three.complete);
    assert!(!top_three.candidate_set_exhaustive);
    assert_eq!(top_three.counters.candidate_docs, SEGMENTS as u64);
    assert_eq!(
        index.manual_event_range_order_decodes_for_test(),
        3,
        "the hot loop must not decode discarded candidates' order keys"
    );
    assert_eq!(
        top_three
            .candidates
            .iter()
            .map(|candidate| candidate.event.event_id)
            .collect::<Vec<_>>(),
        expected[..3],
        "the global fixed heap must retain the best ties across all segments"
    );

    let mut budget = ctx_history_index_query::LEXICAL_WORK_BUDGET_V1;
    budget.maximum_candidate_docs = 1;
    index.reset_manual_lexical_io_observability_for_test();
    let exhausted = index
        .search_event_candidates_any_with_filters_batch_with_budget_for_test(
            &[query.as_str()],
            &EventSearchFilters::default(),
            SEGMENTS,
            budget,
        )
        .unwrap();
    assert_exhausted_at(
        &exhausted,
        ctx_history_index_query::LexicalWorkCounter::CandidateDocs,
        1,
        1,
    );
    let exhaustion = exhausted.exhaustion.as_ref().unwrap();
    assert_eq!(
        exhaustion.segment.as_ref().unwrap().stable_segment_index,
        1,
        "candidate exhaustion must describe the second stable-sorted segment"
    );
    assert_eq!(exhaustion.next_doc, Some(0));
    assert_eq!(exhausted.candidates.len(), 1);
    assert_eq!(
        index.maximum_simultaneous_manual_postings_for_test(),
        simultaneous,
        "opening a later segment must happen only after dropping the prior cursors"
    );
}

#[test]
fn lexical_result_ceiling_distinguishes_4096_from_4097_matches() {
    const MATCHES_AT_LIMIT: u64 = MAX_LEXICAL_QUERY_RESULTS as u64;
    let temp = tempdir().unwrap();
    let source = source("lexical-result-boundary.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for sequence in 1..=MATCHES_AT_LIMIT {
        writer
            .add_core_record(document(&source, sequence, "boundarycommon"))
            .unwrap();
    }
    writer
        .add_core_record(document(&source, MATCHES_AT_LIMIT + 1, "boundaryoverflow"))
        .unwrap();
    writer
        .certify_source(certificate(&source, 1, MATCHES_AT_LIMIT + 1))
        .unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    let exactly_full = index
        .search_event_candidates_batch("boundarycommon", MAX_LEXICAL_QUERY_RESULTS)
        .unwrap();
    assert!(exactly_full.complete);
    assert!(exactly_full.candidate_set_exhaustive);
    assert_eq!(exactly_full.candidates.len(), MAX_LEXICAL_QUERY_RESULTS);

    let overflow = index
        .search_event_candidates_any_with_filters_batch(
            &["boundarycommon", "boundaryoverflow"],
            &EventSearchFilters::default(),
            MAX_LEXICAL_QUERY_RESULTS,
        )
        .unwrap();
    assert!(overflow.complete);
    assert!(!overflow.candidate_set_exhaustive);
    assert_eq!(overflow.candidates.len(), MAX_LEXICAL_QUERY_RESULTS);
    assert_eq!(overflow.counters.candidate_docs, MATCHES_AT_LIMIT + 1);
}

#[test]
fn manual_body_and_list_execution_ignore_deleted_source_revisions() {
    let temp = tempdir().unwrap();
    let source = source("manual-deletion.jsonl");
    let deleted = document(&source, 1, "manualdeletionneedle");
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial.add_core_record(deleted.clone()).unwrap();
    initial.certify_source(certificate(&source, 1, 1)).unwrap();
    initial.commit(|_| true).unwrap();

    let replacement = document(&source, 2, "replacement without old token");
    let mut replacing = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    replacing.begin_source(source.clone()).unwrap();
    replacing.add_core_record(replacement.clone()).unwrap();
    replacing
        .certify_source(certificate(&source, 2, 1))
        .unwrap();
    replacing.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    let body = index
        .search_event_candidates_batch("manualdeletionneedle", 10)
        .unwrap();
    assert!(body.complete);
    assert!(body.candidates.is_empty());
    assert_eq!(body.counters.term_expansions, 0);

    let listed = index
        .list_event_candidates_with_filters_batch(&EventSearchFilters::default(), 10)
        .unwrap();
    assert!(listed.complete);
    assert_eq!(listed.candidates.len(), 1);
    assert_eq!(listed.candidates[0].event.event_id, replacement.event_id);
    assert_ne!(listed.candidates[0].event.event_id, deleted.event_id);
}
