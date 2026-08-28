use std::time::{Duration, Instant};

use ctx_history_capture::{ProviderImportSummary, ProviderImportWorkResult};
use ctx_history_core::CaptureProvider;
use ctx_history_ingest_application::ImportTotals;
use ctx_history_refresh::{
    RefreshOutcomeCode, RefreshTerminalFailureScope, RefreshTerminalFailureType,
};

use crate::analytics::{
    duration_bucket, DurationBucket, ForegroundProviderRefreshV1, Outcome, ProviderCoreResult,
    ProviderRefreshChange, ProviderRefreshCompletedV1, ProviderRefreshCountsV1,
    ProviderRefreshFailureCode, ProviderRefreshFailureScope, ProviderRefreshFailureType,
    ProviderRefreshResult, ProviderRefreshTrigger, PublicEventV1,
};

use super::SourceStats;

#[derive(Debug, Default)]
pub(crate) struct ProviderRefreshCollector {
    aggregates: Vec<ProviderRefreshAggregate>,
    core_refresh: Option<CoreRefreshAnalyticsFacts>,
    refresh_started: Option<Instant>,
    refresh_duration: Duration,
}

#[derive(Debug, Clone, Copy)]
enum CoreRefreshAnalyticsFacts {
    Published {
        trigger: ProviderRefreshTrigger,
        generation_changed: bool,
        source_failure_total: usize,
        rejected_record_total: u64,
    },
    Failed {
        trigger: ProviderRefreshTrigger,
        failure_scope: ProviderRefreshFailureScope,
        failure_type: ProviderRefreshFailureType,
        failure_code: ProviderRefreshFailureCode,
        retryable: bool,
        work_remaining: bool,
    },
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ProviderRefreshRuntimeFacts {
    duration: Duration,
    core_result: ProviderCoreResult,
}

impl ProviderRefreshRuntimeFacts {
    pub(crate) fn observed_success(duration: Duration, summary: &ProviderImportSummary) -> Self {
        let no_op = summary.work_result() == ProviderImportWorkResult::NoOp;
        Self::success(
            duration,
            if no_op {
                ProviderCoreResult::NoOp
            } else {
                ProviderCoreResult::Complete
            },
        )
    }

    pub(crate) fn success(duration: Duration, core_result: ProviderCoreResult) -> Self {
        Self {
            duration,
            core_result,
        }
    }
}

#[derive(Debug)]
struct ProviderRefreshAggregate {
    provider: CaptureProvider,
    trigger: ProviderRefreshTrigger,
    totals: ImportTotals,
    work_result: ProviderImportWorkResult,
    duration: Duration,
    duration_complete: bool,
    core_result: Option<ProviderCoreResult>,
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
        summary: &ProviderImportSummary,
        stats: &SourceStats,
        facts: ProviderRefreshRuntimeFacts,
    ) {
        let aggregate = self.aggregate_mut(provider, trigger);
        aggregate.totals.add(summary, stats);
        aggregate.work_result = aggregate.work_result.merge(summary.work_result());
        aggregate.record_facts(facts);
        if summary.failed > 0 {
            aggregate.failure_scope =
                merge_failure_scope(aggregate.failure_scope, ProviderRefreshFailureScope::Record);
            aggregate.failure_type = merge_failure_type(
                aggregate.failure_type,
                ProviderRefreshFailureType::RecordRejection,
            );
        }
    }

    pub(crate) fn record_core_publication(
        &mut self,
        trigger: ProviderRefreshTrigger,
        generation_changed: bool,
        source_failure_total: usize,
        rejected_record_total: u64,
    ) {
        self.core_refresh = Some(CoreRefreshAnalyticsFacts::Published {
            trigger,
            generation_changed,
            source_failure_total,
            rejected_record_total,
        });
    }

    pub(crate) fn record_terminal_core_failure(
        &mut self,
        trigger: ProviderRefreshTrigger,
        code: Option<RefreshOutcomeCode>,
        work_remaining: bool,
    ) {
        // A terminal Core failure owns the command's one aggregate event. No
        // partial foreground facts can be completed authoritatively afterward.
        self.aggregates.clear();
        let (failure_scope, failure_type) = code
            .and_then(RefreshOutcomeCode::terminal_failure_classification)
            .map(|(scope, failure_type)| {
                (
                    match scope {
                        RefreshTerminalFailureScope::Source => ProviderRefreshFailureScope::Source,
                        RefreshTerminalFailureScope::System => ProviderRefreshFailureScope::System,
                        RefreshTerminalFailureScope::Unknown => {
                            ProviderRefreshFailureScope::Unknown
                        }
                    },
                    match failure_type {
                        RefreshTerminalFailureType::UnsupportedSchema => {
                            ProviderRefreshFailureType::UnsupportedSchema
                        }
                        RefreshTerminalFailureType::MalformedSource => {
                            ProviderRefreshFailureType::MalformedSource
                        }
                        RefreshTerminalFailureType::System => ProviderRefreshFailureType::System,
                        RefreshTerminalFailureType::Unknown => ProviderRefreshFailureType::Unknown,
                    },
                )
            })
            .unwrap_or((
                ProviderRefreshFailureScope::Unknown,
                ProviderRefreshFailureType::Unknown,
            ));
        self.core_refresh = Some(CoreRefreshAnalyticsFacts::Failed {
            trigger,
            failure_scope,
            failure_type,
            failure_code: code
                .map(provider_refresh_failure_code)
                .unwrap_or(ProviderRefreshFailureCode::Unknown),
            retryable: work_remaining,
            work_remaining,
        });
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
                .then_with(|| left.trigger.as_str().cmp(right.trigger.as_str()))
        });
        let fallback_duration =
            (self.aggregates.len() == 1).then_some(single_provider_fallback_duration);
        let mut events = self
            .aggregates
            .into_iter()
            .map(|aggregate| {
                let totals = aggregate.totals;
                let core_result = aggregate.core_result.unwrap_or(ProviderCoreResult::Unknown);
                let refresh_result = refresh_result(&totals, core_result);
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
                        provider: Some(aggregate.provider),
                        trigger: aggregate.trigger,
                        change: if aggregate.work_result == ProviderImportWorkResult::Changed {
                            ProviderRefreshChange::Changed
                        } else {
                            ProviderRefreshChange::NoOp
                        },
                        refresh_result,
                        core_result,
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
                        failure_code: ProviderRefreshFailureCode::None,
                        retryable: false,
                        work_remaining: totals.capture_work_remaining,
                        counts: Some(ProviderRefreshCountsV1::new(
                            count_u64(totals.imported_events),
                            totals.source_bytes,
                        )),
                    },
                );
                event.surface = surface;
                PublicEventV1::ProviderRefreshCompleted(event)
            })
            .collect::<Vec<_>>();
        if let Some(facts) = self.core_refresh {
            match facts {
                CoreRefreshAnalyticsFacts::Failed {
                    trigger,
                    failure_scope,
                    failure_type,
                    failure_code,
                    retryable,
                    work_remaining,
                } => {
                    let mut event = ProviderRefreshCompletedV1::foreground_bucketed(
                        Outcome::Failure,
                        duration_bucket(single_provider_fallback_duration),
                        ForegroundProviderRefreshV1 {
                            provider: None,
                            trigger,
                            change: ProviderRefreshChange::NoOp,
                            refresh_result: ProviderRefreshResult::Failure,
                            core_result: ProviderCoreResult::Failure,
                            failure_scope,
                            failure_type,
                            failure_code,
                            retryable,
                            work_remaining,
                            counts: None,
                        },
                    );
                    event.surface = surface;
                    events.push(PublicEventV1::ProviderRefreshCompleted(event));
                }
                CoreRefreshAnalyticsFacts::Published {
                    trigger,
                    generation_changed,
                    source_failure_total,
                    rejected_record_total,
                } => {
                    let has_source_failures = source_failure_total != 0;
                    let has_rejections = rejected_record_total != 0;
                    let mut event = ProviderRefreshCompletedV1::foreground_bucketed(
                        Outcome::Success,
                        duration_bucket(single_provider_fallback_duration),
                        ForegroundProviderRefreshV1 {
                            provider: None,
                            trigger,
                            change: if generation_changed {
                                ProviderRefreshChange::Changed
                            } else {
                                ProviderRefreshChange::NoOp
                            },
                            refresh_result: if has_source_failures {
                                ProviderRefreshResult::Partial
                            } else {
                                ProviderRefreshResult::Complete
                            },
                            core_result: if generation_changed {
                                ProviderCoreResult::Complete
                            } else {
                                ProviderCoreResult::NoOp
                            },
                            failure_scope: match (has_source_failures, has_rejections) {
                                (false, false) => ProviderRefreshFailureScope::None,
                                (false, true) => ProviderRefreshFailureScope::Record,
                                (true, false) => ProviderRefreshFailureScope::Unknown,
                                (true, true) => ProviderRefreshFailureScope::Mixed,
                            },
                            failure_type: match (has_source_failures, has_rejections) {
                                (false, false) => ProviderRefreshFailureType::None,
                                (false, true) => ProviderRefreshFailureType::RecordRejection,
                                (true, false) => ProviderRefreshFailureType::Unknown,
                                (true, true) => ProviderRefreshFailureType::Mixed,
                            },
                            failure_code: ProviderRefreshFailureCode::None,
                            retryable: false,
                            work_remaining: false,
                            counts: None,
                        },
                    );
                    event.surface = surface;
                    events.push(PublicEventV1::ProviderRefreshCompleted(event));
                }
            }
        }
        events
    }

    fn aggregate_mut(
        &mut self,
        provider: CaptureProvider,
        trigger: ProviderRefreshTrigger,
    ) -> &mut ProviderRefreshAggregate {
        if let Some(index) = self
            .aggregates
            .iter()
            .position(|aggregate| aggregate.provider == provider && aggregate.trigger == trigger)
        {
            return &mut self.aggregates[index];
        }
        self.aggregates.push(ProviderRefreshAggregate {
            provider,
            trigger,
            totals: ImportTotals::default(),
            work_result: ProviderImportWorkResult::NoOp,
            duration: Duration::ZERO,
            duration_complete: true,
            core_result: None,
            failure_scope: None,
            failure_type: None,
        });
        self.aggregates
            .last_mut()
            .expect("a provider refresh aggregate was just inserted")
    }
}

fn provider_refresh_failure_code(code: RefreshOutcomeCode) -> ProviderRefreshFailureCode {
    match code {
        RefreshOutcomeCode::SourceUnavailable => ProviderRefreshFailureCode::SourceUnavailable,
        RefreshOutcomeCode::ExplicitSourcePathMissing => {
            ProviderRefreshFailureCode::ExplicitSourcePathMissing
        }
        RefreshOutcomeCode::SourceChanged => ProviderRefreshFailureCode::SourceChanged,
        RefreshOutcomeCode::MalformedSource => ProviderRefreshFailureCode::MalformedSource,
        RefreshOutcomeCode::UnsupportedSchema => ProviderRefreshFailureCode::UnsupportedSchema,
        RefreshOutcomeCode::SourceFailures => ProviderRefreshFailureCode::SourceFailures,
        RefreshOutcomeCode::LogicalSourceFailures => {
            ProviderRefreshFailureCode::LogicalSourceFailures
        }
        RefreshOutcomeCode::SourceUnclaimed => ProviderRefreshFailureCode::SourceUnclaimed,
        RefreshOutcomeCode::SourceRefreshFailed => ProviderRefreshFailureCode::SourceRefreshFailed,
        RefreshOutcomeCode::SourceRefreshInternal => {
            ProviderRefreshFailureCode::SourceRefreshInternal
        }
        RefreshOutcomeCode::ResourceUnavailable => ProviderRefreshFailureCode::ResourceUnavailable,
        RefreshOutcomeCode::IndexIncompatible => ProviderRefreshFailureCode::IndexIncompatible,
        RefreshOutcomeCode::IndexCorruption => ProviderRefreshFailureCode::IndexCorruption,
        RefreshOutcomeCode::SourceRefreshAdmissionFailed => {
            ProviderRefreshFailureCode::SourceRefreshAdmissionFailed
        }
        RefreshOutcomeCode::AllProviderTerminalCoverageUnavailable => {
            ProviderRefreshFailureCode::AllProviderTerminalCoverageUnavailable
        }
        RefreshOutcomeCode::Completed
        | RefreshOutcomeCode::CompletedWithRejections
        | RefreshOutcomeCode::CompletedWithSourceFailures
        | RefreshOutcomeCode::CompletedWithRejectionsAndSourceFailures => {
            ProviderRefreshFailureCode::None
        }
    }
}

impl ProviderRefreshAggregate {
    fn record_facts(&mut self, facts: ProviderRefreshRuntimeFacts) {
        self.duration = self.duration.saturating_add(facts.duration);
        self.core_result = merge_core_result(self.core_result, facts.core_result);
    }
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

fn refresh_result(totals: &ImportTotals, core_result: ProviderCoreResult) -> ProviderRefreshResult {
    if totals.outcome().0 == ctx_history_ingest_application::ImportOutcome::Failure {
        ProviderRefreshResult::Failure
    } else if totals.failed_sources > 0
        || matches!(
            core_result,
            ProviderCoreResult::Partial | ProviderCoreResult::Failure
        )
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
