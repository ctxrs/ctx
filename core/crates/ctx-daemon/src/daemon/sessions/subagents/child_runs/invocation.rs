use std::sync::Arc;

use ctx_core::ids::{SessionId, TurnId, WorktreeId};
use ctx_core::models::{SessionEvent, SessionEventType, SubagentInvocationChild};
use ctx_observability::logs;
use ctx_store::Store;
use ctx_workspace_active_snapshot::WorkspaceActiveSnapshotHub;

use crate::daemon::sessions::subagents::SessionSubagentMcpControlPublicationHost;
use crate::daemon::{session_store_access_anyhow, SessionStoreAccessError, WeakSessionStoreLookup};

use super::super::errors::{internal_api_error, ApiResult};
use super::status::subagent_status_from_turn_status;
use super::wait::{wait_for_run_terminal_turn, SessionEventHeadSubscriber};
pub(in crate::daemon::sessions::subagents) use finalize::finalize_subagent_invocation;

#[path = "invocation/finalize.rs"]
mod finalize;

#[derive(Clone)]
pub(in crate::daemon) struct SubagentChildRunHost {
    session_stores: WeakSessionStoreLookup,
    event_heads: SessionEventHeadSubscriber,
    active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
}

impl SubagentChildRunHost {
    pub(in crate::daemon) fn new(
        session_stores: WeakSessionStoreLookup,
        event_heads: SessionEventHeadSubscriber,
        active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
    ) -> Self {
        Self {
            session_stores,
            event_heads,
            active_snapshot,
        }
    }

    pub(super) async fn store_for_session_allow_archived(
        &self,
        session_id: SessionId,
    ) -> Result<Option<Store>, SessionStoreAccessError> {
        self.session_stores
            .existing_session_store_allow_archived(session_id)
            .await
    }

    pub(super) fn event_heads(&self) -> &SessionEventHeadSubscriber {
        &self.event_heads
    }

    pub(super) async fn publish_event(&self, event: SessionEvent) {
        let Some(runtime) = self.event_heads.runtime() else {
            return;
        };
        let Some((session_stores, workspace_stores)) = self.session_stores.upgraded_lookups()
        else {
            return;
        };
        let publish_host = SessionSubagentMcpControlPublicationHost::new(
            session_stores,
            workspace_stores,
            Arc::clone(&self.active_snapshot),
        );
        runtime.publish_event_with_host(&publish_host, event).await;
    }
}

pub(in crate::daemon::sessions::subagents) async fn emit_subagent_invocation_notice(
    host: &SubagentChildRunHost,
    parent_session_id: SessionId,
    parent_turn_id: Option<TurnId>,
    payload: serde_json::Value,
) -> ApiResult<()> {
    let store = match host
        .store_for_session_allow_archived(parent_session_id)
        .await
    {
        Ok(Some(store)) => store,
        Ok(None) => return Ok(()),
        Err(SessionStoreAccessError::NotFound) => {
            return Err(super::super::not_found("parent session not found"));
        }
        Err(error) => return Err(internal_api_error(session_store_access_anyhow(error))),
    };
    let event = store
        .append_session_event(
            parent_session_id,
            None,
            parent_turn_id,
            SessionEventType::Notice,
            payload,
        )
        .await
        .map_err(internal_api_error)?;
    host.publish_event(event).await;
    Ok(())
}

pub(in crate::daemon::sessions::subagents) async fn run_subagent_child(
    host: &SubagentChildRunHost,
    child: SubagentInvocationChild,
    _parent_worktree_id: WorktreeId,
) -> Result<(), String> {
    let run_id = child
        .run_id
        .ok_or_else(|| "subagent run_id missing".to_string())?;
    let store = match host
        .store_for_session_allow_archived(child.child_session_id)
        .await
    {
        Ok(Some(store)) => store,
        Ok(None) => return Ok(()),
        Err(SessionStoreAccessError::NotFound) => return Ok(()),
        Err(error) => {
            return Err(logs::redact_sensitive(
                &session_store_access_anyhow(error).to_string(),
            ))
        }
    };
    let status = match wait_for_run_terminal_turn(
        host.event_heads(),
        &store,
        child.child_session_id,
        run_id,
    )
    .await
    {
        Ok(Some(turn)) => subagent_status_from_turn_status(turn.status).to_string(),
        Ok(None) => return Ok(()),
        Err(_) => "unknown".to_string(),
    };

    let child_updated_at = chrono::Utc::now();
    let mut updated_child = child.clone();
    updated_child.status = status.clone();
    updated_child.updated_at = child_updated_at;
    store
        .upsert_subagent_invocation_child(updated_child)
        .await
        .map_err(|error| logs::redact_sensitive(&error.to_string()))?;
    Ok(())
}
