use super::*;

#[test]
fn rejected_background_admission_renders_honest_freshness_on_existing_generation() {
    let temp = tempfile::tempdir().unwrap();
    let generation = crate::test_query_authority::publish_empty_generation(temp.path());
    let request = SourceSearchRequest {
        query: "available history".to_owned(),
        terms: Vec::new(),
        limit: 10,
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
        content_scope: ctx_history_index::SearchContentScope::All,
        event_type: None,
        file: None,
        session: None,
        exclude_sessions: Vec::new(),
        events: false,
        include_current_session: true,
        backend: Some(SearchBackend::Lexical),
        semantic_weight: 0.0,
    };
    let pin = ctx_daemon_cli::pin_active_verified_generation(temp.path()).unwrap();
    let refresh = refresh_for_search_with(
        &request,
        RefreshArg::Background,
        temp.path(),
        |_root, mode| {
            Ok(SourceBackedRefreshObservation {
                mode,
                status: "admission_rejected".to_owned(),
                request_id: None,
                daemon_available: true,
                source_count: 0,
                request_previous_generation: None,
                request_generation_changed: false,
                scanned_routes: None,
                receipt: None,
                pin,
            })
        },
    )
    .unwrap();
    assert_eq!(refresh.status, "admission_rejected");
    let plan = ctx_history_read_application::plan_search(
        request,
        ctx_history_read_application::SearchPolicy {
            default_backend: SearchBackend::Lexical,
            semantic: SemanticAvailability::Unavailable(SemanticReason::PolicyDisabled),
        },
    )
    .unwrap();
    let mut observation = initial_search_observation();
    let (value, result) = search_pinned_generation(
        plan,
        temp.path(),
        RefreshArg::Background,
        refresh,
        false,
        &crate::semantic::SemanticQueryAdapter::new(temp.path()),
        None,
        &mut observation,
    )
    .unwrap();
    assert_eq!(result.index().generation_id(), generation);
    assert_eq!(value["retrieval"]["generation_id"], generation);
    assert_eq!(value["freshness"]["mode"], "background");
    assert_eq!(value["freshness"]["status"], "admission_rejected");
    assert_eq!(value["freshness"]["source_count"], 0);
    assert!(value["freshness"].get("request_id").is_none());
    assert!(value["freshness"].get("receipt").is_none());
    assert!(
        observation.failure_phase.is_none(),
        "search itself succeeded"
    );
}
