use std::path::{Path, PathBuf};

use anyhow::Result;
use ctx_history_core::utc_now;
use ctx_pro_host_protocol::CoreMaterializationFinalizationProgress;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::{compact_json, pro::stable_error_code};

use super::super::paths_status::{
    daemon_jobs_path, read_daemon_job_status, read_daemon_job_status_strict,
    write_daemon_job_status,
};

const SOURCE_BACKED_PRO_CATCH_UP_STATUS_FILE: &str = "pro-catch-up.json";
pub(super) const SOURCE_BACKED_PRO_CATCH_UP_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum SourceBackedProCatchUpState {
    Pending,
    Error,
    Completed,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct SourceBackedProCatchUpStatus {
    pub(super) schema_version: u16,
    pub(super) owner: String,
    pub(super) kind: String,
    pub(super) status: SourceBackedProCatchUpState,
    pub(super) pending: bool,
    pub(super) retryable: bool,
    pub(super) core_generation_id: String,
    pub(super) receipt_core_generation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) finalization_progress: Option<CoreMaterializationFinalizationProgress>,
    pub(super) attempts: u64,
    pub(super) last_attempt_at_ms: i64,
    #[serde(default)]
    pub(super) last_attempt_duration_us: u64,
    pub(super) error_code: Option<String>,
    pub(super) last_error: Option<String>,
    #[serde(default)]
    pub(super) reason: Option<String>,
    #[serde(default)]
    pub(super) consecutive_failures: u32,
    #[serde(default)]
    pub(super) retry_after_ms: Option<u64>,
    #[serde(default)]
    pub(super) retry_not_before_at_ms: Option<i64>,
}

impl SourceBackedProCatchUpStatus {
    pub(super) fn pending(core_generation_id: &str, attempts: u64) -> Self {
        Self {
            schema_version: SOURCE_BACKED_PRO_CATCH_UP_SCHEMA_VERSION,
            owner: "daemon".to_owned(),
            kind: "source_backed_pro_catch_up".to_owned(),
            status: SourceBackedProCatchUpState::Pending,
            pending: true,
            retryable: true,
            core_generation_id: core_generation_id.to_owned(),
            receipt_core_generation_id: None,
            finalization_progress: None,
            attempts,
            last_attempt_at_ms: utc_now().timestamp_millis(),
            last_attempt_duration_us: 0,
            error_code: None,
            last_error: None,
            reason: None,
            consecutive_failures: 0,
            retry_after_ms: None,
            retry_not_before_at_ms: None,
        }
    }

    pub(super) fn error(mut self, error: SourceBackedProCatchUpError) -> Self {
        self.status = SourceBackedProCatchUpState::Error;
        self.pending = true;
        self.retryable = error.retryable();
        if !self.retryable {
            self.reason = Some(error.code().to_owned());
        }
        self.error_code = Some(error.code().to_owned());
        self.last_error = Some(error.to_string());
        self
    }

    pub(super) fn finalizing(mut self, progress: CoreMaterializationFinalizationProgress) -> Self {
        self.status = SourceBackedProCatchUpState::Pending;
        self.pending = true;
        self.retryable = false;
        self.receipt_core_generation_id = None;
        self.finalization_progress = Some(progress);
        self.error_code = None;
        self.last_error = None;
        self.reason = Some("finalizing".to_owned());
        self.consecutive_failures = 0;
        self.retry_after_ms = None;
        self.retry_not_before_at_ms = None;
        self
    }

    pub(super) fn completed(mut self, receipt_generation: String) -> Self {
        self.status = SourceBackedProCatchUpState::Completed;
        self.pending = false;
        self.retryable = false;
        self.receipt_core_generation_id = Some(receipt_generation);
        self.finalization_progress = None;
        self.error_code = None;
        self.last_error = None;
        self.reason = None;
        self.consecutive_failures = 0;
        self.retry_after_ms = None;
        self.retry_not_before_at_ms = None;
        self
    }

    pub(super) fn cancelled(mut self, reason: &str) -> Self {
        self.status = SourceBackedProCatchUpState::Error;
        self.pending = false;
        self.retryable = false;
        self.receipt_core_generation_id = None;
        self.finalization_progress = None;
        self.error_code = Some("cancelled".to_owned());
        self.last_error = Some(reason.to_owned());
        self.reason = Some("cancelled".to_owned());
        self.consecutive_failures = 0;
        self.retry_after_ms = None;
        self.retry_not_before_at_ms = None;
        self
    }

    pub(super) fn with_duration(mut self, duration_us: u64) -> Self {
        self.last_attempt_duration_us = duration_us;
        self
    }

    pub(super) fn is_completed_for(&self, core_generation_id: &str) -> bool {
        self.status == SourceBackedProCatchUpState::Completed
            && self.core_generation_id == core_generation_id
            && self.receipt_core_generation_id.as_deref() == Some(core_generation_id)
    }

    pub(super) fn is_scheduled_target(&self) -> bool {
        self.pending
            && (self.retryable || self.reason.as_deref() == Some("finalizing"))
            && self.status != SourceBackedProCatchUpState::Completed
    }

    pub(super) fn to_json(&self) -> Result<Value> {
        Ok(compact_json(serde_json::to_value(self)?))
    }
}

#[derive(Debug, Error)]
pub(super) enum SourceBackedProCatchUpError {
    #[error(
        "source_pro_generation_mismatch: expected Core generation {expected}, but {authority} \
         was supplied by {surface}"
    )]
    GenerationMismatch {
        expected: String,
        authority: String,
        surface: &'static str,
    },
    #[error("source_pro_index_unavailable: {0}")]
    IndexUnavailable(String),
    #[error("{code}: {message}")]
    Projection { code: String, message: String },
}

impl SourceBackedProCatchUpError {
    pub(super) fn code(&self) -> &str {
        match self {
            Self::GenerationMismatch { .. } => "source_pro_generation_mismatch",
            Self::IndexUnavailable(_) => "source_pro_index_unavailable",
            Self::Projection { code, .. } => code,
        }
    }

    pub(super) fn projection(error: anyhow::Error) -> Self {
        Self::Projection {
            code: stable_error_code(&error)
                .unwrap_or("pro_core_materialization_unavailable")
                .to_owned(),
            message: error.to_string(),
        }
    }

    pub(super) fn retryable(&self) -> bool {
        match self {
            Self::GenerationMismatch { .. } => false,
            Self::IndexUnavailable(_) => true,
            Self::Projection { code, .. } => !matches!(
                code.as_str(),
                "pro_not_installed" | "invalid_response" | "protocol_mismatch" | "cancelled"
            ),
        }
    }
}

pub(super) fn status_path(data_root: &Path) -> PathBuf {
    daemon_jobs_path(data_root).join(SOURCE_BACKED_PRO_CATCH_UP_STATUS_FILE)
}

pub(super) fn read_status(data_root: &Path) -> Option<SourceBackedProCatchUpStatus> {
    read_status_json(data_root).and_then(|value| serde_json::from_value(value).ok())
}

enum DurableProCatchUpJobRead {
    Missing,
    Valid(SourceBackedProCatchUpStatus),
    Malformed(anyhow::Error),
}

fn read_durable_status(data_root: &Path) -> DurableProCatchUpJobRead {
    let value = match read_daemon_job_status_strict(&status_path(data_root)) {
        Ok(Some(value)) => value,
        Ok(None) => return DurableProCatchUpJobRead::Missing,
        Err(error) => {
            return DurableProCatchUpJobRead::Malformed(anyhow::anyhow!(
                "invalid_response: durable source-backed Pro catch-up job is unreadable: {error:#}"
            ))
        }
    };
    match serde_json::from_value(value) {
        Ok(status) => DurableProCatchUpJobRead::Valid(status),
        Err(error) => DurableProCatchUpJobRead::Malformed(anyhow::anyhow!(
            "invalid_response: durable source-backed Pro catch-up job is malformed: {error}"
        )),
    }
}

pub(super) fn require_durable_status(
    data_root: &Path,
) -> Result<Option<SourceBackedProCatchUpStatus>> {
    match read_durable_status(data_root) {
        DurableProCatchUpJobRead::Missing => Ok(None),
        DurableProCatchUpJobRead::Valid(status) => Ok(Some(status)),
        DurableProCatchUpJobRead::Malformed(error) => Err(error),
    }
}

pub(super) fn persist_status(
    data_root: &Path,
    status: &SourceBackedProCatchUpStatus,
) -> Result<()> {
    write_daemon_job_status(&status_path(data_root), &status.to_json()?)
}

pub(in crate::semantic) fn read_status_json(data_root: &Path) -> Option<Value> {
    read_daemon_job_status(&status_path(data_root))
}

pub(in crate::semantic) fn persist_status_json(data_root: &Path, status: &Value) -> Result<()> {
    write_daemon_job_status(&status_path(data_root), status)
}

pub(in crate::semantic) fn status_generation(data_root: &Path) -> Option<String> {
    read_status(data_root).map(|status| status.core_generation_id)
}

pub(in crate::semantic) fn scheduled_target_generation(data_root: &Path) -> Result<Option<String>> {
    let Some(status) = require_durable_status(data_root)? else {
        return Ok(None);
    };
    if !status.is_scheduled_target() {
        return Ok(None);
    }
    if let Some(progress) = &status.finalization_progress {
        progress.validate().map_err(|error| {
            anyhow::anyhow!("invalid durable Pro finalization target: {}", error.message)
        })?;
        if progress.core_generation_id != status.core_generation_id {
            anyhow::bail!(
                "invalid durable Pro finalization target: progress generation does not match its job"
            );
        }
    }
    Ok(Some(status.core_generation_id))
}

pub(in crate::semantic) fn status_has_finalization_pending(
    data_root: &Path,
    core_generation_id: &str,
) -> bool {
    read_status(data_root).is_some_and(|status| {
        status.status == SourceBackedProCatchUpState::Pending
            && status.pending
            && !status.retryable
            && status.reason.as_deref() == Some("finalizing")
            && status.core_generation_id == core_generation_id
            && status
                .finalization_progress
                .as_ref()
                .is_some_and(|progress| progress.core_generation_id == core_generation_id)
    })
}
