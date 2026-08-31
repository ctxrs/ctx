use std::path::Path;

use anyhow::Result;
use serde_json::Value;

use crate::daemon_retry::{semantic_failure_class_from_job, SemanticFailureClass};
use crate::paths_status::{
    daemon_semantic_job_path, daemon_status_path, read_daemon_job_status_strict,
};

/// Exact daemon configuration authority required by one semantic completion
/// request. The caller supplies policy; this type deliberately does not assume
/// automatic indexing, Full mode, or daemon ownership.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonSemanticConfigBinding {
    daemon_enabled: bool,
    daemon_mode: String,
    semantic_enabled: bool,
    semantic_executor: String,
    semantic_contract_fingerprint: String,
}

impl DaemonSemanticConfigBinding {
    pub fn new(
        daemon_enabled: bool,
        daemon_mode: impl Into<String>,
        semantic_enabled: bool,
        semantic_executor: impl Into<String>,
        semantic_contract_fingerprint: impl Into<String>,
    ) -> Self {
        Self {
            daemon_enabled,
            daemon_mode: daemon_mode.into(),
            semantic_enabled,
            semantic_executor: semantic_executor.into(),
            semantic_contract_fingerprint: semantic_contract_fingerprint.into(),
        }
    }
}

/// Durable identity that must match before a daemon-owned semantic generation
/// can be accepted as complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonSemanticCompletionTarget {
    core_generation_id: String,
    model_contract_fingerprint: String,
    source_contract_fingerprint: String,
    config: DaemonSemanticConfigBinding,
}

impl DaemonSemanticCompletionTarget {
    pub fn new(
        core_generation_id: impl Into<String>,
        model_contract_fingerprint: impl Into<String>,
        source_contract_fingerprint: impl Into<String>,
        config: DaemonSemanticConfigBinding,
    ) -> Self {
        Self {
            core_generation_id: core_generation_id.into(),
            model_contract_fingerprint: model_contract_fingerprint.into(),
            source_contract_fingerprint: source_contract_fingerprint.into(),
            config,
        }
    }

    pub fn core_generation_id(&self) -> &str {
        &self.core_generation_id
    }
}

/// Daemon semantic state observed while waiting for one exact target.
///
/// The timestamps are liveness receipts for diagnostics and retry scheduling,
/// not semantic-progress evidence. Use [`Self::substantively_advances_from`]
/// when accounting against a bounded no-progress budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonSemanticProgress {
    pub reload_status: Option<String>,
    pub reload_last_attempt_at_ms: Option<i64>,
    pub reload_last_applied_at_ms: Option<i64>,
    pub requested_config_matches: bool,
    pub applied_config_matches: bool,
    pub job_target_matches: bool,
    pub job_status: Option<String>,
    pub job_last_run_at_ms: Option<i64>,
    /// Opaque durable sequence owned by the exact semantic projection target.
    /// It is the only no-progress-budget authority.
    pub job_semantic_progress_sequence: Option<u64>,
    pub job_indexed_chunks: Option<u64>,
    pub job_source_generation_ready: Option<bool>,
    pub job_source_work_remaining: Option<bool>,
}

impl DaemonSemanticProgress {
    /// Returns whether this observation proves a substantive advance beyond
    /// the last one accepted for the same completion attempt.
    ///
    /// Only a strict increase in the durable sequence is an advance. Reload
    /// timestamps, job-status churn, source counters, retries, deferrals,
    /// target changes, and malformed receipts deliberately do not reset the
    /// no-progress budget.
    pub fn substantively_advances_from(&self, previous: Option<&Self>) -> bool {
        self.exact_config_is_active()
            && self.job_target_matches
            && self
                .job_semantic_progress_sequence
                .is_some_and(|current| current > 0)
            && previous
                .and_then(|progress| progress.job_semantic_progress_sequence)
                .is_none_or(|previous| {
                    self.job_semantic_progress_sequence
                        .is_some_and(|current| current > previous)
                })
    }

    fn exact_config_is_active(&self) -> bool {
        self.reload_status.as_deref() == Some("applied")
            && self.requested_config_matches
            && self.applied_config_matches
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonSemanticCompletionObservation {
    Ready,
    Pending(DaemonSemanticProgress),
    Unavailable {
        detail: String,
    },
    ActivationFailed {
        detail: String,
        retryable: bool,
    },
    ConfigurationFailed {
        detail: String,
        retryable: bool,
    },
    JobFailed {
        detail: String,
        retryable: bool,
        failure_class: Option<String>,
    },
}

/// Strictly reads the daemon lifecycle and semantic job receipts, then
/// classifies them against one exact V2 generation/configuration target.
pub fn observe_exact_daemon_semantic_completion(
    data_root: &Path,
    target: &DaemonSemanticCompletionTarget,
) -> Result<DaemonSemanticCompletionObservation> {
    let Some(status) = read_daemon_job_status_strict(&daemon_status_path(data_root))? else {
        return Ok(DaemonSemanticCompletionObservation::Unavailable {
            detail: "daemon lifecycle status is unavailable".to_owned(),
        });
    };
    if status.get("status").and_then(Value::as_str) != Some("running") {
        return Ok(DaemonSemanticCompletionObservation::Unavailable {
            detail: status
                .get("last_error")
                .and_then(Value::as_str)
                .unwrap_or("daemon is not running")
                .to_owned(),
        });
    }
    let job = read_daemon_job_status_strict(&daemon_semantic_job_path(data_root))?;
    Ok(classify_exact_daemon_semantic_completion(
        &status,
        job.as_ref(),
        target,
    ))
}

pub fn classify_exact_daemon_semantic_completion(
    daemon_status: &Value,
    semantic_job: Option<&Value>,
    target: &DaemonSemanticCompletionTarget,
) -> DaemonSemanticCompletionObservation {
    let reload = daemon_status.get("config_reload");
    let reload_status = reload
        .and_then(|value| value.get("status"))
        .and_then(Value::as_str);
    let requested_config_matches = reload
        .and_then(|value| value.get("requested"))
        .is_some_and(|config| config_matches(config, &target.config));
    let applied_config_matches = reload
        .and_then(|value| value.get("applied"))
        .is_some_and(|config| config_matches(config, &target.config));
    let exact_reload = requested_config_matches.then_some(reload).flatten();
    let exact_job = semantic_job.filter(|job| job_matches(job, target));

    if requested_config_matches && reload_status == Some("activation_failed") {
        return DaemonSemanticCompletionObservation::ActivationFailed {
            detail: reload
                .and_then(|value| value.get("last_error"))
                .and_then(Value::as_str)
                .unwrap_or("daemon semantic activation failed")
                .to_owned(),
            retryable: true,
        };
    }
    if requested_config_matches && reload_status == Some("failed") {
        return DaemonSemanticCompletionObservation::ConfigurationFailed {
            detail: reload
                .and_then(|value| value.get("last_error"))
                .and_then(Value::as_str)
                .unwrap_or("daemon configuration reload failed")
                .to_owned(),
            retryable: true,
        };
    }

    let selected_config_active =
        requested_config_matches && applied_config_matches && reload_status == Some("applied");
    if let Some(job) = exact_job.filter(|_| selected_config_active) {
        let job_status = job.get("status").and_then(Value::as_str);
        if job_status == Some("ready") {
            return DaemonSemanticCompletionObservation::Ready;
        }
        let blocking_legacy_failure = job_status == Some("skipped")
            && semantic_failure_class_from_job(job)
                .is_some_and(SemanticFailureClass::blocks_until_restart);
        if job_status == Some("failed") || blocking_legacy_failure {
            return DaemonSemanticCompletionObservation::JobFailed {
                detail: job
                    .get("last_error")
                    .and_then(Value::as_str)
                    .or_else(|| job.get("reason").and_then(Value::as_str))
                    .unwrap_or("daemon semantic job failed")
                    .to_owned(),
                retryable: job
                    .get("retryable")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                failure_class: job
                    .get("failure_class")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            };
        }
    }

    DaemonSemanticCompletionObservation::Pending(DaemonSemanticProgress {
        reload_status: exact_reload
            .and_then(|value| value.get("status"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        reload_last_attempt_at_ms: exact_reload
            .and_then(|value| value.get("last_attempt_at_ms"))
            .and_then(Value::as_i64),
        reload_last_applied_at_ms: exact_reload
            .and_then(|value| value.get("last_applied_at_ms"))
            .and_then(Value::as_i64),
        requested_config_matches,
        applied_config_matches,
        job_target_matches: exact_job.is_some(),
        job_status: exact_job
            .and_then(|job| job.get("status"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        job_last_run_at_ms: exact_job
            .and_then(|job| job.get("last_run_at_ms"))
            .and_then(Value::as_i64),
        job_semantic_progress_sequence: exact_job
            .and_then(|job| job.get("semantic_progress_sequence"))
            .and_then(Value::as_u64)
            .filter(|sequence| *sequence > 0),
        job_indexed_chunks: exact_job
            .and_then(|job| job.get("indexed_chunks"))
            .and_then(Value::as_u64),
        job_source_generation_ready: exact_job
            .and_then(|job| job.get("source_generation_ready"))
            .and_then(Value::as_bool),
        job_source_work_remaining: exact_job
            .and_then(|job| job.get("source_work_remaining"))
            .and_then(Value::as_bool),
    })
}

fn config_matches(config: &Value, expected: &DaemonSemanticConfigBinding) -> bool {
    config.get("daemon_enabled").and_then(Value::as_bool) == Some(expected.daemon_enabled)
        && config.get("daemon_mode").and_then(Value::as_str) == Some(expected.daemon_mode.as_str())
        && config.get("semantic_enabled").and_then(Value::as_bool)
            == Some(expected.semantic_enabled)
        && config.get("semantic_executor").and_then(Value::as_str)
            == Some(expected.semantic_executor.as_str())
        && config
            .get("semantic_contract_fingerprint")
            .and_then(Value::as_str)
            == Some(expected.semantic_contract_fingerprint.as_str())
}

fn job_matches(job: &Value, target: &DaemonSemanticCompletionTarget) -> bool {
    job.get("core_generation_id").and_then(Value::as_str)
        == Some(target.core_generation_id.as_str())
        && job
            .get("model_contract_fingerprint")
            .and_then(Value::as_str)
            == Some(target.model_contract_fingerprint.as_str())
        && job
            .get("source_contract_fingerprint")
            .and_then(Value::as_str)
            == Some(target.source_contract_fingerprint.as_str())
}

#[cfg(test)]
#[path = "semantic_completion/tests.rs"]
mod tests;
