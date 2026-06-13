mod agent_control;
mod child_runs;
mod context;
mod details;
mod errors;
mod init;
mod mcp_control;
mod providers;
mod request;
mod types;
mod worktrees;

use ctx_subagent_service::{
    collect_provider_ids, DEFAULT_MAX_ACTIVE_SUBAGENTS_PER_PARENT, DEFAULT_MAX_SUBAGENT_DEPTH,
};

pub use self::types::{
    AgentDetail, AgentInitItem, AgentInitReq, AgentResult, AgentSummary, ArchiveAgentReq,
    ArchiveAgentResp, ContextWindowSummary, GetAgentReq, GetAgentResp, InterruptAgentReq,
    InterruptAgentResp, SendInputReq, SendInputResp, SpawnAgentReq, SpawnAgentResp, WaitAgentReq,
    WaitAgentResp,
};
use ctx_core::models::SubagentInvocationChild;

pub(in crate::daemon) use self::agent_control::{SubagentSpawnHost, SubagentSpawnHostParts};
use self::child_runs::{
    emit_subagent_invocation_notice, finalize_subagent_invocation, run_subagent_child,
    wait_for_run_assistant_message_in_store,
};
pub(in crate::daemon) use self::child_runs::{
    PersistedSubagentPrompt, SessionEventHeadSubscriber, SubagentChildRunHost,
};
pub(in crate::daemon) use self::context::worktree_path_for_child_in_store;
use self::details::build_spawned_agent_detail;
pub(in crate::daemon) use self::details::{
    build_agent_detail_for_mcp_read, build_agent_summary, collect_wait_targets,
    resolve_child_agent_session,
};
use self::errors::ApiResult;
pub(in crate::daemon) use self::errors::{api_error, internal_api_error, not_found};
pub use self::errors::{SubagentError, SubagentErrorKind};
pub use self::mcp_control::SessionSubagentMcpControlHandle;
pub(in crate::daemon) use self::mcp_control::{
    SessionSubagentMcpControlFuture, SessionSubagentMcpControlHandleParts,
    SessionSubagentMcpControlLifecycleHost, SessionSubagentMcpControlPublicationHost,
    SessionSubagentMcpControlSchedulerSpawner,
};
use self::request::ensure_requested_labels_available;
pub(in crate::daemon) use self::worktrees::{
    cleanup_archived_subagent_worktree_with_host, SubagentArchiveWorktreeCleanupHost,
};

pub(in crate::daemon) use init::init_subagents;

#[derive(Clone)]
pub struct SpawnedChild {
    child: SubagentInvocationChild,
    worktree_path: Option<String>,
    last_event_seq: i64,
}
