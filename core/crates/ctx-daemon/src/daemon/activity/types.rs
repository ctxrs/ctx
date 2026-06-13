use super::*;
pub use ctx_update_service::route_contract::{
    ActiveTurnRecord, DaemonSandboxWorkActivitySummary, DaemonTurnActivitySummary,
};

pub(super) fn active_turn_record(workspace_id: WorkspaceId, turn: SessionTurn) -> ActiveTurnRecord {
    ActiveTurnRecord {
        workspace_id: workspace_id.0.to_string(),
        session_id: turn.session_id.0.to_string(),
        run_id: turn.run_id.map(|run_id| run_id.0.to_string()),
        turn_id: turn.turn_id.0.to_string(),
        status: turn_status_name(&turn.status).to_string(),
    }
}

fn turn_status_name(status: &SessionTurnStatus) -> &'static str {
    match status {
        SessionTurnStatus::Queued => "queued",
        SessionTurnStatus::Starting => "starting",
        SessionTurnStatus::Running => "running",
        SessionTurnStatus::Completed => "completed",
        SessionTurnStatus::Failed => "failed",
        SessionTurnStatus::Interrupted => "interrupted",
    }
}
