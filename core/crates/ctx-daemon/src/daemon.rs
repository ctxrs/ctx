use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use ctx_core::ids::WorkspaceId;
use ctx_core::models::{ExecutionEnvironment, SessionTurn, SessionTurnStatus};
#[cfg(test)]
use ctx_store::{StoreManager, StoreManagerConfig};

use crate::daemon::scheduler::reconcile_turn_terminal_state;
use ctx_provider_runtime::provider_usage;

mod activity;
pub mod blobs;
pub mod diagnostics;
pub mod dictation;
pub mod execution_effective;
pub mod execution_setup;
pub mod git_status;
mod handle;
pub mod health;
mod launch_route_handles;
mod lifecycle;
mod listener;
pub mod logs;
pub mod maintenance;
mod maintenance_route_handles;
mod managed_auto_update;
mod mcp_auth;
mod memleak_debug;
pub mod merge_queue;
mod merge_queue_route_handles;
pub mod mobile_access;
mod mobile_route_handles;
mod mobile_startup;
pub mod org_policy;
mod org_policy_route;
mod provider_capability_hosts;
mod provider_child_reclassifier;
pub mod provider_guard;
mod provider_launch_host;
pub mod provider_restart;
mod provider_route_handles;
pub mod providers;
pub mod repo_onboarding;
pub mod resource_governance;
pub mod resource_telemetry;
pub mod resource_utilization;
mod resource_utilization_route_handles;
mod retention;
mod route_builders;
mod route_capabilities;
mod route_files;
mod route_handles;
mod run_archive_route_handles;
mod runtime;
pub mod scheduler;
mod session_control_effects;
mod session_route_handles;
pub mod sessions;
pub mod settings;
mod settings_route_handles;
mod state;
pub mod storage_guard;
mod task_route_handles;
mod task_session_effects;
pub mod tasks;
pub mod terminals;
#[cfg(any(test, feature = "test-support"))]
mod test_support_access;
pub mod tool_cgroup;
pub mod updates;
pub mod web_sessions;
mod workspace_route_handles;
#[cfg(test)]
mod workspace_runtime;
mod workspace_stream_route_handles;
pub mod workspaces;

#[cfg(test)]
pub(in crate::daemon) use self::runtime::spawn_startup_provider_status_refresh;
use activity::reconcile_running_turns;
pub use activity::reconcile_running_turns_with_reason;
pub use activity::{
    daemon_sandbox_work_activity_summary, daemon_turn_activity_summary, ActiveTurnRecord,
    DaemonSandboxWorkActivitySummary, DaemonTurnActivitySummary,
};
pub use blobs::{BlobHandle, OpenedBlob};
pub use diagnostics::DiagnosticsSnapshotError;
pub use handle::DaemonHandle;
pub use health::HealthSnapshotError;
pub(in crate::daemon) use launch_route_handles::ProviderWorkspaceLaunchRuntime;
pub use launch_route_handles::{
    ExecutionLaunchHandle, LinuxSandboxRuntimeHandle, TerminalRouteHandle, WebSessionRouteHandle,
};
pub(in crate::daemon) use lifecycle::spawn_deferred_daemon_shutdown;
pub(in crate::daemon) use lifecycle::{DaemonShutdownHost, DaemonShutdownHostParts};
#[cfg(test)]
pub use listener::daemon_public_base_url_from_env;
pub use maintenance_route_handles::{
    DaemonShutdownHandle, UpdateActivityHandle, UpdateDrainHandle,
};
pub use mcp_auth::issue_provider_session_mcp_token;
pub use mcp_auth::{
    emit_mcp_token_denied, issue_provider_session_mcp_token_with_capabilities,
    require_scoped_mcp_session_context, revoke_provider_session_mcp_token, verify_mcp_auth_token,
    ScopedMcpSessionAccessError,
};
pub use merge_queue_route_handles::MergeQueueApiHandle;
pub use mobile_route_handles::{MobileRuntimeHandle, MobileSecureProxyHandle};
pub use provider_route_handles::{
    ProviderAccountsHandle, ProviderAdminHandle, ProviderAuthImportHandle, ProviderBootstrapHandle,
    ProviderHarnessConfigHandle, ProviderInstallHandle, ProviderOptionsHandle,
    ProviderStatusHandle, ProviderUsageHandle, ProviderWorkspaceAuthHandle,
};
pub use resource_utilization_route_handles::ResourceUtilizationHandle;
#[cfg(test)]
pub use retention::prune_archived_session_data_for_all_workspaces;
pub(crate) use route_builders::route_handles_from_state;
#[cfg(any(test, feature = "test-support"))]
pub(crate) use route_builders::workspace_attachments_runtime_from_state;
#[cfg(test)]
pub(crate) use route_builders::workspace_primary_branch_with_refresh_effect_from_state;
#[cfg(test)]
pub(crate) use route_builders::workspace_vcs_stream_with_refresh_effect_from_state;
pub use route_capabilities::{DaemonRouteHandles, DaemonShutdownSignal};
pub use route_files::RouteFileDownloadError;
pub use route_handles::{
    AuthHandle, DiagnosticsHandle, DictationHandle, HealthHandle, LogsHandle, MobileStoreHandle,
    OrgPolicyHandle, RepoOnboardingHandle, RequestBaseHandle, TelemetryHandle, UpdateReleaseHandle,
};
pub use run_archive_route_handles::RunArchiveHandle;
pub use runtime::{bootstrap_daemon_runtime, DaemonRuntime};
pub use session_control_effects::SessionControlHandle;
pub use session_route_handles::{
    SessionArtifactsHandle, SessionFileCompletionsHandle, SessionMessageCommandHandle,
    SessionReadModelsHandle, SessionSubagentMcpReadHandle, SessionSubagentReadHandle,
    SessionTitleModelModeHandle, SessionVcsHandle,
};
pub use sessions::subagents::SessionSubagentMcpControlHandle;
pub use sessions::title_generation::TitleGenerationLocalHandle;
pub use sessions::DemoSeedTranscriptHandle;
pub use settings_route_handles::SettingsHandle;
pub(in crate::daemon) use state::{
    merge_queue_route_host_from_state, session_store_access_anyhow, ProtectedWorkspaceStoreLookup,
    SessionStoreLookup, WeakSessionStoreLookup,
};
pub use state::{AppRuntimeFlags, DaemonState};
pub use state::{
    CacheSweepConfig, SessionStoreAccessError, StoreLookup, TimedEntry, WorkspaceStoreAccessError,
};
pub use task_route_handles::{
    TaskCreationHandle, TaskLifecycleHandle, TaskListingHandle, TaskReadStateHandle,
    TaskSessionAdmissionHandle, TaskSessionListingHandle, TaskTitleHandle,
};
#[cfg(test)]
pub(crate) use workspace_route_handles::WorkspacePrimaryBranchRefreshEffect;
pub use workspace_route_handles::{
    WorkspaceAttachmentsHandle, WorkspaceDeletionHandle, WorkspaceExecutionConfigHandle,
    WorkspaceFileCompletionsHandle, WorkspaceHarnessContainerHandle,
    WorkspaceMergeQueueConfigHandle, WorkspaceOrgPolicyHandle, WorkspacePrimaryBranchHandle,
    WorkspacePromptBootstrapConfigHandle, WorkspaceProviderModelPreferenceHandle,
    WorkspaceRegistryHandle, WorkspaceWorktreeHandle,
};
pub use workspace_stream_route_handles::{
    WorkspaceActiveHandle, WorkspaceStreamHandle, WorkspaceVcsStreamHandle,
};
pub use workspaces::{WorkspaceStreamAccessError, WorkspaceStreamRouteAdmission};

#[cfg(test)]
mod tests;
