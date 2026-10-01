use super::*;
use ctx_client_observability::analytics as wire;
use ctx_graph::ctx_graph_core::observation as g;
use ctx_sift::observation as s;
use std::time::Duration;

fn graph_sample() -> g::GraphObservation {
    let mut facts =
        g::GraphObservation::new(g::GraphOperation::Search, g::GraphInvocation::NativeStdio);
    facts.execution_succeeded = Some(true);
    facts.duration = Some(Duration::from_millis(2));
    facts
}
#[test]
fn graph_preserves_work_failure_and_error_envelope_delivery_independently() {
    let mut facts = graph_sample();
    facts.phase = g::GraphPhase::Query;
    facts.fail(g::GraphFailureKind::EndpointAmbiguous);
    facts.output_served = Some(true);
    facts.output_boundary = g::GraphOutputBoundary::StdioFlush;
    let event = graph_completed(facts).unwrap();
    assert_eq!(
        event.completion.delivery,
        wire::DeliveryEvidence::KnownComplete
    );
    assert_eq!(
        event.completion.execution.unwrap_err().class,
        wire::ProductFailureClass::InvalidRequest
    );
    assert_eq!(
        event.details.failure.unwrap().kind,
        wire::GraphFailureKind::EndpointAmbiguous
    );
    facts = graph_sample();
    facts.output_served = Some(false);
    facts.output_failure = Some(g::GraphFailureKind::BrokenPipe);
    let event = graph_completed(facts).unwrap();
    assert_eq!(event.completion.execution, Ok(()));
    assert_eq!(event.completion.delivery, wire::DeliveryEvidence::Failed);
}
#[test]
fn graph_handoff_is_unknown_and_unfinished_work_is_not_a_success() {
    let mut facts = graph_sample();
    facts.output_served = Some(true);
    facts.output_boundary = g::GraphOutputBoundary::HttpBody;
    assert_eq!(
        graph_completed(facts).unwrap().completion.delivery,
        wire::DeliveryEvidence::Unknown
    );
    facts.execution_succeeded = None;
    assert!(graph(facts).is_none());
    facts.operation = g::GraphOperation::Serve;
    facts.lifecycle = Some(g::GraphLifecycle::Ready);
    assert!(matches!(
        graph(facts),
        Some(wire::PublicEventV1::ProductRuntime(
            wire::ProductRuntimeV1 {
                phase: wire::ProductRuntimePhase::Ready,
                ..
            }
        ))
    ));
}
#[test]
fn graph_preserves_zero_and_missing_receipts_without_cost_or_estimates() {
    let mut facts = graph_sample();
    facts.nodes = Some(0);
    facts.semantic.configured = Some(true);
    facts.semantic.receipts = Some(2);
    facts.semantic.input.known_sum = Some(0);
    facts.semantic.input.reporting_receipts = 1;
    facts.semantic.known_cost_usd = Some(0.2);
    facts.estimated_json_tokens = Some(99);
    let event = graph_completed(facts).unwrap();
    assert_eq!(event.nodes, Some(0));
    assert_eq!(event.edges, None);
    let usage = event.details.semantic.unwrap();
    assert_eq!(usage.input.known_sum, Some(0));
    assert_eq!(usage.input.reporting_receipts, 1);
    assert_eq!(usage.output.known_sum, None);
    assert_eq!(usage.reserved_generations, None);
}
fn stream(input: u64, emitted: u64) -> s::StreamFacts {
    s::StreamFacts {
        input_bytes: Some(input),
        emitted_bytes: Some(emitted),
        input_complete: true,
        output_complete: true,
        tokens: Some(s::TokenCounts { input, emitted }),
        missing: None,
        ..Default::default()
    }
}
#[test]
fn sift_sums_the_same_presented_streams_once_and_keeps_expansion() {
    let sample = sift(s::SiftObservation {
        delivery: s::Delivery::Flushed,
        child: s::ChildOutcome::ExitedNonzero,
        streams: [Some(stream(10, 3)), Some(stream(2, 12))],
        ..Default::default()
    });
    assert_eq!(sample.observed, 1);
    assert_eq!(sample.execution_failed, 0);
    assert_eq!(sample.complete, 1);
    assert_eq!(
        sample.bytes,
        Some(wire::PresentedTotals {
            samples: 1,
            input: 12,
            output: 15
        })
    );
    assert_eq!(sample.tokens, sample.bytes);
    assert!(sample.into_event().is_some());
}
#[test]
fn sift_incomplete_delivery_and_sessions_never_claim_savings() {
    for (terminal, delivery) in [
        (s::Terminal::Invocation, s::Delivery::Failed),
        (s::Terminal::ProtocolSession, s::Delivery::Flushed),
    ] {
        let sample = sift(s::SiftObservation {
            terminal,
            delivery,
            streams: [Some(stream(10, 1)), None],
            ..Default::default()
        });
        assert_eq!(sample.bytes, None);
        assert_eq!(sample.tokens, None);
        assert_eq!(sample.complete, 0);
        assert!(sample.into_event().is_some());
    }
    let sample = sift(s::SiftObservation {
        delivery: s::Delivery::Flushed,
        streams: [Some(stream(10, 1)), Some(s::StreamFacts::default())],
        ..Default::default()
    });
    // The unmeasured second stream is absent from both sides, never zero.
    assert_eq!(
        sample.tokens,
        Some(wire::PresentedTotals {
            samples: 1,
            input: 10,
            output: 1
        })
    );
    assert_eq!(sample.complete, 1);
    assert_eq!(sample.cohort.missingness, None);
}
#[test]
fn sift_memoized_provider_usage_is_not_new_token_usage() {
    let sample = sift(s::SiftObservation {
        semantic: Some(s::SemanticFacts {
            mode: s::SemanticMode::Select,
            disposition: s::SemanticDisposition::Selected,
            provider: s::ProviderOutcome::Memoized,
            request_attempted: false,
            cache_hit: true,
            http_class: None,
            request_input_tokens: Some(20),
            request_output_tokens: Some(4),
            provider_duration: Some(Duration::from_millis(2)),
            passages: None,
            selected: None,
            omitted: None,
            ordinary_tokens: None,
            candidate_tokens: Some(1),
        }),
        ..Default::default()
    });
    let semantic = sample.semantic.unwrap();
    assert_eq!(semantic.input_tokens, None);
    assert_eq!(semantic.output_tokens, None);
    assert_eq!(semantic.provider_latency, None);
    assert_eq!(sample.tokens, None);
    assert!(sample.into_event().is_some());
}
#[test]
fn remote_mcp_keeps_typed_work_failure_even_when_an_error_was_delivered() {
    use crate::remote_history as r;
    let facts = r::RemoteCompletion {
        operation: r::RemoteOperation::Search,
        duration: Duration::from_millis(9),
        stage: r::RemoteStage::Request,
        failure: Some(r::RemoteFailure::Sharing(
            ctx_history_sharing::SharingFailure::Forbidden,
        )),
        facts: r::RemoteReadFacts {
            output_flushed: Some(true),
            ..Default::default()
        },
    };
    let event = remote(facts, true);
    assert_eq!(event.surface, wire::RemoteSurface::Mcp);
    assert_eq!(
        event.completion.execution.unwrap_err().class,
        wire::ProductFailureClass::Forbidden
    );
    assert_eq!(
        event.completion.delivery,
        wire::DeliveryEvidence::KnownComplete
    );
    assert_eq!(event.result, None);
}
#[test]
fn remote_output_failure_distinguishes_session_prefix_from_returned_page() {
    use crate::remote_history as r;
    let mut facts = r::RemoteCompletion {
        operation: r::RemoteOperation::Session,
        duration: Duration::from_millis(9),
        stage: r::RemoteStage::Render,
        failure: Some(r::RemoteFailure::Output),
        facts: r::RemoteReadFacts::default(),
    };
    let event = remote(facts, false);
    assert_eq!(
        event.completion.execution.unwrap_err().class,
        wire::ProductFailureClass::Io
    );
    assert_eq!(event.completion.delivery, wire::DeliveryEvidence::Failed);
    assert_eq!(event.completion.timings.work, None);
    assert_eq!(event.read.query_duration, None);
    assert_eq!(event.read.page_count, None);
    assert_eq!(event.result, None);

    let request_duration = Duration::from_millis(5);
    facts.facts.requests = 1;
    facts.facts.request_duration = Some(request_duration);
    facts.facts.pages = 1;
    facts.facts.returned = Some(2);
    let event = remote(facts, false);
    assert!(event.completion.execution.is_ok());
    assert_eq!(event.completion.delivery, wire::DeliveryEvidence::Failed);
    assert_eq!(event.completion.timings.work, Some(request_duration));
    assert_eq!(event.read.query_duration, Some(request_duration));
    assert_eq!(event.read.page_count, Some(1));
    assert_eq!(event.result.unwrap().count, 2);
}
#[test]
fn optional_telemetry_uses_the_explicit_root_and_bad_config_only_disables_observation() {
    let _lock = ctx_app_config::TEST_LOCAL_USAGE_ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let _environment =
        crate::observability_composition::consent_tests::isolate_analytics_environment(temp.path());
    let explicit = temp.path().join("explicit");
    crate::observability_composition::consent_tests::configure(
        &explicit,
        true,
        "https://example.invalid/analytics",
    );
    let owner = EngineTelemetry::optional(Some(explicit.clone())).unwrap();
    assert_eq!(owner.root, explicit);
    assert!(owner.pending_delivery.is_none());
    assert!(!explicit.join("usage.sqlite").exists());
    std::fs::write(
        ctx_app_config::AppConfig::config_path(&explicit),
        "[malformed",
    )
    .unwrap();
    assert!(EngineTelemetry::optional(Some(explicit.clone())).is_none());
    assert_eq!(
        observe_graph(Some(explicit), |observer| {
            assert!(observer.is_none());
            23
        }),
        23
    );
}
