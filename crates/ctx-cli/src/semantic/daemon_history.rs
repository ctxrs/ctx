use std::{path::Path, time::Instant};

use anyhow::{anyhow, Result};
use ctx_history_core::utc_now;
use serde_json::{json, Value};

use crate::{
    analytics::PublicEventV1,
    commands::{
        import::{error_summary, import_totals_json, ImportTotals, ProviderRefreshCollector},
        search::{
            refresh_sources_for_search, search_refresh_plugin_sources, search_refresh_sources,
            RefreshArg,
        },
    },
    compact_json,
    config::AppConfig,
};

use super::{
    daemon::DaemonRuntime,
    daemon_retry::DaemonRetryBackoff,
    daemon_scheduler::{daemon_job_should_backoff, preserve_daemon_retry_state},
    indexing::semantic_text_hash,
    paths_status::{daemon_history_refresh_job_path, read_daemon_job_status},
};

const DAEMON_REJECTION_DIAGNOSTIC_SOURCES_MAX: usize = 256;

#[cfg(all(test, ctx_sqlite_vec))]
use super::daemon::daemon_test_job;

pub(super) fn restore_daemon_history_runtime_state(runtime: &mut DaemonRuntime, data_root: &Path) {
    let status = read_daemon_job_status(&daemon_history_refresh_job_path(data_root));
    let status = status.as_ref();
    runtime.history_retry.restore(status);
    runtime.history_source_cursor = status
        .and_then(|value| value.get("scheduler_next_source_cursor"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(usize::MAX as u64) as usize;
    runtime.history_followup_passes_remaining = status
        .and_then(|value| value.get("scheduler_followup_passes_remaining"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(usize::MAX as u64) as usize;
    runtime.history_retry_drain_passes_remaining = status
        .and_then(|value| value.get("scheduler_retry_drain_passes_remaining"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(usize::MAX as u64) as usize;
    runtime.history_rejected_records_by_source.clear();
    if let Some(by_source) = status
        .and_then(|value| value.get("scheduler_rejected_records_by_source"))
        .and_then(Value::as_object)
    {
        for (fingerprint, rejected_records) in by_source
            .iter()
            .take(DAEMON_REJECTION_DIAGNOSTIC_SOURCES_MAX)
        {
            let Some(rejected_records) = rejected_records.as_u64().filter(|count| *count > 0)
            else {
                continue;
            };
            if !fingerprint.is_empty() && fingerprint.len() <= 128 {
                runtime
                    .history_rejected_records_by_source
                    .insert(fingerprint.clone(), rejected_records);
            }
        }
    }
}

pub(super) fn finish_daemon_history_refresh_job(
    runtime: &mut DaemonRuntime,
    job: &mut Value,
) -> bool {
    update_daemon_history_rejection_diagnostics(runtime, job);
    update_daemon_history_followup_frontier(runtime, job);
    preserve_daemon_history_runtime_state(job, runtime);
    daemon_history_refresh_job_did_work(job)
        || runtime.history_followup_passes_remaining > 0
        || runtime.history_retry_drain_passes_remaining > 0
}

pub(super) fn preserve_daemon_history_runtime_state(job: &mut Value, runtime: &DaemonRuntime) {
    job["scheduler_next_source_cursor"] = json!(runtime.history_source_cursor);
    job["scheduler_followup_passes_remaining"] = json!(runtime.history_followup_passes_remaining);
    job["scheduler_retry_drain_passes_remaining"] =
        json!(runtime.history_retry_drain_passes_remaining);
    job["scheduler_rejected_records_by_source"] = json!(runtime.history_rejected_records_by_source);
    job["rejection_diagnostics"] = json!({
        "rejected_records": runtime
            .history_rejected_records_by_source
            .values()
            .copied()
            .fold(0_u64, u64::saturating_add),
        "sources_completed_with_rejections": runtime
            .history_rejected_records_by_source
            .len(),
    });
    preserve_daemon_retry_state(job, &runtime.history_retry);
}

fn update_daemon_history_rejection_diagnostics(runtime: &mut DaemonRuntime, job: &Value) {
    if let Some(discovered) = job
        .get("scheduler_discovered_source_fingerprints")
        .and_then(Value::as_array)
    {
        runtime
            .history_rejected_records_by_source
            .retain(|fingerprint, _| {
                discovered
                    .iter()
                    .any(|value| value.as_str() == Some(fingerprint.as_str()))
            });
    }
    if job.get("status").and_then(Value::as_str) != Some("completed")
        || job
            .get("totals")
            .and_then(|totals| totals.get("failed_sources"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
            > 0
    {
        return;
    }
    let Some(fingerprint) = job
        .get("source_fingerprint")
        .and_then(Value::as_str)
        .filter(|fingerprint| !fingerprint.is_empty() && fingerprint.len() <= 128)
    else {
        return;
    };
    let rejected_records = job
        .get("totals")
        .and_then(|totals| totals.get("rejected_records"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if rejected_records == 0 {
        runtime
            .history_rejected_records_by_source
            .remove(fingerprint);
    } else if runtime.history_rejected_records_by_source.len()
        < DAEMON_REJECTION_DIAGNOSTIC_SOURCES_MAX
        || runtime
            .history_rejected_records_by_source
            .contains_key(fingerprint)
    {
        runtime
            .history_rejected_records_by_source
            .insert(fingerprint.to_owned(), rejected_records);
    }
}

pub(super) fn daemon_history_retry_blocks_scheduler(runtime: &DaemonRuntime) -> bool {
    runtime.history_retry_drain_passes_remaining == 0 && !runtime.history_retry.ready()
}

pub(super) fn history_retry_due(runtime: &DaemonRuntime) -> bool {
    runtime.history_retry.consecutive_failures > 0 && runtime.history_retry.ready()
}

pub(super) fn record_daemon_history_job_retry(
    runtime: &mut DaemonRuntime,
    mut job: Value,
) -> Value {
    let selected_source = job.get("scheduler_source_index").is_some();
    let draining_before = runtime.history_retry_drain_passes_remaining > 0;
    if selected_source && draining_before {
        runtime.history_retry_drain_passes_remaining = runtime
            .history_retry_drain_passes_remaining
            .saturating_sub(1);
    }

    if daemon_job_should_backoff(&job) {
        let delay = runtime.history_retry.record_failure();
        if selected_source && !draining_before {
            runtime.history_retry_drain_passes_remaining =
                job.get("discovered_source_count")
                    .and_then(Value::as_u64)
                    .unwrap_or(1)
                    .saturating_sub(1)
                    .min(usize::MAX as u64) as usize;
        }
        job["retryable"] = Value::Bool(true);
        job["retry_after_ms"] = json!(delay.as_millis() as u64);
        job["consecutive_failures"] = json!(runtime.history_retry.consecutive_failures);
        job["retry_not_before_at_ms"] = json!(runtime.history_retry.retry_not_before_at_ms);
    } else if job.get("reason").and_then(Value::as_str) == Some("no_sources") {
        runtime.history_retry_drain_passes_remaining = 0;
        runtime.history_retry.reset();
    } else if selected_source
        && runtime.history_retry_drain_passes_remaining == 0
        && runtime.history_retry.ready()
    {
        runtime.history_retry.reset();
    }
    job
}

pub(super) fn update_daemon_history_followup_frontier(runtime: &mut DaemonRuntime, job: &Value) {
    if job.get("reason").and_then(Value::as_str) == Some("no_sources") {
        runtime.history_followup_passes_remaining = 0;
        return;
    }
    if job.get("status").and_then(Value::as_str) != Some("completed") {
        return;
    }
    if job.get("capture_work_remaining").and_then(Value::as_bool) == Some(true) {
        runtime.history_followup_passes_remaining =
            job.get("discovered_source_count")
                .and_then(Value::as_u64)
                .unwrap_or(1)
                .clamp(1, usize::MAX as u64) as usize;
    } else {
        runtime.history_followup_passes_remaining =
            runtime.history_followup_passes_remaining.saturating_sub(1);
    }
}

#[derive(Debug)]
pub(super) struct DaemonHistoryRefreshJob {
    pub(super) job: Value,
    pub(super) provider_refresh_events: Vec<PublicEventV1>,
}

pub(super) fn run_daemon_history_refresh_job(
    data_root: &Path,
    next_source_cursor: &mut usize,
    config: &AppConfig,
) -> Result<DaemonHistoryRefreshJob> {
    #[cfg(all(test, ctx_sqlite_vec))]
    if let Some(value) = daemon_test_job("history_refresh") {
        return Ok(DaemonHistoryRefreshJob {
            job: value,
            provider_refresh_events: Vec::new(),
        });
    }

    let last_run_at_ms = utc_now().timestamp_millis();
    let mut sources = search_refresh_sources(None);
    let mut plugin_sources = search_refresh_plugin_sources(
        data_root,
        None,
        &crate::search_filters::SourceIdentityFilters::default(),
    )?;
    sources.sort_by(|left, right| {
        (left.provider.as_str(), left.source_format, &left.path).cmp(&(
            right.provider.as_str(),
            right.source_format,
            &right.path,
        ))
    });
    plugin_sources.sort_by_key(|source| source.label());
    let discovered_source_count = sources.len().saturating_add(plugin_sources.len());
    let discovered_source_fingerprints = sources
        .iter()
        .map(|source| search_refresh_source_fingerprint(std::slice::from_ref(source)))
        .chain(
            plugin_sources
                .iter()
                .map(|source| semantic_text_hash(&source.label())),
        )
        .collect::<Vec<_>>();
    if discovered_source_count == 0 {
        let mut job = daemon_history_refresh_job_json(
            "skipped",
            0,
            ImportTotals::default(),
            last_run_at_ms,
            Some("no_sources"),
            None,
        );
        job["scheduler_discovered_source_fingerprints"] = json!([]);
        return Ok(DaemonHistoryRefreshJob {
            job,
            provider_refresh_events: Vec::new(),
        });
    }
    let selected_index = daemon_take_next_source_index(next_source_cursor, discovered_source_count)
        .ok_or_else(|| anyhow!("daemon source scheduler lost its discovered source"))?;
    let (selected_sources, selected_plugin_sources, source_fingerprint) =
        if selected_index < sources.len() {
            let source = sources.swap_remove(selected_index);
            let fingerprint = search_refresh_source_fingerprint(std::slice::from_ref(&source));
            (vec![source], Vec::new(), fingerprint)
        } else {
            let plugin = plugin_sources.swap_remove(selected_index - sources.len());
            let fingerprint = semantic_text_hash(&plugin.label());
            (Vec::new(), vec![plugin], fingerprint)
        };
    let refresh_started = Instant::now();
    let mut provider_refreshes = ProviderRefreshCollector::default();
    let refresh_result = refresh_sources_for_search(
        data_root,
        selected_sources,
        selected_plugin_sources,
        RefreshArg::Background,
        true,
        &mut provider_refreshes,
        config,
        *next_source_cursor == 0,
    );
    let provider_refresh_events = provider_refreshes.finish_for_daemon(refresh_started.elapsed());
    let mut job = match refresh_result {
        Ok(totals) => {
            let capture_work_remaining = totals.capture_work_remaining;
            let mut job =
                daemon_history_refresh_job_json("completed", 1, totals, last_run_at_ms, None, None);
            job["capture_work_remaining"] = json!(capture_work_remaining);
            job
        }
        Err(error) => daemon_history_refresh_job_json(
            "failed",
            1,
            ImportTotals::default(),
            last_run_at_ms,
            None,
            Some(error_summary(&error)),
        ),
    };
    if let Some(map) = job.as_object_mut() {
        map.insert("source_fingerprint".to_owned(), json!(source_fingerprint));
        map.insert("passes".to_owned(), json!(1));
        map.insert(
            "discovered_source_count".to_owned(),
            json!(discovered_source_count),
        );
        map.insert("scheduler_source_index".to_owned(), json!(selected_index));
        map.insert(
            "scheduler_next_source_cursor".to_owned(),
            json!(*next_source_cursor),
        );
        map.insert(
            "scheduler_discovered_source_fingerprints".to_owned(),
            json!(discovered_source_fingerprints),
        );
    }
    Ok(DaemonHistoryRefreshJob {
        job,
        provider_refresh_events,
    })
}

pub(super) fn daemon_take_next_source_index(
    cursor: &mut usize,
    source_count: usize,
) -> Option<usize> {
    if source_count == 0 {
        return None;
    }
    let selected = *cursor % source_count;
    *cursor = selected.saturating_add(1) % source_count;
    Some(selected)
}

pub(super) fn daemon_history_refresh_skipped_job(reason: &str) -> Value {
    daemon_history_refresh_job_json(
        "skipped",
        0,
        ImportTotals::default(),
        utc_now().timestamp_millis(),
        Some(reason),
        None,
    )
}

pub(super) fn daemon_history_refresh_failed_job(message: String) -> Value {
    daemon_history_refresh_job_json(
        "failed",
        0,
        ImportTotals::default(),
        utc_now().timestamp_millis(),
        None,
        Some(message),
    )
}

pub(super) fn daemon_history_refresh_retry_backoff_job(backoff: &DaemonRetryBackoff) -> Value {
    let mut job = daemon_history_refresh_skipped_job("retry_backoff");
    job["retryable"] = Value::Bool(true);
    job["retry_after_ms"] = json!(backoff.retry_after_ms().unwrap_or(0));
    job["consecutive_failures"] = json!(backoff.consecutive_failures);
    job["retry_not_before_at_ms"] = json!(backoff.retry_not_before_at_ms);
    job
}

pub(super) fn daemon_history_refresh_job_json(
    status: &str,
    source_count: usize,
    totals: ImportTotals,
    last_run_at_ms: i64,
    reason: Option<&str>,
    last_error: Option<String>,
) -> Value {
    compact_json(json!({
        "mode": RefreshArg::Background.as_str(),
        "status": status,
        "source_count": source_count,
        "totals": import_totals_json(&totals),
        "reason": reason,
        "last_run_at_ms": last_run_at_ms,
        "last_error": last_error,
    }))
}

pub(super) fn daemon_history_refresh_job_did_work(value: &Value) -> bool {
    let Some(totals) = value.get("totals") else {
        return false;
    };
    ["imported_sessions", "imported_events", "imported_edges"]
        .into_iter()
        .any(|key| totals.get(key).and_then(Value::as_u64).unwrap_or(0) > 0)
}

pub(super) fn search_refresh_source_fingerprint(
    sources: &[crate::provider_sources::SourceInfo],
) -> String {
    let mut items = sources
        .iter()
        .map(|source| {
            format!(
                "{}|{}|{}",
                source.provider.as_str(),
                source.source_format,
                source.path.display()
            )
        })
        .collect::<Vec<_>>();
    items.sort();
    semantic_text_hash(&items.join("\n"))
}

#[cfg(test)]
mod canonical_pro_progression_tests {
    use ctx_history_capture::{CaptureWorkLimit, ProviderImportSummary};

    use crate::commands::{
        import::{
            import_custom_history_with_canonical_pro_progression, CanonicalProSourceProgression,
        },
        search::{
            history_source_plugin_work_limit, progress_search_refresh_canonical_pro, RefreshArg,
        },
    };

    #[derive(Default)]
    struct TestCanonicalProProgression {
        frontier_checks: usize,
    }

    impl CanonicalProSourceProgression for TestCanonicalProProgression {
        fn progress_to_committed_core_frontier(&mut self) {
            self.frontier_checks += 1;
        }
    }

    #[test]
    fn daemon_refresh_shared_path_progresses_changed_or_failed_core_attempts() {
        let mut changed = ProviderImportSummary::default();
        changed.imported = 1;
        let no_op = ProviderImportSummary::default();
        let mut progression = TestCanonicalProProgression::default();

        // `run_daemon_history_refresh_job` delegates to the search refresh path
        // above, so this is the exact post-Core gate used by daemon refreshes.
        progress_search_refresh_canonical_pro(Some(&mut progression), Some(&changed));
        progress_search_refresh_canonical_pro(Some(&mut progression), Some(&no_op));
        progress_search_refresh_canonical_pro(Some(&mut progression), None);

        assert_eq!(progression.frontier_checks, 2);
    }

    #[test]
    fn daemon_plugin_refresh_progresses_one_core_page_before_followup() {
        let mut progression = TestCanonicalProProgression::default();
        let mut attempts = 0_usize;

        // Daemon history refresh passes `Background` to
        // `refresh_sources_for_search`, including for its selected plugin.
        let summary = import_custom_history_with_canonical_pro_progression(
            history_source_plugin_work_limit(RefreshArg::Background),
            Some(&mut progression),
            |work_limit| {
                assert_eq!(work_limit, CaptureWorkLimit::OneSafeGroup);
                attempts += 1;
                let mut summary = ProviderImportSummary::default();
                summary.imported = 1;
                summary.work_remaining = true;
                Ok(summary)
            },
        )
        .unwrap();

        assert_eq!(attempts, 1);
        assert_eq!(progression.frontier_checks, 1);
        assert!(summary.work_remaining);
    }
}
