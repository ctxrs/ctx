use std::time::Duration;

use ctx_history_core::CaptureProvider;
use ctx_history_refresh::{
    published_refresh_receipt_for_recovery, RefreshOutcomeCode, RefreshStatus, RefreshStatusKind,
    RefreshTerminalFailureScope, RefreshTerminalFailureType, SourceBackedRefreshProgress,
    SourceBackedRefreshReceipt,
};
use serde_json::Value;

use ctx_client_observability::analytics::{
    bytes_bucket, count_bucket, duration_bucket, DurationBucket, ForegroundProviderRefreshV1,
    Outcome, ProviderCoreResult, ProviderRefreshChange, ProviderRefreshCompletedV1,
    ProviderRefreshConfiguredIndexingMode, ProviderRefreshContentEvidence,
    ProviderRefreshCorpusStockV1, ProviderRefreshCountsV1, ProviderRefreshDaemonTriggerKind,
    ProviderRefreshFailureScope, ProviderRefreshFailureType, ProviderRefreshReconciliationDemand,
    ProviderRefreshResult, ProviderRefreshSourceMode, ProviderRefreshTerminalHealthV1,
    ProviderRefreshTrigger, ProviderRefreshWorkKind, PublicEventV1, Surface,
};

pub(super) fn provider_refresh_event(
    job: &Value,
    successor_pending: bool,
    automatic_indexing_enabled: Option<bool>,
) -> Option<PublicEventV1> {
    let terminal_status = job.get("status").and_then(Value::as_str)?;
    if !matches!(terminal_status, "completed" | "failed")
        || job.get("operation").and_then(Value::as_str) != Some("refresh")
    {
        return None;
    }
    let trigger = match job.get("trigger").and_then(Value::as_str)? {
        "setup" => ProviderRefreshTrigger::Setup,
        "search" => ProviderRefreshTrigger::Search,
        "periodic" => ProviderRefreshTrigger::Daemon,
        // The foreground import collector owns the command's one aggregate;
        // the daemon must not emit a contradictory duplicate for its Core run.
        "import" => return None,
        _ => return None,
    };
    let progress = SourceBackedRefreshProgress::from_status_json(job).ok()?;
    let provider = single_provider(&progress.providers);
    let source_mode = provider.map(|_| ProviderRefreshSourceMode::Discovered);
    let sources = progress_total_sources(job, &progress);
    let duration = refresh_duration(job);

    if terminal_status == "failed" {
        return failed_provider_refresh_event(
            job,
            successor_pending,
            trigger,
            provider,
            source_mode,
            sources,
            duration,
            automatic_indexing_enabled,
        );
    }

    let receipt = published_refresh_receipt_for_recovery(job).ok()?;
    let source_failures = u64::try_from(receipt.source_failure_total()).ok()?;
    let rejections = receipt.rejected_record_total();
    let partial = source_failures != 0 || rejections != 0;
    let structured_retryable = job
        .get("structured_outcome")
        .and_then(|outcome| outcome.get("retryable"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let (failure_scope, failure_type) = completed_failure(&receipt, rejections, source_failures);
    let generation_changed = receipt.generation_changed;
    let work_kind = if !generation_changed {
        Some(ProviderRefreshWorkKind::NoOp)
    } else if receipt.previous_generation.is_none() {
        Some(ProviderRefreshWorkKind::Fresh)
    } else {
        None
    };
    let counts = provider.map(|_| {
        ProviderRefreshCountsV1::sparse_refresh_receipt(
            sources,
            None,
            Some(rejections),
            Some(source_failures),
            None,
        )
    });
    let event = ProviderRefreshCompletedV1::bucketed(
        Surface::Daemon,
        Outcome::Success,
        duration,
        ForegroundProviderRefreshV1 {
            provider,
            trigger,
            source_mode,
            change: if generation_changed {
                ProviderRefreshChange::Changed
            } else {
                ProviderRefreshChange::NoOp
            },
            content_evidence: ProviderRefreshContentEvidence::Unknown,
            work_kind,
            refresh_result: if partial {
                ProviderRefreshResult::Partial
            } else {
                ProviderRefreshResult::Complete
            },
            core_result: if generation_changed {
                ProviderCoreResult::Complete
            } else {
                ProviderCoreResult::NoOp
            },
            failure_scope,
            failure_type,
            work_remaining: successor_pending || structured_retryable,
            // A removed source is not an exact retired-record aggregate.
            retired_records: None,
            counts,
            // Daemon refreshes do not own exact process CPU or peak-RSS
            // observations around only this shared engine call.
            performance: None,
        },
    )
    .with_terminal_health(refresh_terminal_health(
        job,
        successor_pending,
        automatic_indexing_enabled,
        None,
    ));
    // The existing terminal delivery is best-effort and non-retryable. Only a
    // changed publication with this exact receipt contributes the optional
    // sample; it is not a census or delivery/coverage denominator.
    let event = if generation_changed {
        match corpus_stock(&receipt) {
            Some(stock) => event.with_corpus_stock(stock),
            None => event,
        }
    } else {
        event
    };
    Some(PublicEventV1::ProviderRefreshCompleted(event))
}

fn corpus_stock(receipt: &SourceBackedRefreshReceipt) -> Option<ProviderRefreshCorpusStockV1> {
    let current = receipt.current;
    Some(ProviderRefreshCorpusStockV1 {
        indexed_documents: count_bucket(current.indexed_documents),
        retained_records: count_bucket(current.retained_records),
        rejected_records: count_bucket(current.rejected_records),
        certified_source_bytes: bytes_bucket(current.certified_source_bytes),
        removed_source_count: count_bucket(u64::try_from(current.removed_source_count).ok()?),
    })
}

#[allow(clippy::too_many_arguments)]
fn failed_provider_refresh_event(
    job: &Value,
    successor_pending: bool,
    trigger: ProviderRefreshTrigger,
    provider: Option<CaptureProvider>,
    source_mode: Option<ProviderRefreshSourceMode>,
    sources: Option<u64>,
    duration: DurationBucket,
    automatic_indexing_enabled: Option<bool>,
) -> Option<PublicEventV1> {
    let status = RefreshStatus::parse_schema_v1(job.clone()).ok()?;
    let RefreshStatusKind::Logical(logical) = status.kind().ok()? else {
        return None;
    };
    let outcome = logical.structured_outcome?;
    if !outcome.code.is_failure() {
        return None;
    }
    let retained_previous_generation = outcome.retained_generation.is_some();
    let (failure_scope, failure_type) = terminal_failure(outcome.code);
    let counts = provider
        .map(|_| ProviderRefreshCountsV1::sparse_refresh_receipt(sources, None, None, None, None));
    Some(PublicEventV1::ProviderRefreshCompleted(
        ProviderRefreshCompletedV1::bucketed(
            Surface::Daemon,
            Outcome::Failure,
            duration,
            ForegroundProviderRefreshV1 {
                provider,
                trigger,
                source_mode,
                change: ProviderRefreshChange::NoOp,
                content_evidence: ProviderRefreshContentEvidence::Unknown,
                work_kind: None,
                refresh_result: ProviderRefreshResult::Failure,
                core_result: ProviderCoreResult::Failure,
                failure_scope,
                failure_type,
                work_remaining: successor_pending || outcome.retryable,
                retired_records: None,
                counts,
                performance: None,
            },
        )
        .with_terminal_health(refresh_terminal_health(
            job,
            successor_pending,
            automatic_indexing_enabled,
            Some(retained_previous_generation),
        )),
    ))
}

fn refresh_terminal_health(
    job: &Value,
    successor_pending: bool,
    automatic_indexing_enabled: Option<bool>,
    retained_previous_generation: Option<bool>,
) -> ProviderRefreshTerminalHealthV1 {
    ProviderRefreshTerminalHealthV1 {
        configured_indexing_mode: automatic_indexing_enabled.map(|enabled| {
            if enabled {
                ProviderRefreshConfiguredIndexingMode::Automatic
            } else {
                ProviderRefreshConfiguredIndexingMode::Manual
            }
        }),
        daemon_trigger_kind: daemon_trigger_kind(job),
        reconciliation_demand: reconciliation_demand(job),
        retained_previous_generation,
        queue_wait_duration: elapsed_millis(job, "requested_at_ms", "started_at_ms")
            .map(duration_bucket),
        discovery_duration: timing_duration(job, "discovery").map(duration_bucket),
        scan_stage_duration: timing_duration(job, "scan_stage").map(duration_bucket),
        commit_duration: timing_duration(job, "commit").map(duration_bucket),
        coalesced_request_count: job
            .get("coalesced_requests")
            .and_then(Value::as_u64)
            .map(count_bucket),
        successor_pending,
        processed_sessions: progress_u64(job, "processed_sessions").map(count_bucket),
        processed_messages: progress_u64(job, "processed_messages").map(count_bucket),
        processed_tool_calls: progress_u64(job, "processed_tool_calls").map(count_bucket),
        processed_bytes: progress_u64(job, "processed_bytes").map(bytes_bucket),
    }
}

fn daemon_trigger_kind(job: &Value) -> Option<ProviderRefreshDaemonTriggerKind> {
    if job.get("trigger").and_then(Value::as_str)? != "periodic" {
        return None;
    }
    match job
        .get("refresh_scope")
        .and_then(|scope| scope.get("kind"))
        .and_then(Value::as_str)?
    {
        "exact" => Some(ProviderRefreshDaemonTriggerKind::DaemonWatch),
        "all" if job.get("previous_generation").is_none_or(Value::is_null) => {
            Some(ProviderRefreshDaemonTriggerKind::StartupCatchUp)
        }
        "all" => Some(ProviderRefreshDaemonTriggerKind::PeriodicReconciliation),
        _ => None,
    }
}

fn reconciliation_demand(job: &Value) -> Option<ProviderRefreshReconciliationDemand> {
    match job.get("reconciliation_demand").and_then(Value::as_str)? {
        "incremental" => Some(ProviderRefreshReconciliationDemand::Incremental),
        "exhaustive" => Some(ProviderRefreshReconciliationDemand::Exhaustive),
        _ => None,
    }
}

fn elapsed_millis(job: &Value, started_field: &str, finished_field: &str) -> Option<Duration> {
    job.get(finished_field)
        .and_then(Value::as_i64)
        .zip(job.get(started_field).and_then(Value::as_i64))
        .and_then(|(finished, started)| finished.checked_sub(started))
        .and_then(|millis| u64::try_from(millis).ok())
        .map(Duration::from_millis)
}

fn timing_duration(job: &Value, field: &str) -> Option<Duration> {
    job.get("timings_us")
        .and_then(Value::as_object)
        .and_then(|timings| timings.get(field))
        .and_then(Value::as_u64)
        .map(Duration::from_micros)
}

fn completed_failure(
    receipt: &SourceBackedRefreshReceipt,
    rejections: u64,
    source_failures: u64,
) -> (ProviderRefreshFailureScope, ProviderRefreshFailureType) {
    match (rejections != 0, source_failures != 0) {
        (false, false) => (
            ProviderRefreshFailureScope::None,
            ProviderRefreshFailureType::None,
        ),
        (true, false) => (
            ProviderRefreshFailureScope::Record,
            ProviderRefreshFailureType::RecordRejection,
        ),
        (true, true) => (
            ProviderRefreshFailureScope::Mixed,
            ProviderRefreshFailureType::Mixed,
        ),
        (false, true) => (
            ProviderRefreshFailureScope::Source,
            homogeneous_source_failure_type(receipt),
        ),
    }
}

fn homogeneous_source_failure_type(
    receipt: &SourceBackedRefreshReceipt,
) -> ProviderRefreshFailureType {
    let mut classes = Vec::new();
    for route in &receipt.route_results {
        if route.source_failures.len() == route.source_failure_total {
            classes.extend(
                route
                    .source_failures
                    .iter()
                    .map(|failure| failure.class.as_str()),
            );
        } else if route.source_failure_total == 1 && route.source_failures.is_empty() {
            let Some(class) = route.outcome.failure_class() else {
                return ProviderRefreshFailureType::Unknown;
            };
            classes.push(class);
        } else {
            return ProviderRefreshFailureType::Unknown;
        }
    }
    classes.sort_unstable();
    classes.dedup();
    match classes.as_slice() {
        ["incompatible"] => ProviderRefreshFailureType::UnsupportedSchema,
        ["unreadable"] => ProviderRefreshFailureType::MalformedSource,
        [_] => ProviderRefreshFailureType::Unknown,
        [] => ProviderRefreshFailureType::Unknown,
        _ => ProviderRefreshFailureType::Mixed,
    }
}

fn terminal_failure(
    code: RefreshOutcomeCode,
) -> (ProviderRefreshFailureScope, ProviderRefreshFailureType) {
    let Some((scope, failure_type)) = code.terminal_failure_classification() else {
        return (
            ProviderRefreshFailureScope::Unknown,
            ProviderRefreshFailureType::Unknown,
        );
    };
    (
        match scope {
            RefreshTerminalFailureScope::Source => ProviderRefreshFailureScope::Source,
            RefreshTerminalFailureScope::System => ProviderRefreshFailureScope::System,
            RefreshTerminalFailureScope::Unknown => ProviderRefreshFailureScope::Unknown,
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
}

fn progress_total_sources(job: &Value, progress: &SourceBackedRefreshProgress) -> Option<u64> {
    let known = match job
        .get("progress")
        .and_then(|progress| progress.get("total_sources_known"))
    {
        Some(Value::Bool(known)) => *known,
        Some(_) => return None,
        None => progress.total_sources != 0,
    };
    known.then(|| u64::try_from(progress.total_sources).ok())?
}

fn progress_u64(job: &Value, field: &str) -> Option<u64> {
    job.get("progress")?.get(field)?.as_u64()
}

fn single_provider(providers: &[String]) -> Option<CaptureProvider> {
    let mut parsed = providers
        .iter()
        .map(|provider| provider.parse::<CaptureProvider>().ok())
        .collect::<Option<Vec<_>>>()?;
    parsed.sort_unstable_by_key(|provider| provider.as_str());
    parsed.dedup();
    (parsed.len() == 1).then(|| parsed[0])
}

fn refresh_duration(job: &Value) -> DurationBucket {
    let duration_millis = job
        .get("finished_at_ms")
        .and_then(Value::as_i64)
        .zip(job.get("started_at_ms").and_then(Value::as_i64))
        .and_then(|(finished, started)| finished.checked_sub(started))
        .and_then(|millis| u64::try_from(millis).ok());
    duration_millis
        .map(Duration::from_millis)
        .map(duration_bucket)
        .unwrap_or(DurationBucket::Unknown)
}

#[cfg(test)]
mod tests {
    use ctx_history_refresh::{
        SourceBackedRefreshCurrent, SourceBackedRefreshReceipt, SourceBackedRefreshRouteResult,
        SourceBackedRefreshSourceFailure,
    };
    use serde_json::json;

    use super::*;
    use ctx_client_observability::analytics::{bytes_bucket, count_bucket};

    fn completed_job(
        trigger: &str,
        previous_generation: Option<&str>,
        generation_changed: bool,
    ) -> Value {
        let published_generation = "generation-b".to_owned();
        let mut route = SourceBackedRefreshRouteResult::succeeded("ab".repeat(32), true);
        route.source_failure_total = 2;
        route.rejected_record_total = 3;
        let receipt = SourceBackedRefreshReceipt {
            previous_generation: previous_generation.map(str::to_owned),
            published_generation: published_generation.clone(),
            generation_changed,
            published_explicit_source_catalog: None,
            current: SourceBackedRefreshCurrent {
                source_count: 1,
                rejected_records: 3,
                certified_source_bytes: 4096,
                removed_source_count: 2,
                ..SourceBackedRefreshCurrent::default()
            },
            route_results: vec![route],
            zero_source_authority: Vec::new(),
            catalog_route_bindings: Vec::new(),
        };
        json!({
            "status": "completed",
            "operation": "refresh",
            "trigger": trigger,
            "source_count": 1,
            "requested_at_ms": 500,
            "started_at_ms": 1_000,
            "finished_at_ms": 3_500,
            "coalesced_requests": 3,
            "refresh_scope": { "kind": "all" },
            "reconciliation_demand": "exhaustive",
            "timings_us": {
                "discovery": 100_000,
                "scan_stage": 2_000_000,
                "commit": 6_000_000,
            },
            "previous_generation": previous_generation,
            "published_generation": published_generation,
            "generation_changed": generation_changed,
            "certified_source_count": 1,
            "certified_source_bytes": 4096,
            "receipt": receipt.to_json(),
            "progress": {
                "phase": "complete",
                "completed_sources": 1,
                "total_sources": 1,
                "total_sources_known": true,
                "providers": ["codex"],
                "processed_sessions": 7,
                "processed_messages": 19,
                "processed_tool_calls": 4,
                "processed_bytes": 4096,
                "completed_records": 21,
            },
            "structured_outcome": { "retryable": false },
        })
    }

    fn refresh(event: PublicEventV1) -> ProviderRefreshCompletedV1 {
        match event {
            PublicEventV1::ProviderRefreshCompleted(event) => event,
            _ => panic!("expected provider refresh event"),
        }
    }

    fn observed_provider_refresh_event(
        job: &Value,
        successor_pending: bool,
    ) -> Option<PublicEventV1> {
        provider_refresh_event(job, successor_pending, Some(true))
    }

    #[test]
    fn cold_setup_receipt_emits_fresh_daemon_event_with_sparse_exact_facts() {
        let event = refresh(
            observed_provider_refresh_event(&completed_job("setup", None, true), false)
                .expect("setup refresh event"),
        );
        let facts = event.foreground.expect("refresh facts");

        assert_eq!(event.surface, Surface::Daemon);
        assert_eq!(facts.trigger, ProviderRefreshTrigger::Setup);
        assert_eq!(
            facts.source_mode,
            Some(ProviderRefreshSourceMode::Discovered)
        );
        assert_eq!(facts.provider, Some(CaptureProvider::Codex));
        assert_eq!(facts.work_kind, Some(ProviderRefreshWorkKind::Fresh));
        assert_eq!(facts.change, ProviderRefreshChange::Changed);
        assert_eq!(facts.refresh_result, ProviderRefreshResult::Partial);
        assert_eq!(facts.failure_scope, ProviderRefreshFailureScope::Mixed);
        assert_eq!(facts.failure_type, ProviderRefreshFailureType::Mixed);
        assert_eq!(facts.retired_records, None);
        let stock = event.corpus_stock.expect("best-effort receipt stock");
        assert_eq!(stock.indexed_documents, count_bucket(0));
        assert_eq!(stock.retained_records, count_bucket(0));
        assert_eq!(stock.rejected_records, count_bucket(3));
        assert_eq!(stock.certified_source_bytes, bytes_bucket(4096));
        assert_eq!(stock.removed_source_count, count_bucket(2));
        let health = event.terminal_health.expect("terminal health");
        assert_eq!(
            health.queue_wait_duration,
            Some(duration_bucket(Duration::from_millis(500)))
        );
        assert_eq!(
            health.discovery_duration,
            Some(duration_bucket(Duration::from_micros(100_000)))
        );
        assert_eq!(
            health.scan_stage_duration,
            Some(duration_bucket(Duration::from_micros(2_000_000)))
        );
        assert_eq!(
            health.commit_duration,
            Some(duration_bucket(Duration::from_micros(6_000_000)))
        );
        assert_eq!(health.coalesced_request_count, Some(count_bucket(3)));
        assert_eq!(
            health.configured_indexing_mode,
            Some(ProviderRefreshConfiguredIndexingMode::Automatic)
        );
        assert_eq!(health.daemon_trigger_kind, None);
        assert_eq!(
            health.reconciliation_demand,
            Some(ProviderRefreshReconciliationDemand::Exhaustive)
        );
        assert_eq!(health.retained_previous_generation, None);
        assert_eq!(health.processed_sessions, Some(count_bucket(7)));
        assert_eq!(health.processed_messages, Some(count_bucket(19)));
        assert_eq!(health.processed_tool_calls, Some(count_bucket(4)));
        assert_eq!(health.processed_bytes, Some(bytes_bucket(4096)));
        assert!(!health.successor_pending);
        assert_eq!(
            event.duration,
            duration_bucket(Duration::from_millis(2_500))
        );
        let counts = facts.counts.expect("receipt counts");
        assert_eq!(counts.sources, Some(count_bucket(1)));
        assert_eq!(counts.sessions, None);
        assert_eq!(counts.events, None);
        assert_eq!(counts.rejections, Some(count_bucket(3)));
        assert_eq!(counts.failures, Some(count_bucket(2)));
        assert_eq!(counts.bytes, None);
        assert_eq!(counts.source_files, None);
        assert_eq!(counts.edges, None);
        assert_eq!(counts.skips, None);
        assert_eq!(facts.performance, None);
    }

    #[test]
    fn incremental_periodic_noop_uses_daemon_trigger_and_reports_successor() {
        let event = refresh(
            observed_provider_refresh_event(
                &completed_job("periodic", Some("generation-b"), false),
                true,
            )
            .expect("periodic refresh event"),
        );
        let facts = event.foreground.expect("refresh facts");

        assert_eq!(event.surface, Surface::Daemon);
        assert_eq!(facts.trigger, ProviderRefreshTrigger::Daemon);
        assert_eq!(facts.change, ProviderRefreshChange::NoOp);
        assert_eq!(facts.work_kind, Some(ProviderRefreshWorkKind::NoOp));
        assert_eq!(facts.core_result, ProviderCoreResult::NoOp);
        assert!(facts.work_remaining);
        assert!(
            event.corpus_stock.is_none(),
            "a no-op terminal outcome is the missingness authority, not a zero stock sample"
        );
        let health = event.terminal_health.expect("terminal health");
        assert!(health.successor_pending);
        assert_eq!(
            health.daemon_trigger_kind,
            Some(ProviderRefreshDaemonTriggerKind::PeriodicReconciliation)
        );
    }

    #[test]
    fn daemon_trigger_subtype_and_manual_configuration_are_not_conflated() {
        let mut watch_job = completed_job("periodic", Some("generation-a"), true);
        watch_job["refresh_scope"] = json!({ "kind": "exact", "routes": ["route-a"] });
        watch_job["reconciliation_demand"] = json!("incremental");

        let watch_event = refresh(
            provider_refresh_event(&watch_job, false, Some(false)).expect("watch refresh event"),
        );
        let health = watch_event.terminal_health.expect("terminal health");

        assert_eq!(
            health.configured_indexing_mode,
            Some(ProviderRefreshConfiguredIndexingMode::Manual)
        );
        assert_eq!(
            health.daemon_trigger_kind,
            Some(ProviderRefreshDaemonTriggerKind::DaemonWatch)
        );
        assert_eq!(
            health.reconciliation_demand,
            Some(ProviderRefreshReconciliationDemand::Incremental)
        );

        let startup_event = refresh(
            observed_provider_refresh_event(&completed_job("periodic", None, true), false)
                .expect("startup catch-up event"),
        );
        assert_eq!(
            startup_event
                .terminal_health
                .expect("terminal health")
                .daemon_trigger_kind,
            Some(ProviderRefreshDaemonTriggerKind::StartupCatchUp)
        );
    }

    #[test]
    fn changed_incremental_receipt_does_not_invent_append_or_rewrite() {
        let event = refresh(
            observed_provider_refresh_event(
                &completed_job("search", Some("generation-a"), true),
                false,
            )
            .expect("search refresh event"),
        );
        let facts = event.foreground.expect("refresh facts");

        assert_eq!(facts.trigger, ProviderRefreshTrigger::Search);
        assert_eq!(facts.change, ProviderRefreshChange::Changed);
        assert_eq!(facts.work_kind, None);
        assert!(event.corpus_stock.is_some());
    }

    #[test]
    fn run_selected_source_total_does_not_use_global_generation_cardinality() {
        let mut job = completed_job("periodic", Some("generation-a"), true);
        job["source_count"] = json!(500);
        job["progress"]["completed_sources"] = json!(2);
        job["progress"]["total_sources"] = json!(2);

        let event = refresh(observed_provider_refresh_event(&job, false).expect("refresh event"));
        let counts = event
            .foreground
            .expect("refresh facts")
            .counts
            .expect("single-provider run counts");

        assert_eq!(counts.sources, Some(count_bucket(2)));
        assert_ne!(counts.sources, Some(count_bucket(500)));
    }

    #[test]
    fn provider_neutral_refresh_omits_source_mode_and_all_run_counts() {
        let mut job = completed_job("periodic", Some("generation-a"), true);
        job["progress"]["providers"] = json!(["codex", "claude"]);

        let event = refresh(observed_provider_refresh_event(&job, false).expect("refresh event"));
        let facts = event.foreground.expect("refresh facts");

        assert_eq!(facts.provider, None);
        assert_eq!(facts.source_mode, None);
        assert_eq!(facts.counts, None);
    }

    #[test]
    fn complete_homogeneous_source_diagnostics_use_exact_bounded_type() {
        let mut job = completed_job("periodic", Some("generation-a"), true);
        let mut receipt = published_refresh_receipt_for_recovery(&job).unwrap();
        let route = receipt.route_results.first_mut().unwrap();
        route.source_failure_total = 1;
        route.source_failures = vec![SourceBackedRefreshSourceFailure {
            route_identity: route.route_identity.clone(),
            source_identity: "cd".repeat(32),
            provider: "codex".to_owned(),
            class: "incompatible".to_owned(),
            carried_forward: true,
            source_selector: "bounded selector".to_owned(),
            detail: "bounded diagnostic".to_owned(),
        }];
        route.rejected_record_total = 0;
        receipt.current.rejected_records = 0;
        job["receipt"] = receipt.to_json();

        let event = refresh(observed_provider_refresh_event(&job, false).expect("refresh event"));
        let facts = event.foreground.expect("refresh facts");

        assert_eq!(facts.failure_scope, ProviderRefreshFailureScope::Source);
        assert_eq!(
            facts.failure_type,
            ProviderRefreshFailureType::UnsupportedSchema
        );
    }

    #[test]
    fn omitted_source_diagnostics_keep_known_scope_but_unknown_type() {
        let mut job = completed_job("periodic", Some("generation-a"), true);
        let mut receipt = published_refresh_receipt_for_recovery(&job).unwrap();
        let route = receipt.route_results.first_mut().unwrap();
        route.rejected_record_total = 0;
        receipt.current.rejected_records = 0;
        job["receipt"] = receipt.to_json();

        let event = refresh(observed_provider_refresh_event(&job, false).expect("refresh event"));
        let facts = event.foreground.expect("refresh facts");

        assert_eq!(facts.failure_scope, ProviderRefreshFailureScope::Source);
        assert_eq!(facts.failure_type, ProviderRefreshFailureType::Unknown);
    }

    #[test]
    fn aggregate_terminal_source_failures_do_not_invent_mixed_type() {
        for code in [
            RefreshOutcomeCode::SourceFailures,
            RefreshOutcomeCode::LogicalSourceFailures,
        ] {
            assert_eq!(
                terminal_failure(code),
                (
                    ProviderRefreshFailureScope::Source,
                    ProviderRefreshFailureType::Unknown,
                )
            );
        }
    }

    #[test]
    fn terminal_source_failure_emits_one_truthful_failure_event() {
        let route = "ef".repeat(32);
        let request_id = "019fcaaa-0000-7000-8000-000000000410";
        let job = json!({
            "status": "failed",
            "request_state": "failed",
            "operation": "refresh",
            "trigger": "setup",
            "request_id": request_id,
            "logical_request_id": request_id,
            "logical_phase": "terminal",
            "physical_attempt_id": request_id,
            "physical_attempt_state": "failed",
            "progress_owner_request_id": request_id,
            "progress_owner_attempt_state": "failed",
            "requested_at_ms": 500,
            "started_at_ms": 1_000,
            "finished_at_ms": 3_500,
            "progress": {
                "phase": "failed",
                "whole_run_stage": "failed",
                "completed_sources": 1,
                "total_sources": 2,
                "total_sources_known": true,
                "providers": ["codex"],
                "processed_sessions": 7,
                "processed_bytes": 4096,
                "estimated_remaining_millis": null,
            },
            "structured_outcome": {
                "code": "unsupported_schema",
                "class": "incompatible",
                "retryable": false,
                "affected_routes": [route],
                "retryable_routes": [],
                "blocked_routes": [route],
                "physical_attempt_id": request_id,
                "retained_generation": "generation-a",
                "retry_advice": "upgrade_or_reconfigure",
            },
        });

        let event =
            refresh(observed_provider_refresh_event(&job, true).expect("failed refresh event"));
        let facts = event.foreground.expect("refresh facts");

        assert_eq!(event.surface, Surface::Daemon);
        assert_eq!(event.outcome, Outcome::Failure);
        assert_eq!(
            event.duration,
            duration_bucket(Duration::from_millis(2_500))
        );
        assert_eq!(facts.trigger, ProviderRefreshTrigger::Setup);
        assert_eq!(facts.change, ProviderRefreshChange::NoOp);
        assert_eq!(facts.work_kind, None);
        assert_eq!(facts.refresh_result, ProviderRefreshResult::Failure);
        assert_eq!(facts.core_result, ProviderCoreResult::Failure);
        assert_eq!(facts.failure_scope, ProviderRefreshFailureScope::Source);
        assert_eq!(
            facts.failure_type,
            ProviderRefreshFailureType::UnsupportedSchema
        );
        assert!(facts.work_remaining);
        assert_eq!(facts.retired_records, None);
        assert!(
            event.corpus_stock.is_none(),
            "failed attempts do not attach a best-effort stock sample"
        );
        let health = event.terminal_health.expect("terminal health");
        assert!(health.successor_pending);
        assert_eq!(health.retained_previous_generation, Some(true));
        assert_eq!(
            facts.counts.expect("known failed-run counts").sources,
            Some(count_bucket(2))
        );
    }

    #[test]
    fn cli_owned_import_success_and_failure_are_not_duplicated_by_daemon() {
        assert!(
            observed_provider_refresh_event(&completed_job("import", None, true), false,).is_none()
        );
        assert!(observed_provider_refresh_event(
            &json!({
                "status": "failed",
                "operation": "refresh",
                "trigger": "import",
            }),
            true,
        )
        .is_none());
    }

    #[test]
    fn malformed_optional_health_fields_do_not_suppress_the_base_event() {
        let mut job = completed_job("periodic", Some("generation-a"), true);
        job["requested_at_ms"] = json!(2_000);
        job["started_at_ms"] = json!(1_000);
        job["coalesced_requests"] = json!("invalid");
        job["timings_us"]["discovery"] = json!("invalid");
        job["timings_us"]["scan_stage"] = Value::Null;

        let event =
            refresh(observed_provider_refresh_event(&job, false).expect("base refresh event"));
        let health = event.terminal_health.expect("terminal health");

        assert_eq!(health.queue_wait_duration, None);
        assert_eq!(health.discovery_duration, None);
        assert_eq!(health.scan_stage_duration, None);
        assert!(health.commit_duration.is_some());
        assert_eq!(health.coalesced_request_count, None);
    }

    #[test]
    fn explicit_zero_terminal_health_remains_present() {
        let mut job = completed_job("periodic", Some("generation-a"), true);
        job["requested_at_ms"] = json!(1_000);
        job["started_at_ms"] = json!(1_000);
        job["coalesced_requests"] = json!(0);

        let event =
            refresh(observed_provider_refresh_event(&job, false).expect("base refresh event"));
        let health = event.terminal_health.expect("terminal health");

        assert_eq!(
            health.queue_wait_duration,
            Some(duration_bucket(Duration::ZERO))
        );
        assert_eq!(health.coalesced_request_count, Some(count_bucket(0)));
    }
}
