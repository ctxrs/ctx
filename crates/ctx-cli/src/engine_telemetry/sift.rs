use ctx_client_observability::analytics as wire;
use ctx_sift::observation as native;
use std::time::Duration;

use super::sift_vocabulary as vocabulary;

/// One sample for the parent's fixed-cohort accumulator. This never tokenizes.
pub(crate) fn sift(facts: native::SiftObservation) -> wire::SiftSummaryV1 {
    let mut summary = wire::SiftSummaryV1::new(
        vocabulary::operation(facts.operation),
        vocabulary::mode(facts.mode),
        Duration::ZERO,
    );
    summary.host = facts
        .host
        .map(vocabulary::host)
        .unwrap_or(wire::SiftHost::Unknown);
    summary.observed = 1;
    summary.execution_failed = u64::from(
        matches!(
            facts.outcome,
            native::Outcome::Failure | native::Outcome::FailOpen
        ) && facts
            .failure
            .is_none_or(|f| f.phase != native::Phase::Output),
    );
    summary.output_failed = u64::from(facts.delivery == native::Delivery::Failed);
    summary.latency = Some(histogram(facts.duration));
    summary.cohort = wire::SiftCohort {
        entry: Some(vocabulary::entry(facts.entry)),
        terminal: Some(vocabulary::terminal(facts.terminal)),
        outcome: Some(vocabulary::outcome(facts.outcome)),
        delivery: Some(vocabulary::delivery(facts.delivery)),
        skip: facts.skip.map(vocabulary::skip_reason),
        failure_phase: facts.failure.map(|f| vocabulary::phase(f.phase)),
        failure_kind: facts.failure.map(|f| vocabulary::failure_kind(f.kind)),
        child: Some(vocabulary::child_outcome(facts.child)),
        missingness: agreed_missingness(&facts.streams).map(vocabulary::missingness),
    };
    // Session terminals are a separate population; only request/invocation
    // terminals may report presented content, never the session envelope.
    if facts.terminal != native::Terminal::ProtocolSession
        && facts.delivery == native::Delivery::Flushed
    {
        summary.bytes = presented(&facts.streams, |stream| {
            Some((stream.input_bytes?, stream.emitted_bytes?))
        });
        summary.tokens = presented(&facts.streams, |stream| {
            stream.tokens.map(|t| (t.input, t.emitted))
        });
    }
    if summary.bytes.is_some() || summary.tokens.is_some() {
        summary.complete = 1;
    } else if facts
        .streams
        .iter()
        .flatten()
        .any(|s| s.input_bytes.is_some() || s.emitted_bytes.is_some())
    {
        summary.partial = 1;
    } else {
        summary.unmeasured = 1;
    }
    summary.semantic = facts.semantic.map(|s| wire::SiftSemanticSummary {
        mode: vocabulary::semantic_mode(s.mode),
        disposition: vocabulary::semantic_disposition(s.disposition),
        provider: vocabulary::provider_outcome(s.provider),
        observed: 1,
        requests_attempted: u64::from(s.request_attempted),
        cache_hits: u64::from(s.cache_hit),
        input_tokens: s
            .request_input_tokens
            .filter(|_| s.request_attempted && !s.cache_hit)
            .map(measured),
        output_tokens: s
            .request_output_tokens
            .filter(|_| s.request_attempted && !s.cache_hit)
            .map(measured),
        provider_latency: s
            .provider_duration
            .filter(|_| s.request_attempted)
            .map(histogram),
    });
    summary
}

fn measured(total: u64) -> wire::MeasuredTotal {
    wire::MeasuredTotal { samples: 1, total }
}

fn histogram(duration: Duration) -> [u64; 14] {
    let mut bins = [0; 14];
    bins[wire::native_duration_index(duration)] = 1;
    bins
}

fn agreed_missingness(streams: &[Option<native::StreamFacts>; 2]) -> Option<native::Missingness> {
    let mut values = streams.iter().flatten().map(|s| s.missing);
    let first = values.next()?;
    values.all(|v| v == first).then_some(first).flatten()
}

fn presented(
    streams: &[Option<native::StreamFacts>; 2],
    pair: impl Fn(&native::StreamFacts) -> Option<(u64, u64)>,
) -> Option<wire::PresentedTotals> {
    let mut seen = false;
    let mut input = 0u64;
    let mut output = 0u64;
    for stream in streams.iter().flatten() {
        if !stream.input_complete || !stream.output_complete {
            continue;
        }
        let Some((before, after)) = pair(stream) else {
            continue;
        };
        input = input.checked_add(before)?;
        output = output.checked_add(after)?;
        seen = true;
    }
    seen.then_some(wire::PresentedTotals {
        samples: 1,
        input,
        output,
    })
}
