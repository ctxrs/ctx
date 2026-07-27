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
use crate::provider_sources::SourceInfo;

use super::{ImportFailureScope, ImportFailureType, ImportTotals, SourceStats};

#[derive(Debug)]
pub(crate) struct ImportSourceOutcome {
    pub(crate) index: usize,
    pub(crate) source: SourceInfo,
    pub(crate) stats: SourceStats,
    pub(crate) summary: ProviderImportSummary,
    pub(crate) runtime_facts: Option<ProviderRefreshRuntimeFacts>,
}

#[derive(Debug)]
pub(crate) struct ImportSourceFailure {
    pub(crate) index: usize,
    pub(crate) source: SourceInfo,
    pub(crate) stats: SourceStats,
    pub(crate) error: String,
    pub(crate) failure_scope: ImportFailureScope,
    pub(crate) failure_type: ImportFailureType,
    pub(crate) rejected_summary: Option<ProviderImportSummary>,
    pub(crate) system_error: Option<anyhow::Error>,
    pub(crate) runtime_facts: Option<ProviderRefreshRuntimeFacts>,
}

#[derive(Debug)]
pub(crate) enum ImportSourceRun {
    Imported(ImportSourceOutcome),
    Failed(ImportSourceFailure),
}

impl ImportSourceRun {
    pub(crate) fn index(&self) -> usize {
        match self {
            Self::Imported(outcome) => outcome.index,
            Self::Failed(failure) => failure.index,
        }
    }
}

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
    failure_scope: ProviderRefreshFailureScope,
    failure_type: ProviderRefreshFailureType,
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

    pub(crate) fn observed_failure(
        duration: Duration,
        failure_scope: ImportFailureScope,
        failure_type: ImportFailureType,
    ) -> Self {
        Self::failure(
            duration,
            None,
            ProviderCoreResult::Unknown,
            ProviderProResult::Unknown,
            ProviderProResult::Unknown,
            None,
            failure_scope,
            failure_type,
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
            failure_scope: ProviderRefreshFailureScope::None,
            failure_type: ProviderRefreshFailureType::None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn failure(
        duration: Duration,
        work_kind: Option<ProviderRefreshWorkKind>,
        core_result: ProviderCoreResult,
        canonical_pro_result: ProviderProResult,
        output_pro_result: ProviderProResult,
        retired_records: Option<u64>,
        failure_scope: ImportFailureScope,
        failure_type: ImportFailureType,
    ) -> Self {
        Self {
            duration,
            work_kind,
            core_result,
            canonical_pro_result,
            output_pro_result,
            retired_records,
            failure_scope: map_failure_scope(failure_scope),
            failure_type: map_failure_type(failure_type),
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
    pub(crate) fn record_import_outcome(
        &mut self,
        trigger: ProviderRefreshTrigger,
        source_mode: ProviderRefreshSourceMode,
        outcome: &ImportSourceOutcome,
    ) {
        if let Some(facts) = outcome.runtime_facts {
            self.record_success_with_facts(
                outcome.source.provider,
                trigger,
                source_mode,
                &outcome.summary,
                &outcome.stats,
                facts,
            );
        } else {
            self.record_success(
                outcome.source.provider,
                trigger,
                source_mode,
                &outcome.summary,
                &outcome.stats,
            );
        }
    }

    pub(crate) fn record_import_failure(
        &mut self,
        trigger: ProviderRefreshTrigger,
        source_mode: ProviderRefreshSourceMode,
        failure: &ImportSourceFailure,
    ) {
        if let Some(facts) = failure.runtime_facts {
            self.record_failure_with_facts(
                failure.source.provider,
                trigger,
                source_mode,
                &failure.stats,
                failure.rejected_summary.as_ref(),
                facts,
            );
        } else {
            self.record_failure(
                failure.source.provider,
                trigger,
                source_mode,
                &failure.stats,
                failure.rejected_summary.as_ref(),
            );
        }
    }

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

    pub(crate) fn record_success(
        &mut self,
        provider: CaptureProvider,
        trigger: ProviderRefreshTrigger,
        source_mode: ProviderRefreshSourceMode,
        summary: &ProviderImportSummary,
        stats: &SourceStats,
    ) {
        let aggregate = self.aggregate_mut(provider, trigger, source_mode);
        aggregate.totals.add(summary, stats);
        aggregate.work_result = aggregate.work_result.merge(summary.work_result());
        aggregate.duration_complete = false;
        aggregate.content_evidence =
            merge_content_evidence(aggregate.content_evidence, content_evidence(summary));
        aggregate.core_result = merge_core_result(
            aggregate.core_result,
            if summary.work_result() == ProviderImportWorkResult::NoOp {
                ProviderCoreResult::NoOp
            } else {
                ProviderCoreResult::Complete
            },
        );
        aggregate.canonical_pro_result =
            merge_pro_result(aggregate.canonical_pro_result, ProviderProResult::Unknown);
        aggregate.output_pro_result =
            merge_pro_result(aggregate.output_pro_result, ProviderProResult::Unknown);
        if summary.work_result() == ProviderImportWorkResult::NoOp {
            aggregate.work_kind =
                merge_work_kind(aggregate.work_kind, ProviderRefreshWorkKind::NoOp);
        } else {
            aggregate.work_kind_complete = false;
            aggregate.retired_records_complete = false;
        }
        if summary.failed > 0 {
            aggregate.failure_scope =
                merge_failure_scope(aggregate.failure_scope, ProviderRefreshFailureScope::Record);
            aggregate.failure_type = merge_failure_type(
                aggregate.failure_type,
                ProviderRefreshFailureType::RecordRejection,
            );
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

    pub(crate) fn record_failure(
        &mut self,
        provider: CaptureProvider,
        trigger: ProviderRefreshTrigger,
        source_mode: ProviderRefreshSourceMode,
        stats: &SourceStats,
        rejected_summary: Option<&ProviderImportSummary>,
    ) {
        let aggregate = self.aggregate_mut(provider, trigger, source_mode);
        if let Some(summary) = rejected_summary {
            aggregate.totals.add_rejected_source(summary, stats);
            aggregate.content_evidence =
                merge_content_evidence(aggregate.content_evidence, content_evidence(summary));
            aggregate.failure_scope =
                merge_failure_scope(aggregate.failure_scope, ProviderRefreshFailureScope::Source);
            aggregate.failure_type = merge_failure_type(
                aggregate.failure_type,
                ProviderRefreshFailureType::RecordRejection,
            );
        } else {
            aggregate.totals.add_source_failure(stats);
            aggregate.content_evidence = merge_content_evidence(
                aggregate.content_evidence,
                ProviderRefreshContentEvidence::Unknown,
            );
            aggregate.failure_scope = merge_failure_scope(
                aggregate.failure_scope,
                ProviderRefreshFailureScope::Unknown,
            );
            aggregate.failure_type =
                merge_failure_type(aggregate.failure_type, ProviderRefreshFailureType::Unknown);
        }
        aggregate.duration_complete = false;
        aggregate.work_kind_complete = false;
        aggregate.retired_records_complete = false;
        aggregate.core_result =
            merge_core_result(aggregate.core_result, ProviderCoreResult::Unknown);
        aggregate.canonical_pro_result =
            merge_pro_result(aggregate.canonical_pro_result, ProviderProResult::Unknown);
        aggregate.output_pro_result =
            merge_pro_result(aggregate.output_pro_result, ProviderProResult::Unknown);
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_failure_with_facts(
        &mut self,
        provider: CaptureProvider,
        trigger: ProviderRefreshTrigger,
        source_mode: ProviderRefreshSourceMode,
        stats: &SourceStats,
        rejected_summary: Option<&ProviderImportSummary>,
        facts: ProviderRefreshRuntimeFacts,
    ) {
        let aggregate = self.aggregate_mut(provider, trigger, source_mode);
        if let Some(summary) = rejected_summary {
            aggregate.totals.add_rejected_source(summary, stats);
            aggregate.content_evidence =
                merge_content_evidence(aggregate.content_evidence, content_evidence(summary));
        } else {
            aggregate.totals.add_source_failure(stats);
            aggregate.content_evidence = merge_content_evidence(
                aggregate.content_evidence,
                ProviderRefreshContentEvidence::Unknown,
            );
        }
        aggregate.record_facts(facts);
    }

    pub(crate) fn finish(mut self) -> Vec<PublicEventV1> {
        self.stop_timing();
        let duration = self.refresh_duration;
        self.finish_for_surface(crate::analytics::Surface::Cli, duration)
    }

    pub(crate) fn finish_for_daemon(mut self, duration: Duration) -> Vec<PublicEventV1> {
        for aggregate in &mut self.aggregates {
            aggregate.trigger = ProviderRefreshTrigger::Daemon;
        }
        self.finish_for_surface(crate::analytics::Surface::Daemon, duration)
    }

    pub(crate) fn finish_for_surface(
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
        self.failure_scope = merge_failure_scope(self.failure_scope, facts.failure_scope);
        self.failure_type = merge_failure_type(self.failure_type, facts.failure_type);
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

fn map_failure_scope(scope: ImportFailureScope) -> ProviderRefreshFailureScope {
    match scope {
        ImportFailureScope::Source => ProviderRefreshFailureScope::Source,
        ImportFailureScope::System => ProviderRefreshFailureScope::System,
    }
}

fn map_failure_type(failure_type: ImportFailureType) -> ProviderRefreshFailureType {
    match failure_type {
        ImportFailureType::RecordRejection => ProviderRefreshFailureType::RecordRejection,
        ImportFailureType::UnsupportedSchema => ProviderRefreshFailureType::UnsupportedSchema,
        ImportFailureType::NotFound => ProviderRefreshFailureType::NotFound,
        ImportFailureType::Permission => ProviderRefreshFailureType::Permission,
        ImportFailureType::SourceDatabase => ProviderRefreshFailureType::SourceDatabase,
        ImportFailureType::MalformedSource => ProviderRefreshFailureType::MalformedSource,
        ImportFailureType::Store => ProviderRefreshFailureType::Store,
        ImportFailureType::WorkerPanic => ProviderRefreshFailureType::WorkerPanic,
        ImportFailureType::SystemIo => ProviderRefreshFailureType::SystemIo,
        ImportFailureType::System => ProviderRefreshFailureType::System,
        ImportFailureType::Other => ProviderRefreshFailureType::Other,
    }
}

fn count_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use crate::analytics::{BytesBucket, CountBucket};

    use super::*;

    fn foreground(event: &PublicEventV1) -> &ForegroundProviderRefreshV1 {
        let PublicEventV1::ProviderRefreshCompleted(event) = event else {
            panic!("expected a provider refresh event");
        };
        event.foreground.as_ref().unwrap()
    }

    #[test]
    fn aggregates_many_source_and_record_results_once_per_provider() {
        let mut collector = ProviderRefreshCollector::default();
        let mut first = ProviderImportSummary::default();
        first.imported_sessions = 1;
        first.imported_events = 2;
        first.skipped = 40;
        let mut second = ProviderImportSummary::default();
        second.imported_sessions = 2;
        second.imported_events = 5;
        second.imported_edges = 1;
        second.work_remaining = true;
        collector.record_success(
            CaptureProvider::Codex,
            ProviderRefreshTrigger::Setup,
            ProviderRefreshSourceMode::Discovered,
            &first,
            &SourceStats {
                files: 20,
                bytes: 1024,
                ..SourceStats::default()
            },
        );
        collector.record_success(
            CaptureProvider::Codex,
            ProviderRefreshTrigger::Setup,
            ProviderRefreshSourceMode::Discovered,
            &second,
            &SourceStats {
                files: 30,
                bytes: 2048,
                ..SourceStats::default()
            },
        );

        collector.refresh_duration = Duration::from_secs(1);
        let events = collector.finish();

        assert_eq!(
            events.len(),
            1,
            "locations and records must not emit events"
        );
        let refresh = foreground(&events[0]);
        assert_eq!(refresh.provider, CaptureProvider::Codex);
        assert_eq!(refresh.change, ProviderRefreshChange::Changed);
        assert_eq!(refresh.refresh_result, ProviderRefreshResult::Complete);
        assert_eq!(refresh.core_result, ProviderCoreResult::Complete);
        assert_eq!(
            refresh.content_evidence,
            ProviderRefreshContentEvidence::Accepted
        );
        assert_eq!(refresh.work_kind, None);
        assert!(refresh.work_remaining);
        assert_eq!(refresh.counts.sources, CountBucket::TwoToFive);
        assert_eq!(
            refresh.counts.source_files,
            CountBucket::TwentyOneToOneHundred
        );
        assert_eq!(refresh.counts.sessions, CountBucket::TwoToFive);
        assert_eq!(refresh.counts.events, CountBucket::SixToTwenty);
        assert_eq!(refresh.counts.edges, CountBucket::One);
        assert_eq!(refresh.counts.skips, CountBucket::TwentyOneToOneHundred);
        assert_eq!(refresh.counts.bytes, BytesBucket::UnderOneHundredKb);
    }

    #[test]
    fn distinguishes_no_op_from_changed_and_buckets_rejections_and_failures() {
        let mut collector = ProviderRefreshCollector::default();
        let mut no_op = ProviderImportSummary::default();
        no_op.skipped = 3;
        collector.record_success(
            CaptureProvider::Codex,
            ProviderRefreshTrigger::Search,
            ProviderRefreshSourceMode::Discovered,
            &no_op,
            &SourceStats::default(),
        );
        let mut rejected = ProviderImportSummary::default();
        rejected.failed = 7;
        collector.record_failure(
            CaptureProvider::Custom,
            ProviderRefreshTrigger::Search,
            ProviderRefreshSourceMode::HistorySourcePlugin,
            &SourceStats {
                bytes: 12_000,
                ..SourceStats::default()
            },
            Some(&rejected),
        );

        let events = collector.finish();

        assert_eq!(events.len(), 2);
        let codex = foreground(&events[0]);
        assert_eq!(codex.provider, CaptureProvider::Codex);
        assert_eq!(codex.change, ProviderRefreshChange::NoOp);
        assert_eq!(codex.work_kind, Some(ProviderRefreshWorkKind::NoOp));
        assert_eq!(codex.refresh_result, ProviderRefreshResult::Complete);
        assert_eq!(codex.core_result, ProviderCoreResult::NoOp);
        assert_eq!(codex.counts.skips, CountBucket::TwoToFive);
        let custom = foreground(&events[1]);
        assert_eq!(custom.provider, CaptureProvider::Custom);
        assert_eq!(custom.change, ProviderRefreshChange::NoOp);
        assert_eq!(custom.counts.rejections, CountBucket::SixToTwenty);
        assert_eq!(custom.counts.failures, CountBucket::One);
        assert_eq!(custom.counts.bytes, BytesBucket::UnderOneHundredKb);
        assert_eq!(custom.refresh_result, ProviderRefreshResult::Failure);
        assert_eq!(custom.core_result, ProviderCoreResult::Unknown);
        assert_eq!(custom.failure_scope, ProviderRefreshFailureScope::Source);
        assert_eq!(
            custom.failure_type,
            ProviderRefreshFailureType::RecordRejection
        );
        let PublicEventV1::ProviderRefreshCompleted(custom_event) = &events[1] else {
            unreachable!();
        };
        assert_eq!(custom_event.outcome, Outcome::Failure);
        assert_eq!(custom_event.duration, DurationBucket::Unknown);
    }

    #[test]
    fn exact_provider_durations_are_independent_in_multi_provider_batches() {
        let mut collector = ProviderRefreshCollector::default();
        let mut codex_summary = ProviderImportSummary::default();
        codex_summary.imported_events = 1;
        collector.record_success_with_facts(
            CaptureProvider::Codex,
            ProviderRefreshTrigger::Import,
            ProviderRefreshSourceMode::Discovered,
            &codex_summary,
            &SourceStats::default(),
            ProviderRefreshRuntimeFacts::success(
                Duration::from_millis(40),
                Some(ProviderRefreshWorkKind::Fresh),
                ProviderCoreResult::Complete,
                ProviderProResult::Complete,
                ProviderProResult::NotRequested,
                Some(0),
            ),
        );
        let mut claude_summary = ProviderImportSummary::default();
        claude_summary.imported_events = 1;
        collector.record_success_with_facts(
            CaptureProvider::Claude,
            ProviderRefreshTrigger::Import,
            ProviderRefreshSourceMode::Discovered,
            &claude_summary,
            &SourceStats::default(),
            ProviderRefreshRuntimeFacts::success(
                Duration::from_secs(7),
                Some(ProviderRefreshWorkKind::Append),
                ProviderCoreResult::Complete,
                ProviderProResult::NoOp,
                ProviderProResult::Complete,
                Some(9),
            ),
        );

        collector.refresh_duration = Duration::from_secs(90);
        let events = collector.finish();
        let event_for = |provider| {
            events
                .iter()
                .find(|event| foreground(event).provider == provider)
                .unwrap()
        };
        let PublicEventV1::ProviderRefreshCompleted(codex) = event_for(CaptureProvider::Codex)
        else {
            unreachable!();
        };
        let PublicEventV1::ProviderRefreshCompleted(claude) = event_for(CaptureProvider::Claude)
        else {
            unreachable!();
        };

        assert_eq!(codex.duration, DurationBucket::UnderOneHundredMs);
        assert_eq!(claude.duration, DurationBucket::UnderThirtySeconds);
        assert_ne!(codex.duration, duration_bucket(Duration::from_secs(90)));
        assert_ne!(claude.duration, duration_bucket(Duration::from_secs(90)));
        assert_eq!(
            foreground(event_for(CaptureProvider::Codex)).work_kind,
            Some(ProviderRefreshWorkKind::Fresh)
        );
        assert_eq!(
            foreground(event_for(CaptureProvider::Claude)).retired_records,
            Some(CountBucket::SixToTwenty)
        );
    }

    #[test]
    fn observed_fact_seam_keeps_unavailable_dimensions_unknown() {
        let mut summary = ProviderImportSummary::default();
        summary.imported_events = 1;
        let success =
            ProviderRefreshRuntimeFacts::observed_success(Duration::from_secs(3), &summary);
        assert_eq!(success.duration, Duration::from_secs(3));
        assert_eq!(success.work_kind, None);
        assert_eq!(success.core_result, ProviderCoreResult::Complete);
        assert_eq!(success.canonical_pro_result, ProviderProResult::Unknown);
        assert_eq!(success.output_pro_result, ProviderProResult::Unknown);
        assert_eq!(success.retired_records, None);

        let failure = ProviderRefreshRuntimeFacts::observed_failure(
            Duration::from_secs(4),
            ImportFailureScope::System,
            ImportFailureType::WorkerPanic,
        );
        assert_eq!(failure.work_kind, None);
        assert_eq!(failure.core_result, ProviderCoreResult::Unknown);
        assert_eq!(failure.failure_scope, ProviderRefreshFailureScope::System);
        assert_eq!(
            failure.failure_type,
            ProviderRefreshFailureType::WorkerPanic
        );
    }

    #[test]
    fn detailed_facts_distinguish_partial_lane_and_failure_results() {
        let mut collector = ProviderRefreshCollector::default();
        let mut summary = ProviderImportSummary::default();
        summary.imported_events = 3;
        collector.record_success_with_facts(
            CaptureProvider::Codex,
            ProviderRefreshTrigger::Search,
            ProviderRefreshSourceMode::Discovered,
            &summary,
            &SourceStats::default(),
            ProviderRefreshRuntimeFacts::success(
                Duration::from_secs(2),
                Some(ProviderRefreshWorkKind::Append),
                ProviderCoreResult::Complete,
                ProviderProResult::Complete,
                ProviderProResult::Complete,
                Some(3),
            ),
        );
        collector.record_failure_with_facts(
            CaptureProvider::Codex,
            ProviderRefreshTrigger::Search,
            ProviderRefreshSourceMode::Discovered,
            &SourceStats::default(),
            None,
            ProviderRefreshRuntimeFacts::failure(
                Duration::from_secs(4),
                Some(ProviderRefreshWorkKind::Replace),
                ProviderCoreResult::Failure,
                ProviderProResult::Failure,
                ProviderProResult::Behind,
                Some(2),
                ImportFailureScope::System,
                ImportFailureType::Store,
            ),
        );

        let events = collector.finish();
        let refresh = foreground(&events[0]);
        let PublicEventV1::ProviderRefreshCompleted(event) = &events[0] else {
            unreachable!();
        };
        assert_eq!(event.outcome, Outcome::Success);
        assert_eq!(event.duration, DurationBucket::UnderThirtySeconds);
        assert_eq!(refresh.refresh_result, ProviderRefreshResult::Partial);
        assert_eq!(refresh.core_result, ProviderCoreResult::Partial);
        assert_eq!(refresh.work_kind, Some(ProviderRefreshWorkKind::Mixed));
        assert_eq!(refresh.canonical_pro_result, ProviderProResult::Partial);
        assert_eq!(refresh.output_pro_result, ProviderProResult::Behind);
        assert_eq!(refresh.failure_scope, ProviderRefreshFailureScope::System);
        assert_eq!(refresh.failure_type, ProviderRefreshFailureType::Store);
        assert_eq!(refresh.retired_records, Some(CountBucket::TwoToFive));
    }

    #[test]
    fn pro_lag_makes_refresh_partial_without_falsifying_core_result() {
        let mut collector = ProviderRefreshCollector::default();
        let mut summary = ProviderImportSummary::default();
        summary.imported_events = 1;
        collector.record_success_with_facts(
            CaptureProvider::Codex,
            ProviderRefreshTrigger::Search,
            ProviderRefreshSourceMode::Discovered,
            &summary,
            &SourceStats::default(),
            ProviderRefreshRuntimeFacts::success(
                Duration::from_secs(1),
                None,
                ProviderCoreResult::Complete,
                ProviderProResult::Complete,
                ProviderProResult::Behind,
                None,
            ),
        );

        let events = collector.finish();
        let refresh = foreground(&events[0]);
        let PublicEventV1::ProviderRefreshCompleted(event) = &events[0] else {
            unreachable!();
        };
        assert_eq!(event.outcome, Outcome::Success);
        assert_eq!(event.duration, DurationBucket::UnderFiveSeconds);
        assert_eq!(refresh.refresh_result, ProviderRefreshResult::Partial);
        assert_eq!(refresh.core_result, ProviderCoreResult::Complete);
        assert_eq!(refresh.output_pro_result, ProviderProResult::Behind);
        assert_eq!(refresh.work_kind, None);
        assert_eq!(refresh.retired_records, None);
    }

    #[test]
    fn every_capture_provider_emits_without_usage_suppression() {
        let providers = [
            CaptureProvider::Codex,
            CaptureProvider::Claude,
            CaptureProvider::Pi,
            CaptureProvider::OpenCode,
            CaptureProvider::Kilo,
            CaptureProvider::KiroCli,
            CaptureProvider::Antigravity,
            CaptureProvider::Gemini,
            CaptureProvider::Tabnine,
            CaptureProvider::Cursor,
            CaptureProvider::Windsurf,
            CaptureProvider::Zed,
            CaptureProvider::CopilotCli,
            CaptureProvider::FactoryAiDroid,
            CaptureProvider::QwenCode,
            CaptureProvider::KimiCodeCli,
            CaptureProvider::Auggie,
            CaptureProvider::Junie,
            CaptureProvider::Firebender,
            CaptureProvider::ForgeCode,
            CaptureProvider::DeepAgents,
            CaptureProvider::MistralVibe,
            CaptureProvider::Mux,
            CaptureProvider::RovoDev,
            CaptureProvider::OpenClaw,
            CaptureProvider::Hermes,
            CaptureProvider::NanoClaw,
            CaptureProvider::AstrBot,
            CaptureProvider::Shelley,
            CaptureProvider::Continue,
            CaptureProvider::OpenHands,
            CaptureProvider::Cline,
            CaptureProvider::RooCode,
            CaptureProvider::Crush,
            CaptureProvider::Goose,
            CaptureProvider::Lingma,
            CaptureProvider::Qoder,
            CaptureProvider::Warp,
            CaptureProvider::CodeBuddy,
            CaptureProvider::Trae,
            CaptureProvider::Shell,
            CaptureProvider::Git,
            CaptureProvider::Jj,
            CaptureProvider::Gh,
            CaptureProvider::Custom,
            CaptureProvider::Unknown,
            CaptureProvider::MiMoCode,
        ];
        let mut collector = ProviderRefreshCollector::default();
        for provider in providers {
            collector.record_success(
                provider,
                ProviderRefreshTrigger::Search,
                ProviderRefreshSourceMode::Discovered,
                &ProviderImportSummary::default(),
                &SourceStats::default(),
            );
        }

        let events = collector.finish();

        assert_eq!(events.len(), providers.len());
        for provider in providers {
            assert!(events
                .iter()
                .any(|event| foreground(event).provider == provider));
        }
    }

    #[test]
    fn shared_collector_marks_daemon_owned_refreshes_with_daemon_trigger() {
        let mut collector = ProviderRefreshCollector::default();
        let mut summary = ProviderImportSummary::default();
        summary.imported_sessions = 2;
        collector.record_success(
            CaptureProvider::Codex,
            ProviderRefreshTrigger::Search,
            ProviderRefreshSourceMode::Discovered,
            &summary,
            &SourceStats::default(),
        );

        let events = collector.finish_for_daemon(Duration::from_secs(1));
        let PublicEventV1::ProviderRefreshCompleted(event) = &events[0] else {
            panic!("expected provider refresh event");
        };
        assert_eq!(event.surface, crate::analytics::Surface::Daemon);
        assert_eq!(
            foreground(&events[0]).trigger,
            ProviderRefreshTrigger::Daemon
        );
        assert_eq!(
            foreground(&events[0]).counts.sessions,
            CountBucket::TwoToFive
        );
    }
}
