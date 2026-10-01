use std::time::Duration;

use ctx_history_server as source;

use super::{counts, measured, wire, Accumulator, Summary};

pub(super) fn record(
    memory: &mut Accumulator,
    observation: source::ServerObservation,
    elapsed: Duration,
    now: i64,
) {
    use source::ServerObservation as O;
    let mut summary = match observation {
        O::Lifecycle {
            kind,
            stage,
            duration,
            failure,
            backlog,
        } => {
            let phase = match kind {
                source::ServerLifecycle::Ready => wire::ProductRuntimePhase::Ready,
                source::ServerLifecycle::Liveness => wire::ProductRuntimePhase::Liveness,
                source::ServerLifecycle::Failed => wire::ProductRuntimePhase::Failed,
                source::ServerLifecycle::Stopped => wire::ProductRuntimePhase::Stopped,
                source::ServerLifecycle::ShuttingDown => wire::ProductRuntimePhase::ShuttingDown,
            };
            let mut event =
                wire::ProductRuntimeV1::new(wire::ProductRuntimeKind::Server, phase, duration);
            // The saturated sample is a lower bound; the wire only has exact buckets.
            event.pending_work = backlog
                .map(|v| v.pending_operations_capped)
                .filter(|&pending| pending < 1001);
            event.failure = failure.map(|v| runtime_failure(stage, v));
            if event.failure.is_some() && phase != wire::ProductRuntimePhase::Failed {
                let mut failed = event;
                failed.phase = wire::ProductRuntimePhase::Failed;
                memory.terminal(failed.into_event());
            }
            memory.terminal(event.into_event());
            return;
        }
        O::IndexerHealth { failure } => {
            let phase = if failure.is_some() {
                wire::ProductRuntimePhase::Failed
            } else {
                wire::ProductRuntimePhase::Recovered
            };
            let mut event =
                wire::ProductRuntimeV1::new(wire::ProductRuntimeKind::Server, phase, elapsed);
            event.failure = failure.map(|v| runtime_failure(source::ServerStage::Serve, v));
            memory.terminal(event.into_event());
            return;
        }
        O::Request {
            operation: op,
            duration,
            failure,
            body_handed_off,
            response_class,
            body_outcome,
            response_bytes,
            execution,
            read,
        } => {
            let handoff = wire::HandoffCounts {
                complete: u64::from(body_handed_off == Some(true)),
                failed: u64::from(body_handed_off == Some(false)),
                unknown: u64::from(body_handed_off.is_none()),
            };
            let facts = wire::ServerSummaryFacts::Request {
                handoff,
                body: Some(body(body_outcome)),
                response_bytes: Some(measured(response_bytes)),
                execution: execution.map(|v| counts(Some(v.duration), v.failure.is_some())),
                read: read.map(read_totals),
            };
            let failed = failure.is_some()
                || execution.is_some_and(|v| v.failure.is_some())
                || body_handed_off == Some(false)
                || matches!(
                    body_outcome,
                    source::ServerBodyOutcome::Failed | source::ServerBodyOutcome::Dropped
                )
                || matches!(
                    response_class,
                    source::ServerResponseClass::ClientError
                        | source::ServerResponseClass::ServerError
                        | source::ServerResponseClass::Unavailable
                );
            let mut summary = wire::ServerSummaryV1::new(operation(op), Duration::ZERO, facts);
            summary.counts = counts(Some(duration), failed);
            summary.failure = failure.map(server_failure);
            summary.response_class = Some(response(response_class));
            summary
        }
        O::Execution {
            operation: op,
            duration,
            failure,
        } => {
            let mut summary = wire::ServerSummaryV1::new(
                operation(op),
                Duration::ZERO,
                wire::ServerSummaryFacts::Execution,
            );
            summary.counts = counts(Some(duration), failure.is_some());
            summary.failure = failure.map(server_failure);
            summary
        }
        O::Read {
            operation: op,
            facts,
        } => wire::ServerSummaryV1::new(
            operation(op),
            Duration::ZERO,
            wire::ServerSummaryFacts::Read(read_totals(facts)),
        ),
        O::Upload {
            operation: op,
            bytes,
            replay,
        } => wire::ServerSummaryV1::new(
            operation(op),
            Duration::ZERO,
            wire::ServerSummaryFacts::Upload {
                bytes,
                replayed: u64::from(replay),
            },
        ),
        O::Publication(facts) => {
            let op = match facts.kind {
                source::ServerPublicationKind::Published
                | source::ServerPublicationKind::AlreadyAccepted => wire::ServerOperation::Publish,
                source::ServerPublicationKind::Withdrawn => wire::ServerOperation::Withdraw,
                source::ServerPublicationKind::Removed => wire::ServerOperation::Remove,
                source::ServerPublicationKind::Cancelled => wire::ServerOperation::CancelPublish,
            };
            wire::ServerSummaryV1::new(
                op,
                Duration::ZERO,
                wire::ServerSummaryFacts::Publication {
                    kind: publication(facts.kind),
                    replayed: u64::from(facts.replay),
                    bytes: facts.bytes.map(measured),
                    records: facts.records.map(measured),
                },
            )
        }
        O::Index {
            duration,
            failure,
            facts,
        } => {
            let mut summary = wire::ServerSummaryV1::new(
                wire::ServerOperation::IndexerHealth,
                Duration::ZERO,
                wire::ServerSummaryFacts::Index(wire::ServerIndexTotals {
                    processed_operations: facts.processed_operations.map(measured),
                    records: facts.records.map(measured),
                    bytes: facts.bytes.map(measured),
                    coverage_lag: facts.coverage_lag.map(measured),
                    reads_available: facts.reads_available.map(|v| measured(u64::from(v))),
                    activated: u64::from(facts.activated),
                }),
            );
            summary.counts = counts(Some(duration), failure.is_some());
            summary.failure = failure.map(server_failure);
            summary
        }
    };
    // Measurements without a clock are still observed; never invent latency.
    if summary.counts.observed == 0 {
        summary.counts = counts(None, false);
    }
    memory.window.record(Summary::Server(summary), now);
}

fn read_totals(facts: source::ServerReadFacts) -> wire::ServerReadTotals {
    // Requested limit is not an observed result. The current wire contract
    // carries returned/limited/continuation facts, without the numeric limit.
    wire::ServerReadTotals {
        observed: 1,
        returned: facts.returned,
        nonempty: u64::from(facts.returned > 0),
        bytes: facts.bytes.map(measured),
        continuation_requested: u64::from(facts.continuation_requested),
        has_more: facts.has_more.map(|v| measured(u64::from(v))),
        complete: facts.complete.map(|v| measured(u64::from(v))),
        exhaustive: facts.exhaustive.map(|v| measured(u64::from(v))),
        response_limited: facts.response_limited.map(|v| measured(u64::from(v))),
        snippets_truncated: facts.snippets_truncated.map(measured),
        coverage_lag: facts.coverage_lag.map(measured),
        query_latency: facts.query_duration.map(super::histogram),
    }
}

fn operation(value: source::ServerOperation) -> wire::ServerOperation {
    use source::ServerOperation as S;
    use wire::ServerOperation as W;
    match value {
        S::Health => W::Health,
        S::Enroll => W::Enroll,
        S::Principals => W::Principals,
        S::Credentials => W::Credentials,
        S::RevokePrincipal => W::RevokePrincipal,
        S::RevokeCredential => W::RevokeCredential,
        S::Whoami => W::Whoami,
        S::Invite => W::Invite,
        S::Grants => W::Grants,
        S::RevokeMember => W::RevokeMember,
        S::BeginUpload => W::BeginUpload,
        S::UploadStatus => W::UploadStatus,
        S::UploadChunk => W::UploadChunk,
        S::Publish => W::Publish,
        S::CancelPublish => W::CancelPublish,
        S::Withdraw => W::Withdraw,
        S::Remove => W::Remove,
        S::Receipt => W::Receipt,
        S::Publications => W::Publications,
        S::Publication => W::Publication,
        S::Status => W::Status,
        S::Search => W::Search,
        S::Event => W::Event,
        S::Session => W::Session,
        S::Unknown => W::Unknown,
    }
}

fn server_failure(value: source::ServerFailure) -> wire::ServerFailure {
    use source::ServerFailure as S;
    use wire::ServerFailure as W;
    match value {
        S::Unauthorized => W::Unauthorized,
        S::Forbidden => W::Forbidden,
        S::NotFound => W::NotFound,
        S::Conflict => W::Conflict,
        S::Cancelled => W::Cancelled,
        S::Expired => W::Expired,
        S::Invalid => W::Invalid,
        S::Capacity => W::Capacity,
        S::RequestCapacity => W::RequestCapacity,
        S::WorkCapacity => W::WorkCapacity,
        S::Timeout => W::Timeout,
        S::Interrupted => W::Interrupted,
        S::Body => W::Body,
        S::BodyTooLarge => W::BodyTooLarge,
        S::Method => W::Method,
        S::Unavailable => W::Unavailable,
        S::Index => W::Index,
        S::Io => W::Io,
        S::Catalog => W::Catalog,
        S::Json => W::Json,
        S::Core => W::Core,
        S::Identity => W::Identity,
        S::Archive => W::Archive,
        S::Other => W::Other,
    }
}

fn response(value: source::ServerResponseClass) -> wire::ResponseClass {
    match value {
        source::ServerResponseClass::Unavailable => wire::ResponseClass::Unavailable,
        source::ServerResponseClass::Success => wire::ResponseClass::Success,
        source::ServerResponseClass::Redirect => wire::ResponseClass::Redirect,
        source::ServerResponseClass::ClientError => wire::ResponseClass::ClientError,
        source::ServerResponseClass::ServerError => wire::ResponseClass::ServerError,
        source::ServerResponseClass::Other => wire::ResponseClass::Other,
    }
}

fn body(value: source::ServerBodyOutcome) -> wire::ServerBodyOutcome {
    match value {
        source::ServerBodyOutcome::Suppressed => wire::ServerBodyOutcome::Suppressed,
        source::ServerBodyOutcome::Complete => wire::ServerBodyOutcome::Complete,
        source::ServerBodyOutcome::Failed => wire::ServerBodyOutcome::Failed,
        source::ServerBodyOutcome::Dropped => wire::ServerBodyOutcome::Dropped,
    }
}

fn publication(value: source::ServerPublicationKind) -> wire::ServerPublicationKind {
    match value {
        source::ServerPublicationKind::Published => wire::ServerPublicationKind::Published,
        source::ServerPublicationKind::Withdrawn => wire::ServerPublicationKind::Withdrawn,
        source::ServerPublicationKind::Removed => wire::ServerPublicationKind::Removed,
        source::ServerPublicationKind::Cancelled => wire::ServerPublicationKind::Cancelled,
        source::ServerPublicationKind::AlreadyAccepted => {
            wire::ServerPublicationKind::AlreadyAccepted
        }
    }
}

fn runtime_failure(
    stage: source::ServerStage,
    failure: source::ServerFailure,
) -> wire::ProductFailure {
    use source::{ServerFailure as F, ServerStage as S};
    let stage = match stage {
        S::Configuration => wire::ProductFailureStage::Parse,
        S::AuthorityOpen | S::Runtime | S::Bind => wire::ProductFailureStage::Prepare,
        S::ReadyCallback => wire::ProductFailureStage::Output,
        S::Serve | S::Shutdown => wire::ProductFailureStage::Execute,
    };
    let class = match failure {
        F::Unauthorized => wire::ProductFailureClass::Unauthorized,
        F::Forbidden => wire::ProductFailureClass::Forbidden,
        F::NotFound => wire::ProductFailureClass::NotFound,
        F::Conflict => wire::ProductFailureClass::Conflict,
        F::Cancelled | F::Interrupted => wire::ProductFailureClass::Cancelled,
        F::Expired | F::Timeout => wire::ProductFailureClass::Timeout,
        F::Invalid | F::Body | F::BodyTooLarge | F::Method | F::Json => {
            wire::ProductFailureClass::InvalidRequest
        }
        F::Capacity | F::RequestCapacity | F::WorkCapacity => wire::ProductFailureClass::Capacity,
        F::Io => wire::ProductFailureClass::Io,
        F::Index | F::Catalog | F::Core | F::Archive => wire::ProductFailureClass::Store,
        F::Unavailable | F::Identity | F::Other => wire::ProductFailureClass::Other,
    };
    wire::ProductFailure { stage, class }
}
