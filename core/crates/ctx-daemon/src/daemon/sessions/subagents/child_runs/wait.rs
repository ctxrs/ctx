use std::sync::{Arc, Weak};
use std::time::Instant;

use ctx_core::ids::{RunId, SessionId};
use ctx_core::models::SessionTurn;
use ctx_observability::logs;
use ctx_session_runtime::runtime::SessionRuntime;
use tokio::sync::watch;

use crate::daemon::scheduler::SchedulerCommand;

use super::status::subagent_terminal_status_from_turn_status;

async fn latest_terminal_turn_for_run(
    store: &ctx_store::Store,
    session_id: SessionId,
    run_id: RunId,
) -> Result<Option<SessionTurn>, String> {
    let turn = store
        .get_latest_turn_for_run(session_id, run_id)
        .await
        .map_err(|error| logs::redact_sensitive(&error.to_string()))?;
    Ok(turn.and_then(|turn| {
        subagent_terminal_status_from_turn_status(turn.status.clone()).map(|_| turn)
    }))
}

#[derive(Clone)]
pub(in crate::daemon) struct SessionEventHeadSubscriber {
    runtime: Weak<SessionRuntime<SchedulerCommand>>,
}

impl SessionEventHeadSubscriber {
    pub(in crate::daemon) fn new(runtime: Weak<SessionRuntime<SchedulerCommand>>) -> Self {
        Self { runtime }
    }

    pub(super) fn runtime(&self) -> Option<Arc<SessionRuntime<SchedulerCommand>>> {
        self.runtime.upgrade()
    }

    pub(super) async fn subscribe(&self, session_id: SessionId) -> Option<watch::Receiver<i64>> {
        let runtime = self.runtime.upgrade()?;
        Some(runtime.subscribe_session_event_head(session_id).await)
    }
}

pub(in crate::daemon::sessions::subagents) async fn wait_for_run_terminal_turn(
    event_heads: &SessionEventHeadSubscriber,
    store: &ctx_store::Store,
    session_id: SessionId,
    run_id: RunId,
) -> Result<Option<SessionTurn>, String> {
    if let Some(turn) = latest_terminal_turn_for_run(store, session_id, run_id).await? {
        return Ok(Some(turn));
    }
    let Some(mut rx) = event_heads.subscribe(session_id).await else {
        return Ok(None);
    };

    loop {
        tokio::select! {
            changed = rx.changed() => {
                if changed.is_err() {
                    let Some(next) = event_heads.subscribe(session_id).await else {
                        return Ok(None);
                    };
                    rx = next;
                }
                if let Some(turn) = latest_terminal_turn_for_run(store, session_id, run_id).await?
                {
                    return Ok(Some(turn));
                }
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(250)) => {
                if let Some(turn) = latest_terminal_turn_for_run(store, session_id, run_id).await?
                {
                    return Ok(Some(turn));
                }
            }
        }
    }
}

pub(in crate::daemon) async fn wait_for_run_assistant_message_in_store(
    store: &ctx_store::Store,
    session_id: SessionId,
    run_id: RunId,
) -> Result<Option<String>, String> {
    let deadline = Instant::now() + std::time::Duration::from_secs(2);

    loop {
        if let Some(message) = store
            .get_last_assistant_message_for_run(session_id, run_id)
            .await
            .map_err(|error| logs::redact_sensitive(&error.to_string()))?
        {
            return Ok(Some(message.content));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}
