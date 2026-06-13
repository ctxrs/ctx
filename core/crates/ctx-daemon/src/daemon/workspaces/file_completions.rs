use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ctx_core::ids::{SessionId, WorkspaceId};
use ctx_core::models::{ExecutionEnvironment, Workspace, Worktree};
use ctx_observability::perf_telemetry::{PerfMetric, PerfMetricKind, PerfTelemetry};
use ctx_storage_admission::is_storage_exhaustion_error;
use ctx_store::Store;
use ctx_worktree_data_plane::{
    resolve_worktree_data_plane_with_host as resolve_worktree_data_plane, WorktreeDataPlaneHost,
};
use ctx_worktree_vcs_service::{
    filter_and_rank_paths, list_host_git_files as service_list_host_git_files,
    workspace_has_git_repo, CachedFileCompletions,
};

use crate::daemon::state::WorkspaceFileCompletionsCache;
use crate::daemon::{
    SessionFileCompletionsHandle, SessionStoreAccessError, TimedEntry,
    WorkspaceFileCompletionsHandle,
};

mod container;

const DEFAULT_LIMIT: u32 = 20;
const MAX_LIMIT: u32 = 200;
const CACHE_TTL: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum FileCompletionsErrorKind {
    NotFound,
    Forbidden,
    InsufficientStorage,
    Internal,
}

#[derive(Debug)]
pub struct FileCompletionsError {
    kind: FileCompletionsErrorKind,
    message: String,
}

impl FileCompletionsError {
    fn not_found(message: impl Into<String>) -> Self {
        Self {
            kind: FileCompletionsErrorKind::NotFound,
            message: message.into(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            kind: FileCompletionsErrorKind::Internal,
            message: message.into(),
        }
    }

    pub(super) fn from_internal_error(context: &str, error: anyhow::Error) -> Self {
        let kind = if ctx_settings_service::is_execution_policy_denial(&error) {
            FileCompletionsErrorKind::Forbidden
        } else if error
            .chain()
            .any(|cause| is_storage_exhaustion_error(&cause.to_string()))
        {
            FileCompletionsErrorKind::InsufficientStorage
        } else {
            FileCompletionsErrorKind::Internal
        };
        Self {
            kind,
            message: format!("{context}: {error:#}"),
        }
    }

    pub fn kind(&self) -> FileCompletionsErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl SessionFileCompletionsHandle {
    pub(in crate::daemon) async fn complete_files_for_session(
        &self,
        session_id: SessionId,
        query: Option<String>,
        limit: Option<u32>,
    ) -> Result<Vec<String>, FileCompletionsError> {
        complete_files_for_session_with_runtime(self, session_id, query, limit).await
    }
}

impl WorkspaceFileCompletionsHandle {
    pub(in crate::daemon) async fn complete_files_for_workspace(
        &self,
        workspace_id: WorkspaceId,
        query: Option<String>,
        limit: Option<u32>,
    ) -> Result<Vec<String>, FileCompletionsError> {
        complete_files_for_workspace_with_runtime(
            self.global_store(),
            self.workspace_file_completions_cache(),
            self.perf_telemetry(),
            workspace_id,
            query,
            limit,
        )
        .await
    }
}

#[async_trait::async_trait]
impl WorktreeDataPlaneHost for SessionFileCompletionsHandle {
    async fn get_workspace(
        handle: &Self,
        workspace_id: WorkspaceId,
    ) -> anyhow::Result<Option<Workspace>> {
        handle.global_store().get_workspace(workspace_id).await
    }

    async fn workspace_store(handle: &Self, workspace_id: WorkspaceId) -> anyhow::Result<Store> {
        handle.store_for_workspace(workspace_id).await
    }
}

async fn complete_files_for_session_with_runtime(
    handle: &SessionFileCompletionsHandle,
    session_id: SessionId,
    query: Option<String>,
    limit: Option<u32>,
) -> Result<Vec<String>, FileCompletionsError> {
    let store = store_for_existing_session(handle, session_id).await?;
    let session = store
        .get_session(session_id)
        .await
        .map_err(|err| FileCompletionsError::internal(format!("loading session: {err}")))?
        .ok_or_else(|| FileCompletionsError::not_found("session not found"))?;
    let worktree = store
        .get_worktree(session.worktree_id)
        .await
        .map_err(|err| FileCompletionsError::internal(format!("loading session worktree: {err}")))?
        .ok_or_else(|| FileCompletionsError::not_found("session worktree not found"))?;
    let data_plane = resolve_worktree_data_plane(handle, &worktree)
        .await
        .map_err(|err| FileCompletionsError::internal(format!("resolving data plane: {err}")))?;
    let execution_environment = match data_plane.execution_mode {
        ctx_settings_model::ExecutionMode::Host => ExecutionEnvironment::Host,
        ctx_settings_model::ExecutionMode::Sandbox => ExecutionEnvironment::Sandbox,
    };
    if session.execution_environment != execution_environment {
        tracing::warn!(
            session_id = %session.id.0,
            stored = session.execution_environment.as_str(),
            resolved = execution_environment.as_str(),
            "session file completions resolved a different execution_environment than persisted metadata"
        );
    }

    let files = cached_worktree_files(handle, &worktree, execution_environment).await?;
    Ok(rank_files(files.as_ref(), query, limit))
}

pub(in crate::daemon::workspaces) async fn complete_files_for_workspace_with_runtime(
    global_store: &Store,
    workspace_file_completions_cache: &WorkspaceFileCompletionsCache,
    perf_telemetry: &PerfTelemetry,
    workspace_id: WorkspaceId,
    query: Option<String>,
    limit: Option<u32>,
) -> Result<Vec<String>, FileCompletionsError> {
    let workspace = global_store
        .get_workspace(workspace_id)
        .await
        .map_err(|err| FileCompletionsError::internal(format!("loading workspace: {err}")))?
        .ok_or_else(|| FileCompletionsError::not_found("workspace not found"))?;

    let root = PathBuf::from(&workspace.root_path);
    if !workspace_has_git_repo(&root).await {
        return Ok(Vec::new());
    }

    let files = cached_workspace_files(
        workspace_file_completions_cache,
        perf_telemetry,
        workspace_id,
        &root,
    )
    .await?;
    Ok(rank_files(files.as_ref(), query, limit))
}

async fn store_for_existing_session(
    handle: &SessionFileCompletionsHandle,
    session_id: SessionId,
) -> Result<ctx_store::Store, FileCompletionsError> {
    handle
        .existing_session_store(session_id)
        .await
        .map_err(session_store_access_file_completions_error)
}

fn session_store_access_file_completions_error(
    error: SessionStoreAccessError,
) -> FileCompletionsError {
    match error {
        SessionStoreAccessError::NotFound => {
            FileCompletionsError::not_found("session store not found")
        }
        SessionStoreAccessError::LookupUnavailable(err) => {
            FileCompletionsError::internal(format!("session store unavailable: {err:#}"))
        }
        SessionStoreAccessError::StoreUnavailable => {
            FileCompletionsError::internal("session store unavailable")
        }
    }
}

async fn cached_worktree_files(
    handle: &SessionFileCompletionsHandle,
    worktree: &Worktree,
    execution_environment: ExecutionEnvironment,
) -> Result<Arc<Vec<String>>, FileCompletionsError> {
    let now = Instant::now();
    let mut cache = handle.worktree_file_completions_cache().lock().await;
    if let Some(entry) = cache.get_mut(&worktree.id) {
        entry.touch();
        if now.duration_since(entry.value.cached_at) <= CACHE_TTL {
            return Ok(entry.value.files.clone());
        }
    }
    drop(cache);
    load_and_cache_worktree_files(handle, worktree, execution_environment, now).await
}

async fn cached_workspace_files(
    workspace_file_completions_cache: &WorkspaceFileCompletionsCache,
    perf_telemetry: &PerfTelemetry,
    workspace_id: WorkspaceId,
    root: &Path,
) -> Result<Arc<Vec<String>>, FileCompletionsError> {
    let now = Instant::now();
    let mut cache = workspace_file_completions_cache.lock().await;
    if let Some(entry) = cache.get_mut(&workspace_id) {
        entry.touch();
        if now.duration_since(entry.value.cached_at) <= CACHE_TTL {
            return Ok(entry.value.files.clone());
        }
    }
    drop(cache);
    load_and_cache_workspace_files(
        workspace_file_completions_cache,
        perf_telemetry,
        workspace_id,
        root,
        now,
    )
    .await
}

async fn load_and_cache_worktree_files(
    handle: &SessionFileCompletionsHandle,
    worktree: &Worktree,
    execution_environment: ExecutionEnvironment,
    now: Instant,
) -> Result<Arc<Vec<String>>, FileCompletionsError> {
    let started_at = Instant::now();
    let data_plane = resolve_worktree_data_plane(handle, worktree)
        .await
        .map_err(|err| FileCompletionsError::internal(format!("resolving data plane: {err}")))?;
    let root = data_plane.live_worktree_root.clone();
    let files = if matches!(
        data_plane.execution_mode,
        ctx_settings_model::ExecutionMode::Sandbox
    ) {
        Arc::new(
            container::list_container_worktree_files(handle, worktree, execution_environment)
                .await?,
        )
    } else {
        Arc::new(list_host_git_files(&root).await?)
    };

    let mut cache = handle.worktree_file_completions_cache().lock().await;
    cache.insert(
        worktree.id,
        TimedEntry::new(CachedFileCompletions {
            cached_at: now,
            files: files.clone(),
        }),
    );
    record_list_files_metric(handle.perf_telemetry(), "list_files_worktree", started_at).await;
    Ok(files)
}

async fn load_and_cache_workspace_files(
    workspace_file_completions_cache: &WorkspaceFileCompletionsCache,
    perf_telemetry: &PerfTelemetry,
    workspace_id: WorkspaceId,
    root: &Path,
    now: Instant,
) -> Result<Arc<Vec<String>>, FileCompletionsError> {
    let started_at = Instant::now();
    let files = Arc::new(list_host_git_files(root).await?);

    let mut cache = workspace_file_completions_cache.lock().await;
    cache.insert(
        workspace_id,
        TimedEntry::new(CachedFileCompletions {
            cached_at: now,
            files: files.clone(),
        }),
    );
    record_list_files_metric(perf_telemetry, "list_files_workspace", started_at).await;
    Ok(files)
}

async fn list_host_git_files(root: &Path) -> Result<Vec<String>, FileCompletionsError> {
    service_list_host_git_files(root)
        .await
        .map_err(|err| FileCompletionsError::internal(format!("listing host git files: {err}")))
}

fn rank_files(paths: &[String], query: Option<String>, limit: Option<u32>) -> Vec<String> {
    let query = query.unwrap_or_default();
    let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT) as usize;
    filter_and_rank_paths(paths, &query, limit)
}

async fn record_list_files_metric(
    perf_telemetry: &PerfTelemetry,
    event: &'static str,
    started_at: Instant,
) {
    let mut labels = HashMap::new();
    labels.insert("event".to_string(), event.to_string());
    labels.insert("source".to_string(), "daemon".to_string());
    let metric = PerfMetric {
        name: "fs.list_files_ms".to_string(),
        kind: PerfMetricKind::Histogram,
        unit: "ms".to_string(),
        value: started_at.elapsed().as_millis() as f64,
        labels,
    };
    perf_telemetry.record_metric(metric, None, None, None).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_error_classifies_execution_policy_denials() {
        let error = ctx_settings_service::HostExecutionPolicy::SandboxOnly
            .validate_execution_environment(ExecutionEnvironment::Host)
            .expect_err("host execution should be denied");

        let error = FileCompletionsError::from_internal_error("resolving settings", error);

        assert_eq!(error.kind(), FileCompletionsErrorKind::Forbidden);
    }

    #[test]
    fn internal_error_classifies_storage_exhaustion() {
        let error = anyhow::anyhow!("wrapper")
            .context("Insufficient storage capacity for creating an isolated task worktree");

        let error = FileCompletionsError::from_internal_error("resolving settings", error);

        assert_eq!(error.kind(), FileCompletionsErrorKind::InsufficientStorage);
    }
}
