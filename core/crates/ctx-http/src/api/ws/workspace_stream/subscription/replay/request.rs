use super::*;
use ctx_workspace_stream_service::replay::WorkspaceStreamReplayProgram;

pub(in crate::api::ws::workspace_stream::subscription) struct WorkspaceStreamReplayRequest<'a> {
    pub(in crate::api::ws::workspace_stream::subscription) state: &'a WorkspaceStreamHandle,
    pub(in crate::api::ws::workspace_stream::subscription) workspace_id: WorkspaceId,
    pub(in crate::api::ws::workspace_stream::subscription) runtime: &'a mut WorkspaceStreamRuntime,
    pub(in crate::api::ws::workspace_stream::subscription) labels: &'a WorkspaceStreamLabels,
    pub(in crate::api::ws::workspace_stream::subscription) live_rx:
        &'a mut tokio::sync::broadcast::Receiver<WorkspaceActiveSnapshotEvent>,
    pub(in crate::api::ws::workspace_stream::subscription) replay_program:
        WorkspaceStreamReplayProgram,
    pub(in crate::api::ws::workspace_stream::subscription) initial_deferred_live_events:
        Vec<WorkspaceActiveSnapshotEvent>,
}
