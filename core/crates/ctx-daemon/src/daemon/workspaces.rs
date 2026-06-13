mod active_snapshot_state;
pub mod attachments;
mod cache_stats;
mod deletion;
mod diff_exec;
mod execution;
mod execution_config;
mod execution_config_route_host;
mod file_completions;
mod harness_container;
mod hydration;
mod merge_queue_config;
mod model_preferences;
mod primary_branch;
mod primary_branch_route_host;
mod prompt_bootstrap_config;
mod provider_model_preferences_route;
mod registry;
mod retry;
mod route_config;
mod route_contract;
mod run_archive;
mod runtime;
mod sandbox_binding;
pub mod stream;
mod task_worktree_host;
pub mod vcs_hooks;
mod workspace_file_completions_route;
mod worktree_cleanup;
mod worktrees;

pub use active_snapshot_state::load_workspace_active_snapshot_state;
pub use cache_stats::WorkspaceCacheDebugStats;
pub use deletion::WorkspaceDeleteError;
pub(in crate::daemon) use deletion::{WorkspaceDeletionRuntime, WorkspaceDeletionRuntimeDeps};
pub(in crate::daemon) use diff_exec::{
    diff_worktree_for_session, diff_worktree_summary_for_session,
};
pub use execution::{
    execution_environment_from_settings, resolve_existing_worktree_execution,
    ResolvedExistingWorktreeExecution,
};
pub use file_completions::{FileCompletionsError, FileCompletionsErrorKind};
pub use harness_container::WorkspaceHarnessContainerError;
pub(in crate::daemon) use hydration::WorkspaceActiveHydrationRuntime;
pub use hydration::{WorkspaceHydrationError, WorkspaceHydrationErrorKind};
pub use model_preferences::{
    WorkspaceProviderModelPreference, WorkspaceProviderModelPreferenceError,
};
pub use retry::retry_global_index_write;
pub(in crate::daemon::workspaces) use route_config::workspace_store_route_error;
pub(in crate::daemon::workspaces) use route_config::WorkspaceRouteError;
pub use run_archive::RunArchiveIngestError;
pub(in crate::daemon) use runtime::WorkspaceActiveCacheRuntime;
pub use sandbox_binding::rematerialize_sandbox_binding_for_worktree;
pub use stream::{WorkspaceStreamAccessError, WorkspaceStreamRouteAdmission};
pub(in crate::daemon) use task_worktree_host::{TaskWorktreeHost, TaskWorktreeHostParts};
pub use vcs_hooks::{cleanup_workspace_hooks, cleanup_worktree_hooks, ensure_task_commit_hook};
pub use worktree_cleanup::{
    cleanup_task_worktrees, cleanup_task_worktrees_with_host, managed_worktree_root,
    managed_worktree_root_for_data_root, BranchCleanupErrorMode, TaskWorktreeCleanupTarget,
};
