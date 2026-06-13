use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use ctx_core::ids::SessionId;
use ctx_core::models::SessionEvent;
use ctx_store::Store;

use crate::daemon::state::SessionStoreLookup;

pub(in crate::daemon) type SessionArtifactsFuture<T> =
    Pin<Box<dyn Future<Output = T> + Send + 'static>>;
#[derive(Clone)]
pub struct SessionArtifactsHandle {
    lookup: SessionStoreLookup,
    tool_output_spool_dir: PathBuf,
    effects: Arc<SessionArtifactEffects>,
}

impl SessionArtifactsHandle {
    pub(in crate::daemon) fn new(
        lookup: SessionStoreLookup,
        tool_output_spool_dir: PathBuf,
        effects: Arc<SessionArtifactEffects>,
    ) -> Self {
        Self {
            lookup,
            tool_output_spool_dir,
            effects,
        }
    }

    pub(in crate::daemon) async fn existing_session_store(
        &self,
        session_id: SessionId,
    ) -> Result<Store, crate::daemon::SessionStoreAccessError> {
        self.lookup.existing_session_store(session_id).await
    }

    pub(in crate::daemon) async fn existing_session_store_for_write(
        &self,
        session_id: SessionId,
    ) -> Result<Store, crate::daemon::SessionStoreAccessError> {
        self.lookup
            .existing_session_store_for_write(session_id)
            .await
    }

    pub(in crate::daemon) async fn require_scoped_mcp_session_context(
        &self,
        mcp_auth: ctx_mcp_auth::McpAuthContext,
        session_id: SessionId,
    ) -> Result<(), crate::daemon::ScopedMcpSessionAccessError> {
        self.lookup
            .require_scoped_mcp_session_context(mcp_auth, session_id)
            .await
    }

    pub(in crate::daemon) fn session_tool_output_spool_dir(
        &self,
        session_id: SessionId,
    ) -> PathBuf {
        self.tool_output_spool_dir.join(session_id.0.to_string())
    }

    pub(in crate::daemon) async fn publish_event(&self, event: SessionEvent) {
        self.effects.publish_event(event).await;
    }
}

pub(in crate::daemon) struct SessionArtifactEffects {
    publish_event: Arc<dyn Fn(SessionEvent) -> SessionArtifactsFuture<()> + Send + Sync>,
}

impl SessionArtifactEffects {
    pub(in crate::daemon) fn new(
        publish_event: Arc<dyn Fn(SessionEvent) -> SessionArtifactsFuture<()> + Send + Sync>,
    ) -> Arc<Self> {
        Arc::new(Self { publish_event })
    }

    pub(in crate::daemon) async fn publish_event(&self, event: SessionEvent) {
        (self.publish_event)(event).await;
    }
}
