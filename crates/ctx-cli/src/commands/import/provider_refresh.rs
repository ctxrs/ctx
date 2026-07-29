use std::time::{Duration, Instant};

use ctx_history_capture::{ProviderImportSummary, ProviderImportWorkResult};
use ctx_history_core::CaptureProvider;

use crate::analytics::{
    count_bucket, duration_bucket, DurationBucket, ForegroundProviderRefreshV1, Outcome,
    ProviderCoreResult, ProviderProResult, ProviderRefreshChange, ProviderRefreshCompletedV1,
    ProviderRefreshContentEvidence, ProviderRefreshCountsV1, ProviderRefreshFailureScope,
    ProviderRefreshFailureType, ProviderRefreshResult, ProviderRefreshSourceMode,
    ProviderRefreshTrigger, ProviderRefreshWorkKind, PublicEventV1,
};

use super::{ImportTotals, SourceStats};

#[derive(Debug, Default)]
pub(crate) struct ProviderRefreshCollector {
    aggregates: Vec<ProviderRefreshAggregate>,
    refresh_started: Option<Instant>,
    refresh_duration: Duration,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ProviderRefreshRuntimeFacts {
    duration: Duration,
    work_kind: Option<ProviderRefreshWorkKind>,
    core_result: ProviderCoreResult,
    canonical_pro_result: ProviderProResult,
    output_pro_result: ProviderProResult,
    retired_records: Option<u64>,
}

impl ProviderRefreshRuntimeFacts {
    pub(crate) fn observed_success(duration: Duration, summary: &ProviderImportSummary) -> Self {
        let no_op = summary.work_result() == ProviderImportWorkResult::NoOp;
        Self::success(
            duration,
            no_op.then_some(ProviderRefreshWorkKind::NoOp),
            if no_op {
                ProviderCoreResult::NoOp
            } else {
                ProviderCoreResult::Complete
            },
            ProviderProResult::Unknown,
            ProviderProResult::Unknown,
            no_op.then_some(0),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn success(
        duration: Duration,
        work_kind: Option<ProviderRefreshWorkKind>,
        core_result: ProviderCoreResult,
        canonical_pro_result: ProviderProResult,
        output_pro_result: ProviderProResult,
        retired_records: Option<u64>,
    ) -> Self {
        Self {
            duration,
            work_kind,
            core_result,
            canonical_pro_result,
            output_pro_result,
            retired_records,
        }
    }
}

#[derive(Debug)]
struct ProviderRefreshAggregate {
    provider: CaptureProvider,
    trigger: ProviderRefreshTrigger,
    source_mode: ProviderRefreshSourceMode,
    totals: ImportTotals,
    work_result: ProviderImportWorkResult,
    duration: Duration,
    duration_complete: bool,
    work_kind: Option<ProviderRefreshWorkKind>,
    work_kind_complete: bool,
    content_evidence: Option<ProviderRefreshContentEvidence>,
    core_result: Option<ProviderCoreResult>,
    canonical_pro_result: Option<ProviderProResult>,
    output_pro_result: Option<ProviderProResult>,
    retired_records: u64,
    retired_records_complete: bool,
    failure_scope: Option<ProviderRefreshFailureScope>,
    failure_type: Option<ProviderRefreshFailureType>,
}

impl ProviderRefreshCollector {
    pub(crate) fn start_timing(&mut self) {
        if self.refresh_started.is_none() {
            self.refresh_started = Some(Instant::now());
        }
    }

    pub(crate) fn stop_timing(&mut self) {
        if let Some(started) = self.refresh_started.take() {
            self.refresh_duration = self.refresh_duration.saturating_add(started.elapsed());
        }
    }

    pub(crate) fn record_success_with_facts(
        &mut self,
        provider: CaptureProvider,
        trigger: ProviderRefreshTrigger,
        source_mode: ProviderRefreshSourceMode,
        summary: &ProviderImportSummary,
        stats: &SourceStats,
        facts: ProviderRefreshRuntimeFacts,
    ) {
        let aggregate = self.aggregate_mut(provider, trigger, source_mode);
        aggregate.totals.add(summary, stats);
        aggregate.work_result = aggregate.work_result.merge(summary.work_result());
        aggregate.record_facts(facts);
        aggregate.content_evidence =
            merge_content_evidence(aggregate.content_evidence, content_evidence(summary));
        if summary.failed > 0 {
            aggregate.failure_scope =
                merge_failure_scope(aggregate.failure_scope, ProviderRefreshFailureScope::Record);
            aggregate.failure_type = merge_failure_type(
                aggregate.failure_type,
                ProviderRefreshFailureType::RecordRejection,
            );
        }
    }

    pub(crate) fn finish(mut self) -> Vec<PublicEventV1> {
        self.stop_timing();
        let duration = self.refresh_duration;
        self.finish_for_surface(crate::analytics::Surface::Cli, duration)
    }

    fn finish_for_surface(
        mut self,
        surface: crate::analytics::Surface,
        single_provider_fallback_duration: Duration,
    ) -> Vec<PublicEventV1> {
        self.aggregates.sort_by(|left, right| {
            left.provider
                .as_str()
                .cmp(right.provider.as_str())
                .then_with(|| left.source_mode.as_str().cmp(right.source_mode.as_str()))
                .then_with(|| left.trigger.as_str().cmp(right.trigger.as_str()))
        });
        let fallback_duration =
            (self.aggregates.len() == 1).then_some(single_provider_fallback_duration);
        self.aggregates
            .into_iter()
            .map(|aggregate| {
                let totals = aggregate.totals;
                let source_count = totals
                    .imported_sources
                    .saturating_add(totals.failed_sources);
                let core_result = aggregate.core_result.unwrap_or(ProviderCoreResult::Unknown);
                let canonical_pro_result = aggregate
                    .canonical_pro_result
                    .unwrap_or(ProviderProResult::Unknown);
                let output_pro_result = aggregate
                    .output_pro_result
                    .unwrap_or(ProviderProResult::Unknown);
                let refresh_result = refresh_result(
                    &totals,
                    core_result,
                    canonical_pro_result,
                    output_pro_result,
                );
                let outcome = if refresh_result == ProviderRefreshResult::Failure {
                    Outcome::Failure
                } else {
                    Outcome::Success
                };
                let duration = if aggregate.duration_complete {
                    duration_bucket(aggregate.duration)
                } else {
                    fallback_duration
                        .map(duration_bucket)
                        .unwrap_or(DurationBucket::Unknown)
                };
                let has_failures = totals.failed > 0 || totals.failed_sources > 0;
                let mut event = ProviderRefreshCompletedV1::foreground_bucketed(
                    outcome,
                    duration,
                    ForegroundProviderRefreshV1 {
                        provider: aggregate.provider,
                        trigger: aggregate.trigger,
                        source_mode: aggregate.source_mode,
                        change: if aggregate.work_result == ProviderImportWorkResult::Changed {
                            ProviderRefreshChange::Changed
                        } else {
                            ProviderRefreshChange::NoOp
                        },
                        content_evidence: aggregate
                            .content_evidence
                            .unwrap_or(ProviderRefreshContentEvidence::Unknown),
                        work_kind: aggregate
                            .work_kind_complete
                            .then_some(aggregate.work_kind)
                            .flatten(),
                        refresh_result,
                        core_result,
                        canonical_pro_result,
                        output_pro_result,
                        failure_scope: if has_failures {
                            aggregate
                                .failure_scope
                                .unwrap_or(ProviderRefreshFailureScope::Unknown)
                        } else {
                            ProviderRefreshFailureScope::None
                        },
                        failure_type: if has_failures {
                            aggregate
                                .failure_type
                                .unwrap_or(ProviderRefreshFailureType::Unknown)
                        } else {
                            ProviderRefreshFailureType::None
                        },
                        work_remaining: totals.capture_work_remaining,
                        retired_records: aggregate
                            .retired_records_complete
                            .then(|| count_bucket(aggregate.retired_records)),
                        counts: ProviderRefreshCountsV1::new(
                            count_u64(source_count),
                            count_u64(totals.source_files),
                            count_u64(totals.imported_sessions),
                            count_u64(totals.imported_events),
                            count_u64(totals.imported_edges),
                            count_u64(totals.skipped),
                            count_u64(totals.failed),
                            count_u64(totals.failed_sources),
                            totals.source_bytes,
                        ),
                        performance: None,
                    },
                );
                event.surface = surface;
                PublicEventV1::ProviderRefreshCompleted(event)
            })
            .collect()
    }

    fn aggregate_mut(
        &mut self,
        provider: CaptureProvider,
        trigger: ProviderRefreshTrigger,
        source_mode: ProviderRefreshSourceMode,
    ) -> &mut ProviderRefreshAggregate {
        if let Some(index) = self.aggregates.iter().position(|aggregate| {
            aggregate.provider == provider
                && aggregate.trigger == trigger
                && aggregate.source_mode == source_mode
        }) {
            return &mut self.aggregates[index];
        }
        self.aggregates.push(ProviderRefreshAggregate {
            provider,
            trigger,
            source_mode,
            totals: ImportTotals::default(),
            work_result: ProviderImportWorkResult::NoOp,
            duration: Duration::ZERO,
            duration_complete: true,
            work_kind: None,
            work_kind_complete: true,
            content_evidence: None,
            core_result: None,
            canonical_pro_result: None,
            output_pro_result: None,
            retired_records: 0,
            retired_records_complete: true,
            failure_scope: None,
            failure_type: None,
        });
        self.aggregates
            .last_mut()
            .expect("a provider refresh aggregate was just inserted")
    }
}

impl ProviderRefreshAggregate {
    fn record_facts(&mut self, facts: ProviderRefreshRuntimeFacts) {
        self.duration = self.duration.saturating_add(facts.duration);
        if let Some(work_kind) = facts.work_kind {
            self.work_kind = merge_work_kind(self.work_kind, work_kind);
        } else {
            self.work_kind_complete = false;
        }
        self.core_result = merge_core_result(self.core_result, facts.core_result);
        self.canonical_pro_result =
            merge_pro_result(self.canonical_pro_result, facts.canonical_pro_result);
        self.output_pro_result = merge_pro_result(self.output_pro_result, facts.output_pro_result);
        if let Some(retired_records) = facts.retired_records {
            self.retired_records = self.retired_records.saturating_add(retired_records);
        } else {
            self.retired_records_complete = false;
        }
    }
}

fn content_evidence(summary: &ProviderImportSummary) -> ProviderRefreshContentEvidence {
    if summary.has_accepted_content() {
        ProviderRefreshContentEvidence::Accepted
    } else {
        ProviderRefreshContentEvidence::None
    }
}

fn merge_content_evidence(
    current: Option<ProviderRefreshContentEvidence>,
    next: ProviderRefreshContentEvidence,
) -> Option<ProviderRefreshContentEvidence> {
    if current == Some(ProviderRefreshContentEvidence::Unknown)
        || next == ProviderRefreshContentEvidence::Unknown
    {
        return Some(ProviderRefreshContentEvidence::Unknown);
    }
    Some(match current {
        None => next,
        Some(current) if current == next => current,
        Some(_) => ProviderRefreshContentEvidence::Mixed,
    })
}

fn merge_work_kind(
    current: Option<ProviderRefreshWorkKind>,
    next: ProviderRefreshWorkKind,
) -> Option<ProviderRefreshWorkKind> {
    Some(match current {
        None => next,
        Some(current) if current == next => current,
        Some(_) => ProviderRefreshWorkKind::Mixed,
    })
}

fn merge_core_result(
    current: Option<ProviderCoreResult>,
    next: ProviderCoreResult,
) -> Option<ProviderCoreResult> {
    let Some(current) = current else {
        return Some(next);
    };
    if current == next {
        return Some(current);
    }
    if current == ProviderCoreResult::Unknown || next == ProviderCoreResult::Unknown {
        return Some(ProviderCoreResult::Unknown);
    }
    if current == ProviderCoreResult::Partial || next == ProviderCoreResult::Partial {
        return Some(ProviderCoreResult::Partial);
    }
    if current == ProviderCoreResult::Failure || next == ProviderCoreResult::Failure {
        return Some(ProviderCoreResult::Partial);
    }
    Some(ProviderCoreResult::Complete)
}

fn merge_pro_result(
    current: Option<ProviderProResult>,
    next: ProviderProResult,
) -> Option<ProviderProResult> {
    let Some(current) = current else {
        return Some(next);
    };
    if current == next {
        return Some(current);
    }
    if current == ProviderProResult::Unknown || next == ProviderProResult::Unknown {
        return Some(ProviderProResult::Unknown);
    }
    if current == ProviderProResult::Partial || next == ProviderProResult::Partial {
        return Some(ProviderProResult::Partial);
    }
    if current == ProviderProResult::Failure || next == ProviderProResult::Failure {
        return Some(ProviderProResult::Partial);
    }
    if current == ProviderProResult::Behind || next == ProviderProResult::Behind {
        return Some(ProviderProResult::Behind);
    }
    match (current, next) {
        (ProviderProResult::NoOp, ProviderProResult::Complete)
        | (ProviderProResult::Complete, ProviderProResult::NoOp)
        | (ProviderProResult::NotRequested, ProviderProResult::Complete)
        | (ProviderProResult::Complete, ProviderProResult::NotRequested) => {
            Some(ProviderProResult::Complete)
        }
        (ProviderProResult::Unavailable, ProviderProResult::Complete)
        | (ProviderProResult::Complete, ProviderProResult::Unavailable)
        | (ProviderProResult::Unavailable, ProviderProResult::NoOp)
        | (ProviderProResult::NoOp, ProviderProResult::Unavailable) => {
            Some(ProviderProResult::Partial)
        }
        _ => Some(ProviderProResult::Unknown),
    }
}

fn merge_failure_scope(
    current: Option<ProviderRefreshFailureScope>,
    next: ProviderRefreshFailureScope,
) -> Option<ProviderRefreshFailureScope> {
    if next == ProviderRefreshFailureScope::None {
        return current;
    }
    if current == Some(ProviderRefreshFailureScope::Unknown)
        || next == ProviderRefreshFailureScope::Unknown
    {
        return Some(ProviderRefreshFailureScope::Unknown);
    }
    Some(match current {
        None => next,
        Some(current) if current == next => current,
        Some(_) => ProviderRefreshFailureScope::Mixed,
    })
}

fn merge_failure_type(
    current: Option<ProviderRefreshFailureType>,
    next: ProviderRefreshFailureType,
) -> Option<ProviderRefreshFailureType> {
    if next == ProviderRefreshFailureType::None {
        return current;
    }
    if current == Some(ProviderRefreshFailureType::Unknown)
        || next == ProviderRefreshFailureType::Unknown
    {
        return Some(ProviderRefreshFailureType::Unknown);
    }
    Some(match current {
        None => next,
        Some(current) if current == next => current,
        Some(_) => ProviderRefreshFailureType::Mixed,
    })
}

fn refresh_result(
    totals: &ImportTotals,
    core_result: ProviderCoreResult,
    canonical_pro_result: ProviderProResult,
    output_pro_result: ProviderProResult,
) -> ProviderRefreshResult {
    if totals.imported_sources == 0 && totals.failed_sources > 0 {
        ProviderRefreshResult::Failure
    } else if totals.failed_sources > 0
        || totals.failed > 0
        || matches!(
            core_result,
            ProviderCoreResult::Partial | ProviderCoreResult::Failure
        )
        || [canonical_pro_result, output_pro_result]
            .into_iter()
            .any(|result| {
                matches!(
                    result,
                    ProviderProResult::Partial
                        | ProviderProResult::Behind
                        | ProviderProResult::Failure
                )
            })
    {
        ProviderRefreshResult::Partial
    } else {
        ProviderRefreshResult::Complete
    }
}

fn count_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests;
