use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ctx_core::ids::{SessionId, TurnId};
use ctx_core::models::{Session, SessionSummary, SubagentInvocation};
use ctx_store::Store;

use crate::daemon::state::{session_store_access_anyhow, SessionStoreLookup};

#[derive(Clone)]
pub struct SessionSubagentReadHandle {
    session_stores: SessionStoreLookup,
}

impl SessionSubagentReadHandle {
    pub(in crate::daemon) fn new(session_stores: SessionStoreLookup) -> Self {
        Self { session_stores }
    }

    async fn load_session_store_and_parent(
        &self,
        session_id: SessionId,
    ) -> anyhow::Result<Option<(Store, Session)>> {
        let store = match self.session_stores.existing_session_store(session_id).await {
            Ok(store) => store,
            Err(crate::daemon::SessionStoreAccessError::NotFound) => return Ok(None),
            Err(error) => return Err(session_store_access_anyhow(error)),
        };
        let Some(session) = store.get_session(session_id).await? else {
            return Ok(None);
        };
        Ok(Some((store, session)))
    }

    pub(in crate::daemon) async fn list_session_subagents_for_request(
        &self,
        session_id: SessionId,
    ) -> anyhow::Result<Option<Vec<SessionSummary>>> {
        let Some((store, session)) = self.load_session_store_and_parent(session_id).await? else {
            return Ok(None);
        };
        store.list_subagent_sessions(session.id).await.map(Some)
    }

    pub(in crate::daemon) async fn list_session_subagent_invocations_for_request(
        &self,
        session_id: SessionId,
        turn_id: Option<TurnId>,
    ) -> anyhow::Result<Option<Vec<SubagentInvocation>>> {
        let Some((store, session)) = self.load_session_store_and_parent(session_id).await? else {
            return Ok(None);
        };
        store
            .list_subagent_invocations_for_session(session.id, turn_id)
            .await
            .map(Some)
    }

    pub(in crate::daemon) async fn get_session_subagent_invocation_for_request(
        &self,
        session_id: SessionId,
        invocation_id: &str,
    ) -> anyhow::Result<Option<SubagentInvocation>> {
        let Some((store, _session)) = self.load_session_store_and_parent(session_id).await? else {
            return Ok(None);
        };
        let Some(invocation) = store.get_subagent_invocation(invocation_id).await? else {
            return Ok(None);
        };
        if invocation.parent_session_id != session_id {
            return Ok(None);
        }
        Ok(Some(invocation))
    }
}

pub(in crate::daemon) type SessionSubagentMcpReadFuture<T> =
    Pin<Box<dyn Future<Output = T> + Send + 'static>>;
pub(in crate::daemon) type SessionSubagentMcpReadProviderTimeout =
    Arc<dyn Fn() -> SessionSubagentMcpReadFuture<Duration> + Send + Sync>;
pub(in crate::daemon) type SessionSubagentMcpReadLegacyContextWindowRejectCounter =
    Arc<dyn Fn(String) -> SessionSubagentMcpReadFuture<()> + Send + Sync>;

#[derive(Clone)]
pub struct SessionSubagentMcpReadHandle {
    session_stores: SessionStoreLookup,
    provider_inactivity_timeout: SessionSubagentMcpReadProviderTimeout,
    emit_legacy_context_window_key_reject: SessionSubagentMcpReadLegacyContextWindowRejectCounter,
}

impl SessionSubagentMcpReadHandle {
    pub(in crate::daemon) fn new(
        session_stores: SessionStoreLookup,
        provider_inactivity_timeout: SessionSubagentMcpReadProviderTimeout,
        emit_legacy_context_window_key_reject: SessionSubagentMcpReadLegacyContextWindowRejectCounter,
    ) -> Self {
        Self {
            session_stores,
            provider_inactivity_timeout,
            emit_legacy_context_window_key_reject,
        }
    }

    async fn load_parent_session(
        &self,
        parent_id: SessionId,
    ) -> Result<(Store, Session), crate::daemon::sessions::subagents::SubagentError> {
        let store = match self.session_stores.existing_session_store(parent_id).await {
            Ok(store) => store,
            Err(crate::daemon::SessionStoreAccessError::NotFound) => {
                return Err(crate::daemon::sessions::subagents::not_found(
                    "parent session not found",
                ));
            }
            Err(error) => {
                return Err(crate::daemon::sessions::subagents::internal_api_error(
                    session_store_access_anyhow(error),
                ));
            }
        };
        let parent = store
            .get_session(parent_id)
            .await
            .map_err(crate::daemon::sessions::subagents::internal_api_error)?
            .ok_or_else(|| {
                crate::daemon::sessions::subagents::not_found("parent session not found")
            })?;
        Ok((store, parent))
    }

    async fn provider_inactivity_timeout(&self) -> Duration {
        (self.provider_inactivity_timeout)().await
    }

    pub(in crate::daemon) async fn require_scoped_mcp_session_context(
        &self,
        mcp_auth: ctx_mcp_auth::McpAuthContext,
        session_id: SessionId,
    ) -> Result<(), crate::daemon::ScopedMcpSessionAccessError> {
        self.session_stores
            .require_scoped_mcp_session_context(mcp_auth, session_id)
            .await
    }

    pub(in crate::daemon) async fn list_agents(
        &self,
        parent_id: SessionId,
    ) -> Result<
        Vec<crate::daemon::sessions::subagents::AgentSummary>,
        crate::daemon::sessions::subagents::SubagentError,
    > {
        let (store, parent) = self.load_parent_session(parent_id).await?;
        let inactivity_timeout = self.provider_inactivity_timeout().await;
        let subs = store
            .list_subagent_sessions(parent.id)
            .await
            .map_err(crate::daemon::sessions::subagents::internal_api_error)?;
        let mut agents = Vec::with_capacity(subs.len());
        for sub in subs {
            let (summary, _latest_turn) = crate::daemon::sessions::subagents::build_agent_summary(
                &store,
                sub.id,
                &sub.title,
                inactivity_timeout,
            )
            .await?;
            agents.push(summary);
        }
        Ok(agents)
    }

    pub(in crate::daemon) async fn get_agent(
        &self,
        parent_id: SessionId,
        req: crate::daemon::sessions::subagents::GetAgentReq,
    ) -> Result<
        crate::daemon::sessions::subagents::GetAgentResp,
        crate::daemon::sessions::subagents::SubagentError,
    > {
        let (store, parent) = self.load_parent_session(parent_id).await?;
        let inactivity_timeout = self.provider_inactivity_timeout().await;
        let child = crate::daemon::sessions::subagents::resolve_child_agent_session(
            &store,
            &parent,
            &req.agent_id,
        )
        .await?;
        let detail = crate::daemon::sessions::subagents::build_agent_detail_for_mcp_read(
            &store,
            &parent,
            &child,
            inactivity_timeout,
            &self.emit_legacy_context_window_key_reject,
        )
        .await?;
        Ok(crate::daemon::sessions::subagents::GetAgentResp { agent: detail })
    }

    pub(in crate::daemon) async fn wait_agent(
        &self,
        parent_id: SessionId,
        req: crate::daemon::sessions::subagents::WaitAgentReq,
    ) -> Result<
        crate::daemon::sessions::subagents::WaitAgentResp,
        crate::daemon::sessions::subagents::SubagentError,
    > {
        let agent_ids = ctx_subagent_service::normalize_wait_agent_ids(
            req.agent_id.as_deref(),
            req.agent_ids.as_deref(),
        )
        .map_err(|error| {
            crate::daemon::sessions::subagents::api_error(
                crate::daemon::sessions::subagents::SubagentErrorKind::BadRequest,
                error,
            )
        })?;
        let (store, parent) = self.load_parent_session(parent_id).await?;
        let inactivity_timeout = self.provider_inactivity_timeout().await;
        let targets =
            crate::daemon::sessions::subagents::collect_wait_targets(&store, &parent, &agent_ids)
                .await?;
        let mode = ctx_subagent_service::parse_wait_mode(req.mode.as_deref()).map_err(|error| {
            crate::daemon::sessions::subagents::api_error(
                crate::daemon::sessions::subagents::SubagentErrorKind::BadRequest,
                error,
            )
        })?;
        let until =
            ctx_subagent_service::parse_wait_until(req.until.as_deref()).map_err(|error| {
                crate::daemon::sessions::subagents::api_error(
                    crate::daemon::sessions::subagents::SubagentErrorKind::BadRequest,
                    error,
                )
            })?;
        if req.since_seq.is_some() && targets.len() != 1 {
            return Err(crate::daemon::sessions::subagents::api_error(
                crate::daemon::sessions::subagents::SubagentErrorKind::BadRequest,
                "since_seq is only supported with a single agent_id",
            ));
        }

        let timeout_ms = req.timeout_ms.unwrap_or(30_000);
        let mut details = self
            .collect_wait_details(&store, &parent, &targets, inactivity_timeout)
            .await?;
        let thresholds = subagent_wait_update_thresholds(&details, until, req.since_seq);

        if ctx_subagent_service::wait_predicate_satisfied(
            &subagent_agent_wait_details(&details),
            mode,
            until,
            &thresholds,
        ) {
            return Ok(subagent_wait_response("matched", mode, until, details));
        }
        if timeout_ms == 0 {
            return Ok(subagent_wait_response("timeout", mode, until, details));
        }

        let started_at = Instant::now();
        while started_at.elapsed() < Duration::from_millis(timeout_ms) {
            tokio::time::sleep(Duration::from_millis(100)).await;
            details = self
                .collect_wait_details(&store, &parent, &targets, inactivity_timeout)
                .await?;
            if ctx_subagent_service::wait_predicate_satisfied(
                &subagent_agent_wait_details(&details),
                mode,
                until,
                &thresholds,
            ) {
                return Ok(subagent_wait_response("matched", mode, until, details));
            }
        }

        Ok(subagent_wait_response("timeout", mode, until, details))
    }

    async fn collect_wait_details(
        &self,
        store: &Store,
        parent: &Session,
        targets: &[Session],
        inactivity_timeout: Duration,
    ) -> Result<
        Vec<crate::daemon::sessions::subagents::AgentDetail>,
        crate::daemon::sessions::subagents::SubagentError,
    > {
        let mut details = Vec::with_capacity(targets.len());
        for target in targets {
            details.push(
                crate::daemon::sessions::subagents::build_agent_detail_for_mcp_read(
                    store,
                    parent,
                    target,
                    inactivity_timeout,
                    &self.emit_legacy_context_window_key_reject,
                )
                .await?,
            );
        }
        Ok(details)
    }
}

fn subagent_wait_update_thresholds(
    details: &[crate::daemon::sessions::subagents::AgentDetail],
    until: ctx_subagent_service::AgentWaitUntil,
    since_seq: Option<i64>,
) -> HashMap<String, i64> {
    let mut thresholds = HashMap::new();
    match until {
        ctx_subagent_service::AgentWaitUntil::Terminal => {}
        ctx_subagent_service::AgentWaitUntil::Update => {
            if let Some(since_seq) = since_seq {
                thresholds.insert(details[0].agent.agent_id.clone(), since_seq);
            } else {
                for detail in details {
                    thresholds.insert(detail.agent.agent_id.clone(), detail.agent.last_event_seq);
                }
            }
        }
    }
    thresholds
}

fn subagent_wait_response(
    wait_status: &str,
    mode: ctx_subagent_service::AgentWaitMode,
    until: ctx_subagent_service::AgentWaitUntil,
    results: Vec<crate::daemon::sessions::subagents::AgentDetail>,
) -> crate::daemon::sessions::subagents::WaitAgentResp {
    crate::daemon::sessions::subagents::WaitAgentResp {
        wait_status: wait_status.to_string(),
        mode: mode.as_str().to_string(),
        until: until.as_str().to_string(),
        results,
    }
}

fn subagent_agent_wait_details(
    details: &[crate::daemon::sessions::subagents::AgentDetail],
) -> Vec<ctx_subagent_service::AgentWaitDetail<'_>> {
    details
        .iter()
        .map(|detail| ctx_subagent_service::AgentWaitDetail {
            agent_id: &detail.agent.agent_id,
            has_current_run: detail.agent.current_run_id.is_some(),
            has_latest_result: detail.agent.latest_result_status.is_some(),
            last_event_seq: detail.agent.last_event_seq,
        })
        .collect()
}
