use super::super::*;
use super::Summary;

pub(super) fn same_cohort(a: Summary, b: Summary) -> bool {
    match (a, b) {
        (Summary::Sift(a), Summary::Sift(b)) => {
            a.operation == b.operation
                && a.mode == b.mode
                && a.host == b.host
                && a.cohort == b.cohort
                && a.semantic.map(|s| (s.mode, s.disposition, s.provider))
                    == b.semantic.map(|s| (s.mode, s.disposition, s.provider))
        }
        (Summary::Server(a), Summary::Server(b)) => {
            a.operation == b.operation
                && a.failure == b.failure
                && a.response_class == b.response_class
                && match (a.facts, b.facts) {
                    (
                        ServerSummaryFacts::Request { body: a, .. },
                        ServerSummaryFacts::Request { body: b, .. },
                    ) => a == b,
                    (ServerSummaryFacts::Execution, ServerSummaryFacts::Execution)
                    | (ServerSummaryFacts::Read(_), ServerSummaryFacts::Read(_))
                    | (ServerSummaryFacts::Upload { .. }, ServerSummaryFacts::Upload { .. })
                    | (ServerSummaryFacts::Index(_), ServerSummaryFacts::Index(_)) => true,
                    (
                        ServerSummaryFacts::Publication { kind: a, .. },
                        ServerSummaryFacts::Publication { kind: b, .. },
                    ) => a == b,
                    _ => false,
                }
        }
        (Summary::Sharing(a), Summary::Sharing(b)) => {
            a.operation == b.operation
                && a.phase == b.phase
                && a.tick == b.tick
                && a.failure == b.failure
                && a.selection == b.selection
                && a.selection_complete == b.selection_complete
        }
        _ => false,
    }
}
pub(super) fn combine(a: Summary, b: Summary) -> Option<Summary> {
    if !same_cohort(a, b) {
        return None;
    }
    let v = match (a, b) {
        (Summary::Sift(a), Summary::Sift(b)) => Summary::Sift(sift(a, b)?),
        (Summary::Server(a), Summary::Server(b)) => Summary::Server(server(a, b)?),
        (Summary::Sharing(a), Summary::Sharing(b)) => Summary::Sharing(sharing(a, b)?),
        _ => return None,
    };
    v.event().map(|_| v)
}
fn sum(a: u64, b: u64) -> Option<u64> {
    a.checked_add(b)
}
fn optional_sum(a: Option<u64>, b: Option<u64>) -> Option<Option<u64>> {
    Some(match (a, b) {
        (Some(a), Some(b)) => Some(sum(a, b)?),
        (Some(v), None) | (None, Some(v)) => Some(v),
        (None, None) => None,
    })
}
fn measured(a: Option<MeasuredTotal>, b: Option<MeasuredTotal>) -> Option<Option<MeasuredTotal>> {
    Some(match (a, b) {
        (Some(a), Some(b)) => Some(MeasuredTotal {
            samples: sum(a.samples, b.samples)?,
            total: sum(a.total, b.total)?,
        }),
        (Some(v), None) | (None, Some(v)) => Some(v),
        (None, None) => None,
    })
}
fn presented(
    a: Option<PresentedTotals>,
    b: Option<PresentedTotals>,
) -> Option<Option<PresentedTotals>> {
    Some(match (a, b) {
        (Some(a), Some(b)) => Some(PresentedTotals {
            samples: sum(a.samples, b.samples)?,
            input: sum(a.input, b.input)?,
            output: sum(a.output, b.output)?,
        }),
        (Some(v), None) | (None, Some(v)) => Some(v),
        (None, None) => None,
    })
}
fn histogram(a: Option<[u64; 14]>, b: Option<[u64; 14]>) -> Option<Option<[u64; 14]>> {
    Some(match (a, b) {
        (Some(mut a), Some(b)) => {
            for i in 0..14 {
                a[i] = sum(a[i], b[i])?;
            }
            Some(a)
        }
        (Some(v), None) | (None, Some(v)) => Some(v),
        (None, None) => None,
    })
}
fn counts(a: WindowCounts, b: WindowCounts) -> Option<WindowCounts> {
    Some(WindowCounts {
        observed: sum(a.observed, b.observed)?,
        failed: sum(a.failed, b.failed)?,
        latency: histogram(a.latency, b.latency)?,
    })
}
fn sift(mut a: SiftSummaryV1, b: SiftSummaryV1) -> Option<SiftSummaryV1> {
    a.observed = sum(a.observed, b.observed)?;
    a.execution_failed = sum(a.execution_failed, b.execution_failed)?;
    a.output_failed = sum(a.output_failed, b.output_failed)?;
    a.complete = sum(a.complete, b.complete)?;
    a.partial = sum(a.partial, b.partial)?;
    a.unmeasured = sum(a.unmeasured, b.unmeasured)?;
    a.bytes = presented(a.bytes, b.bytes)?;
    a.tokens = presented(a.tokens, b.tokens)?;
    a.latency = histogram(a.latency, b.latency)?;
    a.collection_limited |= b.collection_limited;
    a.semantic = match (a.semantic, b.semantic) {
        (Some(mut a), Some(b)) => {
            a.observed = sum(a.observed, b.observed)?;
            a.requests_attempted = sum(a.requests_attempted, b.requests_attempted)?;
            a.cache_hits = sum(a.cache_hits, b.cache_hits)?;
            a.input_tokens = measured(a.input_tokens, b.input_tokens)?;
            a.output_tokens = measured(a.output_tokens, b.output_tokens)?;
            a.provider_latency = histogram(a.provider_latency, b.provider_latency)?;
            Some(a)
        }
        (None, None) => None,
        _ => return None,
    };
    Some(a)
}
fn read(mut a: ServerReadTotals, b: ServerReadTotals) -> Option<ServerReadTotals> {
    a.observed = sum(a.observed, b.observed)?;
    a.returned = sum(a.returned, b.returned)?;
    a.nonempty = sum(a.nonempty, b.nonempty)?;
    a.continuation_requested = sum(a.continuation_requested, b.continuation_requested)?;
    a.bytes = measured(a.bytes, b.bytes)?;
    a.has_more = measured(a.has_more, b.has_more)?;
    a.complete = measured(a.complete, b.complete)?;
    a.exhaustive = measured(a.exhaustive, b.exhaustive)?;
    a.response_limited = measured(a.response_limited, b.response_limited)?;
    a.snippets_truncated = measured(a.snippets_truncated, b.snippets_truncated)?;
    a.coverage_lag = measured(a.coverage_lag, b.coverage_lag)?;
    a.query_latency = histogram(a.query_latency, b.query_latency)?;
    Some(a)
}
fn optional_read(
    a: Option<ServerReadTotals>,
    b: Option<ServerReadTotals>,
) -> Option<Option<ServerReadTotals>> {
    Some(match (a, b) {
        (Some(a), Some(b)) => Some(read(a, b)?),
        (Some(v), None) | (None, Some(v)) => Some(v),
        (None, None) => None,
    })
}
fn optional_counts(
    a: Option<WindowCounts>,
    b: Option<WindowCounts>,
) -> Option<Option<WindowCounts>> {
    Some(match (a, b) {
        (Some(a), Some(b)) => Some(counts(a, b)?),
        (Some(v), None) | (None, Some(v)) => Some(v),
        (None, None) => None,
    })
}
fn index(mut a: ServerIndexTotals, b: ServerIndexTotals) -> Option<ServerIndexTotals> {
    a.processed_operations = measured(a.processed_operations, b.processed_operations)?;
    a.records = measured(a.records, b.records)?;
    a.bytes = measured(a.bytes, b.bytes)?;
    a.coverage_lag = measured(a.coverage_lag, b.coverage_lag)?;
    a.reads_available = measured(a.reads_available, b.reads_available)?;
    a.activated = sum(a.activated, b.activated)?;
    Some(a)
}
fn server(mut a: ServerSummaryV1, b: ServerSummaryV1) -> Option<ServerSummaryV1> {
    a.counts = counts(a.counts, b.counts)?;
    a.collection_limited |= b.collection_limited;
    match (&mut a.facts, b.facts) {
        (
            ServerSummaryFacts::Request {
                handoff: a,
                response_bytes: ab,
                execution: ae,
                read: ar,
                ..
            },
            ServerSummaryFacts::Request {
                handoff: b,
                response_bytes: bb,
                execution: be,
                read: br,
                ..
            },
        ) => {
            a.complete = sum(a.complete, b.complete)?;
            a.failed = sum(a.failed, b.failed)?;
            a.unknown = sum(a.unknown, b.unknown)?;
            *ab = measured(*ab, bb)?;
            *ae = optional_counts(*ae, be)?;
            *ar = optional_read(*ar, br)?;
        }
        (ServerSummaryFacts::Execution, ServerSummaryFacts::Execution) => {}
        (ServerSummaryFacts::Read(a), ServerSummaryFacts::Read(b)) => *a = read(*a, b)?,
        (
            ServerSummaryFacts::Upload {
                bytes: a,
                replayed: ar,
            },
            ServerSummaryFacts::Upload {
                bytes: b,
                replayed: br,
            },
        ) => {
            *a = sum(*a, b)?;
            *ar = sum(*ar, br)?;
        }
        (
            ServerSummaryFacts::Publication {
                replayed: a,
                bytes: ab,
                records: ar,
                ..
            },
            ServerSummaryFacts::Publication {
                replayed: b,
                bytes: bb,
                records: br,
                ..
            },
        ) => {
            *a = sum(*a, b)?;
            *ab = measured(*ab, bb)?;
            *ar = measured(*ar, br)?;
        }
        (ServerSummaryFacts::Index(a), ServerSummaryFacts::Index(b)) => *a = index(*a, b)?,
        _ => return None,
    }
    Some(a)
}
fn sharing(mut a: SharingSummaryV1, b: SharingSummaryV1) -> Option<SharingSummaryV1> {
    a.counts = counts(a.counts, b.counts)?;
    a.collection_limited |= b.collection_limited;
    a.selected_count = optional_sum(a.selected_count, b.selected_count)?;
    a.bytes = measured(a.bytes, b.bytes)?;
    a.records = measured(a.records, b.records)?;
    a.recovered_receipts = optional_sum(a.recovered_receipts, b.recovered_receipts)?;
    a.already_accepted = optional_sum(a.already_accepted, b.already_accepted)?;
    a.progress_after_failure = optional_sum(a.progress_after_failure, b.progress_after_failure)?;
    a.retry_attempts = measured(a.retry_attempts, b.retry_attempts)?;
    a.retry_delay = histogram(a.retry_delay, b.retry_delay)?;
    Some(a)
}
