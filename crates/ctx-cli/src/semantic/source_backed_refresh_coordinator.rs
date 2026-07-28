use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration as StdDuration,
};

use anyhow::{anyhow, Context, Result};
use ctx_history_capture::{ingest_codex_source_backed_v0, ProviderSourceStatus};
use ctx_history_core::{utc_now, CaptureProvider};
use ctx_history_index::VerifiedIndex;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{compact_json, provider_sources::discovered_sources_for_provider_report};

use super::{
    paths_status::{daemon_source_backed_refresh_job_path, write_daemon_job_status},
    query_service::{daemon_source_refresh_request, DaemonSourceRefreshServiceUnavailable},
};

const SOURCE_BACKED_INDEX_DIRECTORY: &str = "source-backed-lexical-v0";
const CODEX_SESSION_SOURCE_FORMAT: &str = "codex_session_jsonl_tree";
const SOURCE_REFRESH_REQUEST_OP: &str = "source_refresh_request";
const SOURCE_REFRESH_STATUS_OP: &str = "source_refresh_status";
const SOURCE_REFRESH_ATTEMPT_HISTORY: usize = 64;
const SOURCE_REFRESH_POLL_INTERVAL: StdDuration = StdDuration::from_millis(50);
const SOURCE_REFRESH_IPC_TIMEOUT: StdDuration = StdDuration::from_secs(2);
const SOURCE_REFRESH_RESPONSE_MAX_BYTES: u64 = 64 * 1024;

#[allow(dead_code)] // All modes become live when the source-backed CLI lane connects.
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
            "last_error": self.last_error,
        }))
    }
}

#[derive(Default)]
struct SourceBackedRefreshCoordinatorState {
    active_request_id: Option<String>,
    attempts: VecDeque<SourceBackedRefreshAttempt>,
}

pub(in crate::semantic) struct SourceBackedRefreshCoordinator {
    state: Mutex<SourceBackedRefreshCoordinatorState>,
}

impl Default for SourceBackedRefreshCoordinator {
    fn default() -> Self {
        Self {
            state: Mutex::new(SourceBackedRefreshCoordinatorState::default()),
        }
    }
}

pub(in crate::semantic) struct SourceBackedRefreshRun {
    pub(in crate::semantic) job: Value,
    pub(in crate::semantic) did_work: bool,
    pub(in crate::semantic) failed: bool,
}

impl SourceBackedRefreshCoordinator {
    pub(in crate::semantic) fn new() -> Self {
        Self::default()
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
                let response = self.enqueue(previous_generation);
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
        self.run_next_with(
            |request_id, coordinator| {
                execute_source_backed_refresh(data_root, request_id, coordinator)
            },
            || published_generation_id(data_root),
        )
    }

    fn enqueue(&self, observed_generation: Option<String>) -> Value {
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

    fn run_next_with<Execute, Probe>(
        &self,
        execute: Execute,
        probe: Probe,
    ) -> Option<SourceBackedRefreshRun>
    where
        Execute: FnOnce(&str, &Self) -> Result<String>,
        Probe: FnOnce() -> Result<Option<String>>,
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
        let mut state = self.lock_state();
        let (failed, did_work, job) = {
            let attempt = find_attempt_mut(&mut state, &request_id)?;
            attempt.finished_at_ms = Some(utc_now().timestamp_millis());
            attempt.progress.current_source = None;

            match (execution, observed_generation) {
                (Ok(expected), Ok(Some(observed))) if expected == observed => {
                    attempt.state = SourceBackedRefreshState::Published;
                    attempt.published_generation = Some(observed.clone());
                    attempt.progress.phase = "published".to_owned();
                    attempt.progress.completed_sources = attempt.progress.total_sources;
                }
                (Ok(expected), Ok(observed)) => {
                    attempt.state = SourceBackedRefreshState::Failed;
                    attempt.published_generation = observed.clone();
                    attempt.progress.phase = "failed".to_owned();
                    attempt.last_error = Some(format!(
                        "source-backed refresh returned generation {expected}, but the verified published generation is {:?}",
                        observed
                    ));
                }
                (Ok(expected), Err(error)) => {
                    attempt.state = SourceBackedRefreshState::Failed;
                    attempt.progress.phase = "failed".to_owned();
                    attempt.last_error = Some(format!(
                        "source-backed refresh returned generation {expected}, but publication verification failed: {error:#}"
                    ));
                }
                (Err(error), Ok(observed)) => {
                    attempt.state = SourceBackedRefreshState::Failed;
                    attempt.published_generation = observed.clone();
                    attempt.progress.phase = "failed".to_owned();
                    attempt.last_error = Some(format!("{error:#}"));
                }
                (Err(error), Err(probe_error)) => {
                    attempt.state = SourceBackedRefreshState::Failed;
                    attempt.progress.phase = "failed".to_owned();
                    attempt.last_error = Some(format!(
                        "{error:#}; verifying the retained generation also failed: {probe_error:#}"
                    ));
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

fn execute_source_backed_refresh(
    data_root: &Path,
    request_id: &str,
    coordinator: &SourceBackedRefreshCoordinator,
) -> Result<String> {
    record_source_backed_refresh_progress(
        data_root,
        coordinator,
        request_id,
        "discovering",
        0,
        0,
        None,
    )?;
    let report = discovered_sources_for_provider_report(CaptureProvider::Codex);
    let mut roots = report
        .sources
        .into_iter()
        .filter(|source| {
            source.exists
                && source.source_format == CODEX_SESSION_SOURCE_FORMAT
                && source.status == ProviderSourceStatus::Available
        })
        .map(|source| source.path)
        .collect::<Vec<_>>();
    roots.sort();
    roots.dedup();

    if roots.is_empty() {
        let detail = report
            .issues
            .first()
            .map(|issue| issue.reason)
            .unwrap_or("no ordinary Codex rollout/session JSONL tree was discovered");
        return Err(anyhow!("cannot discover Codex session sources: {detail}"));
    }
    if roots.len() != 1 {
        return Err(anyhow!(
            "source-backed daemon refresh discovered {} Codex roots; atomic multi-root publication requires the capture-owned grouped refresh hook",
            roots.len()
        ));
    }

    let root = roots
        .pop()
        .ok_or_else(|| anyhow!("source-backed refresh lost its discovered Codex root"))?;
    record_source_backed_refresh_progress(
        data_root,
        coordinator,
        request_id,
        "refreshing",
        0,
        1,
        Some(root.display().to_string()),
    )?;
    let receipt = ingest_codex_source_backed_v0(&root, source_backed_index_root(data_root))
        .with_context(|| format!("refresh source-backed Codex tree {}", root.display()))?;
    record_source_backed_refresh_progress(
        data_root,
        coordinator,
        request_id,
        "verifying",
        1,
        1,
        None,
    )?;
    Ok(receipt.commit.generation_id)
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

#[allow(dead_code)] // The source-backed CLI owner consumes this cross-lane seam.
pub(crate) struct PinnedSourceBackedGeneration {
    index: VerifiedIndex,
}

#[allow(dead_code)] // The source-backed CLI owner consumes this cross-lane seam.
impl PinnedSourceBackedGeneration {
    pub(crate) fn generation_id(&self) -> &str {
        self.index.generation_id()
    }

    pub(crate) fn into_index(self) -> VerifiedIndex {
        self.index
    }
}

#[allow(dead_code)] // The source-backed CLI owner consumes this cross-lane seam.
pub(crate) struct SourceBackedRefreshObservation {
    pub(crate) mode: SourceBackedRefreshMode,
    pub(crate) status: String,
    pub(crate) request_id: Option<String>,
    pub(crate) daemon_available: bool,
    pub(crate) pin: PinnedSourceBackedGeneration,
}

/// Coordinates source-backed refresh without ever falling back to a foreground
/// writer. The returned reader is already pinned to one verified generation.
#[allow(dead_code)] // The source-backed CLI owner consumes this cross-lane seam.
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
        let response = daemon_source_refresh_request(
            data_root,
            compact_json(json!({
                "schema_version": 1,
                "op": SOURCE_REFRESH_STATUS_OP,
                "request_id": request_id,
            })),
            SOURCE_REFRESH_IPC_TIMEOUT,
            SOURCE_REFRESH_RESPONSE_MAX_BYTES,
        )
        .context("wait for daemon-owned source-backed refresh publication")?
        .ok_or_else(|| {
            anyhow!(
                "daemon became unavailable while waiting for source-backed refresh request {request_id}"
            )
        })?;
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
                pin,
            });
        }
    }
    let detail = error
        .map(|error| format!(": {error:#}"))
        .unwrap_or_default();
    Err(anyhow!(
        "the ctx daemon is unavailable for source-backed refresh{detail}; no foreground writer was started"
    ))
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

fn pin_published_generation(data_root: &Path) -> Result<Option<PinnedSourceBackedGeneration>> {
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

    use super::*;

    fn request_id(response: &Value) -> String {
        response
            .get("request_id")
            .and_then(Value::as_str)
            .expect("request ID")
            .to_owned()
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
                    Ok("generation-2".to_owned())
                },
                || Ok(Some("generation-2".to_owned())),
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
        assert!(coordinator
            .run_next_with(
                |_, _| panic!("duplicate writer launched"),
                || Ok(Some("generation-2".to_owned()))
            )
            .is_none());
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
}
