use std::time::Duration;

use ctx_history_core::CaptureProvider;
use ctx_history_refresh::{
    published_refresh_receipt_for_recovery, RefreshOutcomeCode, RefreshStatus, RefreshStatusKind,
    RefreshTerminalFailureScope, RefreshTerminalFailureType, SourceBackedRefreshProgress,
    SourceBackedRefreshReceipt,
};
use serde_json::Value;

use ctx_client_observability::analytics::{
    duration_bucket, DurationBucket, ForegroundProviderRefreshV1, Outcome, ProviderCoreResult,
    ProviderRefreshChange, ProviderRefreshCompletedV1, ProviderRefreshCountsV1,
    ProviderRefreshFailureCode, ProviderRefreshFailureScope, ProviderRefreshFailureType,
    ProviderRefreshResult, ProviderRefreshTerminalHealthV1, ProviderRefreshTrigger, PublicEventV1,
    Surface,
};

pub(super) fn provider_refresh_event(
    job: &Value,
    successor_pending: bool,
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
    let duration = refresh_duration(job);

    if terminal_status == "failed" {
        return failed_provider_refresh_event(job, successor_pending, trigger, provider, duration);
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
    let counts = provider
        .map(|_| ProviderRefreshCountsV1::sparse(None, progress_u64(job, "processed_bytes")));
    let event = ProviderRefreshCompletedV1::bucketed(
        Surface::Daemon,
        Outcome::Success,
        duration,
        ForegroundProviderRefreshV1 {
            provider,
            trigger,
            change: if generation_changed {
                ProviderRefreshChange::Changed
            } else {
                ProviderRefreshChange::NoOp
            },
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
            failure_code: ProviderRefreshFailureCode::None,
            retryable: structured_retryable,
            work_remaining: successor_pending || structured_retryable,
            counts,
        },
    )
    .with_terminal_health(refresh_terminal_health(successor_pending, None));
    Some(PublicEventV1::ProviderRefreshCompleted(event))
}
fn failed_provider_refresh_event(
    job: &Value,
    successor_pending: bool,
    trigger: ProviderRefreshTrigger,
    provider: Option<CaptureProvider>,
    duration: DurationBucket,
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
        .map(|_| ProviderRefreshCountsV1::sparse(None, progress_u64(job, "processed_bytes")));
    Some(PublicEventV1::ProviderRefreshCompleted(
        ProviderRefreshCompletedV1::bucketed(
            Surface::Daemon,
            Outcome::Failure,
            duration,
            ForegroundProviderRefreshV1 {
                provider,
                trigger,
                change: ProviderRefreshChange::NoOp,
                refresh_result: ProviderRefreshResult::Failure,
                core_result: ProviderCoreResult::Failure,
                failure_scope,
                failure_type,
                failure_code: terminal_failure_code(outcome.code),
                retryable: outcome.retryable,
                work_remaining: successor_pending || outcome.retryable,
                counts,
            },
        )
        .with_terminal_health(refresh_terminal_health(
            successor_pending,
            Some(retained_previous_generation),
        )),
    ))
}

fn terminal_failure_code(code: RefreshOutcomeCode) -> ProviderRefreshFailureCode {
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

fn refresh_terminal_health(
    successor_pending: bool,
    retained_previous_generation: Option<bool>,
) -> ProviderRefreshTerminalHealthV1 {
    ProviderRefreshTerminalHealthV1 {
        retained_previous_generation,
        successor_pending,
    }
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
    use ctx_client_observability::analytics::bytes_bucket;

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
            "started_at_ms": 1_000,
            "finished_at_ms": 3_500,
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
                "processed_bytes": 4096,
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

    #[test]
    fn completed_refresh_emits_minimal_terminal_and_coarse_work_facts() {
        let event = refresh(
            provider_refresh_event(&completed_job("setup", None, true), false)
                .expect("setup refresh event"),
        );
        let facts = event.foreground.expect("refresh facts");

        assert_eq!(event.surface, Surface::Daemon);
        assert_eq!(event.outcome, Outcome::Success);
        assert_eq!(
            event.duration,
            duration_bucket(Duration::from_millis(2_500))
        );
        assert_eq!(facts.provider, Some(CaptureProvider::Codex));
        assert_eq!(facts.trigger, ProviderRefreshTrigger::Setup);
        assert_eq!(facts.change, ProviderRefreshChange::Changed);
        assert_eq!(facts.refresh_result, ProviderRefreshResult::Partial);
        assert_eq!(facts.core_result, ProviderCoreResult::Complete);
        assert_eq!(facts.failure_scope, ProviderRefreshFailureScope::Mixed);
        assert_eq!(facts.failure_type, ProviderRefreshFailureType::Mixed);
        assert!(!facts.work_remaining);
        let counts = facts.counts.expect("coarse work facts");
        assert_eq!(counts.records, None);
        assert_eq!(counts.logical_bytes, Some(bytes_bucket(4096)));
        assert_eq!(
            event.terminal_health,
            Some(ProviderRefreshTerminalHealthV1 {
                retained_previous_generation: None,
                successor_pending: false,
            })
        );
    }

    #[test]
    fn no_op_refresh_keeps_successor_and_work_remaining() {
        let event = refresh(
            provider_refresh_event(
                &completed_job("periodic", Some("generation-b"), false),
                true,
            )
            .expect("periodic refresh event"),
        );
        let facts = event.foreground.expect("refresh facts");

        assert_eq!(facts.trigger, ProviderRefreshTrigger::Daemon);
        assert_eq!(facts.change, ProviderRefreshChange::NoOp);
        assert_eq!(facts.core_result, ProviderCoreResult::NoOp);
        assert!(facts.work_remaining);
        assert!(
            event
                .terminal_health
                .expect("terminal health")
                .successor_pending
        );
    }

    #[test]
    fn provider_neutral_refresh_omits_run_work() {
        let mut job = completed_job("periodic", Some("generation-a"), true);
        job["progress"]["providers"] = json!(["codex", "claude"]);

        let event = refresh(provider_refresh_event(&job, false).expect("refresh event"));
        let facts = event.foreground.expect("refresh facts");

        assert_eq!(facts.provider, None);
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

        let event = refresh(provider_refresh_event(&job, false).expect("refresh event"));
        let facts = event.foreground.expect("refresh facts");

        assert_eq!(facts.failure_scope, ProviderRefreshFailureScope::Source);
        assert_eq!(
            facts.failure_type,
            ProviderRefreshFailureType::UnsupportedSchema
        );
    }

    #[test]
    fn terminal_failure_keeps_retained_previous_generation() {
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
            "started_at_ms": 1_000,
            "finished_at_ms": 3_500,
            "progress": {
                "phase": "failed",
                "whole_run_stage": "failed",
                "completed_sources": 1,
                "total_sources": 2,
                "total_sources_known": true,
                "providers": ["codex"],
                "completed_records": 5,
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

        let event = refresh(provider_refresh_event(&job, true).expect("failed refresh event"));
        let facts = event.foreground.expect("refresh facts");

        assert_eq!(event.outcome, Outcome::Failure);
        assert_eq!(facts.refresh_result, ProviderRefreshResult::Failure);
        assert_eq!(facts.core_result, ProviderCoreResult::Failure);
        assert_eq!(facts.failure_scope, ProviderRefreshFailureScope::Source);
        assert_eq!(
            facts.failure_type,
            ProviderRefreshFailureType::UnsupportedSchema
        );
        assert!(facts.work_remaining);
        let health = event.terminal_health.expect("terminal health");
        assert!(health.successor_pending);
        assert_eq!(health.retained_previous_generation, Some(true));
    }

    #[test]
    fn cli_owned_import_is_not_duplicated_by_daemon() {
        assert!(provider_refresh_event(&completed_job("import", None, true), false).is_none());
    }
}
