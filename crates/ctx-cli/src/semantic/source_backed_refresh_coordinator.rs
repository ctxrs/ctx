use std::{
    collections::VecDeque,
    fmt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration as StdDuration,
};

use anyhow::{anyhow, Context, Result};
use ctx_history_capture::{
    build_automatic_source_backed_registry, DiscoveryContext, ProviderSourceStatus,
    SourceBackedAutomaticRegistryIssue, SourceBackedAutomaticUnavailableReason,
    SourceBackedRefreshProgress as CaptureSourceBackedRefreshProgress, SourceBackedRouteError,
    SourceBackedRouteErrorKind, SourceBackedRouteResult,
};
use ctx_history_core::utc_now;
#[cfg(test)]
use ctx_history_core::CaptureProvider;
use ctx_history_index::{VerifiedIndex, WriterOptions};
use ctx_pro_host_protocol::{SourceManifest, SourceRemoval};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    compact_json,
    config::{AppConfig, DaemonMode},
    identity,
    upgrade::data_migration::{self, MigrationPhase},
};

use super::{
    paths_status::{
        daemon_source_backed_refresh_job_path, read_daemon_status, write_daemon_job_status,
    },
    query_service::{daemon_source_refresh_request, DaemonSourceRefreshServiceUnavailable},
};

const SOURCE_BACKED_INDEX_DIRECTORY: &str = "source-backed-lexical-v0";
const SOURCE_REFRESH_REQUEST_OP: &str = "source_refresh_request";
const SOURCE_REFRESH_STATUS_OP: &str = "source_refresh_status";
const SOURCE_REFRESH_ATTEMPT_HISTORY: usize = 64;
const SOURCE_REFRESH_POLL_INTERVAL: StdDuration = StdDuration::from_millis(50);
const SOURCE_REFRESH_IPC_TIMEOUT: StdDuration = StdDuration::from_secs(2);
const SOURCE_REFRESH_RESPONSE_MAX_BYTES: u64 = 64 * 1024;
const SOURCE_REBUILD_MIGRATION_JOURNAL: &str = "state.jsonl";
const SOURCE_REFRESH_BUILD_ISSUE_LIMIT: usize = 8;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum SourceBackedRefreshMode {
    Off,
    Background,
    Wait,
}

impl SourceBackedRefreshMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Background => "background",
            Self::Wait => "wait",
        }
    }
}

#[derive(Debug)]
pub(crate) struct SourceBackedRefreshDaemonUnavailable {
    detail: Option<String>,
}

impl SourceBackedRefreshDaemonUnavailable {
    fn new(detail: Option<String>) -> Self {
        Self { detail }
    }
}

impl fmt::Display for SourceBackedRefreshDaemonUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the ctx daemon is unavailable for source-backed refresh")?;
        if let Some(detail) = self.detail.as_deref() {
            write!(formatter, ": {detail}")?;
        }
        formatter.write_str("; no foreground writer was started")
    }
}

impl std::error::Error for SourceBackedRefreshDaemonUnavailable {}

/// Provider-neutral publication returned by the capture-owned refresh
/// executor after it atomically advances the source-backed generation.
#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct SourceBackedRefreshPublication {
    pub(crate) generation_id: String,
    /// Exact metadata-only Pro handoff for this Core generation. Test
    /// executors may omit it; the capture-owned production executor never does.
    pub(crate) source_manifest: Option<SourceManifest>,
    pub(crate) scanned_routes: usize,
    pub(crate) unsupported_routes: usize,
    pub(crate) certified_source_count: usize,
    pub(crate) certified_source_bytes: u64,
    pub(crate) timings: SourceBackedRefreshTimings,
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub(crate) struct SourceBackedRefreshTimings {
    pub(crate) discovery_us: u64,
    pub(crate) scan_stage_us: u64,
    pub(crate) commit_us: u64,
}

struct SourceBackedRefreshProgressUpdate {
    phase: String,
    completed_sources: usize,
    total_sources: usize,
    current_source: Option<String>,
}

/// Daemon-owned execution context passed to the capture refresh callback.
///
/// The callback owns source/provider discovery and publication. The daemon
/// owns request serialization, progress persistence, and publication
/// verification.
#[allow(dead_code)] // All fields are part of the capture-coordinator callback seam.
pub(crate) struct SourceBackedRefreshExecution<'a> {
    pub(crate) data_root: &'a Path,
    pub(crate) index_root: &'a Path,
    pub(crate) request_id: &'a str,
    report_progress: &'a dyn Fn(SourceBackedRefreshProgressUpdate) -> Result<()>,
}

impl SourceBackedRefreshExecution<'_> {
    pub(crate) fn report_progress(
        &self,
        phase: &str,
        completed_sources: usize,
        total_sources: usize,
        current_source: Option<String>,
    ) -> Result<()> {
        (self.report_progress)(SourceBackedRefreshProgressUpdate {
            phase: phase.to_owned(),
            completed_sources,
            total_sources,
            current_source,
        })
    }
}

/// Provider-neutral callback boundary for one daemon-serialized refresh.
pub(crate) trait SourceBackedRefreshExecutor: Send + Sync {
    fn refresh(
        &self,
        execution: SourceBackedRefreshExecution<'_>,
    ) -> Result<SourceBackedRefreshPublication>;

    #[cfg(test)]
    fn implementation_name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }
}

impl<F> SourceBackedRefreshExecutor for F
where
    F: for<'a> Fn(SourceBackedRefreshExecution<'a>) -> Result<SourceBackedRefreshPublication>
        + Send
        + Sync,
{
    fn refresh(
        &self,
        execution: SourceBackedRefreshExecution<'_>,
    ) -> Result<SourceBackedRefreshPublication> {
        self(execution)
    }
}

#[derive(Debug, Default)]
struct CaptureOwnedSourceBackedRefreshExecutor;

impl SourceBackedRefreshExecutor for CaptureOwnedSourceBackedRefreshExecutor {
    fn refresh(
        &self,
        execution: SourceBackedRefreshExecution<'_>,
    ) -> Result<SourceBackedRefreshPublication> {
        execute_capture_owned_refresh(execution)
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum SourceBackedRefreshState {
    Queued,
    Running,
    Published,
    Failed,
}

impl SourceBackedRefreshState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Published => "published",
            Self::Failed => "failed",
        }
    }

    fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Running)
    }
}

#[derive(Debug, Clone)]
struct SourceBackedRefreshProgress {
    phase: String,
    completed_sources: usize,
    total_sources: usize,
    current_source: Option<String>,
}

impl Default for SourceBackedRefreshProgress {
    fn default() -> Self {
        Self {
            phase: "queued".to_owned(),
            completed_sources: 0,
            total_sources: 0,
            current_source: None,
        }
    }
}

impl SourceBackedRefreshProgress {
    fn to_json(&self) -> Value {
        compact_json(json!({
            "phase": self.phase,
            "completed_sources": self.completed_sources,
            "total_sources": self.total_sources,
            "current_source": self.current_source,
        }))
    }
}

#[derive(Debug, Clone)]
struct SourceBackedRefreshAttempt {
    request_id: String,
    state: SourceBackedRefreshState,
    requested_at_ms: i64,
    started_at_ms: Option<i64>,
    finished_at_ms: Option<i64>,
    previous_generation: Option<String>,
    published_generation: Option<String>,
    coalesced_requests: u64,
    progress: SourceBackedRefreshProgress,
    scanned_routes: Option<usize>,
    unsupported_routes: Option<usize>,
    certified_source_count: Option<usize>,
    certified_source_bytes: Option<u64>,
    timings: Option<SourceBackedRefreshTimings>,
    daemon_mode: DaemonMode,
    trigger: &'static str,
    trigger_provenance: &'static str,
    last_error: Option<String>,
}

impl SourceBackedRefreshAttempt {
    fn to_json(&self) -> Value {
        compact_json(json!({
            "ok": true,
            "schema_version": 1,
            "owner": "daemon",
            "request_id": self.request_id,
            "request_state": self.state.as_str(),
            "requested_at_ms": self.requested_at_ms,
            "started_at_ms": self.started_at_ms,
            "finished_at_ms": self.finished_at_ms,
            "previous_generation": self.previous_generation,
            "published_generation": self.published_generation,
            "generation_changed": self.previous_generation != self.published_generation,
            "coalesced_requests": self.coalesced_requests,
            "progress": self.progress.to_json(),
            "scanned_routes": self.scanned_routes,
            "unsupported_routes": self.unsupported_routes,
            "certified_source_count": self.certified_source_count,
            "certified_source_bytes": self.certified_source_bytes,
            "timings_us": self.timings.map(SourceBackedRefreshTimings::to_json),
            "daemon_mode": self.daemon_mode.as_str(),
            "trigger": self.trigger,
            "trigger_provenance": self.trigger_provenance,
            "last_error": self.last_error,
        }))
    }

    fn job_json(&self) -> Value {
        let status = match self.state {
            SourceBackedRefreshState::Published => "completed",
            SourceBackedRefreshState::Failed => "failed",
            SourceBackedRefreshState::Queued | SourceBackedRefreshState::Running => "running",
        };
        compact_json(json!({
            "mode": SourceBackedRefreshMode::Background.as_str(),
            "owner": "daemon",
            "kind": "source_backed",
            "status": status,
            "request_id": self.request_id,
            "request_state": self.state.as_str(),
            "source_count": self.progress.total_sources,
            "last_run_at_ms": self.started_at_ms.unwrap_or(self.requested_at_ms),
            "previous_generation": self.previous_generation,
            "published_generation": self.published_generation,
            "generation_changed": self.previous_generation != self.published_generation,
            "coalesced_requests": self.coalesced_requests,
            "progress": self.progress.to_json(),
            "scanned_routes": self.scanned_routes,
            "unsupported_routes": self.unsupported_routes,
            "certified_source_count": self.certified_source_count,
            "certified_source_bytes": self.certified_source_bytes,
            "timings_us": self.timings.map(SourceBackedRefreshTimings::to_json),
            "daemon_mode": self.daemon_mode.as_str(),
            "trigger": self.trigger,
            "trigger_provenance": self.trigger_provenance,
            "last_error": self.last_error,
        }))
    }
}

impl SourceBackedRefreshTimings {
    fn to_json(self) -> Value {
        json!({
            "discovery": self.discovery_us,
            "scan_stage": self.scan_stage_us,
            "commit": self.commit_us,
        })
    }
}

struct SourceBackedRefreshCoordinatorState {
    active_request_id: Option<String>,
    attempts: VecDeque<SourceBackedRefreshAttempt>,
}

pub(in crate::semantic) struct SourceBackedRefreshCoordinator {
    state: Mutex<SourceBackedRefreshCoordinatorState>,
    executor: Arc<dyn SourceBackedRefreshExecutor>,
}

pub(in crate::semantic) struct SourceBackedRefreshRun {
    pub(in crate::semantic) job: Value,
    pub(in crate::semantic) did_work: bool,
    pub(in crate::semantic) failed: bool,
}

impl SourceBackedRefreshCoordinator {
    pub(in crate::semantic) fn new() -> Self {
        Self::with_executor(Arc::new(CaptureOwnedSourceBackedRefreshExecutor))
    }

    pub(in crate::semantic) fn with_executor(
        executor: Arc<dyn SourceBackedRefreshExecutor>,
    ) -> Self {
        Self {
            state: Mutex::new(SourceBackedRefreshCoordinatorState {
                active_request_id: None,
                attempts: VecDeque::new(),
            }),
            executor,
        }
    }

    pub(in crate::semantic) fn has_pending_request(&self) -> bool {
        let state = self.lock_state();
        state
            .active_request_id
            .as_deref()
            .and_then(|request_id| find_attempt(&state, request_id))
            .is_some_and(|attempt| attempt.state.is_active())
    }

    pub(in crate::semantic) fn handle_ipc_request(
        &self,
        data_root: &Path,
        request: &Value,
    ) -> Result<Option<Value>> {
        match request.get("op").and_then(Value::as_str) {
            Some(SOURCE_REFRESH_REQUEST_OP) => {
                let mode = request.get("mode").and_then(Value::as_str).unwrap_or("");
                if !matches!(mode, "background" | "wait") {
                    return Err(anyhow!("invalid daemon source refresh mode `{mode}`"));
                }
                let previous_generation = published_generation_id(data_root)?;
                let response = self.enqueue_with_metadata(
                    previous_generation,
                    source_refresh_runtime_metadata(data_root),
                );
                let request_id = response
                    .get("request_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("queued source refresh has no request ID"))?;
                if let Some(job) = self.job_status(request_id) {
                    write_daemon_job_status(
                        &daemon_source_backed_refresh_job_path(data_root),
                        &job,
                    )?;
                }
                Ok(Some(response))
            }
            Some(SOURCE_REFRESH_STATUS_OP) => {
                let request_id = request
                    .get("request_id")
                    .and_then(Value::as_str)
                    .filter(|request_id| !request_id.is_empty())
                    .ok_or_else(|| anyhow!("daemon source refresh request ID is missing"))?;
                let status = self.status(request_id).ok_or_else(|| {
                    anyhow!("daemon source refresh request `{request_id}` is unknown")
                })?;
                Ok(Some(status))
            }
            _ => Ok(None),
        }
    }

    pub(in crate::semantic) fn run_next(&self, data_root: &Path) -> Option<SourceBackedRefreshRun> {
        let executor = Arc::clone(&self.executor);
        self.run_next_with(
            |request_id, coordinator| {
                execute_source_backed_refresh(executor.as_ref(), data_root, request_id, coordinator)
            },
            || published_generation_id(data_root),
            |generation_id| complete_pending_source_rebuild(data_root, generation_id),
            |error| record_pending_source_rebuild_failure(data_root, error),
        )
    }

    #[cfg(test)]
    fn enqueue(&self, observed_generation: Option<String>) -> Value {
        self.enqueue_with_metadata(observed_generation, SourceRefreshRuntimeMetadata::default())
    }

    #[cfg(test)]
    pub(in crate::semantic) fn enqueue_for_test(
        &self,
        observed_generation: Option<String>,
    ) -> Value {
        self.enqueue(observed_generation)
    }

    fn enqueue_with_metadata(
        &self,
        observed_generation: Option<String>,
        metadata: SourceRefreshRuntimeMetadata,
    ) -> Value {
        let mut state = self.lock_state();
        if let Some(active_request_id) = state.active_request_id.clone() {
            if let Some(active) = find_attempt_mut(&mut state, &active_request_id) {
                if active.state.is_active() {
                    active.coalesced_requests = active.coalesced_requests.saturating_add(1);
                    return active.to_json();
                }
            }
        }

        let attempt = SourceBackedRefreshAttempt {
            request_id: Uuid::now_v7().to_string(),
            state: SourceBackedRefreshState::Queued,
            requested_at_ms: utc_now().timestamp_millis(),
            started_at_ms: None,
            finished_at_ms: None,
            previous_generation: observed_generation.clone(),
            published_generation: observed_generation,
            coalesced_requests: 0,
            progress: SourceBackedRefreshProgress::default(),
            scanned_routes: None,
            unsupported_routes: None,
            certified_source_count: None,
            certified_source_bytes: None,
            timings: None,
            daemon_mode: metadata.daemon_mode,
            trigger: metadata.trigger,
            trigger_provenance: metadata.trigger_provenance,
            last_error: None,
        };
        let response = attempt.to_json();
        state.active_request_id = Some(attempt.request_id.clone());
        state.attempts.push_back(attempt);
        trim_attempt_history(&mut state);
        response
    }

    fn status(&self, request_id: &str) -> Option<Value> {
        let state = self.lock_state();
        find_attempt(&state, request_id).map(SourceBackedRefreshAttempt::to_json)
    }

    fn job_status(&self, request_id: &str) -> Option<Value> {
        let state = self.lock_state();
        find_attempt(&state, request_id).map(SourceBackedRefreshAttempt::job_json)
    }

    fn set_progress(
        &self,
        request_id: &str,
        phase: &str,
        completed_sources: usize,
        total_sources: usize,
        current_source: Option<String>,
    ) -> Option<Value> {
        let mut state = self.lock_state();
        let Some(attempt) = find_attempt_mut(&mut state, request_id) else {
            return None;
        };
        if attempt.state != SourceBackedRefreshState::Running {
            return None;
        }
        attempt.progress = SourceBackedRefreshProgress {
            phase: phase.to_owned(),
            completed_sources,
            total_sources,
            current_source,
        };
        Some(attempt.job_json())
    }

    fn run_next_with<Execute, Probe, Published, Failed>(
        &self,
        execute: Execute,
        probe: Probe,
        published: Published,
        failed: Failed,
    ) -> Option<SourceBackedRefreshRun>
    where
        Execute: FnOnce(&str, &Self) -> Result<SourceBackedRefreshPublication>,
        Probe: FnOnce() -> Result<Option<String>>,
        Published: FnOnce(&str) -> Result<()>,
        Failed: FnOnce(&str) -> Result<()>,
    {
        let (request_id, previous_generation) = {
            let mut state = self.lock_state();
            let request_id = state.active_request_id.clone()?;
            let attempt = find_attempt_mut(&mut state, &request_id)?;
            if attempt.state != SourceBackedRefreshState::Queued {
                return None;
            }
            attempt.state = SourceBackedRefreshState::Running;
            attempt.started_at_ms = Some(utc_now().timestamp_millis());
            attempt.progress.phase = "starting".to_owned();
            (request_id, attempt.previous_generation.clone())
        };

        let execution = execute(&request_id, self);
        let observed_generation = probe();
        let (verified, observed_for_status) = match (execution, observed_generation) {
            (Ok(publication), Ok(Some(observed))) if publication.generation_id == observed => {
                (Ok((observed.clone(), publication)), Some(observed))
            }
            (Ok(publication), Ok(observed)) => (Err(format!(
                "source-backed refresh returned generation {}, but the verified published generation is {observed:?}",
                publication.generation_id
            )), observed),
            (Ok(publication), Err(error)) => (
                Err(format!(
                    "source-backed refresh returned generation {}, but publication verification failed: {error:#}",
                    publication.generation_id
                )),
                None,
            ),
            (Err(error), Ok(observed)) => (Err(format!("{error:#}")), observed),
            (Err(error), Err(probe_error)) => (Err(format!(
                "{error:#}; verifying the retained generation also failed: {probe_error:#}"
            )), None),
        };
        let verified = match verified {
            Ok((observed, publication)) => match published(&observed) {
                Ok(()) => Ok((observed, publication)),
                Err(error) => {
                    let error = format!("record source-backed rebuild publication: {error:#}");
                    match failed(&error) {
                        Ok(()) => Err(error),
                        Err(record_error) => Err(format!(
                            "{error}; recording the resumable rebuild failure also failed: {record_error:#}"
                        )),
                    }
                }
            },
            Err(error) => match failed(&error) {
                Ok(()) => Err(error),
                Err(record_error) => Err(format!(
                    "{error}; recording the resumable rebuild failure also failed: {record_error:#}"
                )),
            },
        };
        let mut state = self.lock_state();
        let (failed, did_work, job) = {
            let attempt = find_attempt_mut(&mut state, &request_id)?;
            attempt.finished_at_ms = Some(utc_now().timestamp_millis());
            attempt.progress.current_source = None;
            if observed_for_status.is_some() {
                attempt.published_generation = observed_for_status;
            }

            match verified {
                Ok((observed, publication)) => {
                    attempt.state = SourceBackedRefreshState::Published;
                    attempt.published_generation = Some(observed.clone());
                    attempt.progress.phase = "published".to_owned();
                    attempt.progress.completed_sources = attempt.progress.total_sources;
                    attempt.scanned_routes = Some(publication.scanned_routes);
                    attempt.unsupported_routes = Some(publication.unsupported_routes);
                    attempt.certified_source_count = Some(publication.certified_source_count);
                    attempt.certified_source_bytes = Some(publication.certified_source_bytes);
                    attempt.timings = Some(publication.timings);
                }
                Err(error) => {
                    attempt.state = SourceBackedRefreshState::Failed;
                    attempt.progress.phase = "failed".to_owned();
                    attempt.last_error = Some(error);
                }
            }

            let failed = attempt.state == SourceBackedRefreshState::Failed;
            let did_work = !failed && attempt.published_generation != previous_generation;
            (failed, did_work, attempt.job_json())
        };
        if state.active_request_id.as_deref() == Some(request_id.as_str()) {
            state.active_request_id = None;
        }
        Some(SourceBackedRefreshRun {
            job,
            did_work,
            failed,
        })
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, SourceBackedRefreshCoordinatorState> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }
}

#[derive(Debug, Clone, Copy)]
struct SourceRefreshRuntimeMetadata {
    daemon_mode: DaemonMode,
    trigger: &'static str,
    trigger_provenance: &'static str,
}

impl Default for SourceRefreshRuntimeMetadata {
    fn default() -> Self {
        Self {
            daemon_mode: DaemonMode::Full,
            trigger: "search",
            trigger_provenance: "manual",
        }
    }
}

fn source_refresh_runtime_metadata(data_root: &Path) -> SourceRefreshRuntimeMetadata {
    let daemon_status = read_daemon_status(data_root);
    let daemon_mode = AppConfig::load(data_root)
        .map(|config| config.daemon.mode)
        .ok()
        .or_else(|| {
            daemon_status
                .as_ref()
                .and_then(|status| status.get("config_reload"))
                .and_then(|reload| reload.get("applied"))
                .and_then(|applied| applied.get("daemon_mode"))
                .and_then(Value::as_str)
                .and_then(DaemonMode::parse)
        })
        .unwrap_or_default();
    let trigger_provenance = if daemon_status
        .as_ref()
        .and_then(|status| status.get("start_mode"))
        .and_then(Value::as_str)
        == Some("auto")
    {
        "autostart"
    } else {
        "manual"
    };
    SourceRefreshRuntimeMetadata {
        daemon_mode,
        trigger: "search",
        trigger_provenance,
    }
}

fn find_attempt<'a>(
    state: &'a SourceBackedRefreshCoordinatorState,
    request_id: &str,
) -> Option<&'a SourceBackedRefreshAttempt> {
    state
        .attempts
        .iter()
        .find(|attempt| attempt.request_id == request_id)
}

fn find_attempt_mut<'a>(
    state: &'a mut SourceBackedRefreshCoordinatorState,
    request_id: &str,
) -> Option<&'a mut SourceBackedRefreshAttempt> {
    state
        .attempts
        .iter_mut()
        .find(|attempt| attempt.request_id == request_id)
}

fn trim_attempt_history(state: &mut SourceBackedRefreshCoordinatorState) {
    while state.attempts.len() > SOURCE_REFRESH_ATTEMPT_HISTORY {
        if state
            .attempts
            .front()
            .is_some_and(|attempt| attempt.state.is_active())
        {
            break;
        }
        state.attempts.pop_front();
    }
}

fn source_backed_index_root(data_root: &Path) -> PathBuf {
    data_root.join(SOURCE_BACKED_INDEX_DIRECTORY)
}

fn published_generation_id(data_root: &Path) -> Result<Option<String>> {
    let index_root = source_backed_index_root(data_root);
    if !index_root.join("meta.json").is_file() {
        return Ok(None);
    }
    Ok(Some(
        VerifiedIndex::open(&index_root)
            .with_context(|| {
                format!(
                    "open verified source-backed lexical index {}",
                    index_root.display()
                )
            })?
            .generation_id()
            .to_owned(),
    ))
}

fn pending_source_rebuild(data_root: &Path) -> Result<bool> {
    if !data_migration::migration_directory(data_root)
        .join(SOURCE_REBUILD_MIGRATION_JOURNAL)
        .is_file()
    {
        return Ok(false);
    }
    Ok(data_migration::inspect(data_root)?.is_some_and(|marker| {
        matches!(
            marker.phase,
            MigrationPhase::RebuildPending | MigrationPhase::SourceRebuildFailed
        ) && marker.source_rebuild_required
    }))
}

fn complete_pending_source_rebuild(data_root: &Path, generation_id: &str) -> Result<()> {
    if pending_source_rebuild(data_root)? {
        data_migration::complete_source_rebuild(data_root, generation_id)?;
    }
    Ok(())
}

fn record_pending_source_rebuild_failure(data_root: &Path, error: &str) -> Result<()> {
    if pending_source_rebuild(data_root)? {
        data_migration::record_source_rebuild_failure(data_root, error)?;
    }
    Ok(())
}

fn execute_source_backed_refresh(
    executor: &dyn SourceBackedRefreshExecutor,
    data_root: &Path,
    request_id: &str,
    coordinator: &SourceBackedRefreshCoordinator,
) -> Result<SourceBackedRefreshPublication> {
    let index_root = source_backed_index_root(data_root);
    let report_progress = |update: SourceBackedRefreshProgressUpdate| {
        record_source_backed_refresh_progress(
            data_root,
            coordinator,
            request_id,
            &update.phase,
            update.completed_sources,
            update.total_sources,
            update.current_source,
        )
    };
    executor.refresh(SourceBackedRefreshExecution {
        data_root,
        index_root: &index_root,
        request_id,
        report_progress: &report_progress,
    })
}

fn execute_capture_owned_refresh(
    execution: SourceBackedRefreshExecution<'_>,
) -> Result<SourceBackedRefreshPublication> {
    let discovery = source_backed_discovery_context()?;
    execute_capture_owned_refresh_with(execution, &discovery, refresh_all_provider_sources)
}

fn execute_capture_owned_refresh_with<Refresh>(
    execution: SourceBackedRefreshExecution<'_>,
    discovery: &DiscoveryContext,
    refresh_all: Refresh,
) -> Result<SourceBackedRefreshPublication>
where
    Refresh: FnOnce(
        &DiscoveryContext,
        &Path,
        &mut dyn FnMut(CaptureSourceBackedRefreshProgress) -> SourceBackedRouteResult<()>,
    ) -> Result<SourceBackedRefreshPublication>,
{
    execution.report_progress("discovering", 0, 0, None)?;
    let mut report_progress = |update: CaptureSourceBackedRefreshProgress| {
        execution
            .report_progress(
                update.phase,
                update.completed_sources,
                update.total_sources,
                update.current_source,
            )
            .map_err(|error| {
                SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Internal,
                    format!("persist daemon source-backed refresh progress: {error:#}"),
                )
            })
    };
    refresh_all(discovery, execution.index_root, &mut report_progress)
}

fn refresh_all_provider_sources(
    discovery: &DiscoveryContext,
    index_root: &Path,
    report_progress: &mut dyn FnMut(
        CaptureSourceBackedRefreshProgress,
    ) -> SourceBackedRouteResult<()>,
) -> Result<SourceBackedRefreshPublication> {
    let build = build_automatic_source_backed_registry(discovery);
    let (executor, issues) = build.into_refresh_executor(WriterOptions::default());
    reject_blocking_automatic_registry_issues(&issues)?;
    let receipt = executor
        .refresh(index_root, report_progress)
        .context("run capture-owned all-provider source-backed refresh")?;
    let removals = receipt
        .removals
        .iter()
        .cloned()
        .map(|removal| SourceRemoval::new(removal.deletion, removal.inventory))
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|error| anyhow!("build certified Pro source removal: {}", error.message))?;
    let source_manifest = SourceManifest::new(
        receipt.commit.generation_id.clone(),
        receipt.sources.clone(),
        removals,
    )
    .map_err(|error| anyhow!("build Pro source manifest: {}", error.message))?;
    Ok(SourceBackedRefreshPublication {
        generation_id: receipt.commit.generation_id,
        source_manifest: Some(source_manifest),
        scanned_routes: receipt.scanned_routes,
        unsupported_routes: receipt.unsupported_routes.len(),
        certified_source_count: receipt.certified_source_count,
        certified_source_bytes: receipt.certified_source_bytes,
        timings: SourceBackedRefreshTimings {
            discovery_us: nonzero_duration_micros(receipt.discovery_duration),
            scan_stage_us: nonzero_duration_micros(receipt.scan_stage_duration),
            commit_us: nonzero_duration_micros(receipt.commit_duration),
        },
    })
}

fn nonzero_duration_micros(duration: StdDuration) -> u64 {
    u64::try_from(duration.as_micros())
        .unwrap_or(u64::MAX)
        .max(1)
}

fn source_backed_discovery_context() -> Result<DiscoveryContext> {
    let home = identity::home_dir()
        .context("resolve the user home for source-backed provider discovery")?;
    Ok(DiscoveryContext::from_process(home))
}

fn reject_blocking_automatic_registry_issues(
    issues: &[SourceBackedAutomaticRegistryIssue],
) -> Result<()> {
    let mut blocker_count = 0usize;
    let mut blocker_details = Vec::new();
    for issue in issues {
        let SourceBackedAutomaticRegistryIssue::Unavailable { source, reason } = issue else {
            continue;
        };
        let blocks_publication = match reason {
            SourceBackedAutomaticUnavailableReason::SourceStatus(
                ProviderSourceStatus::Missing | ProviderSourceStatus::Unknown,
            ) => false,
            SourceBackedAutomaticUnavailableReason::SourceStatus(_)
            | SourceBackedAutomaticUnavailableReason::UnsupportedFormat { .. }
            | SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable { .. }
            | SourceBackedAutomaticUnavailableReason::RegistrationRejected { .. } => source.exists,
        };
        if !blocks_publication {
            continue;
        }
        blocker_count = blocker_count.saturating_add(1);
        if blocker_details.len() < SOURCE_REFRESH_BUILD_ISSUE_LIMIT {
            blocker_details.push(format!(
                "{} {}: {}",
                source.provider.as_str(),
                source.path.display(),
                automatic_registry_issue_reason(reason),
            ));
        }
    }
    if blocker_count == 0 {
        return Ok(());
    }
    let omitted = blocker_count.saturating_sub(blocker_details.len());
    let omitted = if omitted == 0 {
        String::new()
    } else {
        format!("; {omitted} additional blocking issue(s) omitted")
    };
    Err(anyhow!(
        "capture automatic registry has {blocker_count} blocking detected-source issue(s): {}{omitted}",
        blocker_details.join("; ")
    ))
}

fn automatic_registry_issue_reason(reason: &SourceBackedAutomaticUnavailableReason) -> String {
    match reason {
        SourceBackedAutomaticUnavailableReason::SourceStatus(status) => {
            format!("source status is {}", status.as_str())
        }
        SourceBackedAutomaticUnavailableReason::UnsupportedFormat { detail }
        | SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable { detail } => {
            (*detail).to_owned()
        }
        SourceBackedAutomaticUnavailableReason::RegistrationRejected { detail } => detail.clone(),
    }
}

fn record_source_backed_refresh_progress(
    data_root: &Path,
    coordinator: &SourceBackedRefreshCoordinator,
    request_id: &str,
    phase: &str,
    completed_sources: usize,
    total_sources: usize,
    current_source: Option<String>,
) -> Result<()> {
    if let Some(job) = coordinator.set_progress(
        request_id,
        phase,
        completed_sources,
        total_sources,
        current_source,
    ) {
        write_daemon_job_status(&daemon_source_backed_refresh_job_path(data_root), &job)?;
    }
    Ok(())
}

pub(crate) struct PinnedSourceBackedGeneration {
    index: VerifiedIndex,
}

impl PinnedSourceBackedGeneration {
    #[allow(dead_code)] // Available to callers that report the selected pin.
    pub(crate) fn generation_id(&self) -> &str {
        self.index.generation_id()
    }

    pub(super) fn semantic_eligible_event_count(&self) -> Result<u64> {
        self.index
            .semantic_eligible_event_count()
            .map_err(anyhow::Error::new)
    }

    pub(crate) fn into_index(self) -> VerifiedIndex {
        self.index
    }

    #[cfg(test)]
    pub(crate) fn from_index(index: VerifiedIndex) -> Self {
        Self { index }
    }
}

#[allow(dead_code)] // Request metadata is retained for CLI/status integrations.
pub(crate) struct SourceBackedRefreshObservation {
    pub(crate) mode: SourceBackedRefreshMode,
    pub(crate) status: String,
    pub(crate) request_id: Option<String>,
    pub(crate) daemon_available: bool,
    pub(crate) source_count: usize,
    pub(crate) pin: PinnedSourceBackedGeneration,
}

/// Coordinates source-backed refresh without ever falling back to a foreground
/// writer. The returned reader is already pinned to one verified generation.
pub(crate) fn coordinate_source_backed_refresh(
    data_root: &Path,
    mode: SourceBackedRefreshMode,
) -> Result<SourceBackedRefreshObservation> {
    if mode == SourceBackedRefreshMode::Off {
        let pin = pin_published_generation(data_root)?.ok_or_else(|| {
            anyhow!("the source-backed index does not exist; retry with daemon refresh enabled")
        })?;
        return Ok(SourceBackedRefreshObservation {
            mode,
            status: "off".to_owned(),
            request_id: None,
            daemon_available: false,
            source_count: 0,
            pin,
        });
    }

    let request = compact_json(json!({
        "schema_version": 1,
        "op": SOURCE_REFRESH_REQUEST_OP,
        "mode": mode.as_str(),
    }));
    let response = match daemon_source_refresh_request(
        data_root,
        request,
        SOURCE_REFRESH_IPC_TIMEOUT,
        SOURCE_REFRESH_RESPONSE_MAX_BYTES,
    ) {
        Ok(Some(response)) => response,
        Ok(None) => return daemon_unavailable_fallback(data_root, mode, None),
        Err(error)
            if error
                .downcast_ref::<DaemonSourceRefreshServiceUnavailable>()
                .is_some() =>
        {
            return daemon_unavailable_fallback(data_root, mode, Some(error))
        }
        Err(error) => return Err(error.context("request daemon-owned source-backed refresh")),
    };
    validate_daemon_refresh_response(&response)?;
    let request_id = response
        .get("request_id")
        .and_then(Value::as_str)
        .filter(|request_id| !request_id.is_empty())
        .ok_or_else(|| anyhow!("daemon source refresh response has no request ID"))?
        .to_owned();

    if mode == SourceBackedRefreshMode::Background {
        let pin = pin_published_generation(data_root)?.ok_or_else(|| {
            anyhow!(
                "daemon source refresh was queued but no published generation exists; retry with --refresh wait"
            )
        })?;
        return Ok(SourceBackedRefreshObservation {
            mode,
            status: response
                .get("request_state")
                .and_then(Value::as_str)
                .unwrap_or("queued")
                .to_owned(),
            request_id: Some(request_id),
            daemon_available: true,
            source_count: response_source_count(&response),
            pin,
        });
    }

    wait_for_published_generation(data_root, request_id, mode)
}

fn wait_for_published_generation(
    data_root: &Path,
    request_id: String,
    mode: SourceBackedRefreshMode,
) -> Result<SourceBackedRefreshObservation> {
    loop {
        let response = match daemon_source_refresh_request(
            data_root,
            compact_json(json!({
                "schema_version": 1,
                "op": SOURCE_REFRESH_STATUS_OP,
                "request_id": request_id,
            })),
            SOURCE_REFRESH_IPC_TIMEOUT,
            SOURCE_REFRESH_RESPONSE_MAX_BYTES,
        ) {
            Ok(Some(response)) => response,
            Ok(None) => {
                return Err(SourceBackedRefreshDaemonUnavailable::new(Some(format!(
                    "daemon became unavailable while waiting for request {request_id}"
                )))
                .into())
            }
            Err(error)
                if error
                    .downcast_ref::<DaemonSourceRefreshServiceUnavailable>()
                    .is_some() =>
            {
                return Err(
                    SourceBackedRefreshDaemonUnavailable::new(Some(format!("{error:#}"))).into(),
                )
            }
            Err(error) => {
                return Err(error.context("wait for daemon-owned source-backed refresh publication"))
            }
        };
        validate_daemon_refresh_response(&response)?;
        match response.get("request_state").and_then(Value::as_str) {
            Some("published") => {
                let expected = response
                    .get("published_generation")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        anyhow!("published daemon source refresh has no generation ID")
                    })?;
                let pin = pin_published_generation(data_root)?.ok_or_else(|| {
                    anyhow!(
                        "daemon published source-backed generation {expected}, but no verified generation can be opened"
                    )
                })?;
                return Ok(SourceBackedRefreshObservation {
                    mode,
                    status: "published".to_owned(),
                    request_id: Some(request_id),
                    daemon_available: true,
                    source_count: response_source_count(&response),
                    pin,
                });
            }
            Some("failed") => {
                let error = response
                    .get("last_error")
                    .and_then(Value::as_str)
                    .unwrap_or("source-backed refresh failed");
                let retained = response
                    .get("published_generation")
                    .and_then(Value::as_str)
                    .or_else(|| response.get("previous_generation").and_then(Value::as_str))
                    .map(|generation| format!("; retained generation {generation}"))
                    .unwrap_or_default();
                return Err(anyhow!(
                    "daemon-owned source-backed refresh failed: {error}{retained}"
                ));
            }
            Some("queued" | "running") => {
                std::thread::sleep(SOURCE_REFRESH_POLL_INTERVAL);
            }
            Some(state) => {
                return Err(anyhow!(
                    "daemon source refresh request {request_id} has unknown state `{state}`"
                ));
            }
            None => {
                return Err(anyhow!(
                    "daemon source refresh response has no request state"
                ))
            }
        }
    }
}

fn daemon_unavailable_fallback(
    data_root: &Path,
    mode: SourceBackedRefreshMode,
    error: Option<anyhow::Error>,
) -> Result<SourceBackedRefreshObservation> {
    if mode == SourceBackedRefreshMode::Background {
        if let Some(pin) = pin_published_generation(data_root)? {
            return Ok(SourceBackedRefreshObservation {
                mode,
                status: "daemon_unavailable".to_owned(),
                request_id: None,
                daemon_available: false,
                source_count: 0,
                pin,
            });
        }
    }
    Err(SourceBackedRefreshDaemonUnavailable::new(error.map(|error| format!("{error:#}"))).into())
}

fn validate_daemon_refresh_response(response: &Value) -> Result<()> {
    if response.get("ok").and_then(Value::as_bool) == Some(true) {
        return Ok(());
    }
    Err(anyhow!(
        "{}",
        response
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("daemon source refresh request failed")
    ))
}

fn response_source_count(response: &Value) -> usize {
    response
        .get("progress")
        .and_then(|progress| progress.get("total_sources"))
        .or_else(|| response.get("source_count"))
        .and_then(Value::as_u64)
        .and_then(|count| usize::try_from(count).ok())
        .unwrap_or(0)
}

pub(super) fn pin_published_generation(
    data_root: &Path,
) -> Result<Option<PinnedSourceBackedGeneration>> {
    let index_root = source_backed_index_root(data_root);
    if !index_root.join("meta.json").is_file() {
        return Ok(None);
    }
    Ok(Some(PinnedSourceBackedGeneration {
        index: VerifiedIndex::open(&index_root).with_context(|| {
            format!(
                "open verified source-backed lexical index {}",
                index_root.display()
            )
        })?,
    }))
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Barrier,
    };

    use ctx_history_capture::{
        DiscoveryPlatform, DiscoveryPlatformDirs, ProviderCatalogSupport, ProviderImportSupport,
        ProviderSource, ProviderSourceKind,
    };

    use super::*;

    struct TestExecutor {
        calls: Arc<AtomicUsize>,
        generation_id: String,
        failure: Option<String>,
    }

    impl SourceBackedRefreshExecutor for TestExecutor {
        fn refresh(
            &self,
            execution: SourceBackedRefreshExecution<'_>,
        ) -> Result<SourceBackedRefreshPublication> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(
                execution.index_root,
                source_backed_index_root(execution.data_root)
            );
            assert!(!execution.request_id.is_empty());
            if let Some(error) = self.failure.as_deref() {
                return Err(anyhow!("{error}"));
            }
            execution.report_progress("refreshing", 0, 1, Some("provider-neutral".to_owned()))?;
            execution.report_progress("verifying", 1, 1, None)?;
            Ok(test_publication(self.generation_id.clone()))
        }
    }

    fn test_publication(generation_id: impl Into<String>) -> SourceBackedRefreshPublication {
        SourceBackedRefreshPublication {
            generation_id: generation_id.into(),
            source_manifest: None,
            scanned_routes: 1,
            unsupported_routes: 0,
            certified_source_count: 1,
            certified_source_bytes: 128,
            timings: SourceBackedRefreshTimings {
                discovery_us: 11,
                scan_stage_us: 22,
                commit_us: 33,
            },
        }
    }

    fn request_id(response: &Value) -> String {
        response
            .get("request_id")
            .and_then(Value::as_str)
            .expect("request ID")
            .to_owned()
    }

    #[test]
    fn default_executor_invokes_one_all_provider_callback_and_maps_progress() {
        let coordinator = SourceBackedRefreshCoordinator::new();
        assert_eq!(
            coordinator.executor.implementation_name(),
            std::any::type_name::<CaptureOwnedSourceBackedRefreshExecutor>()
        );

        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let index_root = source_backed_index_root(&data_root);
        let discovery = DiscoveryContext::new(
            temp.path(),
            temp.path().join("cwd"),
            DiscoveryPlatform::Linux,
            DiscoveryPlatformDirs::default(),
        );
        let updates = Mutex::new(Vec::new());
        let report_progress = |update: SourceBackedRefreshProgressUpdate| {
            updates.lock().unwrap().push((
                update.phase,
                update.completed_sources,
                update.total_sources,
                update.current_source,
            ));
            Ok(())
        };
        let execution = SourceBackedRefreshExecution {
            data_root: &data_root,
            index_root: &index_root,
            request_id: "all-provider-request",
            report_progress: &report_progress,
        };
        let mut provider_wide_calls = 0;

        let publication = execute_capture_owned_refresh_with(
            execution,
            &discovery,
            |observed_discovery, observed_index_root, progress| {
                provider_wide_calls += 1;
                assert_eq!(observed_discovery, &discovery);
                assert_eq!(observed_index_root, index_root);
                progress(CaptureSourceBackedRefreshProgress {
                    phase: "discovering",
                    completed_sources: 0,
                    total_sources: 2,
                    current_source: None,
                    stage_duration: StdDuration::ZERO,
                    elapsed: StdDuration::ZERO,
                    certified_source_count: None,
                    certified_source_bytes: None,
                })?;
                progress(CaptureSourceBackedRefreshProgress {
                    phase: "refreshing",
                    completed_sources: 1,
                    total_sources: 2,
                    current_source: Some("provider-wide-route".to_owned()),
                    stage_duration: StdDuration::ZERO,
                    elapsed: StdDuration::ZERO,
                    certified_source_count: None,
                    certified_source_bytes: None,
                })?;
                progress(CaptureSourceBackedRefreshProgress {
                    phase: "verifying",
                    completed_sources: 2,
                    total_sources: 2,
                    current_source: None,
                    stage_duration: StdDuration::ZERO,
                    elapsed: StdDuration::ZERO,
                    certified_source_count: None,
                    certified_source_bytes: None,
                })?;
                Ok(test_publication("all-provider-generation"))
            },
        )
        .unwrap();

        assert_eq!(provider_wide_calls, 1);
        assert_eq!(publication.generation_id, "all-provider-generation");
        assert_eq!(
            updates.into_inner().unwrap(),
            vec![
                ("discovering".to_owned(), 0, 0, None),
                ("discovering".to_owned(), 0, 2, None),
                (
                    "refreshing".to_owned(),
                    1,
                    2,
                    Some("provider-wide-route".to_owned()),
                ),
                ("verifying".to_owned(), 2, 2, None),
            ]
        );
    }

    #[test]
    fn missing_roots_are_nonblocking_but_detected_selector_gaps_block_publication() {
        let source = |path: &'static str, exists, status| ProviderSource {
            provider: CaptureProvider::Warp,
            path: PathBuf::from(path),
            exists,
            source_format: "warp_sqlite",
            source_kind: ProviderSourceKind::NativeHistory,
            import_support: ProviderImportSupport::Native,
            catalog_support: ProviderCatalogSupport::Native,
            status,
            unsupported_reason: None,
        };
        let missing = SourceBackedAutomaticRegistryIssue::Unavailable {
            source: source(
                "/unavailable/warp.sqlite",
                false,
                ProviderSourceStatus::Missing,
            ),
            reason: SourceBackedAutomaticUnavailableReason::SourceStatus(
                ProviderSourceStatus::Missing,
            ),
        };
        assert!(reject_blocking_automatic_registry_issues(&[missing]).is_ok());

        let selector_gap = SourceBackedAutomaticRegistryIssue::Unavailable {
            source: source(
                "/detected/warp.sqlite",
                true,
                ProviderSourceStatus::Available,
            ),
            reason: SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable {
                detail: "injected selector gap",
            },
        };
        let error = reject_blocking_automatic_registry_issues(&[selector_gap]).unwrap_err();
        assert!(format!("{error:#}").contains("injected selector gap"));
    }

    #[test]
    fn duplicate_concurrent_requests_launch_one_writer() {
        const REQUESTS: usize = 16;

        let coordinator = Arc::new(SourceBackedRefreshCoordinator::new());
        let barrier = Arc::new(Barrier::new(REQUESTS));
        let mut threads = Vec::new();
        for _ in 0..REQUESTS {
            let coordinator = coordinator.clone();
            let barrier = barrier.clone();
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                coordinator.enqueue(Some("generation-1".to_owned()))
            }));
        }
        let responses = threads
            .into_iter()
            .map(|thread| thread.join().expect("request thread"))
            .collect::<Vec<_>>();
        let expected_request_id = request_id(&responses[0]);
        assert!(responses
            .iter()
            .all(|response| request_id(response) == expected_request_id));

        let writer_launches = AtomicUsize::new(0);
        let run = coordinator
            .run_next_with(
                |request_id, coordinator| {
                    writer_launches.fetch_add(1, Ordering::SeqCst);
                    let _ = coordinator.set_progress(
                        request_id,
                        "refreshing",
                        0,
                        1,
                        Some("source-a".to_owned()),
                    );
                    Ok(test_publication("generation-2"))
                },
                || Ok(Some("generation-2".to_owned())),
                |_| Ok(()),
                |_| Ok(()),
            )
            .expect("queued refresh");

        assert_eq!(writer_launches.load(Ordering::SeqCst), 1);
        assert!(run.did_work);
        assert!(!run.failed);
        let status = coordinator
            .status(&expected_request_id)
            .expect("published request status");
        assert_eq!(status["request_state"], "published");
        assert_eq!(status["published_generation"], "generation-2");
        assert_eq!(
            status["coalesced_requests"].as_u64(),
            Some((REQUESTS - 1) as u64)
        );
        assert_eq!(status["certified_source_count"], 1);
        assert_eq!(status["certified_source_bytes"], 128);
        assert_eq!(status["timings_us"]["discovery"], 11);
        assert_eq!(status["timings_us"]["scan_stage"], 22);
        assert_eq!(status["timings_us"]["commit"], 33);
        assert!(coordinator
            .run_next_with(
                |_, _| panic!("duplicate writer launched"),
                || Ok(Some("generation-2".to_owned())),
                |_| Ok(()),
                |_| Ok(()),
            )
            .is_none());
    }

    #[test]
    fn ipc_job_records_source_refresh_only_search_autostart_provenance() {
        let _env_lock = crate::config::TEST_LOCAL_USAGE_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join(crate::config::CONFIG_FILE),
            "[daemon]\nmode = \"source-refresh-only\"\n",
        )
        .unwrap();
        crate::semantic::paths_status::write_daemon_status(
            temp.path(),
            &json!({
                "schema_version": 1,
                "status": "running",
                "start_mode": "auto",
                "trigger_command": "search",
            }),
        )
        .unwrap();
        let coordinator = SourceBackedRefreshCoordinator::new();

        let response = coordinator
            .handle_ipc_request(
                temp.path(),
                &json!({
                    "op": SOURCE_REFRESH_REQUEST_OP,
                    "mode": "background",
                }),
            )
            .unwrap()
            .expect("source refresh response");
        let job = crate::semantic::paths_status::read_daemon_job_status(
            &daemon_source_backed_refresh_job_path(temp.path()),
        )
        .expect("persisted source refresh job");

        assert_eq!(response["daemon_mode"], "source-refresh-only");
        assert_eq!(response["trigger"], "search");
        assert_eq!(response["trigger_provenance"], "autostart");
        assert_eq!(job["daemon_mode"], "source-refresh-only");
        assert_eq!(job["trigger"], "search");
        assert_eq!(job["trigger_provenance"], "autostart");
    }

    #[test]
    fn failed_refresh_retains_the_previous_published_generation() {
        let coordinator = SourceBackedRefreshCoordinator::new();
        let request = coordinator.enqueue(Some("generation-1".to_owned()));
        let request_id = request_id(&request);
        let run = coordinator
            .run_next_with(
                |request_id, coordinator| {
                    let _ = coordinator.set_progress(
                        request_id,
                        "refreshing",
                        0,
                        1,
                        Some("source-a".to_owned()),
                    );
                    Err(anyhow!("injected writer failure before publication"))
                },
                || Ok(Some("generation-1".to_owned())),
                |_| Ok(()),
                |_| Ok(()),
            )
            .expect("queued refresh");

        assert!(run.failed);
        assert!(!run.did_work);
        let status = coordinator
            .status(&request_id)
            .expect("failed request status");
        assert_eq!(status["request_state"], "failed");
        assert_eq!(status["previous_generation"], "generation-1");
        assert_eq!(status["published_generation"], "generation-1");
        assert!(status["last_error"]
            .as_str()
            .is_some_and(|error| error.contains("injected writer failure")));
        assert_eq!(run.job["status"], "failed");
        assert_eq!(run.job["published_generation"], "generation-1");
        assert_eq!(run.job["progress"]["phase"], "failed");
    }

    #[test]
    fn unverified_returned_generation_is_never_recorded_as_published() {
        let coordinator = SourceBackedRefreshCoordinator::new();
        let request = coordinator.enqueue(Some("generation-1".to_owned()));
        let request_id = request_id(&request);
        let publication_records = AtomicUsize::new(0);
        let failure_records = AtomicUsize::new(0);

        let run = coordinator
            .run_next_with(
                |_, _| Ok(test_publication("generation-2")),
                || Ok(Some("generation-1".to_owned())),
                |_| {
                    publication_records.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
                |_| {
                    failure_records.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
            )
            .expect("queued refresh");

        assert!(run.failed);
        assert!(!run.did_work);
        assert_eq!(publication_records.load(Ordering::SeqCst), 0);
        assert_eq!(failure_records.load(Ordering::SeqCst), 1);
        let status = coordinator
            .status(&request_id)
            .expect("failed request status");
        assert_eq!(status["request_state"], "failed");
        assert_eq!(status["previous_generation"], "generation-1");
        assert_eq!(status["published_generation"], "generation-1");
        assert!(status["last_error"]
            .as_str()
            .is_some_and(|error| error.contains("returned generation generation-2")));
    }

    #[test]
    fn injected_executor_completes_a_pending_source_rebuild() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let prepared = data_migration::prepare(&data_root, &[]).unwrap();
        let generation_id = prepared
            .marker()
            .lexical_generation_id
            .clone()
            .expect("prepared lexical generation");
        let calls = Arc::new(AtomicUsize::new(0));
        let coordinator = SourceBackedRefreshCoordinator::with_executor(Arc::new(TestExecutor {
            calls: calls.clone(),
            generation_id: generation_id.clone(),
            failure: None,
        }));
        coordinator.enqueue(Some(generation_id.clone()));

        let run = coordinator.run_next(&data_root).expect("queued refresh");

        assert!(!run.failed);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let marker = data_migration::inspect(&data_root)
            .unwrap()
            .expect("migration marker");
        assert_eq!(marker.phase, MigrationPhase::Ready);
        assert!(!marker.source_rebuild_required);
        assert_eq!(
            marker.lexical_generation_id.as_deref(),
            Some(generation_id.as_str())
        );
    }

    #[test]
    fn injected_executor_failure_records_a_resumable_source_rebuild() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let prepared = data_migration::prepare(&data_root, &[]).unwrap();
        let generation_id = prepared
            .marker()
            .lexical_generation_id
            .clone()
            .expect("prepared lexical generation");
        let coordinator = SourceBackedRefreshCoordinator::with_executor(Arc::new(TestExecutor {
            calls: Arc::new(AtomicUsize::new(0)),
            generation_id: generation_id.clone(),
            failure: Some("provider-neutral executor failed".to_owned()),
        }));
        coordinator.enqueue(Some(generation_id.clone()));

        let run = coordinator.run_next(&data_root).expect("queued refresh");

        assert!(run.failed);
        assert_eq!(run.job["published_generation"], generation_id);
        let marker = data_migration::inspect(&data_root)
            .unwrap()
            .expect("migration marker");
        assert_eq!(marker.phase, MigrationPhase::SourceRebuildFailed);
        assert!(marker.source_rebuild_required);
        assert!(marker.resumable);
        assert!(marker
            .error
            .as_deref()
            .is_some_and(|error| error.contains("provider-neutral executor failed")));
    }
}
