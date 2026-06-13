use std::collections::VecDeque;
use std::sync::Weak;

use tokio::time::Instant as TokioInstant;

use ctx_core::ids::SessionId;
use ctx_core::models::MessageDelivery;
use ctx_session_tools::interrupt_telemetry::InterruptTelemetryContext;

use crate::daemon::scheduler::host::SessionSchedulerWorkerHost;
use crate::daemon::scheduler::lifecycle::{RunningTurn, StopReason};
use crate::daemon::scheduler::{QueuedMessage, SchedulerCommand};

pub(super) enum SchedulerCommandAction {
    Continue,
    Break,
}

pub(super) async fn handle_scheduler_command(
    cmd: Option<SchedulerCommand>,
    host_weak: &Weak<SessionSchedulerWorkerHost>,
    session_id: SessionId,
    queue: &mut VecDeque<QueuedMessage>,
    running: &mut Option<RunningTurn>,
    running_start_deadline: &mut Option<TokioInstant>,
    suspend_queue: &mut bool,
) -> SchedulerCommandAction {
    match cmd {
        Some(SchedulerCommand::Enqueue(msg)) => {
            enqueue_message(queue, running, suspend_queue, msg);
            SchedulerCommandAction::Continue
        }
        Some(SchedulerCommand::RemoveQueued(id)) => {
            queue.retain(|message| message.message.id != id);
            SchedulerCommandAction::Continue
        }
        Some(SchedulerCommand::Cancel) => {
            stop_running_for_command(
                host_weak,
                session_id,
                running,
                running_start_deadline,
                suspend_queue,
                StopReason::Cancel,
                None,
            )
            .await
        }
        Some(SchedulerCommand::Interrupt(interrupt)) => {
            stop_running_for_command(
                host_weak,
                session_id,
                running,
                running_start_deadline,
                suspend_queue,
                StopReason::Interrupt,
                Some(interrupt),
            )
            .await
        }
        Some(SchedulerCommand::StorageEmergency) => {
            stop_running_for_command(
                host_weak,
                session_id,
                running,
                running_start_deadline,
                suspend_queue,
                StopReason::StorageEmergency,
                None,
            )
            .await
        }
        None => SchedulerCommandAction::Break,
    }
}

fn enqueue_message(
    queue: &mut VecDeque<QueuedMessage>,
    running: &Option<RunningTurn>,
    suspend_queue: &mut bool,
    msg: QueuedMessage,
) {
    if running.is_some() {
        queue.push_back(msg);
        return;
    }
    if matches!(msg.message.delivery, MessageDelivery::Immediate) {
        *suspend_queue = false;
    }
    queue.push_front(msg);
}

async fn stop_running_for_command(
    host_weak: &Weak<SessionSchedulerWorkerHost>,
    session_id: SessionId,
    running: &mut Option<RunningTurn>,
    running_start_deadline: &mut Option<TokioInstant>,
    suspend_queue: &mut bool,
    reason: StopReason,
    interrupt: Option<InterruptTelemetryContext>,
) -> SchedulerCommandAction {
    let Some(turn) = running.take() else {
        return SchedulerCommandAction::Continue;
    };
    *running_start_deadline = None;
    let Some(host) = host_weak.upgrade() else {
        return SchedulerCommandAction::Break;
    };
    let Some(finalized) = host
        .stop_running_turn(session_id, turn, reason, interrupt)
        .await
    else {
        return SchedulerCommandAction::Break;
    };
    *suspend_queue = finalized;
    SchedulerCommandAction::Continue
}
