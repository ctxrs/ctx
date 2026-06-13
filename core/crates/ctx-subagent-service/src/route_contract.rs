use ctx_core::ids::TurnId;
use ctx_core::models::{SessionSummary, SubagentInvocation};
use serde::{Deserialize, Serialize};

use crate::{
    AgentSummary, ArchiveAgentReq, ArchiveAgentResp, GetAgentReq, GetAgentResp, InterruptAgentReq,
    InterruptAgentResp, SendInputReq, SendInputResp, SpawnAgentReq, SpawnAgentResp, WaitAgentReq,
    WaitAgentResp,
};

#[derive(Debug, Clone, Deserialize, Default)]
pub struct SessionSubagentInvocationsRouteQuery {
    #[serde(default)]
    turn_id: Option<String>,
}

impl SessionSubagentInvocationsRouteQuery {
    pub fn into_turn_id(self) -> Result<Option<TurnId>, SessionSubagentRouteError> {
        let Some(raw) = self.turn_id else {
            return Ok(None);
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        uuid::Uuid::parse_str(trimmed)
            .map(TurnId)
            .map(Some)
            .map_err(|_| SessionSubagentRouteError::bad_request("invalid turn id"))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SpawnAgentRouteRequest {
    #[serde(default)]
    tool_call_id: Option<String>,
    #[serde(default)]
    worktree: Option<String>,
    task_label: String,
    prompt: String,
    #[serde(default)]
    harness: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    reasoning_effort: Option<String>,
}

impl SpawnAgentRouteRequest {
    pub fn into_low_level(self) -> SpawnAgentReq {
        SpawnAgentReq {
            tool_call_id: self.tool_call_id,
            worktree: self.worktree,
            task_label: self.task_label,
            prompt: self.prompt,
            harness: self.harness,
            model: self.model,
            reasoning_effort: self.reasoning_effort,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SendInputRouteRequest {
    agent_id: String,
    message: String,
    #[serde(default)]
    interrupt: Option<bool>,
}

impl SendInputRouteRequest {
    pub fn into_low_level(self) -> SendInputReq {
        SendInputReq {
            agent_id: self.agent_id,
            message: self.message,
            interrupt: self.interrupt,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArchiveAgentRouteRequest {
    agent_id: String,
}

impl ArchiveAgentRouteRequest {
    pub fn into_low_level(self) -> ArchiveAgentReq {
        ArchiveAgentReq {
            agent_id: self.agent_id,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct GetAgentRouteRequest {
    agent_id: String,
}

impl GetAgentRouteRequest {
    pub fn into_low_level(self) -> GetAgentReq {
        GetAgentReq {
            agent_id: self.agent_id,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct InterruptAgentRouteRequest {
    agent_id: String,
}

impl InterruptAgentRouteRequest {
    pub fn into_low_level(self) -> InterruptAgentReq {
        InterruptAgentReq {
            agent_id: self.agent_id,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct WaitAgentRouteRequest {
    #[serde(default)]
    agent_id: Option<String>,
    #[serde(default)]
    agent_ids: Option<Vec<String>>,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    until: Option<String>,
    #[serde(default)]
    since_seq: Option<i64>,
}

impl WaitAgentRouteRequest {
    pub fn into_low_level(self) -> WaitAgentReq {
        WaitAgentReq {
            agent_id: self.agent_id,
            agent_ids: self.agent_ids,
            timeout_ms: self.timeout_ms,
            mode: self.mode,
            until: self.until,
            since_seq: self.since_seq,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct SessionSubagentsRouteResponse(Vec<SessionSummary>);

impl SessionSubagentsRouteResponse {
    pub fn new(subagents: Vec<SessionSummary>) -> Self {
        Self(subagents)
    }
}

#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct SessionSubagentInvocationsRouteResponse(Vec<SubagentInvocation>);

impl SessionSubagentInvocationsRouteResponse {
    pub fn new(invocations: Vec<SubagentInvocation>) -> Self {
        Self(invocations)
    }
}

#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct SessionSubagentInvocationRouteResponse(SubagentInvocation);

impl SessionSubagentInvocationRouteResponse {
    pub fn new(invocation: SubagentInvocation) -> Self {
        Self(invocation)
    }
}

#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct SpawnAgentRouteResponse(SpawnAgentResp);

impl SpawnAgentRouteResponse {
    pub fn new(response: SpawnAgentResp) -> Self {
        Self(response)
    }
}

#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct SendInputRouteResponse(SendInputResp);

impl SendInputRouteResponse {
    pub fn new(response: SendInputResp) -> Self {
        Self(response)
    }
}

#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct ArchiveAgentRouteResponse(ArchiveAgentResp);

impl ArchiveAgentRouteResponse {
    pub fn new(response: ArchiveAgentResp) -> Self {
        Self(response)
    }
}

#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct ListAgentsRouteResponse(Vec<AgentSummary>);

impl ListAgentsRouteResponse {
    pub fn new(agents: Vec<AgentSummary>) -> Self {
        Self(agents)
    }
}

#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct GetAgentRouteResponse(GetAgentResp);

impl GetAgentRouteResponse {
    pub fn new(response: GetAgentResp) -> Self {
        Self(response)
    }
}

#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct InterruptAgentRouteResponse(InterruptAgentResp);

impl InterruptAgentRouteResponse {
    pub fn new(response: InterruptAgentResp) -> Self {
        Self(response)
    }
}

#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct WaitAgentRouteResponse(WaitAgentResp);

impl WaitAgentRouteResponse {
    pub fn new(response: WaitAgentResp) -> Self {
        Self(response)
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SessionSubagentRouteErrorKind {
    BadRequest,
    Unauthorized,
    Forbidden,
    NotFound,
    InsufficientStorage,
    Internal,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SessionSubagentRouteError {
    kind: SessionSubagentRouteErrorKind,
    message: String,
}

impl SessionSubagentRouteError {
    fn new(kind: SessionSubagentRouteErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(SessionSubagentRouteErrorKind::BadRequest, message)
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(SessionSubagentRouteErrorKind::Unauthorized, message)
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(SessionSubagentRouteErrorKind::Forbidden, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(SessionSubagentRouteErrorKind::NotFound, message)
    }

    pub fn insufficient_storage(message: impl Into<String>) -> Self {
        Self::new(SessionSubagentRouteErrorKind::InsufficientStorage, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(SessionSubagentRouteErrorKind::Internal, message)
    }

    pub fn kind(&self) -> SessionSubagentRouteErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn listing_query_preserves_turn_id_contract() {
        let query: SessionSubagentInvocationsRouteQuery =
            serde_json::from_value(json!({ "turn_id": "  ", "ignored": true })).unwrap();
        assert_eq!(query.into_turn_id().unwrap(), None);

        let turn_id = TurnId::new();
        let query: SessionSubagentInvocationsRouteQuery = serde_json::from_value(json!({
            "turn_id": turn_id.0.to_string()
        }))
        .unwrap();
        assert_eq!(query.into_turn_id().unwrap(), Some(turn_id));

        let query: SessionSubagentInvocationsRouteQuery =
            serde_json::from_value(json!({ "turn_id": "not-a-turn" })).unwrap();
        let error = query.into_turn_id().unwrap_err();
        assert_eq!(error.kind(), SessionSubagentRouteErrorKind::BadRequest);
        assert_eq!(error.message(), "invalid turn id");
    }

    #[test]
    fn mcp_route_requests_preserve_current_serde_shape() {
        let spawn: SpawnAgentRouteRequest = serde_json::from_value(json!({
            "tool_call_id": "tool",
            "worktree": "new",
            "task_label": "child",
            "prompt": "do work",
            "harness": "codex",
            "model": "gpt",
            "reasoning_effort": "high",
            "ignored": true
        }))
        .unwrap();
        let spawn = spawn.into_low_level();
        assert_eq!(spawn.tool_call_id.as_deref(), Some("tool"));
        assert_eq!(spawn.worktree.as_deref(), Some("new"));
        assert_eq!(spawn.task_label, "child");
        assert_eq!(spawn.prompt, "do work");
        assert_eq!(spawn.harness.as_deref(), Some("codex"));
        assert_eq!(spawn.model.as_deref(), Some("gpt"));
        assert_eq!(spawn.reasoning_effort.as_deref(), Some("high"));

        let send: SendInputRouteRequest = serde_json::from_value(json!({
            "agent_id": "agent",
            "message": "hello",
            "interrupt": true,
            "ignored": true
        }))
        .unwrap();
        let send = send.into_low_level();
        assert_eq!(send.agent_id, "agent");
        assert_eq!(send.message, "hello");
        assert_eq!(send.interrupt, Some(true));

        let wait: WaitAgentRouteRequest = serde_json::from_value(json!({
            "agent_id": "agent",
            "agent_ids": ["a", "b"],
            "timeout_ms": 1,
            "mode": "all",
            "until": "update",
            "since_seq": 2,
            "ignored": true
        }))
        .unwrap();
        let wait = wait.into_low_level();
        assert_eq!(wait.agent_id.as_deref(), Some("agent"));
        assert_eq!(wait.agent_ids, Some(vec!["a".to_string(), "b".to_string()]));
        assert_eq!(wait.timeout_ms, Some(1));
        assert_eq!(wait.mode.as_deref(), Some("all"));
        assert_eq!(wait.until.as_deref(), Some("update"));
        assert_eq!(wait.since_seq, Some(2));
    }

    #[test]
    fn route_error_preserves_kind_and_message() {
        let error = SessionSubagentRouteError::insufficient_storage("no room");
        assert_eq!(
            error.kind(),
            SessionSubagentRouteErrorKind::InsufficientStorage
        );
        assert_eq!(error.message(), "no room");
    }
}
