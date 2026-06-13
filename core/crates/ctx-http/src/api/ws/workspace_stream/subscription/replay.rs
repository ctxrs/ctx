use super::*;
use ctx_workspace_stream_service::replay::{
    WorkspaceStreamReplayStep, WorkspaceStreamSessionReplayOutcome,
};

mod buffers;
mod live_events;
mod request;
mod reset;
mod session;

use buffers::drop_buffered_session_events_at_or_before;
use live_events::flush_replay_ready_deferred_live_events;
pub(super) use live_events::{drain_live_events_blocking_pending_replay, replay_should_stop};
pub(super) use request::WorkspaceStreamReplayRequest;
use reset::queue_failed_replay_reset;
use session::replay_workspace_session;

pub(super) async fn replay_workspace_stream_subscriptions(
    request: WorkspaceStreamReplayRequest<'_>,
) -> Result<Option<HashMap<SessionId, SessionCursor>>, ()> {
    let WorkspaceStreamReplayRequest {
        state,
        workspace_id,
        runtime,
        labels,
        live_rx,
        replay_program,
        initial_deferred_live_events,
    } = request;
    let next_state = runtime.subscription_state.clone();

    let mut pending_replay_sessions = replay_program.pending_replay_sessions;
    let mut deferred_live_events = initial_deferred_live_events;
    let mut next_map = HashMap::new();
    for step in replay_program.steps {
        drain_live_events_blocking_pending_replay(
            state,
            workspace_id,
            live_rx,
            runtime,
            labels,
            &mut deferred_live_events,
            &pending_replay_sessions,
        )
        .await?;
        if replay_should_stop(runtime) {
            return Ok(None);
        }
        let (session_id, after_seq, after_projection_rev, replay_cursor) = match step {
            WorkspaceStreamReplayStep::HeadOnly { session_id, cursor } => {
                next_map.insert(session_id, SessionCursor { last_sent: cursor });
                continue;
            }
            WorkspaceStreamReplayStep::NoReplayRequired { session_id } => {
                pending_replay_sessions.remove(&session_id);
                flush_replay_ready_deferred_live_events(
                    state,
                    workspace_id,
                    runtime,
                    labels,
                    &mut deferred_live_events,
                    &pending_replay_sessions,
                )
                .await?;
                if replay_should_stop(runtime) {
                    return Ok(None);
                }
                continue;
            }
            WorkspaceStreamReplayStep::Replay {
                session_id,
                after_seq,
                after_projection_rev,
                replay_cursor,
            } => (session_id, after_seq, after_projection_rev, replay_cursor),
        };
        drop_buffered_session_events_at_or_before(state, runtime, session_id, replay_cursor).await;
        let replay = replay_workspace_session(
            state,
            workspace_id,
            session_id,
            replay_cursor,
            labels,
            &next_state,
            runtime,
        )
        .await;
        match replay {
            Ok(WorkspaceStreamSessionReplayOutcome::Replay { last_sent }) => {
                next_map.insert(session_id, SessionCursor { last_sent });
                pending_replay_sessions.remove(&session_id);
                flush_replay_ready_deferred_live_events(
                    state,
                    workspace_id,
                    runtime,
                    labels,
                    &mut deferred_live_events,
                    &pending_replay_sessions,
                )
                .await?;
                if replay_should_stop(runtime) {
                    return Ok(None);
                }
                drain_live_events_blocking_pending_replay(
                    state,
                    workspace_id,
                    live_rx,
                    runtime,
                    labels,
                    &mut deferred_live_events,
                    &pending_replay_sessions,
                )
                .await?;
                if replay_should_stop(runtime) {
                    return Ok(None);
                }
            }
            Ok(WorkspaceStreamSessionReplayOutcome::ResetRequired) | Err(_) => {
                queue_failed_replay_reset(
                    state,
                    workspace_id,
                    session_id,
                    after_seq,
                    after_projection_rev,
                    labels,
                    runtime,
                )
                .await?;
                return Ok(None);
            }
        };
    }
    flush_replay_ready_deferred_live_events(
        state,
        workspace_id,
        runtime,
        labels,
        &mut deferred_live_events,
        &pending_replay_sessions,
    )
    .await?;
    if replay_should_stop(runtime) {
        return Ok(None);
    }
    Ok(Some(next_map))
}
