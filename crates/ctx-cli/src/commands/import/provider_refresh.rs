use std::time::{Duration, Instant};

use ctx_history_capture::ProviderImportSummary;
use ctx_history_core::CaptureProvider;

use crate::analytics::{
    ForegroundProviderRefreshV1, Outcome, ProviderRefreshChange, ProviderRefreshCompletedV1,
    ProviderRefreshCountsV1, ProviderRefreshSourceMode, ProviderRefreshTrigger, PublicEventV1,
};
use crate::provider_sources::SourceInfo;

use super::{ImportFailureScope, ImportFailureType, ImportTotals, SourceStats};

#[derive(Debug)]
pub(crate) struct ImportSourceOutcome {
    pub(crate) index: usize,
    pub(crate) source: SourceInfo,
    pub(crate) stats: SourceStats,
    pub(crate) summary: ProviderImportSummary,
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

#[derive(Debug)]
struct ProviderRefreshAggregate {
    provider: CaptureProvider,
    trigger: ProviderRefreshTrigger,
    source_mode: ProviderRefreshSourceMode,
    totals: ImportTotals,
    changed: bool,
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

    pub(crate) fn record_success(
        &mut self,
        provider: CaptureProvider,
        trigger: ProviderRefreshTrigger,
        source_mode: ProviderRefreshSourceMode,
        summary: &ProviderImportSummary,
        stats: &SourceStats,
    ) {
        let aggregate = self.aggregate_mut(provider, trigger, source_mode);
        aggregate.changed |= summary.has_accepted_content();
        aggregate.totals.add(summary, stats);
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
        } else {
            aggregate.totals.add_source_failure(stats);
        }
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
        duration: Duration,
    ) -> Vec<PublicEventV1> {
        self.aggregates.sort_by(|left, right| {
            left.provider
                .as_str()
                .cmp(right.provider.as_str())
                .then_with(|| left.source_mode.as_str().cmp(right.source_mode.as_str()))
                .then_with(|| left.trigger.as_str().cmp(right.trigger.as_str()))
        });
        self.aggregates
            .into_iter()
            .map(|aggregate| {
                let totals = aggregate.totals;
                let source_count = totals
                    .imported_sources
                    .saturating_add(totals.failed_sources);
                let outcome = if totals.imported_sources == 0 && totals.failed_sources > 0 {
                    Outcome::Failure
                } else {
                    Outcome::Success
                };
                let mut event = ProviderRefreshCompletedV1::foreground(
                    outcome,
                    duration,
                    ForegroundProviderRefreshV1 {
                        provider: aggregate.provider,
                        trigger: aggregate.trigger,
                        source_mode: aggregate.source_mode,
                        change: if aggregate.changed {
                            ProviderRefreshChange::Changed
                        } else {
                            ProviderRefreshChange::NoOp
                        },
                        work_remaining: totals.capture_work_remaining,
                        counts: ProviderRefreshCountsV1::new(
                            count_u64(source_count),
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
            changed: false,
        });
        self.aggregates
            .last_mut()
            .expect("a provider refresh aggregate was just inserted")
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
        assert!(refresh.work_remaining);
        assert_eq!(refresh.counts.sources, CountBucket::TwoToFive);
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
        assert_eq!(codex.counts.skips, CountBucket::TwoToFive);
        let custom = foreground(&events[1]);
        assert_eq!(custom.provider, CaptureProvider::Custom);
        assert_eq!(custom.change, ProviderRefreshChange::NoOp);
        assert_eq!(custom.counts.rejections, CountBucket::SixToTwenty);
        assert_eq!(custom.counts.failures, CountBucket::One);
        assert_eq!(custom.counts.bytes, BytesBucket::UnderOneHundredKb);
        let PublicEventV1::ProviderRefreshCompleted(custom_event) = &events[1] else {
            unreachable!();
        };
        assert_eq!(custom_event.outcome, Outcome::Failure);
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
