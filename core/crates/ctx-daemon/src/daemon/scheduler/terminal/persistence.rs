use anyhow::Result;
use serde_json::Value;

use ctx_core::ids::{RunId, SessionId, TurnId};
use ctx_core::models::{RunStatus, SessionEvent, SessionEventType};

use super::super::persistence::SchedulerPersistenceHost;
use super::super::persistence::{
    is_transient_store_error, sleep_store_write_retry, STORE_WRITE_RETRY_LIMIT,
};

async fn publish_persisted_events<H>(host: &H, events: Vec<SessionEvent>)
where
    H: SchedulerPersistenceHost + ?Sized,
{
    for event in events {
        host.publish_event(event).await;
    }
}

async fn cleanup_turn_stream_state(
    store: &ctx_store::Store,
    session_id: SessionId,
    turn_id: TurnId,
    event_types: &[SessionEventType],
) {
    if event_types.is_empty() {
        return;
    }
    if let Err(err) = store
        .delete_session_events_for_turn_types(session_id, turn_id, event_types)
        .await
    {
        tracing::warn!(
            session_id = %session_id.0,
            turn_id = %turn_id.0,
            "failed to delete transient turn events before terminalization: {err:#}"
        );
    }
}

async fn persist_turn_terminal_events_with_retry(
    store: &ctx_store::Store,
    session_id: SessionId,
    run_id: Option<RunId>,
    turn_id: TurnId,
    events: &[(SessionEventType, Value)],
) -> Result<Vec<SessionEvent>> {
    let mut attempt = 0usize;
    loop {
        match store
            .persist_turn_terminal_events(session_id, run_id, turn_id, events.to_vec())
            .await
        {
            Ok(persisted) => return Ok(persisted),
            Err(err) => {
                if !is_transient_store_error(&err) || attempt >= STORE_WRITE_RETRY_LIMIT {
                    return Err(err);
                }
                attempt += 1;
                sleep_store_write_retry(attempt).await;
            }
        }
    }
}

pub(super) async fn persist_terminal_events_with_host<H>(
    host: &H,
    session_id: SessionId,
    run_id: Option<RunId>,
    turn_id: TurnId,
    run_status: RunStatus,
    cleanup_types: &[SessionEventType],
    events: Vec<(SessionEventType, Value)>,
) -> Result<()>
where
    H: SchedulerPersistenceHost + ?Sized,
{
    let store = host.store_for_session(session_id).await?;
    cleanup_turn_stream_state(&store, session_id, turn_id, cleanup_types).await;
    let persisted =
        persist_turn_terminal_events_with_retry(&store, session_id, run_id, turn_id, &events)
            .await?;
    ctx_org_policy::admission::update_run_terminal_status(&store, run_id, run_status).await;
    publish_persisted_events(host, persisted).await;
    Ok(())
}
