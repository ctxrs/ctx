use crate::analytics::{BytesBucket, CountBucket};

use super::*;

fn foreground(event: &PublicEventV1) -> &ForegroundProviderRefreshV1 {
    let PublicEventV1::ProviderRefreshCompleted(event) = event else {
        panic!("expected a provider refresh event");
    };
    event.foreground.as_ref().unwrap()
}

fn record_success(
    collector: &mut ProviderRefreshCollector,
    provider: CaptureProvider,
    trigger: ProviderRefreshTrigger,
    source_mode: ProviderRefreshSourceMode,
    summary: &ProviderImportSummary,
    stats: &SourceStats,
) {
    collector.record_success_with_facts(
        provider,
        trigger,
        source_mode,
        summary,
        stats,
        ProviderRefreshRuntimeFacts::observed_success(Duration::ZERO, summary),
    );
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
    record_success(
        &mut collector,
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
    record_success(
        &mut collector,
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
fn distinguishes_no_op_from_changed() {
    let mut collector = ProviderRefreshCollector::default();
    let mut no_op = ProviderImportSummary::default();
    no_op.skipped = 3;
    record_success(
        &mut collector,
        CaptureProvider::Codex,
        ProviderRefreshTrigger::Search,
        ProviderRefreshSourceMode::Discovered,
        &no_op,
        &SourceStats::default(),
    );

    let events = collector.finish();

    assert_eq!(events.len(), 1);
    let codex = foreground(&events[0]);
    assert_eq!(codex.provider, CaptureProvider::Codex);
    assert_eq!(codex.change, ProviderRefreshChange::NoOp);
    assert_eq!(codex.work_kind, Some(ProviderRefreshWorkKind::NoOp));
    assert_eq!(codex.refresh_result, ProviderRefreshResult::Complete);
    assert_eq!(codex.core_result, ProviderCoreResult::NoOp);
    assert_eq!(codex.counts.skips, CountBucket::TwoToFive);
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
    let PublicEventV1::ProviderRefreshCompleted(codex) = event_for(CaptureProvider::Codex) else {
        unreachable!();
    };
    let PublicEventV1::ProviderRefreshCompleted(claude) = event_for(CaptureProvider::Claude) else {
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
    let success = ProviderRefreshRuntimeFacts::observed_success(Duration::from_secs(3), &summary);
    assert_eq!(success.duration, Duration::from_secs(3));
    assert_eq!(success.work_kind, None);
    assert_eq!(success.core_result, ProviderCoreResult::Complete);
    assert_eq!(success.canonical_pro_result, ProviderProResult::Unknown);
    assert_eq!(success.output_pro_result, ProviderProResult::Unknown);
    assert_eq!(success.retired_records, None);
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
        record_success(
            &mut collector,
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
