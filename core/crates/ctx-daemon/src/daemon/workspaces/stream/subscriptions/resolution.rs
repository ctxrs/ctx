use std::collections::HashMap;

use ctx_core::ids::{SessionId, WorkspaceId};
use ctx_core::models::WorkspaceActiveSnapshotClientMessage;
use ctx_store::Store;
use ctx_workspace_active_snapshot::{
    resolve_workspace_active_snapshot_subscriptions as resolve_workspace_active_snapshot_subscriptions_with_source,
    ResolvedWorkspaceActiveSubscriptions, SessionReplayCursor, WorkspaceActiveSubscriptionSource,
};

use crate::daemon::WorkspaceStreamHandle;

pub async fn resolve_workspace_active_snapshot_subscriptions(
    handle: &WorkspaceStreamHandle,
    workspace_id: WorkspaceId,
    message: WorkspaceActiveSnapshotClientMessage,
    existing: &HashMap<SessionId, SessionReplayCursor>,
) -> Result<ResolvedWorkspaceActiveSubscriptions, ()> {
    resolve_workspace_active_snapshot_subscriptions_with_source(
        &HttpWorkspaceActiveSubscriptionSource { handle },
        workspace_id,
        message,
        existing,
    )
    .await
}

struct HttpWorkspaceActiveSubscriptionSource<'a> {
    handle: &'a WorkspaceStreamHandle,
}

impl WorkspaceActiveSubscriptionSource for HttpWorkspaceActiveSubscriptionSource<'_> {
    async fn session_belongs_to_workspace(
        &self,
        workspace_id: WorkspaceId,
        session_id: SessionId,
    ) -> bool {
        session_belongs_to_workspace(self.handle, workspace_id, session_id).await
    }

    async fn active_tasks(
        &self,
        workspace_id: WorkspaceId,
    ) -> Vec<ctx_core::models::WorkspaceActiveTaskSummary> {
        self.handle
            .active_snapshot()
            .active_snapshot(workspace_id, i64::MAX)
            .await
            .active
            .tasks
    }

    async fn primary_session_id_for_task(
        &self,
        workspace_id: WorkspaceId,
        task_id: ctx_core::ids::TaskId,
    ) -> Result<Option<SessionId>, ()> {
        let store = self
            .handle
            .store_for_workspace(workspace_id)
            .await
            .map_err(|_| ())?;
        let task = store.get_task(task_id).await.map_err(|_| ())?;
        let Some(task) = task else {
            return Ok(None);
        };
        if task.workspace_id != workspace_id {
            return Ok(None);
        }
        Ok(task.primary_session_id)
    }

    async fn session_replay_cursor(
        &self,
        workspace_id: WorkspaceId,
        session_id: SessionId,
    ) -> SessionReplayCursor {
        self.handle
            .active_snapshot()
            .session_replay_cursor(workspace_id, session_id)
            .await
    }
}

async fn session_belongs_to_workspace(
    handle: &WorkspaceStreamHandle,
    workspace_id: WorkspaceId,
    session_id: SessionId,
) -> bool {
    let store = match session_store_for_stream(handle, session_id).await {
        Ok(store) => store,
        Err(_) => return false,
    };
    match store.get_session(session_id).await {
        Ok(Some(session)) => session.workspace_id == workspace_id,
        _ => false,
    }
}

async fn session_store_for_stream(
    handle: &WorkspaceStreamHandle,
    session_id: SessionId,
) -> Result<Store, ()> {
    handle
        .session_store_allow_archived(session_id)
        .await
        .map_err(|_| ())
}
