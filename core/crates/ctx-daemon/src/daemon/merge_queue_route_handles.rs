use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use ctx_core::ids::{SessionId, WorkspaceId};
use ctx_core::models::{SessionEvent, SessionEventType};
use ctx_store::Store;

pub(in crate::daemon) type MergeQueueNoticePublicationFuture =
    Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>>;
pub(in crate::daemon) type MergeQueueNoticePublicationEffect =
    Arc<dyn Fn(MergeQueueNoticeSessionEvent) -> MergeQueueNoticePublicationFuture + Send + Sync>;

pub(in crate::daemon) struct MergeQueueNoticeSessionEvent {
    event: SessionEvent,
}

impl MergeQueueNoticeSessionEvent {
    pub(in crate::daemon) fn new(event: SessionEvent) -> anyhow::Result<Self> {
        if !matches!(&event.event_type, SessionEventType::Notice) {
            anyhow::bail!("merge queue notice publication requires a notice event");
        }
        let kind = event
            .payload_json
            .get("kind")
            .and_then(serde_json::Value::as_str);
        if !matches!(
            kind,
            Some("merge_queue_sync" | "merge_queue_canonical_sync")
        ) {
            anyhow::bail!("merge queue notice publication requires a merge queue payload");
        }
        Ok(Self { event })
    }

    pub(in crate::daemon) fn into_event(self) -> SessionEvent {
        self.event
    }
}

#[derive(Clone)]
pub struct MergeQueueApiHandle {
    host: Arc<crate::daemon::merge_queue::MergeQueueRouteHost>,
}

impl MergeQueueApiHandle {
    pub(in crate::daemon) fn new(
        host: Arc<crate::daemon::merge_queue::MergeQueueRouteHost>,
    ) -> Self {
        Self { host }
    }

    pub(in crate::daemon) async fn existing_workspace_store(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Store, crate::daemon::WorkspaceStoreAccessError> {
        self.host.existing_workspace_store(workspace_id).await
    }

    pub(in crate::daemon) async fn submit_merge_queue_entry(
        &self,
        params: ctx_merge_queue::MergeQueueSubmitParams,
    ) -> anyhow::Result<ctx_core::models::MergeQueueEntry> {
        ctx_merge_queue::submit_merge_queue_entry::<crate::daemon::merge_queue::MergeQueueRouteHost>(
            &self.host,
            params,
        )
        .await
    }

    pub(in crate::daemon) async fn cancel_merge_queue_entry(
        &self,
        workspace_id: WorkspaceId,
        entry_id: ctx_core::ids::MergeQueueEntryId,
    ) -> anyhow::Result<ctx_core::models::MergeQueueEntry> {
        ctx_merge_queue::cancel_merge_queue_entry::<crate::daemon::merge_queue::MergeQueueRouteHost>(
            &self.host,
            workspace_id,
            entry_id,
        )
        .await
    }

    pub(in crate::daemon) async fn retry_merge_queue_entry(
        &self,
        workspace_id: WorkspaceId,
        entry_id: ctx_core::ids::MergeQueueEntryId,
    ) -> anyhow::Result<ctx_core::models::MergeQueueEntry> {
        ctx_merge_queue::retry_merge_queue_entry::<crate::daemon::merge_queue::MergeQueueRouteHost>(
            &self.host,
            workspace_id,
            entry_id,
        )
        .await
    }

    pub(in crate::daemon) async fn get_workspace_merge_queue_entry(
        &self,
        workspace_id: WorkspaceId,
        entry_id: ctx_core::ids::MergeQueueEntryId,
    ) -> anyhow::Result<ctx_core::models::MergeQueueEntry> {
        ctx_merge_queue::get_workspace_merge_queue_entry::<
            crate::daemon::merge_queue::MergeQueueRouteHost,
        >(self.host.as_ref(), workspace_id, entry_id)
        .await
    }

    pub(in crate::daemon) async fn require_scoped_mcp_session_context(
        &self,
        mcp_auth: ctx_mcp_auth::McpAuthContext,
        session_id: SessionId,
    ) -> Result<(), crate::daemon::ScopedMcpSessionAccessError> {
        self.host
            .require_scoped_mcp_session_context(mcp_auth, session_id)
            .await
    }
}
