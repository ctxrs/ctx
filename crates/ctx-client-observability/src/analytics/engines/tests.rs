use super::super::sender::serialize_event;
use super::*;
fn json(event: PublicEventV1) -> Value {
    serialize_event(
        &event,
        chrono::DateTime::parse_from_rfc3339("2026-09-30T00:00:00Z")
            .unwrap()
            .to_utc(),
        None,
        None,
    )
}
#[test]
fn native_boundaries_are_independent_of_legacy_envelope() {
    for (micros, expected) in [
        (0, "lt_1ms"),
        (999, "lt_1ms"),
        (1000, "1ms-5ms"),
        (5000, "5ms-10ms"),
        (10000, "10ms-25ms"),
        (25000, "25ms-50ms"),
        (50000, "50ms-100ms"),
        (100000, "100ms-250ms"),
        (250000, "250ms-1s"),
        (1000000, "1s-5s"),
        (5000000, "5s-30s"),
        (30000000, "30s-2m"),
        (120000000, "2m-10m"),
        (600000000, "10m-1h"),
        (3600000000, "1h+"),
    ] {
        assert_eq!(
            native_duration_bucket(Duration::from_micros(micros)),
            expected
        );
    }
}
#[test]
fn graph_output_failure_keeps_measured_query_result() {
    let mut event = GraphCompletedV1::new(
        GraphOperation::Search,
        GraphSurface::Cli,
        ProductCompletion::new(
            Duration::from_millis(2),
            Ok(()),
            DeliveryEvidence::Failed,
            ProductOutput::Json,
        ),
    );
    event.result = Some(ProductResultFacts {
        count: 3,
        truncated: Some(false),
    });
    let value = json(event.into_event());
    assert_eq!(value["outcome"], "failure");
    assert_eq!(value["duration_bucket"], "lt_100ms");
    assert_eq!(
        value["properties"]["native_total_duration_bucket"],
        "1ms-5ms"
    );
    assert_eq!(value["properties"]["execution_result"], "success");
    assert_eq!(value["properties"]["result_count_bucket"], "2-5");
    assert_eq!(value["properties"]["product_failure_stage"], "output");
}
#[test]
fn measured_savings_use_actual_paired_totals_with_expansion_and_missingness() {
    let mut s = SiftSummaryV1::new(
        SiftOperation::Run,
        SiftMode::Lossless,
        Duration::from_secs(60),
    );
    s.observed = 3;
    s.complete = 2;
    s.unmeasured = 1;
    s.tokens = Some(PresentedTotals {
        samples: 2,
        input: 1000,
        output: 1100,
    });
    let value = json(s.into_event().unwrap());
    assert_eq!(value["properties"]["sift_tokens_change"], "increased");
    assert_eq!(
        value["properties"]["sift_tokens_savings_fraction_bucket"],
        "increased"
    );
    assert_eq!(value["properties"]["unmeasured_count_bucket"], "1");
    assert!(value["properties"].get("sift_bytes_input_bucket").is_none());
    s.tokens = Some(PresentedTotals {
        samples: 2,
        input: 0,
        output: 0,
    });
    assert!(json(s.into_event().unwrap())["properties"]
        .get("sift_tokens_savings_fraction_bucket")
        .is_none());
    s.complete = 1;
    assert!(s.into_event().is_none());
}
#[test]
fn response_construction_is_not_transport_evidence() {
    let event = ServerRequestCompletedV1::new(
        ServerOperation::Search,
        ProductCompletion::new(
            Duration::from_millis(1),
            Ok(()),
            DeliveryEvidence::KnownComplete,
            ProductOutput::Http,
        ),
        ResponseClass::Success,
    );
    assert_eq!(
        json(event.into_event())["properties"]["response_handoff"],
        "unknown"
    );
}

fn assert_fixture(event: PublicEventV1, fixture: &str) {
    let mut actual = json(event);
    let mut expected: Value = serde_json::from_str(fixture).unwrap();
    for key in ["event_id", "occurred_at"] {
        actual.as_object_mut().unwrap().remove(key);
        expected.as_object_mut().unwrap().remove(key);
    }
    assert_eq!(actual, expected);
    assert!(actual["properties"]
        .as_object()
        .unwrap()
        .values()
        .all(|v| v.is_string() || v.is_boolean()));
}
#[test]
fn graph_groups_match_collector_fixture_without_exact_numerics() {
    let mut g = GraphCompletedV1::new(
        GraphOperation::Index,
        GraphSurface::Cli,
        ProductCompletion::new(
            Duration::from_millis(10),
            Ok(()),
            DeliveryEvidence::KnownComplete,
            ProductOutput::Json,
        ),
    );
    g.result = Some(ProductResultFacts {
        count: 3,
        truncated: Some(true),
    });
    g.nodes = Some(42);
    g.edges = Some(8);
    g.files_processed = Some(2);
    g.details.query = Some(GraphQueryFacts {
        bounds: Some(GraphBounds {
            work: true,
            ..Default::default()
        }),
        path: Some(GraphPathDisposition::Incomplete),
        unresolved: Some(1),
        seeds: Some(2),
        duration: Some(Duration::from_micros(900)),
    });
    g.details.index = Some(GraphIndexFacts {
        disposition: Some(GraphIndexDisposition::Committed),
        fresh: Some(true),
        parsed: Some(2),
        rejected: Some(1),
        unchanged: Some(0),
        deleted: Some(1),
        diagnostics: Some(3),
        capture: Some(Duration::from_micros(500)),
        detect: Some(Duration::from_millis(2)),
        extract: Some(Duration::from_millis(5)),
        commit: Some(Duration::from_millis(1)),
    });
    g.details.semantic = Some(GraphSemanticFacts {
        configured: Some(true),
        reserved_generations: Some(4),
        receipts: Some(3),
        input: GraphTokenUsage {
            known_sum: Some(100),
            reporting_receipts: 2,
        },
        ..Default::default()
    });
    assert_fixture(
        g.into_event(),
        include_str!("../../../../../contracts/telemetry-v1/fixtures/graph_completed.valid.json"),
    );
}
#[test]
fn sift_cohort_preserves_fail_open_child_exit_and_presentation_evidence() {
    let mut s = SiftSummaryV1::new(
        SiftOperation::Run,
        SiftMode::Capture,
        Duration::from_secs(60),
    );
    s.host = SiftHost::Codex;
    s.observed = 3;
    s.complete = 1;
    s.partial = 1;
    s.unmeasured = 1;
    s.tokens = Some(PresentedTotals {
        samples: 1,
        input: 1000,
        output: 1100,
    });
    let mut bins = [0; 14];
    bins[1] = 3;
    s.latency = Some(bins);
    s.cohort = SiftCohort {
        entry: Some(SiftEntry::CompletionHook),
        terminal: Some(SiftTerminal::Invocation),
        outcome: Some(SiftTerminalOutcome::FailOpen),
        delivery: Some(SiftDelivery::Unchanged),
        failure_phase: Some(SiftPhase::Codec),
        failure_kind: Some(SiftFailureKind::Tokenizer),
        child: Some(SiftChildOutcome::ExitedNonzero),
        missingness: Some(SiftMissingness::TokenizerUnavailable),
        ..Default::default()
    };
    assert_fixture(
        s.into_event().unwrap(),
        include_str!("../../../../../contracts/telemetry-v1/fixtures/sift_summary.valid.json"),
    );
    s.cohort.entry = Some(SiftEntry::Mcp);
    assert_eq!(json(s.into_event().unwrap())["surface"], "mcp");
    s.semantic = Some(SiftSemanticSummary {
        mode: SiftSemanticMode::Select,
        disposition: SiftSemanticDisposition::Selected,
        provider: SiftProviderOutcome::Memoized,
        observed: 1,
        requests_attempted: 0,
        cache_hits: 1,
        input_tokens: Some(MeasuredTotal {
            samples: 1,
            total: 99,
        }),
        output_tokens: None,
        provider_latency: None,
    });
    assert!(
        s.into_event().is_none(),
        "memoized earlier usage is not this request's measured usage"
    );
}
#[test]
fn server_sharing_and_remote_fixtures_preserve_separate_populations() {
    let mut s = ServerSummaryV1::new(
        ServerOperation::Search,
        Duration::from_secs(60),
        ServerSummaryFacts::Request {
            body: Some(ServerBodyOutcome::Dropped),
            response_bytes: Some(MeasuredTotal {
                samples: 3,
                total: 2048,
            }),
            handoff: HandoffCounts {
                complete: 0,
                failed: 1,
                unknown: 2,
            },
            execution: None,
            read: None,
        },
    );
    let mut bins = [0; 14];
    bins[1] = 3;
    s.counts = WindowCounts {
        observed: 3,
        failed: 1,
        latency: Some(bins),
    };
    assert_fixture(
        s.into_event().unwrap(),
        include_str!("../../../../../contracts/telemetry-v1/fixtures/server_summary.valid.json"),
    );
    s.facts = ServerSummaryFacts::Request {
        body: None,
        response_bytes: None,
        handoff: HandoffCounts {
            complete: 3,
            failed: 1,
            unknown: 1,
        },
        execution: None,
        read: None,
    };
    assert!(s.into_event().is_none());
    let mut h = SharingSummaryV1::new(SharingOperation::Tick, Duration::from_secs(60));
    h.counts.observed = 2;
    h.phase = Some(SharingPhase::Receipt);
    h.tick = Some(SharingTick::Progress);
    h.progress_after_failure = Some(1);
    assert_fixture(
        h.into_event().unwrap(),
        include_str!("../../../../../contracts/telemetry-v1/fixtures/sharing_summary.valid.json"),
    );
    let mut r = RemoteCompletedV1::new(
        RemoteOperation::Search,
        RemoteSurface::Cli,
        ProductCompletion::new(
            Duration::from_millis(2),
            Ok(()),
            DeliveryEvidence::KnownComplete,
            ProductOutput::Json,
        ),
    );
    r.result = Some(ProductResultFacts {
        count: 2,
        truncated: None,
    });
    r.read = RemoteReadFacts {
        page_count: Some(1),
        limit: Some(10),
        client_limited: Some(true),
        complete: Some(false),
        exhaustive: Some(false),
        has_more: Some(true),
        response_limited: Some(false),
        coverage_lag: Some(2),
        query_duration: Some(Duration::from_micros(900)),
    };
    assert_fixture(
        r.into_event(),
        include_str!("../../../../../contracts/telemetry-v1/fixtures/remote_completed.valid.json"),
    );
    let mut runtime = ProductRuntimeV1::new(
        ProductRuntimeKind::Server,
        ProductRuntimePhase::Liveness,
        Duration::from_secs(60),
    );
    runtime.active_requests = Some(2);
    runtime.pending_work = Some(0);
    assert_fixture(
        runtime.into_event(),
        include_str!("../../../../../contracts/telemetry-v1/fixtures/product_runtime.valid.json"),
    );
}

#[test]
fn paired_totals_keep_sample_counts_and_actual_weighted_expansion() {
    #[derive(serde::Deserialize)]
    struct PairCase {
        name: String,
        unit: String,
        samples: u64,
        input: u64,
        output: u64,
        expected: Value,
    }
    let cases: Vec<PairCase> = serde_json::from_str(include_str!(
        "../../../../../contracts/telemetry-v1/fixtures/sift_presented_pairs.json"
    ))
    .unwrap();
    for case in cases {
        let mut s = SiftSummaryV1::new(
            SiftOperation::Compact,
            SiftMode::Capture,
            Duration::from_secs(60),
        );
        s.observed = case.samples + 2;
        s.complete = case.samples;
        s.partial = 1;
        s.unmeasured = 1;
        let pair = PresentedTotals {
            samples: case.samples,
            input: case.input,
            output: case.output,
        };
        if case.unit == "bytes" {
            s.bytes = Some(pair);
        } else {
            s.tokens = Some(pair);
        }
        let event = json(s.into_event().unwrap());
        let measurements: Map<String, Value> = event["properties"]
            .as_object()
            .unwrap()
            .iter()
            .filter(|(k, _)| {
                k.starts_with("sift_tokens_")
                    || k.starts_with("sift_bytes_")
                    || k.as_str() == "sift_token_basis"
            })
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        assert_eq!(Value::Object(measurements), case.expected, "{}", case.name);
        assert_eq!(event["properties"]["partial_measurement_count_bucket"], "1");
        assert_eq!(event["properties"]["unmeasured_count_bucket"], "1");
        if case.unit == "bytes" {
            s.bytes.as_mut().unwrap().samples = 0;
        } else {
            s.tokens.as_mut().unwrap().samples = 0;
        }
        assert!(
            s.into_event().is_none(),
            "measured zero values still require a sample"
        );
    }
    let mut s = SiftSummaryV1::new(
        SiftOperation::Compact,
        SiftMode::Capture,
        Duration::from_secs(60),
    );
    s.observed = 1;
    s.complete = 1;
    s.tokens = Some(PresentedTotals {
        samples: 1,
        input: u64::MAX,
        output: u64::MAX - 1,
    });
    let event = json(s.into_event().unwrap());
    assert_eq!(
        event["properties"]["sift_tokens_savings_fraction_bucket"],
        "lt_10pct"
    );
    assert_eq!(event["properties"]["sift_tokens_delta_bucket"], "1");
    s.tokens.as_mut().unwrap().samples = 2;
    assert!(
        s.into_event().is_none(),
        "sample population may not exceed comparable observations"
    );
}
