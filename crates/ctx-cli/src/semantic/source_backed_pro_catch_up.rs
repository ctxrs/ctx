use std::{
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use ctx_history_core::utc_now;
use ctx_history_index::VerifiedIndex;
use ctx_pro_host_protocol::CoreMaterializationReceipt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::{
    compact_json,
    pro::{preflight_core_materialization, stable_error_code, sync_core_materialization},
};

use super::{
    paths_status::{daemon_jobs_path, read_daemon_job_status, write_daemon_job_status},
    source_backed_refresh_coordinator::{
        nonzero_duration_micros, open_verified_index, source_backed_index_root,
        PinnedCorePublication, PinnedSourceBackedGeneration,
    },
};

const SOURCE_BACKED_PRO_CATCH_UP_STATUS_FILE: &str = "pro-catch-up.json";
const SOURCE_BACKED_PRO_CATCH_UP_SCHEMA_VERSION: u16 = 1;
const SOURCE_BACKED_PRO_CATCH_UP_POLL_INTERVAL: Duration = Duration::from_millis(50);
const SOURCE_BACKED_PRO_CATCH_UP_WAIT_TIMEOUT: Duration = Duration::from_secs(60 * 60);

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

    fn completed(mut self, receipt_generation: String) -> Self {
        self.status = SourceBackedProCatchUpState::Completed;
        self.pending = false;
        self.retryable = false;
        self.receipt_core_generation_id = Some(receipt_generation);
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
        !matches!(
            self,
            Self::Projection { code, .. } if code == "pro_not_installed"
        )
    }
}

pub(super) struct SourceBackedProCatchUpRun {
    pub(super) status: Value,
    pub(super) did_work: bool,
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
            let outcome = sync_core_materialization(data_root, index)?;
            Ok(ProCatchUpSyncOutcome {
                receipt: outcome.receipt,
                did_work: outcome.did_work,
            })
        },
    )
}

struct ProCatchUpAuthority<'a> {
    generation_id: Option<&'a str>,
    verified_index: Option<&'a VerifiedIndex>,
}

struct ProCatchUpSyncOutcome {
    receipt: CoreMaterializationReceipt,
    did_work: bool,
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
    let prior = read_status(data_root);
    let attempts = next_attempt(prior.as_ref(), core_generation_id);
    let attempt_started = Instant::now();
    let pending = SourceBackedProCatchUpStatus::pending(core_generation_id, attempts);
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
        require_generation(
            core_generation_id,
            &outcome.receipt.core_generation_id,
            "Pro Core materialization receipt",
        )?;
        Ok(outcome)
    })();

    match result {
        Ok(outcome) => {
            let completed = pending
                .completed(outcome.receipt.core_generation_id)
                .with_duration(nonzero_duration_micros(attempt_started.elapsed()));
            persist_status(data_root, &completed)?;
            Ok(SourceBackedProCatchUpRun {
                status: completed.to_json()?,
                did_work: outcome.did_work,
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
        if let Some(status) = read_status(data_root) {
            if status.is_completed_for(core_generation_id) {
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

    use ctx_history_index::{GenerationWriter, WriterOptions};

    use crate::semantic::source_backed_refresh_coordinator::count_verified_index_opens;

    use super::*;

    fn empty_index(data_root: &Path) -> VerifiedIndex {
        GenerationWriter::open(
            source_backed_index_root(data_root),
            WriterOptions::default(),
        )
        .unwrap()
        .commit(|_| true)
        .unwrap();
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
        ProCatchUpSyncOutcome {
            receipt: receipt_with_revision(index, materializer_revision),
            did_work,
        }
    }

    #[test]
    fn durable_state_path_is_purpose_based() {
        assert_eq!(
            status_path(Path::new("ctx-data")),
            Path::new("ctx-data/daemon/jobs/pro-catch-up.json")
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
