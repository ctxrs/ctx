use super::*;

#[test]
fn semantic_query_pin_rejects_a_different_core_generation() {
    let pin = SourceBackedSemanticQueryPin {
        core_generation_id: "generation-a".to_owned(),
        pinned: None,
    };

    let error = validate_semantic_query_generation("generation-b", &pin)
        .expect_err("a semantic pin must not cross Core generations");
    let not_ready = error
        .downcast_ref::<SourceBackedSemanticNotReady>()
        .expect("generation mismatch must retain the typed unavailable contract");
    assert_eq!(not_ready.code(), "semantic_generation_receipt_mismatch");
    assert!(not_ready.detail().contains("generation-a"));
    assert!(not_ready.detail().contains("generation-b"));
    assert_eq!(
        not_ready.structured(),
        json!({
            "error": not_ready.to_string(),
            "error_code": "semantic_generation_receipt_mismatch",
            "detail": not_ready.detail(),
            "retryable": true,
        })
    );
}

#[test]
fn semantic_disabled_contract_is_stable_and_not_retryable() {
    let not_ready =
        SourceBackedSemanticNotReady::new("semantic_disabled", "semantic search is disabled");

    assert_eq!(
        not_ready.structured(),
        json!({
            "error": not_ready.to_string(),
            "error_code": "semantic_disabled",
            "detail": "semantic search is disabled",
            "retryable": false,
        })
    );
}

#[test]
fn hydration_client_preserves_budget_code_and_accepts_content_too_large_kind() {
    let unavailable = SourceHydrationUnavailable::new(
        "hydration_budget_exceeded",
        source_hydration_failure_kind("content_too_large").unwrap(),
        "aggregate request budget was exceeded",
        false,
    );

    assert_eq!(unavailable.code(), "hydration_budget_exceeded");
    assert_eq!(unavailable.failure_kind, "content_too_large");
    assert!(!unavailable.retryable_after_refresh());
    assert_eq!(
        unavailable.hydration_failure().kind,
        HydrationFailureKind::ContentTooLarge
    );
}

#[test]
fn transport_overage_maps_without_parsing_error_detail() {
    let error = map_source_hydration_request_error(DaemonQueryResponseTooLarge::new(1024).into());

    let unavailable = error
        .downcast_ref::<SourceHydrationUnavailable>()
        .expect("transport overage remains typed");
    assert_eq!(unavailable.code(), "hydration_budget_exceeded");
    assert_eq!(unavailable.failure_kind, "content_too_large");
    assert!(!unavailable.retryable_after_refresh());
}

#[test]
fn oversized_request_metadata_is_typed_before_transport() {
    let payload = json!({
        "items": ["x".repeat(server::DAEMON_QUERY_REQUEST_MAX_BYTES)],
    });
    let error =
        preflight_source_hydration_payload(&payload).expect_err("oversized metadata must fail");

    let unavailable = error
        .downcast_ref::<SourceHydrationUnavailable>()
        .expect("request metadata overage remains typed");
    assert_eq!(unavailable.code(), "hydration_budget_exceeded");
    assert_eq!(
        unavailable.hydration_failure().kind,
        HydrationFailureKind::ContentTooLarge
    );
}
