use super::*;

pub(super) async fn drop_buffered_session_events_at_or_before(
    state: &WorkspaceStreamHandle,
    runtime: &mut WorkspaceStreamRuntime,
    session_id: SessionId,
    replay_cursor: SessionReplayCursor,
) {
    runtime
        .foreground_head_buffer
        .drop_session_deltas_at_or_before(session_id, replay_cursor, |delta, cursor| {
            state.is_session_head_delta_after_cursor(delta, cursor)
        })
        .await;
    runtime
        .background_head_buffer
        .drop_session_deltas_at_or_before(session_id, replay_cursor, |delta, cursor| {
            state.is_session_head_delta_after_cursor(delta, cursor)
        })
        .await;
    runtime
        .summary_buffer
        .drop_session_events_at_or_before(session_id, replay_cursor, |delta, cursor| {
            state.is_session_summary_delta_after_cursor(delta, cursor)
        })
        .await;
}
