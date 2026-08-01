use super::*;

fn sha1(hex: char) -> GitObjectId {
    GitObjectId {
        format: GitObjectFormat::Sha1,
        hex: hex.to_string().repeat(40),
    }
}

fn sha256(hex: char) -> GitObjectId {
    GitObjectId {
        format: GitObjectFormat::Sha256,
        hex: hex.to_string().repeat(64),
    }
}

fn repository_binding(git_object_format: GitObjectFormat) -> RepositoryBinding {
    RepositoryBinding {
        binding_id: "binding-1".to_owned(),
        logical_repository_id: "repo-1".to_owned(),
        checkout_id: None,
        worktree_id: None,
        aliases: Vec::new(),
        git_object_format: Some(git_object_format),
        local_root_authorization: None,
        evidence: vec![RepositoryEvidence {
            kind: RepositoryEvidenceKind::FileActivity,
            confidence: RepositoryEvidenceConfidence::Explicit,
        }],
        association_policy_revision: CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION,
    }
}

fn with_produced_object(mut record: CoreRecord, object_id: GitObjectId) -> CoreRecord {
    record
        .repository_bindings
        .push(repository_binding(object_id.format));
    record
        .repository_vcs_observations
        .push(RepositoryVcsObservation {
            repository_binding_id: "binding-1".to_owned(),
            kind: RepositoryVcsObservationKind::Outcome(Box::new(RepositoryOutcomeObservation {
                kind: RepositoryOutcomeKind::Commit,
                produced_object_ids: vec![object_id],
                replacement_lineage: Vec::new(),
                pull_request: None,
                observed_at_unix_ms: 1_700_000_000_000,
                linkage: RepositoryOutcomeLinkage {
                    provider: "codex".to_owned(),
                    origin_call_id: "origin-call".to_owned(),
                    result_call_id: "result-call".to_owned(),
                    origin_event_sequence: record.event_sequence,
                    continuation_call_id_sha256: Vec::new(),
                    result_record_sha256: [7; 32],
                },
                outcome_capture_revision: CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
            })),
            object_id: None,
            parent_object_ids: Vec::new(),
            reference: None,
            relative_path: None,
        });
    record.validate_contract().unwrap();
    record
}

fn with_inspected_object(mut record: CoreRecord, object_id: GitObjectId) -> CoreRecord {
    record
        .repository_bindings
        .push(repository_binding(object_id.format));
    record
        .repository_vcs_observations
        .push(RepositoryVcsObservation {
            repository_binding_id: "binding-1".to_owned(),
            kind: RepositoryVcsObservationKind::Commit,
            object_id: Some(object_id),
            parent_object_ids: Vec::new(),
            reference: None,
            relative_path: None,
        });
    record.validate_contract().unwrap();
    record
}

fn publish_records(temp: &TempDir, source: &SourceKey, records: Vec<CoreRecord>) -> VerifiedIndex {
    let document_count = u64::try_from(records.len()).unwrap();
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
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
fn exact_produced_object_hits_precede_twelve_mentions_and_deduplicate() {
    let temp = tempdir().unwrap();
    let source = source("produced-object-ranking.jsonl");
    let object_id = sha1('a');
    let first_producer = with_produced_object(
        document(&source, 20, "created the certified repository outcome"),
        object_id.clone(),
    );
    let second_producer = with_produced_object(
        document(
            &source,
            21,
            &format!("producer also mentions {}", object_id.hex),
        ),
        object_id.clone(),
    );
    let producer_ids = [first_producer.event_id, second_producer.event_id];
    let mut records = (1..=12)
        .map(|sequence| {
            document(
                &source,
                sequence,
                &format!(
                    "ordinary lexical mention {} {}",
                    object_id.hex, object_id.hex
                ),
            )
        })
        .collect::<Vec<_>>();
    records.extend([first_producer, second_producer]);
    let index = publish_records(&temp, &source, records);

    let candidates = index.search_event_candidates(&object_id.hex, 6).unwrap();
    assert_eq!(candidates.len(), 6);
    assert!(candidates[..2]
        .iter()
        .all(|candidate| producer_ids.contains(&candidate.event.event_id)));
    assert!(candidates[2..]
        .iter()
        .all(|candidate| !producer_ids.contains(&candidate.event.event_id)));
    assert_eq!(
        candidates
            .iter()
            .map(|candidate| candidate.event.event_id.as_uuid())
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        candidates.len()
    );
    assert!(producer_ids.contains(
        &index.search_event_candidates(&object_id.hex, 1).unwrap()[0]
            .event
            .event_id
    ));
}

#[test]
fn prose_inspection_and_abbreviated_sha_gain_no_produced_object_authority() {
    let temp = tempdir().unwrap();
    let source = source("produced-object-negative.jsonl");
    let object_id = sha1('b');
    let producer = with_produced_object(
        document(&source, 1, "certified typed outcome without object prose"),
        object_id.clone(),
    );
    let inspection = with_inspected_object(
        document(
            &source,
            2,
            "typed repository inspection without object prose",
        ),
        object_id.clone(),
    );
    let prose = document(
        &source,
        3,
        &format!("please inspect {} before release", object_id.hex),
    );
    let abbreviation = &object_id.hex[..12];
    let abbreviated_mention = document(&source, 4, abbreviation);
    let producer_id = producer.event_id;
    let inspection_id = inspection.event_id;
    let prose_id = prose.event_id;
    let abbreviated_id = abbreviated_mention.event_id;
    let index = publish_records(
        &temp,
        &source,
        vec![producer, inspection, prose, abbreviated_mention],
    );

    let exact = index.search_event_candidates(&object_id.hex, 10).unwrap();
    assert_eq!(exact[0].event.event_id, producer_id);
    assert!(exact
        .iter()
        .any(|candidate| candidate.event.event_id == prose_id));
    assert!(!exact
        .iter()
        .any(|candidate| candidate.event.event_id == inspection_id));

    assert_eq!(
        index
            .search_event_candidates(
                &format!("please inspect {} before release", object_id.hex),
                10
            )
            .unwrap()
            .into_iter()
            .map(|candidate| candidate.event.event_id)
            .collect::<Vec<_>>(),
        vec![prose_id]
    );
    assert_eq!(
        index
            .search_event_candidates(abbreviation, 10)
            .unwrap()
            .into_iter()
            .map(|candidate| candidate.event.event_id)
            .collect::<Vec<_>>(),
        vec![abbreviated_id]
    );
}

#[test]
fn produced_object_ranking_obeys_filters_and_lexical_fallback() {
    let temp = tempdir().unwrap();
    let source = source("produced-object-filters.jsonl");
    let filtered_object_id = sha256('c');
    let fallback_object_id = sha1('d');
    let mut producer = with_produced_object(
        document(&source, 1, "certified filtered producer"),
        filtered_object_id.clone(),
    );
    producer.branch = Some("producer-only".to_owned());
    let producer_id = producer.event_id;
    let mut filtered_mention = document(&source, 2, &filtered_object_id.hex);
    filtered_mention.branch = Some("mentions-only".to_owned());
    let filtered_mention_id = filtered_mention.event_id;
    let fallback_mention = document(&source, 3, &fallback_object_id.hex);
    let fallback_mention_id = fallback_mention.event_id;
    let index = publish_records(
        &temp,
        &source,
        vec![producer, filtered_mention, fallback_mention],
    );

    assert_eq!(
        index
            .search_event_candidates_with_filters(
                &filtered_object_id.hex,
                &EventSearchFilters {
                    branch: Some("mentions-only".to_owned()),
                    ..EventSearchFilters::default()
                },
                10,
            )
            .unwrap()
            .into_iter()
            .map(|candidate| candidate.event.event_id)
            .collect::<Vec<_>>(),
        vec![filtered_mention_id]
    );
    assert_eq!(
        index
            .search_event_candidates_with_filters(
                &filtered_object_id.hex,
                &EventSearchFilters {
                    branch: Some("producer-only".to_owned()),
                    ..EventSearchFilters::default()
                },
                10,
            )
            .unwrap()[0]
            .event
            .event_id,
        producer_id
    );
    assert_eq!(
        index
            .search_event_candidates(&fallback_object_id.hex, 10)
            .unwrap()
            .into_iter()
            .map(|candidate| candidate.event.event_id)
            .collect::<Vec<_>>(),
        vec![fallback_mention_id]
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
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
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
    assert_eq!(
        index
            .search_event_candidates("coveragealpha coveragebeta", 1)
            .unwrap()[0]
            .event
            .event_id,
        exact.event_id
    );
}

fn lexical_query_limit_fixture() -> (TempDir, VerifiedIndex) {
    let temp = tempdir().unwrap();
    let source = source("query-limits.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
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
    assert_eq!(crate::query::lexical_query_constructions(), 0);
    assert_eq!(crate::query::lexical_query_executions(), 0);
}

#[test]
fn lexical_result_limits_reject_oversized_and_usize_max_before_query_work() {
    let (_temp, index) = lexical_query_limit_fixture();
    for requested in [MAX_LEXICAL_QUERY_RESULTS + 1, usize::MAX] {
        crate::query::reset_lexical_query_work();
        let error = index
            .search_event_candidates("bounded", requested)
            .unwrap_err();
        assert!(matches!(
            error,
            IndexError::InvalidLexicalResultLimit { requested: actual, maximum }
                if actual == requested && maximum == MAX_LEXICAL_QUERY_RESULTS
        ));
        assert_no_lexical_query_was_constructed_or_executed();

        crate::query::reset_lexical_query_work();
        let error = index
            .list_event_candidates_with_filters(&EventSearchFilters::default(), requested)
            .unwrap_err();
        assert!(matches!(
            error,
            IndexError::InvalidLexicalResultLimit { requested: actual, maximum }
                if actual == requested && maximum == MAX_LEXICAL_QUERY_RESULTS
        ));
        assert_no_lexical_query_was_constructed_or_executed();
    }
}

#[test]
fn oversized_single_query_is_rejected_before_query_construction() {
    let (_temp, index) = lexical_query_limit_fixture();
    let oversized = "x".repeat(LEXICAL_QUERY_LIMITS.maximum_aggregate_bytes + 1);
    crate::query::reset_lexical_query_work();

    let error = index.search_event_candidates(&oversized, 10).unwrap_err();

    assert!(matches!(
        error,
        IndexError::LexicalQueryBytesTooLarge {
            actual,
            maximum,
        } if actual == LEXICAL_QUERY_LIMITS.maximum_aggregate_bytes + 1
            && maximum == LEXICAL_QUERY_LIMITS.maximum_aggregate_bytes
    ));
    assert_no_lexical_query_was_constructed_or_executed();
}

#[test]
fn repeated_terms_are_rejected_before_query_construction() {
    let (_temp, index) = lexical_query_limit_fixture();
    let alternatives = vec!["bounded"; LEXICAL_QUERY_LIMITS.maximum_alternatives + 1];
    crate::query::reset_lexical_query_work();

    let error = index
        .search_event_candidates_any_with_filters(&alternatives, &EventSearchFilters::default(), 10)
        .unwrap_err();

    assert!(matches!(
        error,
        IndexError::LexicalQueryAlternativesTooMany { observed, maximum }
            if observed == LEXICAL_QUERY_LIMITS.maximum_alternatives + 1
                && maximum == LEXICAL_QUERY_LIMITS.maximum_alternatives
    ));
    assert_no_lexical_query_was_constructed_or_executed();
}

#[test]
fn analyzed_unique_tokens_are_rejected_before_query_construction() {
    let (_temp, index) = lexical_query_limit_fixture();
    let query = (0..=LEXICAL_QUERY_LIMITS.maximum_unique_tokens)
        .map(|index| format!("uniquetoken{index}"))
        .collect::<Vec<_>>()
        .join(" ");
    crate::query::reset_lexical_query_work();

    let error = index.search_event_candidates(&query, 10).unwrap_err();

    assert!(matches!(
        error,
        IndexError::LexicalQueryTokensTooMany { observed, maximum }
            if observed == LEXICAL_QUERY_LIMITS.maximum_unique_tokens + 1
                && maximum == LEXICAL_QUERY_LIMITS.maximum_unique_tokens
    ));
    assert_no_lexical_query_was_constructed_or_executed();
}
