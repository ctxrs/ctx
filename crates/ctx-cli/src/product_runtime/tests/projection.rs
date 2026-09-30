use super::*;

fn server_summary(observation: server_source::ServerObservation) -> wire::ServerSummaryV1 {
    let mut memory = Accumulator::default();
    server::record(&mut memory, observation, Duration::ZERO, 100);
    let events = memory.window.take(110);
    let [PublicEventV1::ServerSummary(value)] = events.as_slice() else {
        panic!("one valid summary: {events:?}")
    };
    *value
}

#[test]
fn lifecycle_preserves_exact_backlog_and_omits_saturated_samples() {
    for (kind, phase) in [
        (
            server_source::ServerLifecycle::Ready,
            wire::ProductRuntimePhase::Ready,
        ),
        (
            server_source::ServerLifecycle::Liveness,
            wire::ProductRuntimePhase::Liveness,
        ),
    ] {
        for (pending_operations_capped, pending_work) in [
            (0, Some(0)),
            (1000, Some(1000)),
            (1001, None),
            (u64::MAX, None),
        ] {
            let mut memory = Accumulator::default();
            server::record(
                &mut memory,
                server_source::ServerObservation::Lifecycle {
                    kind,
                    stage: server_source::ServerStage::Serve,
                    duration: Duration::from_secs(3),
                    failure: None,
                    backlog: Some(server_source::ServerBacklog {
                        pending_operations_capped,
                        staged_uploads_capped: 0,
                        collections_capped: 0,
                    }),
                },
                Duration::from_secs(99),
                100,
            );
            let [PublicEventV1::ProductRuntime(runtime)] = memory.terminals.as_slice() else {
                panic!("one lifecycle event")
            };
            assert_eq!(runtime.kind, wire::ProductRuntimeKind::Server);
            assert_eq!(runtime.phase, phase);
            assert_eq!(runtime.uptime, Duration::from_secs(3));
            assert_eq!(runtime.pending_work, pending_work);
            assert!(memory.window.take(110).is_empty());
        }
    }
}

#[test]
fn transport_completion_does_not_invent_execution_or_read_evidence() {
    for (outcome, handoff, expected) in [
        (
            server_source::ServerBodyOutcome::Complete,
            Some(true),
            (1, 0, 0),
        ),
        (
            server_source::ServerBodyOutcome::Failed,
            Some(false),
            (0, 1, 0),
        ),
        (server_source::ServerBodyOutcome::Dropped, None, (0, 0, 1)),
        (
            server_source::ServerBodyOutcome::Suppressed,
            None,
            (0, 0, 1),
        ),
    ] {
        let value = server_summary(server_source::ServerObservation::Request {
            operation: server_source::ServerOperation::Event,
            duration: Duration::from_millis(10),
            failure: None,
            body_handed_off: handoff,
            response_class: server_source::ServerResponseClass::Success,
            body_outcome: outcome,
            response_bytes: 0,
            execution: None,
            read: None,
        });
        assert_eq!(
            value.counts.failed,
            u64::from(matches!(
                outcome,
                server_source::ServerBodyOutcome::Failed
                    | server_source::ServerBodyOutcome::Dropped
            ))
        );
        let wire::ServerSummaryFacts::Request {
            handoff,
            body,
            response_bytes,
            execution,
            read,
        } = value.facts
        else {
            panic!("request")
        };
        assert_eq!(
            (handoff.complete, handoff.failed, handoff.unknown),
            expected
        );
        assert!(body.is_some());
        assert_eq!(response_bytes, Some(measured(0)));
        assert!(execution.is_none());
        assert!(read.is_none());
    }
}

#[test]
fn request_preserves_failed_execution_partial_read_and_actual_body_bytes() {
    let value = server_summary(server_source::ServerObservation::Request {
        operation: server_source::ServerOperation::Session,
        duration: Duration::from_millis(123),
        failure: Some(server_source::ServerFailure::Interrupted),
        body_handed_off: Some(false),
        response_class: server_source::ServerResponseClass::Success,
        body_outcome: server_source::ServerBodyOutcome::Dropped,
        response_bytes: 17,
        execution: Some(server_source::ServerExecutionFacts {
            duration: Duration::from_millis(7),
            failure: Some(server_source::ServerFailure::Core),
        }),
        read: Some(server_source::ServerReadFacts {
            returned: 2,
            bytes: Some(900),
            limit: Some(10),
            continuation_requested: true,
            has_more: Some(true),
            complete: Some(false),
            exhaustive: None,
            response_limited: Some(true),
            snippets_truncated: Some(1),
            coverage_lag: Some(3),
            query_duration: Some(Duration::from_millis(5)),
        }),
    });
    assert_eq!(value.failure, Some(wire::ServerFailure::Interrupted));
    assert_eq!(value.counts.failed, 1);
    let wire::ServerSummaryFacts::Request {
        body,
        response_bytes,
        execution,
        read,
        ..
    } = value.facts
    else {
        panic!("request")
    };
    assert_eq!(body, Some(wire::ServerBodyOutcome::Dropped));
    assert_eq!(response_bytes, Some(measured(17)));
    assert_eq!(execution.unwrap().failed, 1);
    assert_eq!(execution.unwrap().latency.unwrap()[2], 1);
    let read = read.unwrap();
    assert_eq!(read.returned, 2);
    assert_eq!(read.bytes, Some(measured(900)));
    assert_eq!(read.complete, Some(measured(0)));
    assert_eq!(read.exhaustive, None);
    assert_eq!(read.response_limited, Some(measured(1)));
    assert_eq!(read.continuation_requested, 1);
    assert_eq!(read.coverage_lag, Some(measured(3)));
}

#[test]
fn upload_acceptance_and_index_activation_remain_separate_populations() {
    let upload = server_summary(server_source::ServerObservation::Upload {
        operation: server_source::ServerOperation::UploadChunk,
        bytes: 250,
        replay: true,
    });
    assert_eq!(
        upload.facts,
        wire::ServerSummaryFacts::Upload {
            bytes: 250,
            replayed: 1
        }
    );
    assert!(upload.counts.latency.is_none());
    let accepted = server_summary(server_source::ServerObservation::Publication(
        server_source::ServerPublicationFacts {
            kind: server_source::ServerPublicationKind::Published,
            replay: false,
            bytes: Some(250),
            records: Some(12),
        },
    ));
    assert!(matches!(
        accepted.facts,
        wire::ServerSummaryFacts::Publication {
            kind: wire::ServerPublicationKind::Published,
            ..
        }
    ));
    let indexed = server_summary(server_source::ServerObservation::Index {
        duration: Duration::from_millis(50),
        failure: Some(server_source::ServerFailure::Index),
        facts: server_source::ServerIndexFacts {
            processed_operations: Some(0),
            records: Some(12),
            bytes: None,
            coverage_lag: Some(1),
            reads_available: Some(false),
            activated: false,
        },
    });
    assert_eq!(indexed.counts.failed, 1);
    let wire::ServerSummaryFacts::Index(facts) = indexed.facts else {
        panic!("index")
    };
    assert_eq!(facts.activated, 0);
    assert_eq!(facts.records, Some(measured(12)));
    assert_eq!(facts.bytes, None);
    assert_eq!(facts.reads_available, Some(measured(0)));
}

#[test]
fn sharing_covers_every_population_without_conflating_transfer_acceptance_and_settlement() {
    use sharing_source::{
        SharingFailure as F, SharingObservation as O, SharingPhase as P, SharingTick as T,
    };
    let observations = [
        O::WorkerStarted,
        O::WorkerStartFailed(F::Configuration),
        O::WorkerStopped,
        O::Tick {
            phase: P::Receipt,
            outcome: T::Progress,
            duration: Duration::from_millis(20),
            failure: None,
            progress_after_failure: true,
        },
        O::Selection {
            decision: sharing_source::SelectionDecision::NeedsReview,
            count: 4,
            complete: false,
        },
        O::Queued {
            bytes: 10,
            records: 3,
        },
        O::Transfer { bytes: 5 },
        O::Accepted {
            bytes: 10,
            records: 3,
            recovered_receipt: true,
        },
        O::Settled {
            already_accepted: true,
        },
        O::Retry {
            phase: P::UploadChunk,
            failure: F::Unavailable,
            attempts: 4,
            delay: Duration::from_secs(30),
        },
    ];
    let summaries = observations
        .into_iter()
        .map(sharing::project)
        .collect::<Vec<_>>();
    assert!(summaries.iter().all(|v| v.into_event().is_some()));
    assert_eq!(summaries[1].counts.failed, 1);
    assert_eq!(summaries[3].progress_after_failure, Some(1));
    assert_eq!(summaries[4].selected_count, Some(4));
    assert_eq!(summaries[4].selection_complete, Some(false));
    assert_eq!(summaries[5].records, Some(measured(3)));
    assert_eq!(summaries[6].bytes, Some(measured(5)));
    assert_eq!(summaries[6].records, None);
    assert_eq!(summaries[7].recovered_receipts, Some(1));
    assert_eq!(summaries[8].already_accepted, Some(1));
    assert_eq!(summaries[8].bytes, None);
    assert_eq!(summaries[9].retry_attempts, Some(measured(4)));
    assert_eq!(summaries[9].retry_delay.unwrap()[10], 1);
    assert!(summaries[9].counts.latency.is_none());
}
