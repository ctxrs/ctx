use super::*;
use ctx_store::{Store, StoreManager};

pub(super) async fn collect_turns_by_statuses(
    state: &Arc<DaemonState>,
    statuses: &[SessionTurnStatus],
) -> Result<(usize, Vec<(WorkspaceId, SessionTurn)>)> {
    collect_turns_by_statuses_parts(state.global_store(), &state.core.stores, statuses).await
}

pub(in crate::daemon) async fn collect_turns_by_statuses_parts(
    global_store: &Store,
    stores: &StoreManager,
    statuses: &[SessionTurnStatus],
) -> Result<(usize, Vec<(WorkspaceId, SessionTurn)>)> {
    let workspaces = global_store.list_workspaces().await?;
    let workspace_count = workspaces.len();
    let mut matching_turns = Vec::new();
    for workspace in workspaces {
        let store = stores
            .workspace_transient(workspace.id)
            .await
            .with_context(|| {
                format!(
                    "failed to open workspace {} while collecting turns by status",
                    workspace.id.0
                )
            })?;
        let turns = store
            .list_session_turns_by_statuses(statuses)
            .await
            .with_context(|| {
                format!(
                    "failed to list workspace {} turns by status",
                    workspace.id.0
                )
            });
        store.close().await;
        let mut turns = turns?;
        matching_turns.extend(turns.drain(..).map(|turn| (workspace.id, turn)));
    }
    Ok((workspace_count, matching_turns))
}

pub(in crate::daemon) async fn session_execution_environment_parts(
    stores: &StoreManager,
    cache: &mut HashMap<ctx_core::ids::SessionId, ExecutionEnvironment>,
    session_id: ctx_core::ids::SessionId,
) -> Result<ExecutionEnvironment> {
    if let Some(environment) = cache.get(&session_id).copied() {
        return Ok(environment);
    }
    let store = stores.store_for_session(session_id).await?;
    let session = store
        .get_session(session_id)
        .await?
        .with_context(|| format!("missing session {} for sandbox activity", session_id.0))?;
    let environment = session.execution_environment;
    cache.insert(session_id, environment);
    Ok(environment)
}
