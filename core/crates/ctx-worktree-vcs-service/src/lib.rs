mod cache;
mod diff_output;
mod diff_paths;
mod driver;
mod file_completions;
mod git_commands;
mod local_source;
mod managed_worktree;
mod projection;
mod resolution;
mod runtime;
mod sandbox_source;
mod session_diff;
mod session_diff_exec;
mod session_patch;
mod snapshot;
mod status;
mod vcs_hooks;
mod watch;
mod worktree_creation;

use serde::Serialize;

pub use cache::{
    hydrated_worktree_vcs_snapshot_cache_entry, pending_worktree_vcs_snapshot_cache_entry,
    publish_worktree_vcs_snapshot_cache_entry, published_worktree_vcs_snapshot_cache_entry,
    GitStatusSnapshotCacheEntry, WorktreeVcsSnapshotCacheEntry, WorktreeVcsSnapshotPublishPolicy,
    WORKTREE_VCS_DEBOUNCE_MS, WORKTREE_VCS_MAX_INTERVAL_MS,
};
pub use diff_output::{
    parse_worktree_vcs_diff_summary_counts, WorktreeVcsDiffSummaryCounts,
    WORKTREE_VCS_CONTAINER_DIFF_SCRIPT, WORKTREE_VCS_CONTAINER_DIFF_SUMMARY_SCRIPT,
};
pub use diff_paths::{
    build_diff_path_states, count_diff_paths, load_diff_file_count_from_source,
    load_diff_touched_entries_from_source, WorktreeVcsDiffPathSource,
};
pub use driver::{effective_worktree_vcs_kind, worktree_vcs_driver_for_kind, WorktreeVcsDriver};
pub use file_completions::{
    filter_and_rank_paths, list_host_git_files, merge_and_sort_git_paths, workspace_has_git_repo,
    CachedFileCompletions,
};
pub use git_commands::{
    parse_git_diff_name_status, parse_git_list_untracked, parse_git_refs, parse_git_single_ref,
    WorktreeVcsGitCommand,
};
pub use local_source::LocalWorktreeVcsSource;
pub use managed_worktree::{
    branch_exists, create_managed_worktree, delete_worktree_branch, ensure_worktree_attached,
    is_git_worktree, managed_worktree_path, managed_worktree_record,
    matching_managed_worktree_path, prune_worktrees, remove_worktree,
    standaloneize_worktree_git_dir,
};
pub use projection::{
    plan_worktree_vcs_summary_refresh, plan_worktree_vcs_touched_files_refresh,
    worktree_vcs_dirty_transient_snapshot, worktree_vcs_projection_cache_state,
    worktree_vcs_refresh_transient_snapshot, worktree_vcs_summary_refresh_error_fallback,
    worktree_vcs_summary_refresh_from_file_count, worktree_vcs_summary_refresh_no_repo,
    worktree_vcs_touched_files_error_fallback, worktree_vcs_touched_files_from_entries,
    worktree_vcs_touched_files_large_change_set, worktree_vcs_touched_files_reuse,
    WorktreeVcsProjectionCacheState, WorktreeVcsSummaryRefreshPlan,
    WorktreeVcsSummaryRefreshResult, WorktreeVcsTouchedFilesRefreshPlan,
    WorktreeVcsTouchedFilesRefreshResult,
};
pub use resolution::{
    is_no_vcs_repo_error, resolve_worktree_diff_base_from_source, WorktreeDiffBaseResolution,
    WorktreeVcsDiffBaseQuery, WorktreeVcsDiffBaseSource,
};
pub use runtime::{
    claim_next_worktree_vcs_job, finish_worktree_vcs_job, finish_worktree_vcs_refresh,
    mark_worktree_vcs_runtime_dirty, queue_worktree_vcs_refresh, worktree_vcs_enabled_from_env,
    worktree_vcs_scheduler_concurrency_from_env, WorktreeVcsDirtyBits, WorktreeVcsInvalidation,
    WorktreeVcsRuntimeState, WorktreeVcsSchedulerJob, WorktreeVcsSchedulerRuntime,
};
pub use sandbox_source::{SandboxWorktreeVcsSource, WorktreeVcsSandboxGitExecutor};
pub use session_diff::{
    worktree_vcs_diff_summary_mismatch, worktree_vcs_session_diff_available,
    worktree_vcs_session_diff_summary_available, worktree_vcs_session_diff_summary_no_repo,
    worktree_vcs_session_diff_summary_unavailable, worktree_vcs_session_diff_unavailable,
    WorktreeVcsDiffSummaryMismatch, WorktreeVcsSessionDiffOutcome,
    WorktreeVcsSessionDiffSummaryOutcome,
};
pub use session_diff_exec::{
    load_worktree_vcs_session_diff_from_host, load_worktree_vcs_session_diff_from_sandbox,
    load_worktree_vcs_session_diff_summary_from_host,
    load_worktree_vcs_session_diff_summary_from_sandbox, WorktreeVcsSessionDiffCommand,
    WorktreeVcsSessionDiffSandboxExecutor,
};
pub use session_patch::apply_worktree_vcs_session_patch;
pub use snapshot::{
    build_git_status_entries, build_git_status_summary, build_large_change_set_touched_files,
    build_touched_files, build_worktree_vcs_snapshot, build_worktree_vcs_snapshot_from_source,
    derive_worktree_vcs_freshness, now_epoch_ms, plan_worktree_vcs_commit_info,
    snapshot_fingerprint, snapshot_for_durable_cache, summary_from_file_count, summary_has_counts,
    WorktreeVcsCommitInfoPlan, WorktreeVcsCommitLookup, WorktreeVcsSnapshotBuildParts,
    WorktreeVcsSnapshotCommitInfo,
};
pub use status::{
    git_status_snapshot_from_structured, load_git_status_snapshot_from_source,
    resolve_worktree_vcs_commit_lookup_from_source, session_git_status_summary_from_snapshot,
    worktree_has_vcs_repo_from_source, worktree_vcs_structured_status_from_vcs,
    WorktreeVcsCommitLookupSource, WorktreeVcsStatusSource, WorktreeVcsStructuredStatus,
};
pub use vcs_hooks::{
    cleanup_workspace_hooks, cleanup_worktree_hooks, ensure_task_commit_hook, get_git_config,
    set_git_config, worktree_hooks_dir, SandboxContainerRuntime, VcsHooksHost,
    WorktreeExecutionLocation, WorktreeHookExecution, CORE_HOOKS_PATH_KEY, CTX_PREV_HOOKS_PATH_KEY,
    CTX_TASK_ID_KEY,
};
pub use watch::{
    normalize_worktree_vcs_watch_path, resolve_worktree_vcs_metadata_roots,
    worktree_vcs_invalidation_for_watch_paths, WorktreeVcsWatchDebounceState,
    WORKTREE_VCS_POLL_INTERVAL_MS, WORKTREE_VCS_WATCH_DEBOUNCE_MS,
};
pub use worktree_creation::{
    resolve_worktree_creation_base, WorktreeCreationBase, WorktreeCreationBaseError,
};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GitStatusSnapshot {
    pub raw: String,
    pub summary_line: String,
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub ahead: i64,
    pub behind: i64,
    pub detached: bool,
    pub staged: i64,
    pub unstaged: i64,
    pub untracked: i64,
    pub entries: Vec<GitStatusEntry>,
    pub entries_total_count: i64,
    pub entries_truncated: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GitStatusEntry {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orig_path: Option<String>,
    pub index_status: String,
    pub worktree_status: String,
}

pub const WORKTREE_VCS_TOUCHED_FILES_CAP: usize = 200;
// Above this count, the product surfaces an exact summary but does not compute
// or stream file-by-file review inventory.
pub const WORKTREE_VCS_REVIEWABLE_FILE_LIMIT: i64 = 300;
pub const WORKTREE_VCS_SNAPSHOT_SCHEMA_VERSION: i64 = 2;
pub const DEFAULT_WORKTREE_VCS_SCHEDULER_CONCURRENCY: usize = 2;
