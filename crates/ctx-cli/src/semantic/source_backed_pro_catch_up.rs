use std::{
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use ctx_history_core::utc_now;
use ctx_history_index::VerifiedIndex;
#[cfg(test)]
use ctx_pro_host_protocol::CoreMaterializationFinalizationPhase;
use ctx_pro_host_protocol::{
    CoreMaterializationFinalizationPending, CoreMaterializationFinalizationProgress,
    CoreMaterializationReceipt,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::{
    compact_json,
    pro::{
        preflight_core_materialization, stable_error_code, sync_core_materialization,
        CoreMaterializationSyncOutcome,
    },
};

use super::{
    paths_status::{daemon_jobs_path, read_daemon_job_status, write_daemon_job_status},
    source_backed_refresh_coordinator::{
        nonzero_duration_micros, open_verified_index, source_backed_index_root,
        PinnedCorePublication, PinnedSourceBackedGeneration,
    },
};

#[path = "source_backed_pro_catch_up/recheck.rs"]
mod recheck;

#[cfg(test)]
use recheck::path as recheck_path;
pub(super) use recheck::schedule as helper_recheck_schedule;
use recheck::{
    complete as complete_observed_recheck, read as read_recheck_request,
    read_unlocked as read_recheck_request_unlocked, with_lock as with_recheck_lock,
};
pub(crate) use recheck::{
    publish as publish_helper_recheck_intent, targets as helper_recheck_targets,
    wake as wake_helper_recheck,
};

const SOURCE_BACKED_PRO_CATCH_UP_STATUS_FILE: &str = "pro-catch-up.json";
const SOURCE_BACKED_PRO_CATCH_UP_SCHEMA_VERSION: u16 = 1;
const SOURCE_BACKED_PRO_CATCH_UP_POLL_INTERVAL: Duration = Duration::from_millis(50);
const SOURCE_BACKED_PRO_CATCH_UP_WAIT_TIMEOUT: Duration = Duration::from_secs(60 * 60);
const SOURCE_BACKED_PRO_CATCH_UP_WAKE_TIMEOUT: Duration = Duration::from_millis(500);
const SOURCE_BACKED_PRO_CATCH_UP_WAKE_RESPONSE_MAX_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum SourceBackedProCatchUpState {
    Pending,
    Error,
    Completed,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
struct SourceBackedProCatchUpStatus {
    schema_version: u16,
    owner: String,
    kind: String,
    status: SourceBackedProCatchUpState,
    pending: bool,
    retryable: bool,
    core_generation_id: String,
    receipt_core_generation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    finalization_progress: Option<CoreMaterializationFinalizationProgress>,
    attempts: u64,
    last_attempt_at_ms: i64,
    #[serde(default)]
    last_attempt_duration_us: u64,
    error_code: Option<String>,
    last_error: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    consecutive_failures: u32,
    #[serde(default)]
    retry_after_ms: Option<u64>,
    #[serde(default)]
    retry_not_before_at_ms: Option<i64>,
}

impl SourceBackedProCatchUpStatus {
    fn pending(core_generation_id: &str, attempts: u64) -> Self {
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

    fn error(mut self, error: SourceBackedProCatchUpError) -> Self {
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

    fn finalizing(mut self, progress: CoreMaterializationFinalizationProgress) -> Self {
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

    fn completed(mut self, receipt_generation: String) -> Self {
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

    fn with_duration(mut self, duration_us: u64) -> Self {
        self.last_attempt_duration_us = duration_us;
        self
    }

    fn is_completed_for(&self, core_generation_id: &str) -> bool {
        self.status == SourceBackedProCatchUpState::Completed
            && self.core_generation_id == core_generation_id
            && self.receipt_core_generation_id.as_deref() == Some(core_generation_id)
    }

    fn is_scheduled_target(&self) -> bool {
        self.pending
            && (self.retryable || self.reason.as_deref() == Some("finalizing"))
            && self.status != SourceBackedProCatchUpState::Completed
    }

    fn to_json(&self) -> Result<Value> {
        Ok(compact_json(serde_json::to_value(self)?))
    }
}

#[derive(Debug, Error)]
enum SourceBackedProCatchUpError {
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
    fn code(&self) -> &str {
        match self {
            Self::GenerationMismatch { .. } => "source_pro_generation_mismatch",
            Self::IndexUnavailable(_) => "source_pro_index_unavailable",
            Self::Projection { code, .. } => code,
        }
    }

    fn projection(error: anyhow::Error) -> Self {
        Self::Projection {
            code: stable_error_code(&error)
                .unwrap_or("pro_core_materialization_unavailable")
                .to_owned(),
            message: error.to_string(),
        }
    }

    fn retryable(&self) -> bool {
        match self {
            Self::GenerationMismatch { .. } => false,
            Self::IndexUnavailable(_) => true,
            Self::Projection { code, .. } => !matches!(
                code.as_str(),
                "pro_not_installed" | "invalid_response" | "protocol_mismatch"
            ),
        }
    }
}

pub(super) struct SourceBackedProCatchUpRun {
    pub(super) status: Value,
    pub(super) did_work: bool,
    pub(super) continuation_pending: bool,
}

#[derive(Clone, Copy)]
pub(super) enum SourceBackedProCoreAuthority<'a> {
    Retained(&'a PinnedCorePublication),
    Durable(&'a PinnedSourceBackedGeneration),
}

impl<'a> SourceBackedProCoreAuthority<'a> {
    pub(super) fn generation_id(self) -> &'a str {
        match self {
            Self::Retained(authority) => authority.generation_id(),
            Self::Durable(authority) => authority.generation_id(),
        }
    }

    fn verified_index(self) -> &'a VerifiedIndex {
        match self {
            Self::Retained(authority) => authority.verified_index_ref(),
            Self::Durable(authority) => authority.verified_index(),
        }
    }

    fn surface(self) -> &'static str {
        match self {
            Self::Retained(_) => "retained Core generation pin",
            Self::Durable(_) => "durable active Core generation pin",
        }
    }
}

pub(super) fn run_after_core_publication(
    data_root: &Path,
    core_generation_id: &str,
    authority: SourceBackedProCoreAuthority<'_>,
) -> Result<SourceBackedProCatchUpRun> {
    if authority.generation_id() != core_generation_id {
        return record_preflight_error(
            data_root,
            core_generation_id,
            SourceBackedProCatchUpError::GenerationMismatch {
                expected: core_generation_id.to_owned(),
                authority: authority.generation_id().to_owned(),
                surface: authority.surface(),
            },
        );
    }
    run_with(
        data_root,
        core_generation_id,
        ProCatchUpAuthority {
            generation_id: Some(authority.generation_id()),
            verified_index: Some(authority.verified_index()),
        },
        preflight_core_materialization,
        |data_root, index| {
            Ok(match sync_core_materialization(data_root, index)? {
                CoreMaterializationSyncOutcome::Finished {
                    receipt,
                    did_work,
                    helper_artifact_sha256,
                } => ProCatchUpSyncOutcome::Finished {
                    receipt,
                    did_work,
                    helper_artifact_sha256,
                },
                CoreMaterializationSyncOutcome::FinalizationPending { pending } => {
                    ProCatchUpSyncOutcome::FinalizationPending { pending }
                }
            })
        },
    )
}

struct ProCatchUpAuthority<'a> {
    generation_id: Option<&'a str>,
    verified_index: Option<&'a VerifiedIndex>,
}

enum ProCatchUpSyncOutcome {
    Finished {
        receipt: CoreMaterializationReceipt,
        did_work: bool,
        helper_artifact_sha256: String,
    },
    FinalizationPending {
        pending: CoreMaterializationFinalizationPending,
    },
}

fn run_with<Preflight, Sync>(
    data_root: &Path,
    core_generation_id: &str,
    authority: ProCatchUpAuthority<'_>,
    preflight: Preflight,
    sync: Sync,
) -> Result<SourceBackedProCatchUpRun>
where
    Preflight: FnOnce(&Path) -> Result<()>,
    Sync: FnOnce(&Path, &VerifiedIndex) -> Result<ProCatchUpSyncOutcome>,
{
    // Capture the exact target this run is allowed to satisfy. The completion
    // path additionally requires the helper identity reported by this exact
    // materialization session, so an old helper cannot clear a newer intent.
    let observed_recheck = read_recheck_request(data_root)?;
    let prior = read_status(data_root);
    let attempts = next_attempt(prior.as_ref(), core_generation_id);
    let attempt_started = Instant::now();
    let pending = prior
        .as_ref()
        .filter(|status| status.core_generation_id == core_generation_id)
        .and_then(|status| status.finalization_progress.clone())
        .map(|progress| {
            SourceBackedProCatchUpStatus::pending(core_generation_id, attempts).finalizing(progress)
        })
        .unwrap_or_else(|| SourceBackedProCatchUpStatus::pending(core_generation_id, attempts));
    persist_status(data_root, &pending)?;

    let result = (|| {
        if let Some(generation_id) = authority.generation_id {
            require_generation(
                core_generation_id,
                generation_id,
                "retained Core generation pin",
            )?;
        }
        preflight(data_root).map_err(SourceBackedProCatchUpError::projection)?;
        let opened_index: VerifiedIndex;
        let index = match authority.verified_index {
            Some(index) => index,
            None => {
                opened_index = open_exact_index(data_root, core_generation_id)?;
                &opened_index
            }
        };
        require_generation(
            core_generation_id,
            index.generation_id(),
            "pinned VerifiedIndex",
        )?;
        let outcome = sync(data_root, index).map_err(SourceBackedProCatchUpError::projection)?;
        match &outcome {
            ProCatchUpSyncOutcome::Finished { receipt, .. } => require_generation(
                core_generation_id,
                &receipt.core_generation_id,
                "Pro Core materialization receipt",
            )?,
            ProCatchUpSyncOutcome::FinalizationPending { pending } => require_generation(
                core_generation_id,
                &pending.progress.core_generation_id,
                "Pro Core finalization progress",
            )?,
        }
        Ok(outcome)
    })();

    match result {
        Ok(ProCatchUpSyncOutcome::Finished {
            receipt,
            did_work,
            helper_artifact_sha256,
        }) => {
            let completed = pending
                .completed(receipt.core_generation_id)
                .with_duration(nonzero_duration_micros(attempt_started.elapsed()));
            persist_status(data_root, &completed)?;
            complete_observed_recheck(
                data_root,
                observed_recheck.as_ref(),
                &helper_artifact_sha256,
            )?;
            Ok(SourceBackedProCatchUpRun {
                status: completed.to_json()?,
                did_work,
                continuation_pending: false,
            })
        }
        Ok(ProCatchUpSyncOutcome::FinalizationPending {
            pending: finalization,
        }) => {
            let did_work = !finalization.replayed;
            let finalizing = pending
                .finalizing(finalization.progress)
                .with_duration(nonzero_duration_micros(attempt_started.elapsed()));
            persist_status(data_root, &finalizing)?;
            Ok(SourceBackedProCatchUpRun {
                status: finalizing.to_json()?,
                did_work,
                continuation_pending: true,
            })
        }
        Err(error) => {
            let failed = pending
                .error(error)
                .with_duration(nonzero_duration_micros(attempt_started.elapsed()));
            persist_status(data_root, &failed)?;
            Ok(SourceBackedProCatchUpRun {
                status: failed.to_json()?,
                did_work: false,
                continuation_pending: false,
            })
        }
    }
}

fn record_preflight_error(
    data_root: &Path,
    core_generation_id: &str,
    error: SourceBackedProCatchUpError,
) -> Result<SourceBackedProCatchUpRun> {
    let prior = read_status(data_root);
    let attempts = next_attempt(prior.as_ref(), core_generation_id);
    let failed = SourceBackedProCatchUpStatus::pending(core_generation_id, attempts).error(error);
    persist_status(data_root, &failed)?;
    Ok(SourceBackedProCatchUpRun {
        status: failed.to_json()?,
        did_work: false,
        continuation_pending: false,
    })
}

fn next_attempt(prior: Option<&SourceBackedProCatchUpStatus>, core_generation_id: &str) -> u64 {
    prior
        .filter(|status| status.core_generation_id == core_generation_id)
        .map(|status| status.attempts.saturating_add(1))
        .unwrap_or(1)
}

fn require_generation(
    expected: &str,
    authority: &str,
    surface: &'static str,
) -> std::result::Result<(), SourceBackedProCatchUpError> {
    if authority == expected {
        return Ok(());
    }
    Err(SourceBackedProCatchUpError::GenerationMismatch {
        expected: expected.to_owned(),
        authority: authority.to_owned(),
        surface,
    })
}

fn open_exact_index(
    data_root: &Path,
    core_generation_id: &str,
) -> std::result::Result<VerifiedIndex, SourceBackedProCatchUpError> {
    let index_root = source_backed_index_root(data_root);
    let index = open_verified_index(&index_root)
        .with_context(|| format!("open verified Core index {}", index_root.display()))
        .map_err(|error| SourceBackedProCatchUpError::IndexUnavailable(format!("{error:#}")))?;
    require_generation(
        core_generation_id,
        index.generation_id(),
        "pinned VerifiedIndex",
    )?;
    Ok(index)
}

fn status_path(data_root: &Path) -> PathBuf {
    daemon_jobs_path(data_root).join(SOURCE_BACKED_PRO_CATCH_UP_STATUS_FILE)
}

fn read_status(data_root: &Path) -> Option<SourceBackedProCatchUpStatus> {
    read_status_json(data_root).and_then(|value| serde_json::from_value(value).ok())
}

fn persist_status(data_root: &Path, status: &SourceBackedProCatchUpStatus) -> Result<()> {
    write_daemon_job_status(&status_path(data_root), &status.to_json()?)
}

pub(super) fn read_status_json(data_root: &Path) -> Option<Value> {
    read_daemon_job_status(&status_path(data_root))
}

pub(super) fn persist_status_json(data_root: &Path, status: &Value) -> Result<()> {
    write_daemon_job_status(&status_path(data_root), status)
}

pub(super) fn status_generation(data_root: &Path) -> Option<String> {
    read_status(data_root).map(|status| status.core_generation_id)
}

pub(super) fn scheduled_target_generation(data_root: &Path) -> Result<Option<String>> {
    let Some(value) = read_status_json(data_root) else {
        return Ok(None);
    };
    let status: SourceBackedProCatchUpStatus = serde_json::from_value(value)
        .context("decode durable source-backed Pro catch-up target")?;
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

pub(super) fn status_has_finalization_pending(data_root: &Path, core_generation_id: &str) -> bool {
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

/// Waits for the daemon-owned Pro projection of one exact Core generation.
///
/// This is deliberately a status-only seam. The CLI must not reread provider
/// sources or materialize Pro itself; that work remains owned by the daemon
/// tick that retained the generation-bound Core authority.
pub(crate) fn wait_for_completed_generation(
    data_root: &Path,
    core_generation_id: &str,
) -> Result<()> {
    wait_for_completed_generation_with(
        data_root,
        core_generation_id,
        SOURCE_BACKED_PRO_CATCH_UP_WAIT_TIMEOUT,
        || thread::sleep(SOURCE_BACKED_PRO_CATCH_UP_POLL_INTERVAL),
    )
}

fn wait_for_completed_generation_with(
    data_root: &Path,
    core_generation_id: &str,
    timeout: Duration,
    mut wait: impl FnMut(),
) -> Result<()> {
    let started = Instant::now();
    loop {
        let (pending_recheck, status) = with_recheck_lock(data_root, || {
            Ok((
                read_recheck_request_unlocked(data_root)?.is_some(),
                read_status(data_root),
            ))
        })?;
        if let Some(status) = status {
            if !pending_recheck && status.is_completed_for(core_generation_id) {
                return Ok(());
            }
            if status.core_generation_id == core_generation_id
                && status.status == SourceBackedProCatchUpState::Error
            {
                let code = status.error_code.as_deref().unwrap_or("not_materialized");
                let detail = status
                    .last_error
                    .as_deref()
                    .unwrap_or("daemon source-backed Pro catch-up failed");
                anyhow::bail!("{code}: {detail}");
            }
        }
        if started.elapsed() >= timeout {
            anyhow::bail!(
                "not_materialized: timed out waiting for daemon source-backed Pro generation {core_generation_id}"
            );
        }
        wait();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use ctx_history_core::{
        CertifiedSource, ScannedSourceCounts, SourceAnchor, SourceKey, SourceObservation, TypedKey,
    };
    use ctx_history_index::{GenerationWriter, WriterOptions};

    use crate::semantic::source_backed_refresh_coordinator::{
        count_verified_index_opens, pin_retained_generation,
    };

    use super::*;

    fn empty_index(data_root: &Path) -> VerifiedIndex {
        GenerationWriter::open(
            source_backed_index_root(data_root),
            WriterOptions::default(),
        )
        .unwrap()
        .into_writer()
        .unwrap()
        .commit(|_| true)
        .unwrap();
        open_verified_index(&source_backed_index_root(data_root)).unwrap()
    }

    fn index_with_certified_source_at(
        data_root: &Path,
        source_path: &str,
        observation_byte: u8,
    ) -> VerifiedIndex {
        let source = SourceKey::derive(
            "codex",
            "codex_session_jsonl",
            "session",
            1,
            SourceAnchor::provider_native("session-file", TypedKey::utf8(source_path).unwrap())
                .unwrap(),
        )
        .unwrap();
        let observation =
            SourceObservation::new(source.clone(), "regular-file-v1", vec![observation_byte])
                .unwrap();
        let mut writer = GenerationWriter::open(
            source_backed_index_root(data_root),
            WriterOptions::default(),
        )
        .unwrap()
        .into_writer()
        .unwrap();
        writer.begin_source(source).unwrap();
        writer
            .certify_source(
                CertifiedSource::certify(
                    observation.clone(),
                    observation,
                    "continuation-test-parser-v1",
                    [observation_byte; 32],
                    ScannedSourceCounts::default(),
                )
                .unwrap(),
            )
            .unwrap();
        writer.commit(|_| true).unwrap();
        open_verified_index(&source_backed_index_root(data_root)).unwrap()
    }

    fn receipt_with_revision(
        index: &VerifiedIndex,
        materializer_revision: &str,
    ) -> CoreMaterializationReceipt {
        CoreMaterializationReceipt {
            core_generation_id: index.generation_id().to_owned(),
            core_record_contract_fingerprint: index
                .manifest()
                .core_record_contract_fingerprint
                .clone(),
            source_snapshot_sha256: "a".repeat(64),
            materializer_revision: materializer_revision.to_owned(),
            source_count: 0,
            event_count: 0,
        }
    }

    fn sync_outcome(
        index: &VerifiedIndex,
        materializer_revision: &str,
        did_work: bool,
    ) -> ProCatchUpSyncOutcome {
        ProCatchUpSyncOutcome::Finished {
            receipt: receipt_with_revision(index, materializer_revision),
            did_work,
            helper_artifact_sha256: "a".repeat(64),
        }
    }

    fn finalization_outcome(
        index: &VerifiedIndex,
        phase: CoreMaterializationFinalizationPhase,
        cursor: char,
        replayed: bool,
    ) -> ProCatchUpSyncOutcome {
        ProCatchUpSyncOutcome::FinalizationPending {
            pending: CoreMaterializationFinalizationPending {
                progress: CoreMaterializationFinalizationProgress {
                    materialization_id: "b".repeat(64),
                    core_generation_id: index.generation_id().to_owned(),
                    finish_request_digest: "d".repeat(64),
                    materializer_revision: "test-core-materializer-v1".to_owned(),
                    phase,
                    cursor_sha256: cursor.to_string().repeat(64),
                },
                replayed,
            },
        }
    }

    #[test]
    fn durable_state_path_is_purpose_based() {
        assert_eq!(
            status_path(Path::new("ctx-data")),
            Path::new("ctx-data/daemon/jobs/pro-catch-up.json")
        );
        assert_eq!(
            recheck_path(Path::new("ctx-data")),
            Path::new("ctx-data/daemon/jobs/pro-catch-up-recheck.json")
        );
    }

    #[test]
    fn catch_up_reuses_pinned_core_and_persists_exact_receipt_generation() {
        let temp = tempfile::tempdir().unwrap();
        let index = empty_index(temp.path());
        let generation = index.generation_id().to_owned();
        let (run, opens) = count_verified_index_opens(|| {
            run_with(
                temp.path(),
                &generation,
                ProCatchUpAuthority {
                    generation_id: Some(&generation),
                    verified_index: Some(&index),
                },
                |_| Ok(()),
                |_, supplied| {
                    assert_eq!(supplied.generation_id(), generation);
                    Ok(sync_outcome(supplied, "test-core-materializer-v1", true))
                },
            )
            .unwrap()
        });
        assert_eq!(opens, 0);
        assert!(run.did_work);
        assert_eq!(run.status["status"], "completed");
        assert_eq!(run.status["receipt_core_generation_id"], generation);
    }

    #[test]
    fn finalization_pending_is_a_successful_non_backoff_yield() {
        let temp = tempfile::tempdir().unwrap();
        let index = empty_index(temp.path());
        let generation = index.generation_id().to_owned();
        let run = run_with(
            temp.path(),
            &generation,
            ProCatchUpAuthority {
                generation_id: Some(&generation),
                verified_index: Some(&index),
            },
            |_| Ok(()),
            |_, supplied| {
                Ok(finalization_outcome(
                    supplied,
                    CoreMaterializationFinalizationPhase::EmitReplay,
                    'c',
                    false,
                ))
            },
        )
        .unwrap();

        assert!(run.did_work);
        assert!(run.continuation_pending);
        assert_eq!(run.status["status"], "pending");
        assert_eq!(run.status["pending"], true);
        assert_eq!(run.status["retryable"], false);
        assert_eq!(run.status["reason"], "finalizing");
        assert!(run.status["error_code"].is_null());
        assert_eq!(
            run.status["finalization_progress"]["core_generation_id"],
            generation
        );
        assert!(status_has_finalization_pending(temp.path(), &generation));
    }

    #[test]
    fn lost_pending_response_keeps_exact_target_after_core_advances() {
        let temp = tempfile::tempdir().unwrap();
        let index = index_with_certified_source_at(temp.path(), "continuation-old-core.jsonl", 1);
        let generation = index.generation_id().to_owned();
        let lost = run_with(
            temp.path(),
            &generation,
            ProCatchUpAuthority {
                generation_id: Some(&generation),
                verified_index: Some(&index),
            },
            |_| Ok(()),
            |_, _| anyhow::bail!("helper_crashed: committed Pending response was lost"),
        )
        .unwrap();
        assert_eq!(lost.status["status"], "error");
        assert_eq!(lost.status["retryable"], true);
        assert_eq!(
            scheduled_target_generation(temp.path()).unwrap().as_deref(),
            Some(generation.as_str())
        );

        let newer = index_with_certified_source_at(temp.path(), "continuation-newer-core.jsonl", 2);
        assert_ne!(newer.generation_id(), generation);
        let retained = pin_retained_generation(temp.path(), &generation).unwrap();
        assert_eq!(retained.generation_id(), generation);
        let resumed = run_with(
            temp.path(),
            &generation,
            ProCatchUpAuthority {
                generation_id: Some(retained.generation_id()),
                verified_index: Some(retained.verified_index()),
            },
            |_| Ok(()),
            |_, supplied| {
                Ok(finalization_outcome(
                    supplied,
                    CoreMaterializationFinalizationPhase::EmitReplay,
                    'c',
                    true,
                ))
            },
        )
        .unwrap();
        assert!(resumed.continuation_pending);
        assert_eq!(resumed.status["core_generation_id"], generation);
        assert_eq!(resumed.status["reason"], "finalizing");
    }

    #[test]
    fn lost_continue_response_preserves_finalizing_tuple_until_reconciliation() {
        let temp = tempfile::tempdir().unwrap();
        let index = empty_index(temp.path());
        let generation = index.generation_id().to_owned();
        let first = run_with(
            temp.path(),
            &generation,
            ProCatchUpAuthority {
                generation_id: Some(&generation),
                verified_index: Some(&index),
            },
            |_| Ok(()),
            |_, supplied| {
                Ok(finalization_outcome(
                    supplied,
                    CoreMaterializationFinalizationPhase::EmitReplay,
                    'c',
                    false,
                ))
            },
        )
        .unwrap();
        let expected = first.status["finalization_progress"].clone();

        let lost = run_with(
            temp.path(),
            &generation,
            ProCatchUpAuthority {
                generation_id: Some(&generation),
                verified_index: Some(&index),
            },
            |data_root| {
                let during_attempt = read_status_json(data_root).unwrap();
                assert_eq!(during_attempt["reason"], "finalizing");
                assert_eq!(during_attempt["finalization_progress"], expected);
                Ok(())
            },
            |_, _| anyhow::bail!("helper_crashed: committed Continue response was lost"),
        )
        .unwrap();
        assert_eq!(lost.status["status"], "error");
        assert_eq!(lost.status["retryable"], true);
        assert_eq!(lost.status["finalization_progress"], expected);

        let reconciled = run_with(
            temp.path(),
            &generation,
            ProCatchUpAuthority {
                generation_id: Some(&generation),
                verified_index: Some(&index),
            },
            |data_root| {
                let during_attempt = read_status_json(data_root).unwrap();
                assert_eq!(during_attempt["reason"], "finalizing");
                assert_eq!(during_attempt["finalization_progress"], expected);
                Ok(())
            },
            |_, supplied| {
                Ok(finalization_outcome(
                    supplied,
                    CoreMaterializationFinalizationPhase::EmitFlat,
                    'd',
                    true,
                ))
            },
        )
        .unwrap();
        assert_eq!(reconciled.status["attempts"], 3);
        assert_eq!(reconciled.status["reason"], "finalizing");
        assert_ne!(reconciled.status["finalization_progress"], expected);
    }

    #[test]
    fn finalization_digest_and_revision_mismatches_are_terminal() {
        for mismatch in ["digest", "revision"] {
            let temp = tempfile::tempdir().unwrap();
            let index = empty_index(temp.path());
            let generation = index.generation_id().to_owned();
            run_with(
                temp.path(),
                &generation,
                ProCatchUpAuthority {
                    generation_id: Some(&generation),
                    verified_index: Some(&index),
                },
                |_| Ok(()),
                |_, supplied| {
                    Ok(finalization_outcome(
                        supplied,
                        CoreMaterializationFinalizationPhase::EmitReplay,
                        'c',
                        false,
                    ))
                },
            )
            .unwrap();

            let failed = run_with(
                temp.path(),
                &generation,
                ProCatchUpAuthority {
                    generation_id: Some(&generation),
                    verified_index: Some(&index),
                },
                |_| Ok(()),
                |_, _| anyhow::bail!("invalid_response: finalization {mismatch} mismatch"),
            )
            .unwrap();
            assert_eq!(failed.status["status"], "error", "{mismatch}");
            assert_eq!(failed.status["retryable"], false, "{mismatch}");
            assert_eq!(failed.status["reason"], "invalid_response", "{mismatch}");
            assert!(
                scheduled_target_generation(temp.path()).unwrap().is_none(),
                "{mismatch}"
            );
        }
    }

    #[test]
    fn same_generation_rechecks_helper_after_materializer_revision_change() {
        let temp = tempfile::tempdir().unwrap();
        let index = empty_index(temp.path());
        let generation = index.generation_id().to_owned();
        run_with(
            temp.path(),
            &generation,
            ProCatchUpAuthority {
                generation_id: Some(&generation),
                verified_index: Some(&index),
            },
            |_| Ok(()),
            |_, supplied| Ok(sync_outcome(supplied, "test-core-materializer-v1", true)),
        )
        .unwrap();

        let mut preflighted = false;
        let mut synced = false;
        let rerun = run_with(
            temp.path(),
            &generation,
            ProCatchUpAuthority {
                generation_id: Some(&generation),
                verified_index: Some(&index),
            },
            |_| {
                preflighted = true;
                Ok(())
            },
            |_, supplied| {
                synced = true;
                Ok(sync_outcome(supplied, "test-core-materializer-v2", true))
            },
        )
        .unwrap();

        assert!(preflighted);
        assert!(synced);
        assert!(rerun.did_work);
        assert_eq!(rerun.status["status"], "completed");
        assert_eq!(rerun.status["attempts"], 2);
    }

    #[test]
    fn same_generation_rechecks_helper_after_private_state_loss() {
        let temp = tempfile::tempdir().unwrap();
        let index = empty_index(temp.path());
        let generation = index.generation_id().to_owned();
        let helper_private_state_exists = Cell::new(false);
        run_with(
            temp.path(),
            &generation,
            ProCatchUpAuthority {
                generation_id: Some(&generation),
                verified_index: Some(&index),
            },
            |_| Ok(()),
            |_, supplied| {
                assert!(!helper_private_state_exists.get());
                helper_private_state_exists.set(true);
                Ok(sync_outcome(supplied, "test-core-materializer-v1", true))
            },
        )
        .unwrap();
        assert!(helper_private_state_exists.replace(false));

        let rerun = run_with(
            temp.path(),
            &generation,
            ProCatchUpAuthority {
                generation_id: Some(&generation),
                verified_index: Some(&index),
            },
            |_| Ok(()),
            |_, supplied| {
                assert!(!helper_private_state_exists.get());
                helper_private_state_exists.set(true);
                Ok(sync_outcome(supplied, "test-core-materializer-v1", true))
            },
        )
        .unwrap();

        assert!(helper_private_state_exists.get());
        assert!(rerun.did_work);
        assert_eq!(rerun.status["status"], "completed");
        assert_eq!(rerun.status["attempts"], 2);
    }

    #[test]
    fn same_generation_current_helper_is_revalidated_without_reporting_work() {
        let temp = tempfile::tempdir().unwrap();
        let index = empty_index(temp.path());
        let generation = index.generation_id().to_owned();
        run_with(
            temp.path(),
            &generation,
            ProCatchUpAuthority {
                generation_id: Some(&generation),
                verified_index: Some(&index),
            },
            |_| Ok(()),
            |_, supplied| Ok(sync_outcome(supplied, "test-core-materializer-v1", true)),
        )
        .unwrap();

        let preflighted = Cell::new(false);
        let synced = Cell::new(false);
        let replay = run_with(
            temp.path(),
            &generation,
            ProCatchUpAuthority {
                generation_id: Some(&generation),
                verified_index: Some(&index),
            },
            |_| {
                preflighted.set(true);
                Ok(())
            },
            |_, supplied| {
                synced.set(true);
                Ok(sync_outcome(supplied, "test-core-materializer-v1", false))
            },
        )
        .unwrap();

        assert!(preflighted.get());
        assert!(synced.get());
        assert!(!replay.did_work);
        assert_eq!(replay.status["status"], "completed");
        assert_eq!(replay.status["attempts"], 2);
    }

    #[test]
    fn helper_recheck_blocks_same_generation_completion_until_observed_success() {
        let temp = tempfile::tempdir().unwrap();
        let index = empty_index(temp.path());
        let generation = index.generation_id().to_owned();
        run_with(
            temp.path(),
            &generation,
            ProCatchUpAuthority {
                generation_id: Some(&generation),
                verified_index: Some(&index),
            },
            |_| Ok(()),
            |_, supplied| Ok(sync_outcome(supplied, "test-core-materializer-v1", true)),
        )
        .unwrap();
        wait_for_completed_generation_with(temp.path(), &generation, Duration::ZERO, || {})
            .unwrap();

        publish_helper_recheck_intent(temp.path(), &"a".repeat(64)).unwrap();
        let error =
            wait_for_completed_generation_with(temp.path(), &generation, Duration::ZERO, || {})
                .unwrap_err();
        assert!(error.to_string().contains("timed out"), "{error:#}");

        let rerun = run_with(
            temp.path(),
            &generation,
            ProCatchUpAuthority {
                generation_id: Some(&generation),
                verified_index: Some(&index),
            },
            |_| Ok(()),
            |_, supplied| Ok(sync_outcome(supplied, "test-core-materializer-v2", true)),
        )
        .unwrap();
        assert!(rerun.did_work);
        assert!(read_recheck_request(temp.path()).unwrap().is_none());
        wait_for_completed_generation_with(temp.path(), &generation, Duration::ZERO, || {})
            .unwrap();
    }

    #[test]
    fn older_run_cannot_clear_recheck_published_during_sync() {
        let temp = tempfile::tempdir().unwrap();
        let index = empty_index(temp.path());
        let generation = index.generation_id().to_owned();
        publish_helper_recheck_intent(temp.path(), &"a".repeat(64)).unwrap();
        let first_request = read_recheck_request(temp.path()).unwrap().unwrap();

        run_with(
            temp.path(),
            &generation,
            ProCatchUpAuthority {
                generation_id: Some(&generation),
                verified_index: Some(&index),
            },
            |_| Ok(()),
            |data_root, supplied| {
                publish_helper_recheck_intent(data_root, &"b".repeat(64)).unwrap();
                Ok(sync_outcome(supplied, "test-core-materializer-v1", true))
            },
        )
        .unwrap();

        let current_request = read_recheck_request(temp.path()).unwrap().unwrap();
        assert_ne!(current_request, first_request);
        assert_eq!(current_request.target_helper_sha256(), "b".repeat(64));
        let error =
            wait_for_completed_generation_with(temp.path(), &generation, Duration::ZERO, || {})
                .unwrap_err();
        assert!(error.to_string().contains("timed out"), "{error:#}");
    }

    #[test]
    fn old_helper_cannot_clear_pending_target_identity() {
        let temp = tempfile::tempdir().unwrap();
        let index = empty_index(temp.path());
        let generation = index.generation_id().to_owned();
        publish_helper_recheck_intent(temp.path(), &"b".repeat(64)).unwrap();

        run_with(
            temp.path(),
            &generation,
            ProCatchUpAuthority {
                generation_id: Some(&generation),
                verified_index: Some(&index),
            },
            |_| Ok(()),
            |_, supplied| Ok(sync_outcome(supplied, "test-core-materializer-v1", true)),
        )
        .unwrap();

        let pending = read_recheck_request(temp.path()).unwrap().unwrap();
        assert_eq!(pending.target_helper_sha256(), "b".repeat(64));
    }

    #[test]
    fn pinned_generation_mismatch_fails_before_sync() {
        let temp = tempfile::tempdir().unwrap();
        let index = empty_index(temp.path());
        let expected = "f".repeat(64);
        let run = run_with(
            temp.path(),
            &expected,
            ProCatchUpAuthority {
                generation_id: Some(index.generation_id()),
                verified_index: Some(&index),
            },
            |_| Ok(()),
            |_, _| panic!("mismatched pin must not sync"),
        )
        .unwrap();
        assert!(!run.did_work);
        assert_eq!(run.status["error_code"], "source_pro_generation_mismatch");
    }

    #[test]
    fn production_catch_up_has_no_manifest_resolver_or_provider_io() {
        let source = include_str!("source_backed_pro_catch_up.rs");
        for forbidden in [
            ["Source", "Manifest"].concat(),
            ["source", "_manifest"].concat(),
            ["sync_source", "_manifest_materialization"].concat(),
        ] {
            assert!(!source.contains(&forbidden));
        }
        assert!(source.contains("sync_core_materialization"));
    }
}
