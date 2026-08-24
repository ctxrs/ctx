use super::*;
use ctx_history_core::{
    derive_event_id, derive_session_id, EventIdentityInput, NativeItemKey, NativeSessionKey,
    SessionIdentityInput, SourceAnchor, SourceKey, StableEntityId, TypedKey,
};

fn candidate_source() -> SourceKey {
    SourceKey::derive(
        "codex",
        "root_first_search_test",
        "session",
        1,
        SourceAnchor::provider_native(
            "session-file",
            TypedKey::utf8("root-first-search-test.jsonl").unwrap(),
        )
        .unwrap(),
    )
    .unwrap()
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
    let session_id = candidate_session_id(&source, session);
    let native_item_key =
        NativeItemKey::native_id("message", TypedKey::U64(event_sequence)).unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source: &source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .unwrap();
    EventSearchCandidate {
        score,
        event: EventRecord {
            event_id,
            session_id,
            parent_session_id: parent.map(|parent| candidate_session_id(&source, parent)),
            root_session_id: root.map(|root| candidate_session_id(&source, root)),
            session_relationship: None,
            event_copy: None,
            source,
            provider: "codex".to_owned(),
            source_format: "root_first_search_test".to_owned(),
            provider_session_id: Some(format!("session-{session}")),
            native_event_id: None,
            agent_scope,
            event_sequence,
            occurred_at_unix_ms: Some(i64::try_from(event_sequence).unwrap()),
            event_type: "message".to_owned(),
            role: Some("assistant".to_owned()),
        },
    }
}

fn result_scores(window: &SearchResultWindow) -> Vec<f32> {
    window.hits.iter().map(|hit| hit.score).collect()
}

fn candidate_tail_score(candidates: &[EventSearchCandidate]) -> f32 {
    candidates
        .iter()
        .map(|candidate| candidate.score)
        .min_by(f32::total_cmp)
        .unwrap()
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

#[test]
fn ordinary_search_is_strictly_root_first() {
    let candidates = [
        candidate(90.0, 2, Some(1), Some(AgentScope::Subagent), 1),
        candidate(80.0, 3, Some(3), Some(AgentScope::Subagent), 1),
        candidate(100.0, 1, Some(1), Some(AgentScope::Subagent), 1),
    ];

    let window = shape_search_result_window(candidates.iter(), 2, false);

    assert_eq!(result_scores(&window), [100.0, 80.0]);
    assert!(window.more_available);
}

#[test]
fn parent_only_claim_falls_back_to_the_session_id() {
    let candidates = [
        candidate(100.0, 1, None, Some(AgentScope::Primary), 1),
        candidate_with_parent(90.0, 2, Some(1), None, Some(AgentScope::Subagent), 1),
        candidate(80.0, 3, None, Some(AgentScope::Primary), 1),
    ];

    let window = shape_search_result_window(candidates.iter(), 2, false);

    assert_eq!(result_scores(&window), [100.0, 90.0]);
    assert!(window.more_available);
}

#[test]
fn literal_root_claims_are_used_without_transitive_resolution() {
    let candidates = [
        candidate(100.0, 1, Some(9), Some(AgentScope::Subagent), 1),
        candidate(90.0, 2, Some(9), Some(AgentScope::Subagent), 1),
    ];

    let window = shape_search_result_window(candidates.iter(), 1, false);

    assert_eq!(result_scores(&window), [100.0]);
    assert!(window.more_available);
}

#[test]
fn primary_tolerance_is_inclusive_at_95_percent() {
    let candidates = [
        candidate(100.0, 2, Some(1), Some(AgentScope::Subagent), 1),
        candidate(95.0, 1, Some(1), Some(AgentScope::Primary), 1),
        candidate(97.0, 3, Some(3), Some(AgentScope::Subagent), 1),
    ];

    let window = shape_search_result_window(candidates.iter(), 2, false);

    assert_eq!(result_scores(&window), [95.0, 97.0]);
    assert_eq!(window.hits[0].event.agent_scope, Some(AgentScope::Primary));
}

#[test]
fn primary_below_tolerance_does_not_displace_a_stronger_child() {
    let candidates = [
        candidate(94.99, 1, Some(1), Some(AgentScope::Primary), 1),
        candidate(100.0, 2, Some(1), Some(AgentScope::Subagent), 1),
    ];

    let window = shape_search_result_window(candidates.iter(), 1, false);

    assert_eq!(result_scores(&window), [100.0]);
    assert_eq!(window.hits[0].event.agent_scope, Some(AgentScope::Subagent));
    assert!(window.more_available);
}

#[test]
fn equal_score_roots_use_the_literal_root_id_as_a_stable_tie_break() {
    let first = candidate(10.0, 1, Some(1), Some(AgentScope::Subagent), 1);
    let second = candidate(10.0, 2, Some(2), Some(AgentScope::Subagent), 1);
    let mut expected = [
        first.event.root_session_id.unwrap().as_uuid(),
        second.event.root_session_id.unwrap().as_uuid(),
    ];
    expected.sort();

    let candidates = [second, first];
    let window = shape_search_result_window(candidates.iter(), 2, false);
    let actual = window
        .hits
        .iter()
        .map(|hit| hit.event.root_session_id.unwrap())
        .collect::<Vec<_>>();

    assert_eq!(actual, expected);
}

#[test]
fn additional_sessions_fill_round_robin_in_root_order() {
    let candidates = [
        candidate(50.0, 6, Some(4), Some(AgentScope::Subagent), 1),
        candidate(80.0, 3, Some(1), Some(AgentScope::Subagent), 1),
        candidate(100.0, 1, Some(1), Some(AgentScope::Subagent), 1),
        candidate(60.0, 5, Some(4), Some(AgentScope::Subagent), 1),
        candidate(90.0, 2, Some(1), Some(AgentScope::Subagent), 1),
        candidate(70.0, 4, Some(4), Some(AgentScope::Subagent), 1),
    ];

    let window = shape_search_result_window(candidates.iter(), 5, false);

    assert_eq!(result_scores(&window), [100.0, 70.0, 90.0, 60.0, 80.0]);
    assert!(window.more_available);
}

#[test]
fn ordinary_results_keep_one_event_per_session_and_count_other_matches() {
    let candidates = [
        candidate(90.0, 1, Some(1), Some(AgentScope::Primary), 2),
        candidate(70.0, 2, Some(2), Some(AgentScope::Primary), 1),
        candidate(100.0, 1, Some(1), Some(AgentScope::Primary), 1),
        candidate(80.0, 1, Some(1), Some(AgentScope::Primary), 3),
    ];

    let window = shape_search_result_window(candidates.iter(), 2, false);

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
        candidate(80.0, 1, Some(1), Some(AgentScope::Primary), 3),
        candidate(100.0, 1, Some(1), Some(AgentScope::Primary), 1),
        candidate(90.0, 1, Some(1), Some(AgentScope::Primary), 2),
    ];

    let window = shape_search_result_window(candidates.iter(), 2, true);

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
}

#[test]
fn sibling_heavy_candidate_pool_is_not_decisive_before_enough_roots_arrive() {
    let candidates = [
        candidate(100.0, 1, Some(1), Some(AgentScope::Primary), 1),
        candidate(99.0, 2, Some(1), Some(AgentScope::Subagent), 1),
        candidate(98.0, 3, Some(1), Some(AgentScope::Subagent), 1),
    ];

    assert!(!root_first_candidate_pool_is_decisive(
        &candidates,
        2,
        candidate_tail_score(&candidates)
    ));
}

#[test]
fn candidate_pool_waits_until_unseen_primary_cannot_enter_tolerance() {
    let candidates = [
        candidate(100.0, 2, Some(1), Some(AgentScope::Subagent), 1),
        candidate(99.0, 3, Some(3), Some(AgentScope::Subagent), 1),
        candidate(96.0, 4, Some(4), Some(AgentScope::Subagent), 1),
    ];

    assert!(!root_first_candidate_pool_is_decisive(
        &candidates,
        2,
        candidate_tail_score(&candidates)
    ));

    let mut decisive = candidates.to_vec();
    decisive.push(candidate(94.0, 5, Some(5), Some(AgentScope::Subagent), 1));
    assert!(root_first_candidate_pool_is_decisive(
        &decisive,
        2,
        candidate_tail_score(&decisive)
    ));
}

#[test]
fn candidate_pool_is_decisive_when_qualifying_primaries_are_already_visible() {
    let candidates = [
        candidate(100.0, 2, Some(1), Some(AgentScope::Subagent), 1),
        candidate(99.0, 3, Some(3), Some(AgentScope::Subagent), 1),
        candidate(98.0, 1, Some(1), Some(AgentScope::Primary), 1),
        candidate(97.0, 4, Some(3), Some(AgentScope::Primary), 1),
        candidate(96.0, 5, Some(5), Some(AgentScope::Subagent), 1),
    ];

    assert!(root_first_candidate_pool_is_decisive(
        &candidates,
        2,
        candidate_tail_score(&candidates)
    ));
}

#[test]
fn candidate_pool_waits_past_an_equal_score_primary_tie() {
    let tied = [
        candidate(100.0, 2, Some(1), Some(AgentScope::Subagent), 1),
        candidate(95.0, 1, Some(1), Some(AgentScope::Primary), 1),
    ];
    assert!(!root_first_candidate_pool_is_decisive(&tied, 1, 95.0));
    assert!(root_first_candidate_pool_is_decisive(&tied, 1, 94.99));
}

#[test]
fn equal_score_root_boundary_requires_more_candidates_for_stable_tie_breaking() {
    let candidates = [
        candidate(100.0, 1, Some(1), Some(AgentScope::Primary), 1),
        candidate(90.0, 2, Some(2), Some(AgentScope::Primary), 1),
        candidate(90.0, 3, Some(3), Some(AgentScope::Primary), 1),
    ];

    assert!(!root_first_candidate_pool_is_decisive(
        &candidates,
        2,
        candidate_tail_score(&candidates)
    ));
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
    let child = Uuid::from_u128(2);
    let grandchild = Uuid::from_u128(3);
    let relations = [(child, root), (grandchild, child)];
    assert_eq!(
        resolved_session_tree_ids(root, |anchors| {
            Ok(relations
                .iter()
                .filter(|(_, parent)| anchors.contains(parent))
                .map(|(session, _)| *session)
                .collect())
        })
        .unwrap(),
        Some(vec![root, child, grandchild])
    );
}

#[test]
fn active_tree_claim_closure_accepts_the_session_limit() {
    let root = Uuid::from_u128(1);
    let related = (2..=MAX_ACTIVE_SESSION_TREE_SESSIONS as u128)
        .map(Uuid::from_u128)
        .collect::<Vec<_>>();
    let resolved = resolved_session_tree_ids(root, |_| Ok(related.clone()))
        .unwrap()
        .unwrap();
    assert_eq!(resolved.len(), MAX_ACTIVE_SESSION_TREE_SESSIONS);
}

#[test]
fn active_tree_claim_closure_fails_open_over_the_session_limit() {
    let root = Uuid::from_u128(1);
    let related = (2..=(MAX_ACTIVE_SESSION_TREE_SESSIONS as u128 + 1))
        .map(Uuid::from_u128)
        .collect::<Vec<_>>();
    assert_eq!(
        resolved_session_tree_ids(root, |_| Ok(related.clone())).unwrap(),
        None
    );
}

#[test]
fn active_tree_claim_closure_fails_open_over_the_depth_limit() {
    let root = Uuid::from_u128(1);
    let mut next = 2_u128;
    assert_eq!(
        resolved_session_tree_ids(root, |_| {
            let session_id = Uuid::from_u128(next);
            next += 1;
            Ok(vec![session_id])
        })
        .unwrap(),
        None
    );
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
}

#[test]
fn active_tree_root_rejects_a_missing_parent() {
    let child = ancestry(2, Some(1), Some(1));
    assert_eq!(resolved_test_root(&[child], &BTreeMap::new()), None);
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
