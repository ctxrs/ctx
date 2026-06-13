use super::*;
#[path = "snapshot/events.rs"]
mod events;
#[path = "snapshot/head.rs"]
mod head;
#[path = "snapshot/history.rs"]
mod history;
#[path = "snapshot/state.rs"]
mod state;
#[path = "snapshot/vcs.rs"]
mod vcs;
pub(crate) use events::get_session_events;
pub(crate) use head::get_session_head;
pub(crate) use history::{get_session_history, list_session_turn_tools};
pub(crate) use state::get_session_state;
pub(crate) use vcs::{
    apply_session_diff_patch, get_session_diff, get_session_diff_summary, get_session_git_status,
};

pub(crate) async fn get_session_snapshot(
    State(state): State<SessionReadModelsHandle>,
    Path(id): Path<String>,
    Query(q): Query<SessionSnapshotRouteQuery>,
) -> Result<Json<SessionSnapshotRouteResponse>, StatusCode> {
    state
        .load_session_snapshot_for_route(SessionRouteParams::new(id), q)
        .await
        .map(Json)
        .map_err(session_read_model_status)
}
