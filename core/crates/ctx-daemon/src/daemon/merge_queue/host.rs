use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use ctx_core::ids::{SessionId, WorkspaceId, WorktreeId};
use ctx_core::models::{SessionEventType, Workspace};
use ctx_merge_queue::{MergeQueueHost, MergeQueueNotice, MergeQueueToolExecEvent};
use ctx_observability::ops_events::{OpsEvent, OpsEvents};
use ctx_store::Store;
use ctx_store::StoreManager;

use crate::daemon::{
    merge_queue_route_handles::{MergeQueueNoticePublicationEffect, MergeQueueNoticeSessionEvent},
    ProtectedWorkspaceStoreLookup, ScopedMcpSessionAccessError, SessionStoreLookup,
    WorkspaceStoreAccessError,
};

pub(in crate::daemon) struct MergeQueueRouteHost {
    stores: StoreManager,
    global_store: Store,
    workspace_stores: ProtectedWorkspaceStoreLookup,
    session_stores: SessionStoreLookup,
    merge_queue: Arc<ctx_merge_queue::MergeQueueRuntime>,
    ops_events: OpsEvents,
    publish_merge_queue_notice: MergeQueueNoticePublicationEffect,
}

impl MergeQueueRouteHost {
    pub(in crate::daemon) fn new(
        stores: StoreManager,
        global_store: Store,
        workspace_stores: ProtectedWorkspaceStoreLookup,
        session_stores: SessionStoreLookup,
        merge_queue: Arc<ctx_merge_queue::MergeQueueRuntime>,
        ops_events: OpsEvents,
        publish_merge_queue_notice: MergeQueueNoticePublicationEffect,
    ) -> Self {
        Self {
            stores,
            global_store,
            workspace_stores,
            session_stores,
            merge_queue,
            ops_events,
            publish_merge_queue_notice,
        }
    }

    pub(in crate::daemon) async fn existing_workspace_store(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Store, WorkspaceStoreAccessError> {
        self.workspace_stores
            .existing_workspace_store(workspace_id)
            .await
    }

    pub(in crate::daemon) async fn require_scoped_mcp_session_context(
        &self,
        mcp_auth: ctx_mcp_auth::McpAuthContext,
        session_id: SessionId,
    ) -> Result<(), ScopedMcpSessionAccessError> {
        self.session_stores
            .require_scoped_mcp_session_context(mcp_auth, session_id)
            .await
    }

    async fn publish_merge_queue_notice_event(
        &self,
        event: ctx_core::models::SessionEvent,
    ) -> Result<()> {
        (self.publish_merge_queue_notice)(MergeQueueNoticeSessionEvent::new(event)?).await
    }
}

fn merge_queue_notice_payload(notice: MergeQueueNotice) -> (SessionId, serde_json::Value) {
    match notice {
        MergeQueueNotice::Sync {
            session_id,
            worktree_id,
            target_branch,
            previous_commit_sha,
            commit_sha,
            message,
        } => (
            session_id,
            serde_json::json!({
                "kind": "merge_queue_sync",
                "message": message,
                "worktree_id": worktree_id.0.to_string(),
                "target_branch": target_branch,
                "previous_commit_sha": previous_commit_sha,
                "commit_sha": commit_sha,
                "base_revision": commit_sha,
                "base_commit_sha": commit_sha,
            }),
        ),
        MergeQueueNotice::CanonicalSync {
            session_id,
            worktree_id,
            target_branch,
            commit_sha,
            status,
            message,
        } => (
            session_id,
            serde_json::json!({
                "kind": "merge_queue_canonical_sync",
                "status": status,
                "message": message,
                "worktree_id": worktree_id.map(|id| id.0.to_string()),
                "target_branch": target_branch,
                "commit_sha": commit_sha,
            }),
        ),
    }
}

fn merge_queue_tool_exec_event(event: MergeQueueToolExecEvent) -> OpsEvent {
    let mut ops_event = OpsEvent::new("info", "merge_queue_tool_exec");
    ops_event.session_id = event.session_id.map(|id| id.0.to_string());
    ops_event.worktree_id = event.worktree_id.map(|id| id.0.to_string());
    ops_event.tool_kind = Some("merge_queue".to_string());
    ops_event.cwd = event.workdir.clone();
    ops_event.worktree_root = event.workdir;
    ops_event.meta = Some(serde_json::json!({
        "entry_id": event.entry_id.0.to_string(),
        "command": event.command,
        "tool_slice": event.used_tool_slice,
        "slice": event.tool_slice_unit,
    }));
    ops_event
}

#[async_trait]
impl MergeQueueHost for MergeQueueRouteHost {
    fn merge_queue_runtime(state: &Self) -> &ctx_merge_queue::MergeQueueRuntime {
        state.merge_queue.as_ref()
    }

    async fn protected_workspace_store(state: &Self, workspace_id: WorkspaceId) -> Result<Store> {
        state
            .workspace_stores
            .store_for_workspace(workspace_id)
            .await
    }

    async fn raw_workspace_store(state: &Self, workspace_id: WorkspaceId) -> Result<Store> {
        state.stores.workspace(workspace_id).await
    }

    async fn session_store(state: &Self, session_id: SessionId) -> Result<Store> {
        state
            .session_stores
            .existing_session_store_allow_archived(session_id)
            .await
            .map_err(|error| anyhow::anyhow!("session store unavailable: {error:?}"))
    }

    async fn worktree_store(state: &Self, worktree_id: WorktreeId) -> Result<Store> {
        state.workspace_stores.store_for_worktree(worktree_id).await
    }

    async fn get_workspace(state: &Self, workspace_id: WorkspaceId) -> Result<Option<Workspace>> {
        state.global_store.get_workspace(workspace_id).await
    }

    async fn upsert_workspace_worktree_index(
        state: &Self,
        worktree_id: WorktreeId,
        workspace_id: WorkspaceId,
    ) -> Result<()> {
        state
            .global_store
            .upsert_workspace_worktree_index(worktree_id, workspace_id)
            .await
    }

    async fn publish_notice(state: &Arc<Self>, notice: MergeQueueNotice) -> Result<()> {
        let (session_id, payload) = merge_queue_notice_payload(notice);
        let store = Self::session_store(state.as_ref(), session_id).await?;
        let notice_event = store
            .append_session_event(session_id, None, None, SessionEventType::Notice, payload)
            .await?;
        state.publish_merge_queue_notice_event(notice_event).await
    }

    fn emit_tool_exec(state: &Self, event: MergeQueueToolExecEvent) {
        state.ops_events.emit(merge_queue_tool_exec_event(event));
    }
}
