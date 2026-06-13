use serde::{Deserialize, Serialize};

use crate::SubagentContextWindowSummary;

#[derive(Debug, Deserialize)]
pub struct AgentInitReq {
    #[serde(default)]
    pub tool_call_id: Option<String>,
    #[serde(default)]
    pub response_mode: Option<String>,
    #[serde(default)]
    pub worktree: Option<String>,
    pub agents: Vec<AgentInitItem>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct AgentInitItem {
    pub prompt: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub harness: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SpawnAgentReq {
    #[serde(default)]
    pub tool_call_id: Option<String>,
    #[serde(default)]
    pub worktree: Option<String>,
    pub task_label: String,
    pub prompt: String,
    #[serde(default)]
    pub harness: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct GetAgentReq {
    pub agent_id: String,
}

#[derive(Debug, Deserialize)]
pub struct SendInputReq {
    pub agent_id: String,
    pub message: String,
    #[serde(default)]
    pub interrupt: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct ArchiveAgentReq {
    pub agent_id: String,
}

#[derive(Debug, Deserialize)]
pub struct WaitAgentReq {
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub agent_ids: Option<Vec<String>>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub until: Option<String>,
    #[serde(default)]
    pub since_seq: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct InterruptAgentReq {
    pub agent_id: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct ContextWindowSummary {
    pub total: u64,
    pub used: u64,
    pub remaining: u64,
    pub utilization: f64,
}

impl From<SubagentContextWindowSummary> for ContextWindowSummary {
    fn from(summary: SubagentContextWindowSummary) -> Self {
        Self {
            total: summary.total,
            used: summary.used,
            remaining: summary.remaining,
            utilization: summary.utilization,
        }
    }
}

#[derive(Debug, Serialize, Clone)]
pub struct AgentSummary {
    pub agent_id: String,
    pub task_label: String,
    pub state: String,
    pub health: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_result_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_progress_at: Option<String>,
    pub last_event_seq: i64,
}

#[derive(Debug, Serialize, Clone)]
pub struct AgentResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<ContextWindowSummary>,
}

#[derive(Debug, Serialize, Clone)]
pub struct AgentDetail {
    pub agent: AgentSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_result: Option<AgentResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SpawnAgentResp {
    pub agent: AgentDetail,
}

#[derive(Debug, Serialize)]
pub struct GetAgentResp {
    pub agent: AgentDetail,
}

#[derive(Debug, Serialize)]
pub struct SendInputResp {
    pub agent: AgentDetail,
    pub queued_run_id: String,
    pub delivery: String,
}

#[derive(Debug, Serialize)]
pub struct ArchiveAgentResp {
    pub agent_id: String,
    pub task_label: String,
    pub archived: bool,
    pub cleanup_failed: bool,
}

#[derive(Debug, Serialize)]
pub struct WaitAgentResp {
    pub wait_status: String,
    pub mode: String,
    pub until: String,
    pub results: Vec<AgentDetail>,
}

#[derive(Debug, Serialize)]
pub struct InterruptAgentResp {
    pub agent: AgentDetail,
}
