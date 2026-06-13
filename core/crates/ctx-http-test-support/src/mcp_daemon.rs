use anyhow::{Context, Result};
use ctx_core::ids::SessionId;
use ctx_core::models::{SessionEvent, SessionSummary};
use ctx_daemon::test_support::TestDaemon;
use tempfile::TempDir;

mod fake;
mod git;
mod live;
mod router;

pub struct DaemonBackedParentSession {
    _repo: TempDir,
    _data_dir: TempDir,
    daemon: TestDaemon,
    base_url: String,
    session_id: SessionId,
    mcp_token: String,
}

impl DaemonBackedParentSession {
    pub(crate) fn new(
        repo: TempDir,
        data_dir: TempDir,
        daemon: TestDaemon,
        base_url: String,
        session_id: SessionId,
        mcp_token: String,
    ) -> Self {
        Self {
            _repo: repo,
            _data_dir: data_dir,
            daemon,
            base_url,
            session_id,
            mcp_token,
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    pub fn mcp_token(&self) -> &str {
        &self.mcp_token
    }

    pub async fn list_session_events(&self) -> Result<Vec<SessionEvent>> {
        self.daemon
            .mcp_parent_session_events_for_test(self.session_id)
            .await
            .context("list session events")
    }

    pub async fn list_subagent_sessions(&self) -> Result<Vec<SessionSummary>> {
        self.daemon
            .mcp_subagent_sessions_for_test(self.session_id)
            .await
            .context("list subagent sessions")
    }
}

pub async fn setup_fake_provider_parent_session() -> Result<DaemonBackedParentSession> {
    fake::setup_fake_provider_parent_session().await
}

pub async fn setup_live_provider_parent_session(
    provider_id: &str,
    model_id: &str,
) -> Result<DaemonBackedParentSession> {
    live::setup_live_provider_parent_session(provider_id, model_id).await
}
