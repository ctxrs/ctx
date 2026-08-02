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
fn semantic_query_admission_fails_closed_but_permits_ready_empty() {
    let error =
        source_backed_query_pin_from_readiness("generation-a", SourceBackedGenerationPin::NotReady)
            .err()
            .expect("an unacknowledged generation must fail closed");
    let not_ready = error
        .downcast_ref::<SourceBackedSemanticNotReady>()
        .expect("admission failure must retain the typed unavailable contract");
    assert_eq!(not_ready.code(), "semantic_generation_not_acknowledged");

    let pin = source_backed_query_pin_from_readiness(
        "generation-empty",
        SourceBackedGenerationPin::ReadyEmpty,
    )
    .expect("an acknowledged empty generation must be admitted");
    assert_eq!(pin.core_generation_id, "generation-empty");
    assert!(pin.pinned.is_none());
}
