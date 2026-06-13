use ctx_core::ids::SessionId;
use ctx_mcp_auth::McpAuthContext;
use ctx_observability::logs;
use ctx_route_contracts::sessions::SessionRouteParams;
use ctx_subagent_service::route_contract::{
    ArchiveAgentRouteRequest, ArchiveAgentRouteResponse, GetAgentRouteRequest,
    GetAgentRouteResponse, InterruptAgentRouteRequest, InterruptAgentRouteResponse,
    ListAgentsRouteResponse, SendInputRouteRequest, SendInputRouteResponse,
    SessionSubagentInvocationRouteResponse, SessionSubagentInvocationsRouteQuery,
    SessionSubagentInvocationsRouteResponse, SessionSubagentRouteError,
    SessionSubagentsRouteResponse, SpawnAgentRouteRequest, SpawnAgentRouteResponse,
    WaitAgentRouteRequest, WaitAgentRouteResponse,
};

use crate::daemon::sessions::route_contract::parse_session_route_id;
use crate::daemon::sessions::subagents::{SubagentError, SubagentErrorKind};
use crate::daemon::{
    ScopedMcpSessionAccessError, SessionSubagentMcpControlHandle, SessionSubagentMcpReadHandle,
    SessionSubagentReadHandle,
};

impl SessionSubagentReadHandle {
    pub async fn list_session_subagents_for_route(
        &self,
        params: SessionRouteParams,
    ) -> Result<SessionSubagentsRouteResponse, SessionSubagentRouteError> {
        let session_id = parse_subagent_route_id(params)?;
        let subagents = self
            .list_session_subagents_for_request(session_id)
            .await
            .map_err(|_| SessionSubagentRouteError::internal("internal server error"))?
            .ok_or_else(|| SessionSubagentRouteError::not_found("session not found"))?;
        Ok(SessionSubagentsRouteResponse::new(subagents))
    }

    pub async fn list_session_subagent_invocations_for_route(
        &self,
        params: SessionRouteParams,
        query: SessionSubagentInvocationsRouteQuery,
    ) -> Result<SessionSubagentInvocationsRouteResponse, SessionSubagentRouteError> {
        let session_id = parse_subagent_route_id(params)?;
        let turn_id = query.into_turn_id()?;
        let invocations = self
            .list_session_subagent_invocations_for_request(session_id, turn_id)
            .await
            .map_err(|_| SessionSubagentRouteError::internal("internal server error"))?
            .ok_or_else(|| SessionSubagentRouteError::not_found("session not found"))?;
        Ok(SessionSubagentInvocationsRouteResponse::new(invocations))
    }

    pub async fn get_session_subagent_invocation_for_route(
        &self,
        params: SessionRouteParams,
        invocation_id: String,
    ) -> Result<SessionSubagentInvocationRouteResponse, SessionSubagentRouteError> {
        let session_id = parse_subagent_route_id(params)?;
        let invocation = self
            .get_session_subagent_invocation_for_request(session_id, &invocation_id)
            .await
            .map_err(|_| SessionSubagentRouteError::internal("internal server error"))?
            .ok_or_else(|| SessionSubagentRouteError::not_found("session not found"))?;
        Ok(SessionSubagentInvocationRouteResponse::new(invocation))
    }
}

impl SessionSubagentMcpControlHandle {
    pub async fn spawn_agent_for_mcp_route(
        &self,
        params: SessionRouteParams,
        mcp_auth: Option<McpAuthContext>,
        request: SpawnAgentRouteRequest,
    ) -> Result<SpawnAgentRouteResponse, SessionSubagentRouteError> {
        let parent_id = self
            .resolve_mcp_subagent_parent_session_id(params, mcp_auth)
            .await?;
        self.spawn_agent(parent_id, request.into_low_level())
            .await
            .map(SpawnAgentRouteResponse::new)
            .map_err(subagent_route_error)
    }

    pub async fn send_input_for_mcp_route(
        &self,
        params: SessionRouteParams,
        mcp_auth: Option<McpAuthContext>,
        request: SendInputRouteRequest,
    ) -> Result<SendInputRouteResponse, SessionSubagentRouteError> {
        let parent_id = self
            .resolve_mcp_subagent_parent_session_id(params, mcp_auth)
            .await?;
        self.send_input(parent_id, request.into_low_level())
            .await
            .map(SendInputRouteResponse::new)
            .map_err(subagent_route_error)
    }

    pub async fn archive_agent_for_mcp_route(
        &self,
        params: SessionRouteParams,
        mcp_auth: Option<McpAuthContext>,
        request: ArchiveAgentRouteRequest,
    ) -> Result<ArchiveAgentRouteResponse, SessionSubagentRouteError> {
        let parent_id = self
            .resolve_mcp_subagent_parent_session_id(params, mcp_auth)
            .await?;
        self.archive_agent(parent_id, request.into_low_level())
            .await
            .map(ArchiveAgentRouteResponse::new)
            .map_err(subagent_route_error)
    }

    pub async fn interrupt_agent_for_mcp_route(
        &self,
        params: SessionRouteParams,
        mcp_auth: Option<McpAuthContext>,
        request: InterruptAgentRouteRequest,
    ) -> Result<InterruptAgentRouteResponse, SessionSubagentRouteError> {
        let parent_id = self
            .resolve_mcp_subagent_parent_session_id(params, mcp_auth)
            .await?;
        self.interrupt_agent(parent_id, request.into_low_level())
            .await
            .map(InterruptAgentRouteResponse::new)
            .map_err(subagent_route_error)
    }

    async fn resolve_mcp_subagent_parent_session_id(
        &self,
        params: SessionRouteParams,
        mcp_auth: Option<McpAuthContext>,
    ) -> Result<SessionId, SessionSubagentRouteError> {
        let session_id = parse_subagent_route_id(params)?;
        if let Some(mcp_auth) = mcp_auth {
            self.require_scoped_mcp_session_context(mcp_auth, session_id)
                .await
                .map_err(scoped_mcp_session_route_error)?;
        }
        Ok(session_id)
    }
}

impl SessionSubagentMcpReadHandle {
    pub async fn list_agents_for_mcp_route(
        &self,
        params: SessionRouteParams,
        mcp_auth: Option<McpAuthContext>,
    ) -> Result<ListAgentsRouteResponse, SessionSubagentRouteError> {
        let parent_id = self
            .resolve_mcp_subagent_parent_session_id(params, mcp_auth)
            .await?;
        self.list_agents(parent_id)
            .await
            .map(ListAgentsRouteResponse::new)
            .map_err(subagent_route_error)
    }

    pub async fn get_agent_for_mcp_route(
        &self,
        params: SessionRouteParams,
        mcp_auth: Option<McpAuthContext>,
        request: GetAgentRouteRequest,
    ) -> Result<GetAgentRouteResponse, SessionSubagentRouteError> {
        let parent_id = self
            .resolve_mcp_subagent_parent_session_id(params, mcp_auth)
            .await?;
        self.get_agent(parent_id, request.into_low_level())
            .await
            .map(GetAgentRouteResponse::new)
            .map_err(subagent_route_error)
    }

    pub async fn wait_agent_for_mcp_route(
        &self,
        params: SessionRouteParams,
        mcp_auth: Option<McpAuthContext>,
        request: WaitAgentRouteRequest,
    ) -> Result<WaitAgentRouteResponse, SessionSubagentRouteError> {
        let parent_id = self
            .resolve_mcp_subagent_parent_session_id(params, mcp_auth)
            .await?;
        self.wait_agent(parent_id, request.into_low_level())
            .await
            .map(WaitAgentRouteResponse::new)
            .map_err(subagent_route_error)
    }

    async fn resolve_mcp_subagent_parent_session_id(
        &self,
        params: SessionRouteParams,
        mcp_auth: Option<McpAuthContext>,
    ) -> Result<SessionId, SessionSubagentRouteError> {
        let session_id = parse_subagent_route_id(params)?;
        if let Some(mcp_auth) = mcp_auth {
            self.require_scoped_mcp_session_context(mcp_auth, session_id)
                .await
                .map_err(scoped_mcp_session_route_error)?;
        }
        Ok(session_id)
    }
}

fn parse_subagent_route_id(
    params: SessionRouteParams,
) -> Result<SessionId, SessionSubagentRouteError> {
    parse_session_route_id(params.session_id())
        .map_err(|_| SessionSubagentRouteError::bad_request("invalid session id"))
}

fn scoped_mcp_session_route_error(error: ScopedMcpSessionAccessError) -> SessionSubagentRouteError {
    match error {
        ScopedMcpSessionAccessError::Unauthorized(message) => {
            SessionSubagentRouteError::unauthorized(message)
        }
        ScopedMcpSessionAccessError::SessionNotFound => {
            SessionSubagentRouteError::not_found("session not found")
        }
        ScopedMcpSessionAccessError::StoreUnavailable(error) => {
            SessionSubagentRouteError::internal(logs::redact_sensitive(&error.to_string()))
        }
    }
}

fn subagent_route_error(error: SubagentError) -> SessionSubagentRouteError {
    match error.kind() {
        SubagentErrorKind::BadRequest => SessionSubagentRouteError::bad_request(error.message()),
        SubagentErrorKind::NotFound => SessionSubagentRouteError::not_found(error.message()),
        SubagentErrorKind::Forbidden => SessionSubagentRouteError::forbidden(error.message()),
        SubagentErrorKind::InsufficientStorage => {
            SessionSubagentRouteError::insufficient_storage(error.message())
        }
        SubagentErrorKind::Internal => SessionSubagentRouteError::internal(error.message()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;
    use chrono::Utc;
    use ctx_core::ids::{RunId, SessionId, TurnId};
    use ctx_core::models::{Session, SessionTurn, SessionTurnStatus, SubagentInvocation};
    use ctx_mcp_auth::McpAuthCapabilities;
    use ctx_store::Store;
    use ctx_subagent_service::encode_agent_ref;
    use ctx_subagent_service::route_contract::SessionSubagentRouteErrorKind;
    use serde_json::json;

    use crate::test_support::TestDaemon;

    async fn seeded_subagent_read_parent(
    ) -> anyhow::Result<(tempfile::TempDir, TestDaemon, Session)> {
        let temp = tempfile::tempdir()?;
        let data_root = temp.path().join("data");
        let repo_root = temp.path().join("repo");
        std::fs::create_dir_all(&repo_root)?;
        let daemon = TestDaemon::new_for_test(data_root, "http://127.0.0.1:0".to_string()).await?;
        let parent = daemon
            .seed_mcp_parent_session_for_test(&repo_root, "base".to_string(), "fake", "fake-model")
            .await?;
        Ok((temp, daemon, parent))
    }

    async fn seed_invocation(
        store: &Store,
        id: &str,
        parent_session_id: SessionId,
        parent_turn_id: Option<TurnId>,
    ) -> anyhow::Result<SubagentInvocation> {
        let now = Utc::now();
        Ok(store
            .upsert_subagent_invocation(SubagentInvocation {
                id: id.to_string(),
                tool_call_id: format!("{id}-tool"),
                parent_session_id,
                parent_turn_id,
                requested_count: 1,
                request_json: Some(json!({ "id": id })),
                status: "running".to_string(),
                created_at: now,
                updated_at: now,
                children: Vec::new(),
            })
            .await?)
    }

    async fn seed_parent_turn(
        store: &Store,
        session: &Session,
        order: i64,
    ) -> anyhow::Result<TurnId> {
        let now = Utc::now();
        let turn_id = TurnId::new();
        store
            .insert_session_turn(SessionTurn {
                turn_id,
                session_id: session.id,
                run_id: Some(RunId::new()),
                user_message_id: None,
                status: SessionTurnStatus::Completed,
                start_seq: Some(order),
                end_seq: Some(order + 1),
                started_at: now,
                updated_at: now,
                assistant_partial: None,
                thought_partial: None,
                metrics_json: None,
                failure: None,
                tool_total: 0,
                tool_pending: 0,
                tool_running: 0,
                tool_completed: 0,
                tool_failed: 0,
            })
            .await?;
        Ok(turn_id)
    }

    fn scoped_context_for(session: &Session) -> McpAuthContext {
        McpAuthContext {
            session_id: session.id,
            workspace_id: session.workspace_id,
            worktree_id: session.worktree_id,
            capabilities: McpAuthCapabilities::provider_session(),
        }
    }

    #[test]
    fn invalid_session_id_uses_existing_route_message() {
        let error = parse_subagent_route_id(SessionRouteParams::new("not-a-session")).unwrap_err();
        assert_eq!(error.kind(), SessionSubagentRouteErrorKind::BadRequest);
        assert_eq!(error.message(), "invalid session id");
    }

    #[test]
    fn route_error_mapping_preserves_status_categories_and_messages() {
        for (kind, expected_kind) in [
            (
                SubagentErrorKind::BadRequest,
                SessionSubagentRouteErrorKind::BadRequest,
            ),
            (
                SubagentErrorKind::NotFound,
                SessionSubagentRouteErrorKind::NotFound,
            ),
            (
                SubagentErrorKind::Forbidden,
                SessionSubagentRouteErrorKind::Forbidden,
            ),
            (
                SubagentErrorKind::InsufficientStorage,
                SessionSubagentRouteErrorKind::InsufficientStorage,
            ),
            (
                SubagentErrorKind::Internal,
                SessionSubagentRouteErrorKind::Internal,
            ),
        ] {
            let error = subagent_route_error(SubagentError::new_for_test(kind, "message"));
            assert_eq!(error.kind(), expected_kind);
            assert_eq!(error.message(), "message");
        }
    }

    #[test]
    fn scoped_mcp_errors_preserve_messages_and_redaction() {
        let unauthorized = scoped_mcp_session_route_error(
            ScopedMcpSessionAccessError::Unauthorized("scoped message"),
        );
        assert_eq!(
            unauthorized.kind(),
            SessionSubagentRouteErrorKind::Unauthorized
        );
        assert_eq!(unauthorized.message(), "scoped message");

        let missing = scoped_mcp_session_route_error(ScopedMcpSessionAccessError::SessionNotFound);
        assert_eq!(missing.kind(), SessionSubagentRouteErrorKind::NotFound);
        assert_eq!(missing.message(), "session not found");

        let raw_message = "CTX_MCP_TOKEN=secret-token-123";
        let internal = scoped_mcp_session_route_error(
            ScopedMcpSessionAccessError::StoreUnavailable(anyhow!(raw_message)),
        );
        assert_eq!(internal.kind(), SessionSubagentRouteErrorKind::Internal);
        assert_eq!(internal.message(), logs::redact_sensitive(raw_message));
        assert!(!internal.message().contains("secret-token-123"));
    }

    #[tokio::test]
    async fn read_handle_lists_subagents_for_existing_parent() -> anyhow::Result<()> {
        let (_temp, daemon, parent) = seeded_subagent_read_parent().await?;
        let child = daemon
            .seed_subagent_mcp_existing_label_child_for_test(parent.id, "Reader")
            .await?;

        let response = daemon
            .session_subagent_read_handle_for_test()
            .list_session_subagents_for_route(SessionRouteParams::new(parent.id.0.to_string()))
            .await
            .map_err(|error| anyhow!(error.message().to_string()))?;
        let payload = serde_json::to_value(response)?;
        let subagents = payload.as_array().expect("subagents response is an array");
        assert_eq!(subagents.len(), 1);
        assert_eq!(subagents[0]["id"], child.session_id.0.to_string());
        assert_eq!(subagents[0]["title"], "Reader");
        Ok(())
    }

    #[tokio::test]
    async fn read_handle_lists_invocations_with_and_without_turn_filter() -> anyhow::Result<()> {
        let (_temp, daemon, parent) = seeded_subagent_read_parent().await?;
        let store = daemon.store_for_session(parent.id).await?;
        let turn_a = seed_parent_turn(&store, &parent, 1).await?;
        let turn_b = seed_parent_turn(&store, &parent, 3).await?;
        seed_invocation(&store, "inv-a", parent.id, Some(turn_a)).await?;
        seed_invocation(&store, "inv-b", parent.id, Some(turn_b)).await?;

        let all = daemon
            .session_subagent_read_handle_for_test()
            .list_session_subagent_invocations_for_route(
                SessionRouteParams::new(parent.id.0.to_string()),
                SessionSubagentInvocationsRouteQuery::default(),
            )
            .await
            .map_err(|error| anyhow!(error.message().to_string()))?;
        let all_payload = serde_json::to_value(all)?;
        assert_eq!(
            all_payload
                .as_array()
                .expect("invocations response array")
                .len(),
            2
        );

        let filtered_query: SessionSubagentInvocationsRouteQuery =
            serde_json::from_value(json!({ "turn_id": turn_a.0.to_string() }))?;
        let filtered = daemon
            .session_subagent_read_handle_for_test()
            .list_session_subagent_invocations_for_route(
                SessionRouteParams::new(parent.id.0.to_string()),
                filtered_query,
            )
            .await
            .map_err(|error| anyhow!(error.message().to_string()))?;
        let filtered_payload = serde_json::to_value(filtered)?;
        let filtered_invocations = filtered_payload
            .as_array()
            .expect("filtered invocations response array");
        assert_eq!(filtered_invocations.len(), 1);
        assert_eq!(filtered_invocations[0]["id"], "inv-a");
        Ok(())
    }

    #[tokio::test]
    async fn read_handle_gets_invocation_and_rejects_parent_mismatch() -> anyhow::Result<()> {
        let (_temp, daemon, parent) = seeded_subagent_read_parent().await?;
        let store = daemon.store_for_session(parent.id).await?;
        seed_invocation(&store, "owned-invocation", parent.id, None).await?;
        let foreign_parent = store
            .create_session(
                parent.task_id,
                parent.workspace_id,
                parent.worktree_id,
                parent.execution_environment,
                "fake".to_string(),
                "fake-model".to_string(),
                "assistant".to_string(),
                None,
                None,
                None,
            )
            .await?;
        seed_invocation(&store, "foreign-invocation", foreign_parent.id, None).await?;

        let owned = daemon
            .session_subagent_read_handle_for_test()
            .get_session_subagent_invocation_for_route(
                SessionRouteParams::new(parent.id.0.to_string()),
                "owned-invocation".to_string(),
            )
            .await
            .map_err(|error| anyhow!(error.message().to_string()))?;
        assert_eq!(serde_json::to_value(owned)?["id"], "owned-invocation");

        let mismatch = daemon
            .session_subagent_read_handle_for_test()
            .get_session_subagent_invocation_for_route(
                SessionRouteParams::new(parent.id.0.to_string()),
                "foreign-invocation".to_string(),
            )
            .await
            .unwrap_err();
        assert_eq!(mismatch.kind(), SessionSubagentRouteErrorKind::NotFound);
        assert_eq!(mismatch.message(), "session not found");
        Ok(())
    }

    #[tokio::test]
    async fn mcp_read_handle_lists_agents_with_scoped_context() -> anyhow::Result<()> {
        let (_temp, daemon, parent) = seeded_subagent_read_parent().await?;
        let child = daemon
            .seed_subagent_mcp_existing_label_child_for_test(parent.id, "Scoped Reader")
            .await?;

        let response = daemon
            .session_subagent_mcp_read_handle_for_test()
            .list_agents_for_mcp_route(
                SessionRouteParams::new(parent.id.0.to_string()),
                Some(scoped_context_for(&parent)),
            )
            .await
            .map_err(|error| anyhow!(error.message().to_string()))?;
        let payload = serde_json::to_value(response)?;
        let agents = payload.as_array().expect("agents response is an array");
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0]["agent_id"], encode_agent_ref(child.session_id));
        assert_eq!(agents[0]["task_label"], "Scoped Reader");
        Ok(())
    }

    #[tokio::test]
    async fn mcp_read_handle_rejects_foreign_scoped_context() -> anyhow::Result<()> {
        let (_temp, daemon, parent) = seeded_subagent_read_parent().await?;
        let mut foreign_auth = scoped_context_for(&parent);
        foreign_auth.session_id = SessionId::new();

        let error = daemon
            .session_subagent_mcp_read_handle_for_test()
            .list_agents_for_mcp_route(
                SessionRouteParams::new(parent.id.0.to_string()),
                Some(foreign_auth),
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind(), SessionSubagentRouteErrorKind::Unauthorized);
        assert_eq!(
            error.message(),
            "scoped ctx-mcp token is limited to the current session"
        );
        Ok(())
    }

    #[tokio::test]
    async fn mcp_read_handle_wait_zero_timeout_returns_timeout() -> anyhow::Result<()> {
        let (_temp, daemon, parent) = seeded_subagent_read_parent().await?;
        let child = daemon
            .seed_subagent_mcp_existing_label_child_for_test(parent.id, "Waiter")
            .await?;
        let request: WaitAgentRouteRequest = serde_json::from_value(json!({
            "agent_id": encode_agent_ref(child.session_id),
            "timeout_ms": 0
        }))?;

        let response = daemon
            .session_subagent_mcp_read_handle_for_test()
            .wait_agent_for_mcp_route(
                SessionRouteParams::new(parent.id.0.to_string()),
                Some(scoped_context_for(&parent)),
                request,
            )
            .await
            .map_err(|error| anyhow!(error.message().to_string()))?;
        let payload = serde_json::to_value(response)?;
        assert_eq!(payload["wait_status"], "timeout");
        assert_eq!(payload["results"].as_array().expect("results").len(), 1);
        Ok(())
    }
}
