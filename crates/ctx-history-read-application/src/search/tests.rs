use super::*;
use ctx_history_core::{
    derive_event_id, derive_session_id, EventIdentityInput, NativeItemKey, NativeSessionKey,
    SessionIdentityInput, SourceAnchor, SourceKey, StableEntityId, TypedKey,
};
use ctx_history_index_query::{
    LexicalSearchCandidate, LexicalTermCoverage, LexicalWorkCounter, LexicalWorkCounters,
    LexicalWorkExhaustion, RankedEventRef,
};
use std::{cell::Cell, collections::BTreeMap};

fn candidate_source_for(provider: &str, name: &str) -> SourceKey {
    SourceKey::derive(
        provider,
        "family_search_test",
        "session",
        1,
        SourceAnchor::provider_native("session-file", TypedKey::utf8(name).unwrap()).unwrap(),
    )
    .unwrap()
}

fn candidate_source_named(name: &str) -> SourceKey {
    candidate_source_for("codex", name)
}

fn candidate_source() -> SourceKey {
    candidate_source_named("family-search-test.jsonl")
}

fn candidate_session_id(source: &SourceKey, session: u64) -> StableEntityId {
    let native_session_key =
        NativeSessionKey::native_id("session", TypedKey::U64(session)).unwrap();
    derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: "thread",
        native_session_key: &native_session_key,
    })
    .unwrap()
}

fn candidate_session_coordinate(source: &SourceKey, session: u64) -> SearchSessionCoordinate {
    SearchSessionCoordinate {
        session_id: candidate_session_id(source, session).as_uuid(),
        source_owner_digest: source.identity().digest(),
    }
}

fn candidate(
    score: f32,
    session: u64,
    root: Option<u64>,
    agent_scope: Option<AgentScope>,
    event_sequence: u64,
) -> EventSearchCandidate {
    candidate_with_parent(score, session, None, root, agent_scope, event_sequence)
}

fn candidate_with_parent(
    score: f32,
    session: u64,
    parent: Option<u64>,
    root: Option<u64>,
    agent_scope: Option<AgentScope>,
    event_sequence: u64,
) -> EventSearchCandidate {
    let source = candidate_source();
    candidate_from_source(
        &source,
        score,
        session,
        parent,
        root,
        agent_scope,
        event_sequence,
    )
}

fn candidate_from_source(
    source: &SourceKey,
    score: f32,
    session: u64,
    parent: Option<u64>,
    root: Option<u64>,
    agent_scope: Option<AgentScope>,
    event_sequence: u64,
) -> EventSearchCandidate {
    let session_id = candidate_session_id(source, session);
    let native_item_key =
        NativeItemKey::native_id("message", TypedKey::U64(event_sequence)).unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .unwrap();
    let event = EventRecord {
        event_id,
        session_id,
        parent_session_id: parent.map(|parent| candidate_session_id(source, parent)),
        root_session_id: root.map(|root| candidate_session_id(source, root)),
        session_relationship: None,
        event_copy: None,
        source: source.clone(),
        provider: source.provider().to_owned(),
        source_format: source.source_format().to_owned(),
        provider_session_id: Some(format!("session-{session}")),
        native_event_id: None,
        agent_scope,
        event_sequence,
        occurred_at_unix_ms: Some(i64::try_from(event_sequence).unwrap()),
        event_type: "message".to_owned(),
        role: Some("assistant".to_owned()),
    };
    EventSearchCandidate {
        score,
        event: RankedEventRef::from(&event),
    }
}

fn result_scores<Event>(window: &SearchResultWindow<Event>) -> Vec<f32> {
    window.hits.iter().map(|hit| hit.score).collect()
}

fn ancestry(
    session_id: u128,
    parent_session_id: Option<u128>,
    claimed_root_session_id: Option<u128>,
) -> SessionAncestry {
    SessionAncestry {
        session_id: Uuid::from_u128(session_id),
        parent_session_id: parent_session_id.map(Uuid::from_u128),
        claimed_root_session_id: claimed_root_session_id.map(Uuid::from_u128),
    }
}

#[test]
fn lexical_terminal_state_preserves_stop_and_truncation_branches() {
    let batch = |complete, candidate_set_exhaustive| LexicalSearchBatch {
        candidates: Vec::new(),
        complete,
        candidate_set_exhaustive,
        exhaustion: (!complete).then_some(LexicalWorkExhaustion {
            counter: LexicalWorkCounter::CandidateDocs,
            used: 1,
            limit: 1,
            segment: None,
            next_doc: None,
        }),
        counters: LexicalWorkCounters::default(),
    };
    assert_eq!(
        lexical_terminal_state(&batch(true, true)),
        Some(SearchStopReason::Exhausted)
    );
    assert_eq!(
        lexical_terminal_state(&batch(true, false)),
        Some(SearchStopReason::CandidateCap)
    );
    assert_eq!(lexical_terminal_state(&batch(false, false)), None);
}

fn resolved_test_root(
    sessions: &[SessionAncestry],
    records: &BTreeMap<Uuid, SessionAncestry>,
) -> Option<Uuid> {
    resolved_unique_session_tree_root_id(sessions, |session_id| {
        Ok(records.get(&session_id).copied())
    })
    .unwrap()
}

fn linear_ancestry(depth: usize) -> (SessionAncestry, Uuid, BTreeMap<Uuid, SessionAncestry>) {
    let records = (0..=depth)
        .map(|position| {
            let session_id = 1_000 + position as u128;
            let parent_session_id = (position < depth).then_some(session_id + 1);
            let claimed_root_session_id = parent_session_id.or(Some(session_id));
            ancestry(session_id, parent_session_id, claimed_root_session_id)
        })
        .collect::<Vec<_>>();
    let active = records[0];
    let root_id = records[depth].session_id;
    let records = records
        .into_iter()
        .map(|record| (record.session_id, record))
        .collect();
    (active, root_id, records)
}

fn request() -> SearchRequest {
    SearchRequest {
        query: "  first query  ".to_owned(),
        terms: vec![" second query ".to_owned(), " ".to_owned()],
        limit: 20,
        provider: None,
        history_source: None,
        provider_key: None,
        source_id: None,
        source_format: None,
        source_roots: Vec::new(),
        source_groups: Vec::new(),
        workspace: None,
        since: None,
        primary_only: false,
        content_scope: SearchContentScope::All,
        event_type: None,
        file: None,
        session: None,
        exclude_sessions: Vec::new(),
        events: false,
        include_current_session: false,
        backend: Some(SearchBackend::Lexical),
        semantic_weight: 0.35,
    }
}

#[test]
fn normalized_query_preserves_typed_argument_order() {
    let query = NormalizedSearchQuery::from_request(&request());
    assert_eq!(query.texts(), vec!["first query", "second query"]);
    assert_eq!(query.display(), "first query OR second query");
    assert_eq!(query.positional(), Some("first query"));
    assert_eq!(query.terms(), &["second query"]);
}

#[test]
fn custom_source_filter_rejects_noncustom_provider() {
    let mut request = request();
    request.history_source = Some("plugin/source".to_owned());
    request.provider = Some(CaptureProvider::Claude);
    assert_eq!(
        validate_search_request(&request).unwrap_err().to_string(),
        "custom history source filters require the custom provider"
    );
}

#[test]
fn manual_session_exclusions_trim_selectors_and_reject_blanks() {
    let mut request = request();
    request.exclude_sessions = vec!["  abcdef12  ".to_owned()];
    normalize_search_request(&mut request).unwrap();
    assert_eq!(request.exclude_sessions, vec!["abcdef12".to_owned()]);

    request.exclude_sessions.push("  ".to_owned());
    assert_eq!(
        normalize_search_request(&mut request)
            .unwrap_err()
            .to_string(),
        "exclude_session selector is empty"
    );
}

#[test]
fn provider_root_and_group_selectors_are_normalized_and_bounded() {
    let mut request = request();
    request.source_roots = vec![
        " work ".to_owned(),
        "personal".to_owned(),
        "work".to_owned(),
    ];
    request.source_groups = vec![" personal ".to_owned(), "work".to_owned()];
    normalize_search_request(&mut request).unwrap();
    assert_eq!(request.source_roots, vec!["personal", "work"]);
    assert_eq!(request.source_groups, vec!["personal", "work"]);

    request.source_roots = vec!["bad.root".to_owned()];
    let error = normalize_search_request(&mut request)
        .unwrap_err()
        .to_string();
    assert_eq!(
        error,
        "invalid source root selector; expected 1..=64 ASCII letters, digits, hyphens, or underscores"
    );
    assert!(!error.contains("bad.root"));
}

#[test]
fn manual_session_exclusions_cannot_be_combined_with_positive_session() {
    let mut request = request();
    request.session = Some("abcdef12".to_owned());
    request.exclude_sessions = vec!["abcdef34".to_owned()];
    assert_eq!(
        validate_search_request(&request).unwrap_err().to_string(),
        "excluded sessions cannot be combined with a selected session"
    );
}

#[test]
fn unsupported_semantic_scope_remains_typed() {
    let mut request = request();
    request.backend = Some(SearchBackend::Semantic);
    request.content_scope = SearchContentScope::Outputs;
    let error = unsupported_semantic_scope(&request).unwrap();
    assert_eq!(
        error.reason(),
        Some(SemanticReason::ContentScopeUnsupported)
    );
    assert!(!error.retryable());
}

#[test]
fn all_agents_are_default_and_primary_only_is_the_sole_narrower_scope() {
    let mut request = request();
    assert_eq!(search_agent_scope(&request, None), SearchAgentScope::All);
    assert_eq!(
        search_agent_scope(&request, Some(Uuid::nil())),
        SearchAgentScope::All
    );
    request.primary_only = true;
    assert_eq!(
        search_agent_scope(&request, Some(Uuid::nil())),
        SearchAgentScope::Primary
    );
}

fn same_source_grouping_claims(
    coordinates: &[SearchSessionCoordinate],
    roots: &[(u64, Option<u64>)],
) -> ctx_history_index_query::Result<Vec<SessionGroupingClaims>> {
    let source = candidate_source();
    Ok(coordinates
        .iter()
        .map(|coordinate| {
            assert_eq!(coordinate.source_owner_digest, source.identity().digest());
            let session = roots
                .iter()
                .map(|&(session, _)| session)
                .find(|&session| {
                    candidate_session_id(&source, session).as_uuid() == coordinate.session_id
                })
                .unwrap();
            let session_id = candidate_session_id(&source, session);
            let root_session_id = roots
                .iter()
                .find_map(|&(candidate, root)| (candidate == session).then_some(root))
                .flatten()
                .map(|root| candidate_session_id(&source, root));
            SessionGroupingClaims {
                session_id,
                source_owner: source.identity(),
                parent_session_id: None,
                root_session_id,
                relationship: None,
            }
        })
        .collect())
}

fn shape_same_source_with_roots(
    candidates: &[EventSearchCandidate],
    roots: &[(u64, Option<u64>)],
    limit: usize,
    completeness: DiversificationCompleteness,
) -> (
    SearchResultWindow<RankedEventRef>,
    SearchDiversificationDecision,
) {
    let (window, decision) =
        shape_search_candidates_using(candidates, limit, false, completeness, |coordinates| {
            same_source_grouping_claims(coordinates, roots)
        })
        .unwrap();
    (window, decision)
}

#[test]
fn family_rounds_follow_literal_families_without_candidate_scope_metadata() {
    let candidates = [
        candidate(100.0, 1, None, Some(AgentScope::Subagent), 1),
        candidate(99.0, 2, None, Some(AgentScope::Primary), 1),
        candidate(98.0, 3, None, Some(AgentScope::Primary), 1),
        candidate(97.0, 4, None, Some(AgentScope::Subagent), 1),
    ];
    let roots = [(1, Some(9)), (2, Some(9)), (3, None), (4, Some(9))];

    let (window, decision) = shape_same_source_with_roots(
        &candidates,
        &roots,
        4,
        DiversificationCompleteness::Lexical {
            work_complete: true,
            candidate_set_exhaustive: true,
        },
    );

    assert_eq!(result_scores(&window), [100.0, 98.0, 99.0, 97.0]);
    assert_eq!(
        window.hits[0].event.session_id,
        candidate_session_id(&candidate_source(), 1).as_uuid()
    );
    assert_eq!(decision.status, SearchDiversificationStatus::Applied);
    assert_eq!(decision.changed_final_top_n, Some(true));
}

#[test]
fn family_rounds_prefer_the_repeat_of_the_stronger_champion_family() {
    let candidates = [
        candidate(100.0, 1, None, None, 1),
        candidate(99.0, 2, None, None, 1),
        candidate(98.0, 3, None, None, 1),
        candidate(97.0, 4, None, None, 1),
        candidate(96.0, 5, None, None, 1),
        candidate(95.0, 6, None, None, 1),
        candidate(94.0, 7, None, None, 1),
        candidate(93.0, 8, None, None, 1),
        candidate(92.0, 9, None, None, 1),
        candidate(91.0, 10, None, None, 1),
        candidate(90.0, 11, None, None, 1),
    ];
    let roots = [
        (1, Some(20)),
        (2, Some(30)),
        (3, Some(30)),
        (4, Some(20)),
        (5, None),
        (6, None),
        (7, None),
        (8, None),
        (9, None),
        (10, None),
        (11, None),
    ];

    let (window, decision) = shape_same_source_with_roots(
        &candidates,
        &roots,
        10,
        DiversificationCompleteness::Lexical {
            work_complete: true,
            candidate_set_exhaustive: true,
        },
    );

    assert_eq!(
        result_scores(&window),
        [100.0, 99.0, 96.0, 95.0, 94.0, 93.0, 92.0, 91.0, 90.0, 97.0]
    );
    assert_eq!(decision.status, SearchDiversificationStatus::Applied);
    assert_eq!(decision.changed_final_top_n, Some(true));
}

#[test]
fn coalesced_claims_override_winning_event_root_and_candidate_disagreement() {
    let first_order = [
        candidate(100.0, 1, None, Some(AgentScope::Subagent), 1),
        candidate(99.0, 2, Some(9), Some(AgentScope::Subagent), 1),
        candidate(98.0, 3, None, Some(AgentScope::Subagent), 1),
        candidate(80.0, 1, Some(9), Some(AgentScope::Subagent), 2),
    ];
    let second_order = [
        candidate(100.0, 1, Some(77), Some(AgentScope::Subagent), 2),
        candidate(99.0, 2, None, Some(AgentScope::Subagent), 1),
        candidate(98.0, 3, Some(9), Some(AgentScope::Subagent), 1),
        candidate(80.0, 1, None, Some(AgentScope::Subagent), 1),
    ];
    let roots = [(1, Some(9)), (2, Some(9)), (3, None)];
    for candidates in [&first_order[..], &second_order[..]] {
        let (window, _) = shape_same_source_with_roots(
            candidates,
            &roots,
            3,
            DiversificationCompleteness::BackendUnknown,
        );
        assert_eq!(
            window
                .hits
                .iter()
                .map(|hit| hit.event.session_id)
                .collect::<Vec<_>>(),
            vec![
                candidate_session_id(&candidate_source(), 1),
                candidate_session_id(&candidate_source(), 3),
                candidate_session_id(&candidate_source(), 2),
            ]
            .into_iter()
            .map(StableEntityId::as_uuid)
            .collect::<Vec<_>>()
        );
    }
}

#[test]
fn literal_root_and_own_session_fallback_with_the_same_id_share_one_family() {
    let candidates = [
        candidate(100.0, 1, None, None, 1),
        candidate(90.0, 2, None, None, 1),
        candidate(80.0, 3, None, None, 1),
    ];

    let (window, decision) = shape_same_source_with_roots(
        &candidates,
        &[(1, None), (2, Some(1)), (3, None)],
        3,
        DiversificationCompleteness::Lexical {
            work_complete: true,
            candidate_set_exhaustive: true,
        },
    );

    assert_eq!(result_scores(&window), [100.0, 80.0, 90.0]);
    assert_eq!(decision.status, SearchDiversificationStatus::Applied);
    assert_eq!(decision.changed_final_top_n, Some(true));
}

#[test]
fn complete_decisions_report_changed_false_when_family_order_is_already_global() {
    let candidates = [
        candidate(100.0, 1, None, None, 1),
        candidate(90.0, 2, None, None, 1),
    ];
    let (window, decision) = shape_same_source_with_roots(
        &candidates,
        &[(1, None), (2, None)],
        2,
        DiversificationCompleteness::Lexical {
            work_complete: true,
            candidate_set_exhaustive: true,
        },
    );
    assert_eq!(result_scores(&window), [100.0, 90.0]);
    assert_eq!(decision.changed_final_top_n, Some(false));
}

#[test]
fn ordinary_results_keep_one_event_per_session_and_count_other_matches() {
    let candidates = [
        candidate(100.0, 1, Some(1), Some(AgentScope::Primary), 1),
        candidate(90.0, 1, Some(1), Some(AgentScope::Primary), 2),
        candidate(80.0, 1, Some(1), Some(AgentScope::Primary), 3),
        candidate(70.0, 2, Some(2), Some(AgentScope::Primary), 1),
    ];

    let (window, _) = shape_same_source_with_roots(
        &candidates,
        &[(1, None), (2, None)],
        2,
        DiversificationCompleteness::Lexical {
            work_complete: true,
            candidate_set_exhaustive: true,
        },
    );

    assert_eq!(result_scores(&window), [100.0, 70.0]);
    assert_eq!(window.hits[0].more_matches_in_session, 2);
    assert_eq!(window.hits[1].more_matches_in_session, 0);
    assert_ne!(
        window.hits[0].event.session_id,
        window.hits[1].event.session_id
    );
    assert!(!window.more_available);
}

#[test]
fn dense_event_results_remain_ungrouped_and_score_ordered() {
    let candidates = [
        candidate(100.0, 1, Some(1), Some(AgentScope::Primary), 1),
        candidate(90.0, 1, Some(1), Some(AgentScope::Primary), 2),
        candidate(80.0, 1, Some(1), Some(AgentScope::Primary), 3),
    ];

    let (window, decision) = shape_search_candidates_using(
        &candidates,
        2,
        true,
        DiversificationCompleteness::BackendUnknown,
        |_| unreachable!("dense search must not read grouping claims"),
    )
    .unwrap();

    assert_eq!(result_scores(&window), [100.0, 90.0]);
    assert_eq!(
        window.hits[0].event.session_id,
        window.hits[1].event.session_id
    );
    assert!(window
        .hits
        .iter()
        .all(|hit| hit.more_matches_in_session == 0));
    assert!(window.more_available);
    assert_eq!(decision.status, SearchDiversificationStatus::NotApplicable);
    assert_eq!(decision.changed_final_top_n, None);
}

#[test]
fn lexical_decisiveness_requires_completed_work_and_enough_families_or_exhaustiveness() {
    let candidates = [
        candidate(100.0, 1, None, None, 1),
        candidate(90.0, 2, None, None, 1),
        candidate(80.0, 3, None, None, 1),
    ];
    let roots = [(1, None), (2, None), (3, None)];
    let (_, enough_families) = shape_same_source_with_roots(
        &candidates,
        &roots,
        2,
        DiversificationCompleteness::Lexical {
            work_complete: true,
            candidate_set_exhaustive: false,
        },
    );
    assert_eq!(enough_families.status, SearchDiversificationStatus::Applied);

    let same_family = [(1, Some(9)), (2, Some(9)), (3, Some(9))];
    let (_, insufficient_families) = shape_same_source_with_roots(
        &candidates,
        &same_family,
        2,
        DiversificationCompleteness::Lexical {
            work_complete: true,
            candidate_set_exhaustive: false,
        },
    );
    assert_eq!(
        insufficient_families.status,
        SearchDiversificationStatus::Indeterminate
    );
    assert_eq!(insufficient_families.changed_final_top_n, None);

    let (_, work_exhausted) = shape_same_source_with_roots(
        &candidates,
        &roots,
        2,
        DiversificationCompleteness::Lexical {
            work_complete: false,
            candidate_set_exhaustive: false,
        },
    );
    assert_eq!(
        work_exhausted.status,
        SearchDiversificationStatus::Indeterminate
    );

    let (_, exhaustive_small_set) = shape_same_source_with_roots(
        &candidates[..1],
        &roots[..1],
        2,
        DiversificationCompleteness::Lexical {
            work_complete: true,
            candidate_set_exhaustive: true,
        },
    );
    assert_eq!(
        exhaustive_small_set.status,
        SearchDiversificationStatus::Applied
    );
}

fn lexical_batch(
    candidates: Vec<EventSearchCandidate>,
    complete: bool,
    candidate_set_exhaustive: bool,
) -> LexicalSearchBatch {
    LexicalSearchBatch {
        candidates: candidates
            .into_iter()
            .map(|candidate| LexicalSearchCandidate {
                event: candidate.event,
                score: candidate.score,
                coverage: LexicalTermCoverage {
                    matched_terms: 1,
                    query_terms: 1,
                },
            })
            .collect(),
        complete,
        candidate_set_exhaustive,
        exhaustion: (!complete).then_some(LexicalWorkExhaustion {
            counter: LexicalWorkCounter::CandidateDocs,
            used: 7,
            limit: 7,
            segment: None,
            next_doc: None,
        }),
        counters: LexicalWorkCounters::default(),
    }
}

#[test]
fn ordinary_lexical_search_invokes_one_fixed_batch_and_one_grouping_read() {
    let lexical_calls = Cell::new(0);
    let grouping_calls = Cell::new(0);
    let observed_horizon = Cell::new(0);
    let collection = collect_lexical_search_hits_using(
        20,
        false,
        |horizon| {
            lexical_calls.set(lexical_calls.get() + 1);
            observed_horizon.set(horizon);
            Ok(lexical_batch(Vec::new(), true, true))
        },
        |coordinates| {
            grouping_calls.set(grouping_calls.get() + 1);
            assert!(coordinates.is_empty());
            Ok(Vec::new())
        },
    )
    .unwrap();

    assert_eq!(lexical_calls.get(), 1);
    assert_eq!(grouping_calls.get(), 1);
    assert_eq!(observed_horizon.get(), LEXICAL_SESSION_CANDIDATE_HORIZON);
    assert_eq!(
        collection.diversification.status,
        SearchDiversificationStatus::Applied
    );
}

#[test]
fn dense_lexical_search_uses_only_limit_plus_one_and_never_groups() {
    let observed_horizon = Cell::new(0);
    let collection = collect_lexical_search_hits_using(
        5,
        true,
        |horizon| {
            observed_horizon.set(horizon);
            Ok(lexical_batch(Vec::new(), true, true))
        },
        |_| unreachable!("dense lexical search must not group"),
    )
    .unwrap();

    assert_eq!(observed_horizon.get(), 6);
    assert_eq!(
        collection.diversification.status,
        SearchDiversificationStatus::NotApplicable
    );
}

#[test]
fn zero_limit_is_not_applicable_without_retrieval_or_grouping() {
    let collection = collect_lexical_search_hits_using(
        0,
        false,
        |_| unreachable!("zero limit must not retrieve candidates"),
        |_| unreachable!("zero limit must not group candidates"),
    )
    .unwrap();

    assert!(collection.result_window.hits.is_empty());
    assert_eq!(
        collection.diversification,
        SearchDiversificationDecision {
            status: SearchDiversificationStatus::NotApplicable,
            top_n: 0,
            changed_final_top_n: None,
        }
    );
}

#[test]
fn work_exhaustion_returns_a_truthfully_indeterminate_bounded_prefix() {
    let candidates = vec![
        candidate(100.0, 1, None, None, 1),
        candidate(90.0, 2, None, None, 1),
    ];
    let collection = collect_lexical_search_hits_using(
        2,
        false,
        |_| Ok(lexical_batch(candidates, false, false)),
        |coordinates| same_source_grouping_claims(coordinates, &[(1, None), (2, None)]),
    )
    .unwrap();

    assert_eq!(collection.result_window.hits.len(), 2);
    assert!(!collection.result_window.more_available);
    assert!(collection.candidate_pool_truncated);
    assert_eq!(
        collection.diversification.status,
        SearchDiversificationStatus::Indeterminate
    );
    let diagnostics = collection.lexical_diagnostics.unwrap();
    assert!(!diagnostics.work_complete);
    assert!(!diagnostics.candidate_set_exhaustive);
    assert_eq!(diagnostics.exhaustion.unwrap().counter, "candidate_docs");
}

#[test]
fn source_filter_precedes_source_owned_grouping_and_preserves_the_selected_event() {
    let work = candidate_source_named("work-root/session.jsonl");
    let personal = candidate_source_named("personal-root/session.jsonl");
    let claude = candidate_source_for("claude", "personal-root/session.jsonl");
    let excluded = candidate_source_named("excluded-root/session.jsonl");
    assert_eq!(work.provider(), personal.provider());
    assert_ne!(personal.provider(), claude.provider());
    assert_ne!(work.identity(), personal.identity());
    assert_ne!(personal.identity(), claude.identity());
    for source in [&personal, &claude] {
        assert_ne!(
            candidate_session_id(&work, 1),
            candidate_session_id(source, 1)
        );
        assert_ne!(
            candidate_session_id(&work, 9),
            candidate_session_id(source, 9)
        );
    }

    let candidates = [
        candidate_from_source(&excluded, 200.0, 1, None, Some(9), None, 1),
        candidate_from_source(&work, 100.0, 1, None, Some(9), None, 1),
        candidate_from_source(&work, 99.0, 1, None, Some(9), None, 2),
        candidate_from_source(&work, 98.0, 2, None, Some(9), None, 1),
        candidate_from_source(&personal, 97.0, 1, None, Some(9), None, 1),
        candidate_from_source(&claude, 96.0, 1, None, Some(9), None, 1),
    ];
    assert_eq!(
        candidates[1].event.session_id,
        candidate_session_id(&work, 1).as_uuid()
    );
    assert_eq!(
        candidates[4].event.session_id,
        candidate_session_id(&personal, 1).as_uuid()
    );
    assert_eq!(
        candidates[5].event.session_id,
        candidate_session_id(&claude, 1).as_uuid()
    );
    assert_ne!(candidates[1].event.event_id, candidates[4].event.event_id);
    assert_ne!(candidates[1].event.event_id, candidates[5].event.event_id);
    let selected = candidates[1].event.clone();
    let allowed_sources = [
        work.identity().digest(),
        personal.identity().digest(),
        claude.identity().digest(),
    ];
    let filter_applied = Cell::new(false);

    let collection = collect_lexical_search_hits_using(
        4,
        false,
        |_| {
            filter_applied.set(true);
            Ok(lexical_batch(
                candidates
                    .iter()
                    .filter(|candidate| {
                        allowed_sources.contains(&candidate.event.source_owner_digest)
                    })
                    .cloned()
                    .collect(),
                true,
                true,
            ))
        },
        |coordinates| {
            assert!(filter_applied.get());
            assert_eq!(
                coordinates,
                &[
                    candidate_session_coordinate(&work, 1),
                    candidate_session_coordinate(&work, 2),
                    candidate_session_coordinate(&personal, 1),
                    candidate_session_coordinate(&claude, 1),
                ]
            );
            assert!(coordinates
                .iter()
                .all(|coordinate| coordinate.source_owner_digest != excluded.identity().digest()));
            Ok(coordinates
                .iter()
                .map(|coordinate| {
                    let source = [&work, &personal, &claude]
                        .into_iter()
                        .find(|source| source.identity().digest() == coordinate.source_owner_digest)
                        .unwrap();
                    let session_id = [1_u64, 2]
                        .into_iter()
                        .map(|session| candidate_session_id(source, session))
                        .find(|session| session.as_uuid() == coordinate.session_id)
                        .unwrap();
                    SessionGroupingClaims {
                        session_id,
                        source_owner: source.identity(),
                        parent_session_id: None,
                        root_session_id: Some(candidate_session_id(source, 9)),
                        relationship: None,
                    }
                })
                .collect())
        },
    )
    .unwrap();

    let window = collection.result_window;
    assert_eq!(result_scores(&window), [100.0, 97.0, 96.0, 98.0]);
    assert_eq!(window.hits[0].event, selected);
    assert_eq!(window.hits[0].more_matches_in_session, 1);
    assert_eq!(
        collection.diversification.status,
        SearchDiversificationStatus::Applied
    );
    assert_eq!(collection.diversification.changed_final_top_n, Some(true));
}

#[test]
fn active_tree_root_resolves_a_direct_child() {
    let root = ancestry(1, None, Some(1));
    let child = ancestry(2, Some(1), Some(1));
    let records = BTreeMap::from([(root.session_id, root)]);
    assert_eq!(
        resolved_test_root(&[child], &records),
        Some(root.session_id)
    );
}

#[test]
fn active_tree_root_resolves_a_grandchild_with_an_immediate_parent_claim() {
    let root = ancestry(1, None, None);
    let child = ancestry(2, Some(1), Some(1));
    let grandchild = ancestry(3, Some(2), Some(2));
    let records = BTreeMap::from([(root.session_id, root), (child.session_id, child)]);
    assert_eq!(
        resolved_test_root(&[grandchild], &records),
        Some(root.session_id)
    );
}

#[test]
fn active_tree_claim_closure_includes_nested_descendants() {
    let root = Uuid::from_u128(1);
    let child = ancestry(2, Some(1), Some(1));
    let grandchild = ancestry(3, Some(2), Some(1));
    let relations = [child, grandchild];
    assert_eq!(
        resolved_session_tree_ids(root, |anchors| {
            Ok(relations
                .iter()
                .filter(|session| {
                    [session.parent_session_id, session.claimed_root_session_id]
                        .into_iter()
                        .flatten()
                        .any(|claim| anchors.contains(&claim))
                })
                .copied()
                .collect())
        })
        .unwrap(),
        Some(vec![root, child.session_id, grandchild.session_id])
    );
}

#[test]
fn active_tree_claim_closure_accepts_the_session_limit() {
    let root = Uuid::from_u128(1);
    let related = (2..=MAX_ACTIVE_SESSION_TREE_SESSIONS as u128)
        .map(|session_id| ancestry(session_id, Some(1), Some(1)))
        .collect::<Vec<_>>();
    let resolved = resolved_session_tree_ids(root, |_| Ok(related.clone()))
        .unwrap()
        .unwrap();
    assert_eq!(resolved.len(), MAX_ACTIVE_SESSION_TREE_SESSIONS);
}

#[test]
fn active_tree_claim_closure_fails_open_over_the_session_limit() {
    let root = Uuid::from_u128(1);
    let root_session = ancestry(1, None, None);
    let related = (2..=(MAX_ACTIVE_SESSION_TREE_SESSIONS as u128 + 1))
        .map(|session_id| ancestry(session_id, Some(1), Some(1)))
        .collect::<Vec<_>>();
    assert_eq!(
        resolved_session_tree_ids(root, |_| Ok(related.clone())).unwrap(),
        None
    );
    assert_eq!(
        proven_active_session_tree_ids(
            &[root_session],
            |_| unreachable!(),
            |_| { Ok(related.clone()) }
        ),
        None
    );
}

#[test]
fn active_tree_claim_closure_fails_open_over_the_depth_limit() {
    let root = Uuid::from_u128(1);
    let mut next = 2_u128;
    assert_eq!(
        resolved_session_tree_ids(root, |_| {
            let session = ancestry(next, Some(next - 1), Some(1));
            next += 1;
            Ok(vec![session])
        })
        .unwrap(),
        None
    );
}

#[test]
fn active_tree_claim_closure_accepts_exact_cross_source_session_ids() {
    let root = ancestry(1, None, None);
    let child = ancestry(2, Some(1), Some(1));
    assert_eq!(
        resolved_session_tree_ids(root.session_id, |_| Ok(vec![child])).unwrap(),
        Some(vec![root.session_id, child.session_id])
    );
}

#[test]
fn active_tree_claim_closure_abstains_on_absent_contradictory_or_cyclic_claims() {
    let root = ancestry(1, None, None);
    for candidate in [
        ancestry(2, None, None),
        ancestry(2, Some(1), Some(99)),
        ancestry(2, Some(2), Some(1)),
    ] {
        assert_eq!(
            resolved_session_tree_ids(root.session_id, |_| Ok(vec![candidate])).unwrap(),
            None
        );
    }
}

#[test]
fn active_tree_root_rejects_a_malformed_claimed_root() {
    let root = ancestry(1, None, None);
    let child = ancestry(2, Some(1), Some(99));
    let records = BTreeMap::from([(root.session_id, root)]);
    assert_eq!(resolved_test_root(&[child], &records), None);
}

#[test]
fn active_tree_root_rejects_ambiguous_provider_session_matches() {
    let root = ancestry(1, None, None);
    let first = ancestry(2, Some(1), Some(1));
    let second = ancestry(3, Some(1), Some(1));
    let records = BTreeMap::from([(root.session_id, root)]);
    assert_eq!(resolved_test_root(&[first, second], &records), None);
    assert_eq!(
        proven_active_session_tree_ids(
            &[first, second],
            |session_id| Ok(records.get(&session_id).copied()),
            |_| unreachable!(),
        ),
        None
    );
}

#[test]
fn active_tree_abstains_when_closure_lookup_fails() {
    let root = ancestry(1, None, None);
    assert_eq!(
        proven_active_session_tree_ids(
            &[root],
            |_| unreachable!(),
            |_| { Err(anyhow!("closure lookup failed")) }
        ),
        None
    );
}

#[test]
fn active_tree_root_rejects_a_missing_parent() {
    let child = ancestry(2, Some(1), Some(1));
    assert_eq!(resolved_test_root(&[child], &BTreeMap::new()), None);
}

#[test]
fn active_tree_root_accepts_an_exact_cross_source_parent_id() {
    let child = ancestry(2, Some(1), Some(1));
    let parent = ancestry(1, None, None);
    let records = BTreeMap::from([(parent.session_id, parent)]);
    assert_eq!(
        resolved_test_root(&[child], &records),
        Some(parent.session_id)
    );
}

#[test]
fn active_tree_root_rejects_a_parent_cycle() {
    let first = ancestry(1, Some(2), Some(2));
    let second = ancestry(2, Some(1), Some(1));
    let records = BTreeMap::from([(first.session_id, first), (second.session_id, second)]);
    assert_eq!(resolved_test_root(&[first], &records), None);
}

#[test]
fn active_tree_root_rejects_depth_over_64() {
    let (at_limit, root_id, records) = linear_ancestry(MAX_ACTIVE_SESSION_ANCESTORS);
    assert_eq!(resolved_test_root(&[at_limit], &records), Some(root_id));
    let (over_limit, _, records) = linear_ancestry(MAX_ACTIVE_SESSION_ANCESTORS + 1);
    assert_eq!(resolved_test_root(&[over_limit], &records), None);
}

#[test]
fn weighted_rrf_keeps_exact_endpoint_weights() {
    assert_eq!(weighted_rrf_score(Some(1), None, 0.0), 1.0 / 61.0);
    assert_eq!(weighted_rrf_score(None, Some(1), 1.0), 1.0 / 61.0);
    assert_eq!(weighted_rrf_score(Some(1), None, 1.0), 0.0);
}

#[test]
fn hybrid_fusion_orders_mixed_lexical_and_semantic_ranks() {
    let lexical_first = candidate(1_000.0, 1, None, None, 1);
    let shared_second = candidate(-20.0, 2, None, None, 1);
    let shared_third = candidate(0.0, 3, None, None, 1);
    let semantic_only = candidate(f32::MAX, 4, None, None, 1);

    let fused = fuse_source_candidates(
        vec![
            lexical_first.clone(),
            shared_second.clone(),
            shared_third.clone(),
        ],
        vec![
            shared_third.clone(),
            shared_second.clone(),
            semantic_only.clone(),
        ],
        0.5,
    );

    assert_eq!(
        fused
            .iter()
            .map(|candidate| candidate.event.event_id)
            .collect::<Vec<_>>(),
        vec![
            shared_third.event.event_id,
            shared_second.event.event_id,
            lexical_first.event.event_id,
            semantic_only.event.event_id,
        ]
    );
}

#[test]
fn hybrid_fusion_ignores_raw_score_scales() {
    let lexical = vec![
        candidate(10.0, 1, None, None, 1),
        candidate(9.0, 2, None, None, 1),
    ];
    let semantic = vec![
        candidate(0.99, 2, None, None, 1),
        candidate(0.98, 3, None, None, 1),
    ];
    let mut rescaled_lexical = lexical.clone();
    rescaled_lexical[0].score = f32::MIN;
    rescaled_lexical[1].score = f32::MAX;
    let mut rescaled_semantic = semantic.clone();
    rescaled_semantic[0].score = -0.0;
    rescaled_semantic[1].score = 1_000_000.0;

    let signature = |candidates: Vec<EventSearchCandidate>| {
        candidates
            .into_iter()
            .map(|candidate| (candidate.event.event_id, candidate.score.to_bits()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        signature(fuse_source_candidates(lexical, semantic, 0.35)),
        signature(fuse_source_candidates(
            rescaled_lexical,
            rescaled_semantic,
            0.35,
        ))
    );
}

#[test]
fn hybrid_fusion_keeps_full_ids_that_share_a_compact_uuid_and_ties_deterministically() {
    let first = candidate(10.0, 1, None, None, 1);
    let mut colliding = candidate(20.0, 2, None, None, 1);
    colliding.event.event_id = first.event.event_id;
    colliding.event.event_identity_digest = first.event.event_identity_digest;
    colliding.event.event_identity_digest[20] ^= 1;
    assert_eq!(first.event.event_id, colliding.event.event_id);
    assert_ne!(
        first.event.event_identity_digest,
        colliding.event.event_identity_digest
    );

    let mut expected = vec![
        first.event.event_identity_digest,
        colliding.event.event_identity_digest,
    ];
    expected.sort();
    for fused in [
        fuse_source_candidates(vec![first.clone()], vec![colliding.clone()], 0.5),
        fuse_source_candidates(vec![colliding], vec![first], 0.5),
    ] {
        assert_eq!(
            fused
                .into_iter()
                .map(|candidate| candidate.event.event_identity_digest)
                .collect::<Vec<_>>(),
            expected
        );
    }
}

#[test]
fn hybrid_semantic_endpoint_drops_the_lexical_only_zero_score_tail() {
    let lexical_only = candidate(f32::MAX, 1, None, None, 1);
    let shared = candidate(1.0, 2, None, None, 1);
    let semantic_only = candidate(f32::MIN, 3, None, None, 1);

    let fused = fuse_source_candidates(
        vec![lexical_only.clone(), shared.clone()],
        vec![semantic_only.clone(), shared.clone()],
        1.0,
    );

    assert_eq!(
        fused
            .iter()
            .map(|candidate| candidate.event.event_id)
            .collect::<Vec<_>>(),
        vec![semantic_only.event.event_id, shared.event.event_id]
    );
    assert!(fused.iter().all(|candidate| candidate.score > 0.0));
    assert!(!fused
        .iter()
        .any(|candidate| candidate.event.event_id == lexical_only.event.event_id));
}
