use ctx_core::ids::SessionId;

use ctx_daemon::daemon::WorkspaceStreamHandle;

pub(in crate::api::ws) async fn release_workspace_stream_session_pins<I>(
    state: &WorkspaceStreamHandle,
    current: I,
) where
    I: IntoIterator<Item = SessionId>,
{
    state.release_workspace_stream_session_pins(current).await;
}
