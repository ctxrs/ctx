use std::sync::Weak;
use std::time::Duration;

use tokio::time::Instant as TokioInstant;

use ctx_core::ids::SessionId;

use crate::daemon::scheduler::host::SessionSchedulerWorkerHost;
use crate::daemon::scheduler::lifecycle::{RunningTurn, TurnStartProgress};

pub(super) enum WorkerDeadlineAction {
    Continue,
    Break,
}

pub(super) fn refresh_inactivity_deadline(
    timeout: Option<Duration>,
    running_inactivity_deadline: &mut Option<TokioInstant>,
) {
    if let Some(timeout) = timeout {
        *running_inactivity_deadline = Some(TokioInstant::now() + timeout);
    }
}

pub(super) async fn handle_start_deadline_elapsed(
    host_weak: &Weak<SessionSchedulerWorkerHost>,
    session_id: SessionId,
    running: &mut Option<RunningTurn>,
    running_start_deadline: &mut Option<TokioInstant>,
) -> WorkerDeadlineAction {
    let start_still_pending = running
        .as_ref()
        .is_some_and(|turn| *turn.start_progress.borrow() == TurnStartProgress::Pending);
    *running_start_deadline = None;

    if !start_still_pending {
        return WorkerDeadlineAction::Continue;
    }

    let Some(turn) = running.take() else {
        return clear_running_or_break(host_weak, session_id).await;
    };
    let Some(host) = host_weak.upgrade() else {
        return WorkerDeadlineAction::Break;
    };
    if !host
        .fail_starting_turn(
            session_id,
            turn,
            "provider did not report turn start before deadline",
        )
        .await
    {
        return WorkerDeadlineAction::Break;
    }
    if !host.set_running(session_id, false).await {
        return WorkerDeadlineAction::Break;
    }
    WorkerDeadlineAction::Continue
}

pub(super) async fn handle_inactivity_deadline_elapsed(
    host_weak: &Weak<SessionSchedulerWorkerHost>,
    session_id: SessionId,
    running: &mut Option<RunningTurn>,
    running_start_deadline: &mut Option<TokioInstant>,
    suspend_queue: &mut bool,
) -> WorkerDeadlineAction {
    let Some(turn) = running.take() else {
        return clear_running_or_break(host_weak, session_id).await;
    };
    *running_start_deadline = None;
    let Some(host) = host_weak.upgrade() else {
        return WorkerDeadlineAction::Break;
    };
    let Some(finalized) = host.handle_provider_stall(session_id, turn).await else {
        return WorkerDeadlineAction::Break;
    };
    *suspend_queue = !finalized;
    if !host.set_running(session_id, false).await {
        return WorkerDeadlineAction::Break;
    }
    WorkerDeadlineAction::Continue
}

async fn clear_running_or_break(
    host_weak: &Weak<SessionSchedulerWorkerHost>,
    session_id: SessionId,
) -> WorkerDeadlineAction {
    let Some(host) = host_weak.upgrade() else {
        return WorkerDeadlineAction::Break;
    };
    if !host.set_running(session_id, false).await {
        return WorkerDeadlineAction::Break;
    }
    WorkerDeadlineAction::Continue
}
