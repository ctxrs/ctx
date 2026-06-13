use super::collect::collect_turns_by_statuses_parts;
use super::types::{active_turn_record, DaemonTurnActivitySummary};
use super::*;
use ctx_store::{Store, StoreManager};
use ctx_update_service::UpdateDrainCoordinator;

pub async fn daemon_turn_activity_summary(
    state: &Arc<DaemonState>,
) -> Result<DaemonTurnActivitySummary> {
    daemon_turn_activity_summary_parts(
        state.global_store(),
        &state.core.stores,
        state.core.update_drain.as_ref(),
    )
    .await
}

pub(in crate::daemon) async fn daemon_turn_activity_summary_parts(
    global_store: &Store,
    stores: &StoreManager,
    update_drain: &UpdateDrainCoordinator,
) -> Result<DaemonTurnActivitySummary> {
    let (workspace_count, turns) = collect_turns_by_statuses_parts(
        global_store,
        stores,
        &[
            SessionTurnStatus::Queued,
            SessionTurnStatus::Starting,
            SessionTurnStatus::Running,
        ],
    )
    .await?;
    let queued_turn_count = turns
        .iter()
        .filter(|(_, turn)| matches!(&turn.status, SessionTurnStatus::Queued))
        .count();
    let running_turn_count = turns
        .iter()
        .filter(|(_, turn)| {
            matches!(
                &turn.status,
                SessionTurnStatus::Starting | SessionTurnStatus::Running
            )
        })
        .count();
    let records = turns
        .into_iter()
        .map(|(workspace_id, turn)| active_turn_record(workspace_id, turn))
        .collect::<Vec<_>>();
    let active_turn_count = running_turn_count;
    Ok(DaemonTurnActivitySummary {
        idle: running_turn_count == 0,
        active_turn_count,
        queued_turn_count,
        running_turn_count,
        scanned_workspace_count: workspace_count,
        turns: records,
        update_drain: update_drain.snapshot().await,
    })
}
