use super::*;

#[test]
fn graph_discovery_failure_carries_engine_facts_without_history_facts() {
    let error = execute(Ok(None), UnifiedToolOperation::Graph(GraphOperation::Stats)).unwrap_err();
    let graph = error.usage.graph.unwrap();
    assert_eq!(graph.operation, telemetry::GraphOperation::Stats);
    assert_eq!(
        graph.details.failure.unwrap().kind,
        telemetry::GraphFailureKind::MissingIndex
    );
    assert_eq!(
        graph.completion.delivery,
        telemetry::DeliveryEvidence::Unknown
    );
    assert!(graph.completion.execution.is_err());
    assert_eq!(error.usage.search_execution, None);
    assert_eq!(error.usage.search, None);
}

#[test]
fn graph_success_and_missing_endpoint_capture_typed_facts_before_json() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("graph.db");
    drop(Store::create(&db).unwrap());
    let outcome = execute(
        Ok(Some(&db)),
        UnifiedToolOperation::Graph(GraphOperation::Stats),
    )
    .unwrap();
    let graph = outcome.usage.graph.unwrap();
    assert_eq!(graph.nodes, Some(0));
    assert!(graph.completion.execution.is_ok());
    assert_eq!(
        graph.completion.delivery,
        telemetry::DeliveryEvidence::Unknown
    );
    let failure = execute(
        Ok(Some(&db)),
        UnifiedToolOperation::Graph(GraphOperation::Show {
            symbol: "synthetic-missing-node".into(),
        }),
    )
    .unwrap_err();
    let graph = failure.usage.graph.unwrap();
    assert_eq!(
        graph.details.failure.unwrap().kind,
        telemetry::GraphFailureKind::EndpointNotFound
    );
    assert_eq!(
        graph.details.failure.unwrap().phase,
        telemetry::GraphPhase::Query
    );
}

#[test]
fn sift_pending_pairs_are_from_the_actual_compactor_and_restore_does_not_tokenize() {
    let input = "synthetic command output\n".repeat(32);
    let outcome = execute(
        Ok(None),
        UnifiedToolOperation::OutputCompact {
            text: input.clone(),
        },
    )
    .unwrap();
    let sift = outcome.usage.sift.unwrap();
    assert_eq!(
        sift.cohort.delivery,
        Some(telemetry::SiftDelivery::NotAttempted)
    );
    assert_eq!(sift.bytes.unwrap().input, input.len() as u64);
    assert_eq!(
        sift.bytes.unwrap().output,
        outcome.structured["text"].as_str().unwrap().len() as u64
    );
    assert_eq!(
        sift.tokens.unwrap().input,
        outcome.structured["input_tokens"].as_u64().unwrap()
    );
    assert_eq!(
        sift.tokens.unwrap().output,
        outcome.structured["output_tokens"].as_u64().unwrap()
    );
    let restored = execute(
        Ok(None),
        UnifiedToolOperation::OutputRestore {
            text: input,
            encoding: "raw".into(),
        },
    )
    .unwrap();
    let restored = restored.usage.sift.unwrap();
    assert_eq!(restored.tokens, None);
    assert_eq!(
        restored.cohort.missingness,
        Some(telemetry::SiftMissingness::ViewNotTokenized)
    );
}

#[test]
fn output_validation_failure_retains_no_candidate_savings() {
    let failure = execute(
        Ok(None),
        UnifiedToolOperation::OutputRestore {
            text: String::new(),
            encoding: "invalid-synthetic-encoding".into(),
        },
    )
    .unwrap_err();
    let sample = failure.usage.sift.unwrap();
    assert_eq!(sample.execution_failed, 1);
    assert_eq!(sample.bytes, None);
    assert_eq!(sample.tokens, None);
    assert_eq!(
        sample.cohort.failure_phase,
        Some(telemetry::SiftPhase::Arguments)
    );
    assert!(sample.into_event().is_some());
}
