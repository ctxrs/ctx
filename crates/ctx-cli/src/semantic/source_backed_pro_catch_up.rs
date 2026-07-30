use std::{
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use ctx_history_capture::SourceBackedResolverRegistry;
use ctx_history_core::utc_now;
use ctx_history_index::VerifiedIndex;
use ctx_pro_host_protocol::{SourceManifest, SourceManifestReceipt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::{
    compact_json,
    pro::{stable_error_code, sync_source_manifest_materialization},
};

use super::{
    paths_status::{daemon_jobs_path, read_daemon_job_status, write_daemon_job_status},
    source_backed_refresh_coordinator::{
        source_backed_index_root, GenerationBoundSourceBackedResolver,
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
    #[error("source_pro_authority_missing: no retained source authority for Core generation {0}")]
    MissingAuthority(String),
    #[error(
        "source_pro_generation_mismatch: expected Core generation {expected}, but {authority} \
         was supplied by {surface}"
    )]
    GenerationMismatch {
        expected: String,
        authority: String,
        surface: &'static str,
    },
    #[error("source_pro_manifest_missing: Core generation {0} has no retained source manifest")]
    MissingManifest(String),
    #[error("source_pro_index_unavailable: {0}")]
    IndexUnavailable(String),
    #[error("{code}: {message}")]
    Projection { code: String, message: String },
}

impl SourceBackedProCatchUpError {
    fn code(&self) -> &str {
        match self {
            Self::MissingAuthority(_) => "source_pro_authority_missing",
            Self::GenerationMismatch { .. } => "source_pro_generation_mismatch",
            Self::MissingManifest(_) => "source_pro_manifest_missing",
            Self::IndexUnavailable(_) => "source_pro_index_unavailable",
            Self::Projection { code, .. } => code,
        }
    }

    fn projection(error: anyhow::Error) -> Self {
        Self::Projection {
            code: stable_error_code(&error)
                .unwrap_or("pro_source_materialization_unavailable")
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

pub(super) fn run_after_core_publication(
    data_root: &Path,
    core_generation_id: &str,
    authority: Option<&GenerationBoundSourceBackedResolver>,
) -> Result<SourceBackedProCatchUpRun> {
    let Some(authority) = authority else {
        return record_preflight_error(
            data_root,
            core_generation_id,
            SourceBackedProCatchUpError::MissingAuthority(core_generation_id.to_owned()),
        );
    };
    if authority.generation_id() != core_generation_id {
        return record_preflight_error(
            data_root,
            core_generation_id,
            SourceBackedProCatchUpError::GenerationMismatch {
                expected: core_generation_id.to_owned(),
                authority: authority.generation_id().to_owned(),
                surface: "retained resolver registry",
            },
        );
    }
    let Some(manifest) = authority.source_manifest() else {
        return record_preflight_error(
            data_root,
            core_generation_id,
            SourceBackedProCatchUpError::MissingManifest(core_generation_id.to_owned()),
        );
    };
    run_with(
        data_root,
        core_generation_id,
        authority.generation_id(),
        manifest,
        authority.resolver(),
        |data_root, manifest, index, resolver| {
            sync_source_manifest_materialization(data_root, manifest.clone(), index, resolver)
        },
    )
}

fn run_with<Sync>(
    data_root: &Path,
    core_generation_id: &str,
    authority_generation_id: &str,
    manifest: &SourceManifest,
    resolver: &SourceBackedResolverRegistry,
    sync: Sync,
) -> Result<SourceBackedProCatchUpRun>
where
    Sync: FnOnce(
        &Path,
        &SourceManifest,
        &VerifiedIndex,
        &SourceBackedResolverRegistry,
    ) -> Result<SourceManifestReceipt>,
{
    let prior = read_status(data_root);
    if let Some(status) = prior
        .as_ref()
        .filter(|status| status.is_completed_for(core_generation_id))
    {
        return Ok(SourceBackedProCatchUpRun {
            status: status.to_json()?,
            did_work: false,
        });
    }

    let attempts = next_attempt(prior.as_ref(), core_generation_id);
    let pending = SourceBackedProCatchUpStatus::pending(core_generation_id, attempts);
    persist_status(data_root, &pending)?;

    let result = (|| {
        require_generation(
            core_generation_id,
            authority_generation_id,
            "retained resolver registry",
        )?;
        require_generation(
            core_generation_id,
            &manifest.core_generation_id,
            "retained source manifest",
        )?;
        let index = open_exact_index(data_root, core_generation_id)?;
        let receipt = sync(data_root, manifest, &index, resolver)
            .map_err(SourceBackedProCatchUpError::projection)?;
        require_generation(
            core_generation_id,
            &receipt.core_generation_id,
            "Pro source manifest receipt",
        )?;
        Ok(receipt)
    })();

    match result {
        Ok(receipt) => {
            let completed = pending.completed(receipt.core_generation_id);
            persist_status(data_root, &completed)?;
            Ok(SourceBackedProCatchUpRun {
                status: completed.to_json()?,
                did_work: true,
            })
        }
        Err(error) => {
            let failed = pending.error(error);
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
    let index = VerifiedIndex::open_pinned(&index_root)
        .with_context(|| {
            format!(
                "open verified source-backed lexical index {}",
                index_root.display()
            )
        })
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

pub(super) fn generation_needs_catch_up(data_root: &Path, core_generation_id: &str) -> bool {
    !read_status(data_root).is_some_and(|status| status.is_completed_for(core_generation_id))
}

pub(super) fn status_generation(data_root: &Path) -> Option<String> {
    read_status(data_root).map(|status| status.core_generation_id)
}

/// Waits for the daemon-owned Pro projection of one exact Core generation.
///
/// This is deliberately a status-only seam. The CLI must never rebuild a
/// resolver registry, reread provider sources, or open the legacy Store in
/// order to make Pro ready; those operations remain owned by the daemon tick
/// that retained the generation-bound source authority.
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
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    use ctx_history_index::{GenerationWriter, WriterOptions};

    use super::*;

    #[test]
    fn durable_state_path_is_purpose_based() {
        assert_eq!(
            status_path(Path::new("ctx-data")),
            Path::new("ctx-data/daemon/jobs/pro-catch-up.json")
        );
    }

    fn empty_authority(
        data_root: &Path,
    ) -> (String, SourceManifest, Arc<SourceBackedResolverRegistry>) {
        let writer = GenerationWriter::open(
            source_backed_index_root(data_root),
            WriterOptions::default(),
        )
        .unwrap();
        let receipt = writer.commit(|_| true).unwrap();
        let manifest =
            SourceManifest::new(receipt.generation_id.clone(), Vec::new(), Vec::new()).unwrap();
        let resolver =
            Arc::new(ctx_history_capture::SourceBackedProviderRegistry::new().resolver_registry());
        (receipt.generation_id, manifest, resolver)
    }

    fn receipt(generation_id: &str) -> SourceManifestReceipt {
        SourceManifestReceipt {
            core_generation_id: generation_id.to_owned(),
            manifest_aggregate_sha256: "b".repeat(64),
            materializer_revision: "test-source-materializer".to_owned(),
            progress: Vec::new(),
        }
    }

    #[test]
    fn successful_projection_persists_the_exact_core_generation() {
        let temp = tempfile::tempdir().unwrap();
        let (generation, manifest, resolver) = empty_authority(temp.path());

        let run = run_with(
            temp.path(),
            &generation,
            &generation,
            &manifest,
            resolver.as_ref(),
            |_, supplied_manifest, index, supplied_resolver| {
                assert_eq!(supplied_manifest.core_generation_id, generation);
                assert_eq!(index.generation_id(), generation);
                assert!(std::ptr::eq(supplied_resolver, resolver.as_ref()));
                Ok(receipt(&generation))
            },
        )
        .unwrap();

        assert!(run.did_work);
        assert_eq!(run.status["status"], "completed");
        assert_eq!(run.status["core_generation_id"], generation);
        assert_eq!(run.status["receipt_core_generation_id"], generation);
        assert_eq!(read_status(temp.path()).unwrap().attempts, 1);

        let exact_no_op = run_with(
            temp.path(),
            &generation,
            &generation,
            &manifest,
            resolver.as_ref(),
            |_, _, _, _| panic!("exact completed generation must not invoke Pro again"),
        )
        .unwrap();
        assert!(!exact_no_op.did_work);
        assert_eq!(exact_no_op.status["status"], "completed");
        assert_eq!(exact_no_op.status["core_generation_id"], generation);
        assert_eq!(exact_no_op.status["attempts"], 1);
    }

    #[test]
    fn absent_helper_waits_for_external_install_without_a_retry_timer() {
        let temp = tempfile::tempdir().unwrap();
        let (generation, manifest, resolver) = empty_authority(temp.path());

        let run = run_with(
            temp.path(),
            &generation,
            &generation,
            &manifest,
            resolver.as_ref(),
            |_, _, _, _| Err(anyhow::anyhow!("pro_not_installed: helper unavailable")),
        )
        .unwrap();

        assert!(!run.did_work);
        assert_eq!(run.status["status"], "error");
        assert_eq!(run.status["pending"], true);
        assert_eq!(run.status["retryable"], false);
        assert_eq!(run.status["reason"], "pro_not_installed");
        assert_eq!(run.status["error_code"], "pro_not_installed");
        assert_eq!(run.status["core_generation_id"], generation);
    }

    #[test]
    fn core_no_op_tick_retries_a_pending_generation() {
        let temp = tempfile::tempdir().unwrap();
        let (generation, manifest, resolver) = empty_authority(temp.path());
        let calls = AtomicUsize::new(0);

        let first = run_with(
            temp.path(),
            &generation,
            &generation,
            &manifest,
            resolver.as_ref(),
            |_, _, _, _| {
                calls.fetch_add(1, Ordering::SeqCst);
                Err(anyhow::anyhow!("helper_crashed: retry later"))
            },
        )
        .unwrap();
        assert_eq!(first.status["status"], "error");

        let second = run_with(
            temp.path(),
            &generation,
            &generation,
            &manifest,
            resolver.as_ref(),
            |_, _, _, _| {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(receipt(&generation))
            },
        )
        .unwrap();

        assert!(second.did_work);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(second.status["status"], "completed");
        assert_eq!(second.status["attempts"], 2);
        assert_eq!(second.status["receipt_core_generation_id"], generation);
    }

    #[test]
    fn generation_mismatch_never_completes_projection() {
        let temp = tempfile::tempdir().unwrap();
        let (generation, manifest, resolver) = empty_authority(temp.path());
        let wrong_generation = "b".repeat(64);

        let run = run_with(
            temp.path(),
            &generation,
            &generation,
            &manifest,
            resolver.as_ref(),
            |_, _, _, _| Ok(receipt(&wrong_generation)),
        )
        .unwrap();

        assert!(!run.did_work);
        assert_eq!(run.status["status"], "error");
        assert_eq!(run.status["error_code"], "source_pro_generation_mismatch");
        assert_eq!(run.status["core_generation_id"], generation);
        assert!(run.status["receipt_core_generation_id"].is_null());
    }

    #[test]
    fn explicit_wait_is_generation_exact_and_fails_closed_on_projection_error() {
        let temp = tempfile::tempdir().unwrap();
        let generation = "a".repeat(64);
        let completed =
            SourceBackedProCatchUpStatus::pending(&generation, 1).completed(generation.clone());
        persist_status(temp.path(), &completed).unwrap();
        wait_for_completed_generation_with(temp.path(), &generation, Duration::ZERO, || {
            panic!("completed status must not sleep")
        })
        .unwrap();

        let failed = SourceBackedProCatchUpStatus::pending(&generation, 2).error(
            SourceBackedProCatchUpError::Projection {
                code: "helper_crashed".to_owned(),
                message: "boom".to_owned(),
            },
        );
        persist_status(temp.path(), &failed).unwrap();
        let error =
            wait_for_completed_generation_with(temp.path(), &generation, Duration::ZERO, || {
                panic!("failed status must not sleep")
            })
            .unwrap_err();
        assert!(format!("{error:#}").starts_with("helper_crashed:"));
    }

    #[test]
    fn explicit_wait_times_out_on_a_stale_frontier() {
        let temp = tempfile::tempdir().unwrap();
        let generation = "a".repeat(64);
        let stale =
            SourceBackedProCatchUpStatus::pending(&"b".repeat(64), 1).completed("b".repeat(64));
        persist_status(temp.path(), &stale).unwrap();
        let error =
            wait_for_completed_generation_with(temp.path(), &generation, Duration::ZERO, || {
                panic!("zero timeout must not sleep")
            })
            .unwrap_err();
        assert!(format!("{error:#}").starts_with("not_materialized:"));
    }

    #[test]
    fn source_backed_pro_catch_up_has_no_legacy_projection_authority() {
        let source = include_str!("source_backed_pro_catch_up.rs");
        for forbidden in [
            ["ctx_history_", "store"].concat(),
            ["database_", "path"].concat(),
            ["body_", "preview"].concat(),
            ["projection_", "journal"].concat(),
            ["prepare_nativepath_", "projection"].concat(),
            ["fall", "back"].concat(),
        ] {
            assert!(
                !source.contains(&forbidden),
                "source-backed Pro catch-up contains forbidden architecture term {forbidden}"
            );
        }
        assert!(source.contains("VerifiedIndex::open_pinned"));
        assert!(source.contains("sync_source_manifest_materialization"));
        assert!(source.contains("authority.source_manifest()"));
        assert!(source.contains("authority.resolver()"));
    }
}
