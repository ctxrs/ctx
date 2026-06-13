mod context_window;
mod refs;
mod request;
pub mod route_contract;
mod status;
mod types;
mod wait;

pub use context_window::{
    legacy_context_window_metric_key, summarize_context_window, SubagentContextWindowSummary,
};
pub use refs::{decode_agent_ref, encode_agent_ref, encode_run_ref};
pub use request::{
    build_subagent_request_json, collect_provider_ids, normalize_subagent_labels,
    parse_subagent_worktree, resolve_max_subagents_per_call, SubagentRequestAgent,
    SubagentWorktreeSelection, DEFAULT_MAX_ACTIVE_SUBAGENTS_PER_PARENT,
    DEFAULT_MAX_SUBAGENTS_PER_CALL, DEFAULT_MAX_SUBAGENT_DEPTH,
};
pub use status::{
    agent_active_state, agent_delivery_label, agent_health, agent_terminal_result_status,
    is_active_turn_status, subagent_status_from_turn_status,
    subagent_terminal_status_from_turn_status, turn_status_has_input_backlog,
};
pub use types::{
    AgentDetail, AgentInitItem, AgentInitReq, AgentResult, AgentSummary, ArchiveAgentReq,
    ArchiveAgentResp, ContextWindowSummary, GetAgentReq, GetAgentResp, InterruptAgentReq,
    InterruptAgentResp, SendInputReq, SendInputResp, SpawnAgentReq, SpawnAgentResp, WaitAgentReq,
    WaitAgentResp,
};
pub use wait::{
    normalize_wait_agent_ids, parse_wait_mode, parse_wait_until, wait_predicate_satisfied,
    AgentWaitDetail, AgentWaitMode, AgentWaitUntil,
};
