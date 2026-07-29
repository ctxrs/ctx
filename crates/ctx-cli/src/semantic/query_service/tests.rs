use super::*;

fn exercise_complete_session_budget(
    event_count: usize,
    response_texts: &[String],
    limit_bytes: usize,
    daemon_calls: &mut usize,
) -> Result<()> {
    let event_id = Uuid::nil();
    let events = (0..event_count).collect::<Vec<_>>();
    let mut budget = SourceHydrationOperationBudget::new(limit_bytes);
    for (batch_index, _) in events.chunks(SOURCE_HYDRATION_BATCH_MAX_ITEMS).enumerate() {
        let _remaining = budget.remaining_response_bytes(Some(event_id))?;
        *daemon_calls += 1;
        let text = response_texts.get(batch_index).cloned().unwrap_or_default();
        budget.retain_batch(&[(event_id, text)], Some(event_id))?;
    }
    Ok(())
}

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
}

#[test]
fn hydration_client_preserves_budget_code_and_accepts_content_too_large_kind() {
    let unavailable = SourceHydrationUnavailable::new(
        "hydration_budget_exceeded",
        source_hydration_failure_kind("content_too_large").unwrap(),
        "aggregate request budget was exceeded",
        false,
        None,
    );

    assert_eq!(unavailable.code(), "hydration_budget_exceeded");
    assert_eq!(unavailable.failure_kind, "content_too_large");
    assert!(!unavailable.retryable_after_refresh());
}

#[test]
fn complete_session_200_events_succeeds_when_multi_chunk_aggregate_is_small() {
    let mut daemon_calls = 0;
    exercise_complete_session_budget(
        200,
        &["a".repeat(512), "b".repeat(512)],
        4 * 1024,
        &mut daemon_calls,
    )
    .unwrap();

    assert_eq!(daemon_calls, 2);
}

#[test]
fn complete_session_129th_event_makes_no_daemon_call_after_exact_exhaustion() {
    let mut daemon_calls = 0;
    let error = exercise_complete_session_budget(
        129,
        &["a".repeat(512), "b".to_owned()],
        1024,
        &mut daemon_calls,
    )
    .expect_err("the second chunk must stop before transport");

    assert_eq!(daemon_calls, 1);
    let unavailable = error
        .downcast_ref::<SourceHydrationUnavailable>()
        .expect("aggregate exhaustion remains typed");
    assert_eq!(unavailable.code(), "hydration_budget_exceeded");
    assert_eq!(unavailable.failure_kind, "content_too_large");
    assert!(!unavailable.retryable_after_refresh());
    assert!(unavailable.complete_content_error().is_some());
}

#[test]
fn complete_session_multi_chunk_overage_is_typed_and_stops_before_third_call() {
    let mut daemon_calls = 0;
    let error = exercise_complete_session_budget(
        300,
        &["a".repeat(512), "b".repeat(1200), "c".to_owned()],
        2500,
        &mut daemon_calls,
    )
    .expect_err("aggregate retained content must share one allowance");

    assert_eq!(daemon_calls, 2);
    let unavailable = error
        .downcast_ref::<SourceHydrationUnavailable>()
        .expect("aggregate overage remains typed");
    assert_eq!(unavailable.code(), "hydration_budget_exceeded");
    assert_eq!(unavailable.failure_kind, "content_too_large");
    assert!(!unavailable.retryable_after_refresh());
}

#[test]
fn transport_overage_maps_without_parsing_error_detail() {
    let error = map_source_hydration_request_error(
        DaemonQueryResponseTooLarge::new(1024).into(),
        Some(Uuid::nil()),
    );

    let unavailable = error
        .downcast_ref::<SourceHydrationUnavailable>()
        .expect("transport overage remains typed");
    assert_eq!(unavailable.code(), "hydration_budget_exceeded");
    assert_eq!(unavailable.failure_kind, "content_too_large");
    assert!(!unavailable.retryable_after_refresh());
}
