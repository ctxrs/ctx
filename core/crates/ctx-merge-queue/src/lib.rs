use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
#[cfg(target_os = "linux")]
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::{mpsc, Mutex, Notify};

use ctx_core::ids::{MergeQueueEntryId, MergeQueueRunId, SessionId, WorkspaceId, WorktreeId};
use ctx_core::models::{
    MergeQueueEntry, MergeQueueEntryStatus, MergeQueuePatchSource, MergeQueueRun,
    MergeQueueRunStatus, VcsKind, Workspace, Worktree,
};
use ctx_fs::git::git_status_porcelain;
use ctx_fs::vcs::{self, VcsDriver};
use ctx_store::Store;
use ctx_workspace_config::{load_merge_queue_config, MergeQueueCanonicalSync, MergeQueueConfig};

mod commands;
mod context;
mod execution;
mod runtime_scheduler;
mod storage;
mod sync;
mod target;

use commands::{command_for_shell, merge_queue_command, QueueError};
use context::*;
use execution::*;
pub use runtime_scheduler::{
    activate_workspace_merge_queue, begin_workspace_drain,
    cancel_queued_entries_for_disabled_workspace,
    cancel_store_queued_entries_for_disabled_workspace, finish_workspace_drain,
    reschedule_workspace_after_drain, schedule_store_if_enabled_and_queued,
    schedule_workspace_drain, schedule_workspace_if_enabled_and_queued, spawn_merge_queue_runner,
    WorkspaceDrainStop,
};
use storage::{merge_queue_log_path, open_log_file, write_log_line, write_patch_file};
use sync::maybe_update_worktree_base_commit_for_path;

#[cfg(target_os = "linux")]
const TOOL_SLICE_UNIT: &str = "ctx-tools.slice";
#[cfg(not(target_os = "linux"))]
const TOOL_SLICE_UNIT: &str = "ctx-tools.slice";

const MERGE_QUEUE_CANONICAL_REMOTE: &str = "canonical";
const MERGE_QUEUE_HEAD_REF: &str = "refs/heads/ctx-merge-queue";
const MERGE_QUEUE_CONFLICT_MESSAGE: &str = concat!(
    "Your merge queue submission produces conflicts with the current head. ",
    "Please rebase your changes, carefully considering the intent of your changes and the intent of the upstream changes. ",
    "If in doubt about how to resolve conflicts, please ask for help."
);

#[derive(Debug, Clone)]
pub struct MergeQueueSubmitParams {
    pub session_id: Option<SessionId>,
    pub worktree_id: Option<WorktreeId>,
    pub worktree_root: Option<String>,
    pub target_branch: Option<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MergeQueueToolExecEvent {
    pub entry_id: MergeQueueEntryId,
    pub session_id: Option<SessionId>,
    pub worktree_id: Option<WorktreeId>,
    pub command: String,
    pub workdir: Option<String>,
    pub used_tool_slice: bool,
    pub tool_slice_unit: &'static str,
}

#[derive(Debug, Clone)]
pub enum MergeQueueNotice {
    Sync {
        session_id: SessionId,
        worktree_id: WorktreeId,
        target_branch: String,
        previous_commit_sha: String,
        commit_sha: String,
        message: String,
    },
    CanonicalSync {
        session_id: SessionId,
        worktree_id: Option<WorktreeId>,
        target_branch: String,
        commit_sha: String,
        status: String,
        message: String,
    },
}

#[derive(Default)]
pub struct MergeQueueScheduleState {
    pub running: HashSet<WorkspaceId>,
    pub pending: HashSet<WorkspaceId>,
}

pub struct MergeQueueRuntime {
    notify: Arc<Notify>,
    schedule_tx: mpsc::UnboundedSender<WorkspaceId>,
    schedule_rx: Mutex<Option<mpsc::UnboundedReceiver<WorkspaceId>>>,
    schedule_state: Mutex<MergeQueueScheduleState>,
}

impl MergeQueueRuntime {
    pub fn new() -> Self {
        let (schedule_tx, schedule_rx) = mpsc::unbounded_channel();
        Self {
            notify: Arc::new(Notify::new()),
            schedule_tx,
            schedule_rx: Mutex::new(Some(schedule_rx)),
            schedule_state: Mutex::new(MergeQueueScheduleState::default()),
        }
    }

    pub fn schedule(&self, workspace_id: WorkspaceId) {
        let _ = self.schedule_tx.send(workspace_id);
    }

    pub fn notify_one(&self) {
        self.notify.notify_one();
    }

    pub fn notify_waiters(&self) {
        self.notify.notify_waiters();
    }

    pub fn notifier(&self) -> Arc<Notify> {
        Arc::clone(&self.notify)
    }

    pub async fn take_schedule_rx(&self) -> Option<mpsc::UnboundedReceiver<WorkspaceId>> {
        self.schedule_rx.lock().await.take()
    }

    pub async fn begin_workspace_drain(&self, workspace_id: WorkspaceId) -> bool {
        let mut schedule_state = self.schedule_state.lock().await;
        let inserted = schedule_state.running.insert(workspace_id);
        if inserted {
            schedule_state.pending.remove(&workspace_id);
        } else {
            schedule_state.pending.insert(workspace_id);
        }
        inserted
    }

    pub async fn finish_workspace_drain(&self, workspace_id: WorkspaceId) -> bool {
        let mut schedule_state = self.schedule_state.lock().await;
        schedule_state.running.remove(&workspace_id);
        schedule_state.pending.remove(&workspace_id)
    }

    pub async fn running_workspaces(&self) -> HashSet<WorkspaceId> {
        self.schedule_state.lock().await.running.clone()
    }

    pub async fn is_pending(&self, workspace_id: WorkspaceId) -> bool {
        self.schedule_state
            .lock()
            .await
            .pending
            .contains(&workspace_id)
    }
}

impl Default for MergeQueueRuntime {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
pub trait MergeQueueHost: Send + Sync + 'static {
    fn merge_queue_runtime(state: &Self) -> &MergeQueueRuntime;

    async fn protected_workspace_store(state: &Self, workspace_id: WorkspaceId) -> Result<Store>;
    async fn raw_workspace_store(state: &Self, workspace_id: WorkspaceId) -> Result<Store>;
    async fn session_store(state: &Self, session_id: SessionId) -> Result<Store>;
    async fn worktree_store(state: &Self, worktree_id: WorktreeId) -> Result<Store>;
    async fn get_workspace(state: &Self, workspace_id: WorkspaceId) -> Result<Option<Workspace>>;
    async fn upsert_workspace_worktree_index(
        state: &Self,
        worktree_id: WorktreeId,
        workspace_id: WorkspaceId,
    ) -> Result<()>;
    async fn publish_notice(state: &Arc<Self>, notice: MergeQueueNotice) -> Result<()>;
    fn emit_tool_exec(state: &Self, event: MergeQueueToolExecEvent);
}

fn vcs_driver_for_worktree(worktree: &Worktree) -> Arc<dyn VcsDriver> {
    vcs::driver_for_kind(worktree.vcs_kind.clone())
}

pub async fn get_workspace_merge_queue_entry<H: MergeQueueHost>(
    state: &H,
    workspace_id: WorkspaceId,
    entry_id: MergeQueueEntryId,
) -> Result<MergeQueueEntry> {
    let store = H::protected_workspace_store(state, workspace_id).await?;
    store
        .get_merge_queue_entry(entry_id)
        .await?
        .filter(|entry| entry.workspace_id == workspace_id)
        .ok_or_else(|| anyhow::anyhow!("merge queue entry not found"))
}

pub async fn list_queued_entries_for_workspace<H: MergeQueueHost>(
    state: &H,
    workspace_id: WorkspaceId,
) -> Result<Vec<MergeQueueEntry>> {
    let store = H::raw_workspace_store(state, workspace_id).await?;
    let mut entries = store.list_queued_merge_queue_entries().await?;
    entries.sort_by_key(|entry| entry.created_at);
    Ok(entries)
}

pub async fn submit_merge_queue_entry<H: MergeQueueHost>(
    state: &Arc<H>,
    params: MergeQueueSubmitParams,
) -> Result<MergeQueueEntry> {
    let context = resolve_merge_queue_context(
        state,
        params.session_id,
        params.worktree_id,
        params.worktree_root,
    )
    .await?;
    let workspace = context.workspace;
    let mut worktree = context.worktree;
    let workspace_store = H::protected_workspace_store(state.as_ref(), workspace.id).await?;
    let config = load_merge_queue_config(&workspace_store).await?;
    if !config.enabled {
        bail!("merge queue is disabled for this workspace");
    }

    let target_branch = params
        .target_branch
        .as_deref()
        .unwrap_or(&config.target_branch)
        .trim()
        .to_string();
    if target_branch.is_empty() {
        bail!("target_branch is required");
    }

    let vcs = context.vcs;
    let worktree_root = context.worktree_root;
    vcs.assert_repo(worktree_root.as_path()).await?;
    let dirty = vcs.status_porcelain(worktree_root.as_path()).await?;
    let dirty = if vcs.kind() == VcsKind::Jj {
        dirty
            .into_iter()
            .filter(|entry| entry.starts_with("?? "))
            .collect::<Vec<_>>()
    } else {
        dirty
    };
    if !dirty.is_empty() {
        bail!(
            "worktree has uncommitted changes:\n{}",
            dirty
                .iter()
                .take(24)
                .map(|entry| format!("- {entry}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    let merge_base = vcs
        .merge_base(worktree_root.as_path(), &target_branch, "HEAD")
        .await?;
    let worktree_patch = vcs
        .build_worktree_patch(worktree_root.as_path(), &merge_base)
        .await?;
    if worktree_patch.patch.trim().is_empty() {
        bail!("no changes detected; nothing to submit");
    }
    if worktree.is_none() {
        let worktree_id = WorktreeId::new();
        let worktree_record = Worktree {
            id: worktree_id,
            workspace_id: workspace.id,
            root_path: worktree_root.to_string_lossy().to_string(),
            base_commit_sha: worktree_patch.base_revision.clone(),
            git_branch: None,
            vcs_kind: Some(vcs.kind()),
            base_revision: Some(worktree_patch.base_revision.clone()),
            vcs_ref: None,
            created_at: Utc::now(),
            bootstrap_status: None,
            bootstrap_started_at: None,
            bootstrap_finished_at: None,
            bootstrap_exit_code: None,
            bootstrap_timeout_sec: None,
            bootstrap_error: None,
            bootstrap_log_path: None,
            bootstrap_log_truncated: None,
            bootstrap_command: None,
            bootstrap_script_path: None,
        };
        workspace_store
            .insert_worktree(worktree_record.clone())
            .await?;
        if let Err(err) =
            H::upsert_workspace_worktree_index(state.as_ref(), worktree_id, workspace.id).await
        {
            tracing::warn!(
                worktree_id = %worktree_id.0,
                "failed to update worktree index: {err:?}"
            );
        }
        worktree = Some(worktree_record);
    }
    let patch_source = MergeQueuePatchSource::Generated;
    let base_commit_sha = Some(worktree_patch.base_revision);
    let head_commit_sha = Some(worktree_patch.head_revision);
    let patch_text = worktree_patch.patch;

    let worktree = worktree
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("worktree is required to submit to the merge queue"))?;
    let entry_id = MergeQueueEntryId::new();
    let patch_path =
        write_patch_file(Path::new(&workspace.root_path), entry_id, &patch_text).await?;
    let now = Utc::now();
    let entry = MergeQueueEntry {
        id: entry_id,
        workspace_id: workspace.id,
        worktree_id: Some(worktree.id),
        session_id: params.session_id,
        target_branch,
        message: params.message,
        patch_source,
        base_commit_sha,
        head_commit_sha,
        patch_path: patch_path.to_string_lossy().to_string(),
        patch_size: patch_text.len() as i64,
        status: MergeQueueEntryStatus::Queued,
        result_commit_sha: None,
        error_message: None,
        created_at: now,
        updated_at: now,
    };
    workspace_store.create_merge_queue_entry(&entry).await?;
    H::merge_queue_runtime(state.as_ref()).schedule(workspace.id);
    H::merge_queue_runtime(state.as_ref()).notify_one();
    let entry = wait_for_merge_queue_completion(state, entry.workspace_id, entry.id).await?;
    ensure_merge_queue_success(&entry)?;
    Ok(entry)
}

pub async fn cancel_merge_queue_entry<H: MergeQueueHost>(
    state: &Arc<H>,
    workspace_id: WorkspaceId,
    entry_id: MergeQueueEntryId,
) -> Result<MergeQueueEntry> {
    let mut entry = get_workspace_merge_queue_entry(state.as_ref(), workspace_id, entry_id).await?;
    let store = H::protected_workspace_store(state.as_ref(), workspace_id).await?;
    match entry.status {
        MergeQueueEntryStatus::Queued => {
            entry.status = MergeQueueEntryStatus::Cancelled;
            entry.updated_at = Utc::now();
            store.update_merge_queue_entry(&entry).await?;
            H::merge_queue_runtime(state.as_ref()).notify_waiters();
            Ok(entry)
        }
        MergeQueueEntryStatus::Running => {
            bail!("cannot cancel a running merge queue entry");
        }
        _ => Ok(entry),
    }
}

pub async fn retry_merge_queue_entry<H: MergeQueueHost>(
    state: &Arc<H>,
    workspace_id: WorkspaceId,
    entry_id: MergeQueueEntryId,
) -> Result<MergeQueueEntry> {
    let mut entry = get_workspace_merge_queue_entry(state.as_ref(), workspace_id, entry_id).await?;
    let store = H::protected_workspace_store(state.as_ref(), workspace_id).await?;
    match entry.status {
        MergeQueueEntryStatus::Failed | MergeQueueEntryStatus::Conflict => {
            entry.status = MergeQueueEntryStatus::Queued;
            entry.error_message = None;
            entry.result_commit_sha = None;
            entry.updated_at = Utc::now();
            store.update_merge_queue_entry(&entry).await?;
            H::merge_queue_runtime(state.as_ref()).schedule(workspace_id);
            H::merge_queue_runtime(state.as_ref()).notify_one();
            H::merge_queue_runtime(state.as_ref()).notify_waiters();
            Ok(entry)
        }
        _ => Ok(entry),
    }
}

async fn run_entry<H: MergeQueueHost>(
    state: &Arc<H>,
    workspace: &Workspace,
    mut entry: MergeQueueEntry,
    cfg: &MergeQueueConfig,
) -> Result<()> {
    let store = H::protected_workspace_store(state.as_ref(), workspace.id).await?;
    let run_id = MergeQueueRunId::new();
    let log_path = merge_queue_log_path(Path::new(&workspace.root_path), run_id);
    let mut run = MergeQueueRun {
        id: run_id,
        entry_id: entry.id,
        status: MergeQueueRunStatus::Running,
        started_at: Utc::now(),
        finished_at: None,
        exit_code: None,
        log_path: Some(log_path.to_string_lossy().to_string()),
        error_message: None,
        result_commit_sha: None,
    };
    store.create_merge_queue_run(&run).await?;

    let mut log_file = open_log_file(&log_path).await?;
    write_log_line(&mut log_file, "# ctx merge queue\n").await?;
    write_log_line(
        &mut log_file,
        &format!("entry: {} target: {}\n", entry.id.0, entry.target_branch),
    )
    .await?;

    let result = run_entry_inner(state, workspace, &entry, cfg, &mut log_file).await;
    let now = Utc::now();
    match result {
        Ok(commit_sha) => {
            entry.status = MergeQueueEntryStatus::Passed;
            entry.result_commit_sha = Some(commit_sha.clone());
            entry.error_message = None;
            entry.updated_at = now;
            run.status = MergeQueueRunStatus::Passed;
            run.result_commit_sha = Some(commit_sha.clone());
            run.finished_at = Some(now);
            store.update_merge_queue_entry(&entry).await?;
            store.update_merge_queue_run(&run).await?;
            H::merge_queue_runtime(state.as_ref()).notify_waiters();
            if let Err(err) =
                maybe_sync_originating_worktree(state, workspace, &entry, &commit_sha).await
            {
                tracing::warn!("merge queue sync failed: {err:#}");
            }
        }
        Err(QueueError::Conflict { message }) => {
            entry.status = MergeQueueEntryStatus::Conflict;
            entry.error_message = Some(message.clone());
            entry.updated_at = now;
            run.status = MergeQueueRunStatus::Conflict;
            run.error_message = Some(message);
            run.finished_at = Some(now);
            store.update_merge_queue_entry(&entry).await?;
            store.update_merge_queue_run(&run).await?;
            H::merge_queue_runtime(state.as_ref()).notify_waiters();
        }
        Err(QueueError::Failed {
            message,
            exit_code,
            result_commit_sha,
        }) => {
            entry.status = MergeQueueEntryStatus::Failed;
            entry.error_message = Some(message.clone());
            entry.result_commit_sha = result_commit_sha.clone();
            entry.updated_at = now;
            run.status = MergeQueueRunStatus::Failed;
            run.exit_code = exit_code;
            run.error_message = Some(message);
            run.result_commit_sha = result_commit_sha;
            run.finished_at = Some(now);
            store.update_merge_queue_entry(&entry).await?;
            store.update_merge_queue_run(&run).await?;
            H::merge_queue_runtime(state.as_ref()).notify_waiters();
        }
    }

    Ok(())
}

async fn wait_for_merge_queue_completion<H: MergeQueueHost>(
    state: &Arc<H>,
    workspace_id: WorkspaceId,
    entry_id: MergeQueueEntryId,
) -> Result<MergeQueueEntry> {
    let store = H::protected_workspace_store(state.as_ref(), workspace_id).await?;
    let notify = H::merge_queue_runtime(state.as_ref()).notifier();
    loop {
        let entry = store
            .get_merge_queue_entry(entry_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("merge queue entry not found"))?;
        match entry.status {
            MergeQueueEntryStatus::Queued | MergeQueueEntryStatus::Running => {
                let notified = notify.notified();
                tokio::select! {
                    _ = notified => {}
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                }
            }
            _ => return Ok(entry),
        }
    }
}

fn ensure_merge_queue_success(entry: &MergeQueueEntry) -> Result<()> {
    match entry.status {
        MergeQueueEntryStatus::Passed => Ok(()),
        MergeQueueEntryStatus::Conflict => bail!(
            "merge queue conflict for entry {}: {}",
            entry.id.0,
            entry
                .error_message
                .as_deref()
                .unwrap_or("conflict while applying changes")
        ),
        MergeQueueEntryStatus::Failed => bail!(
            "merge queue failed for entry {}: {}",
            entry.id.0,
            entry
                .error_message
                .as_deref()
                .unwrap_or("merge queue run failed")
        ),
        MergeQueueEntryStatus::Cancelled => bail!("merge queue entry {} was cancelled", entry.id.0),
        MergeQueueEntryStatus::Queued | MergeQueueEntryStatus::Running => bail!(
            "merge queue entry {} is still running; try again",
            entry.id.0
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn runtime_tracks_running_and_pending_workspaces() {
        let runtime = MergeQueueRuntime::new();
        let workspace_id = WorkspaceId::new();

        assert!(runtime.begin_workspace_drain(workspace_id).await);
        assert!(!runtime.begin_workspace_drain(workspace_id).await);
        assert!(runtime.is_pending(workspace_id).await);
        assert!(runtime.finish_workspace_drain(workspace_id).await);
        assert!(!runtime.is_pending(workspace_id).await);
        assert!(runtime.running_workspaces().await.is_empty());
    }
}
