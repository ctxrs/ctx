use std::sync::Weak;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::Instant as TokioInstant;

use ctx_core::models::Session;

use super::host::SessionSchedulerWorkerHost;
use super::lifecycle::RunningTurn;
use super::SchedulerCommand;

mod bootstrap;
mod commands;
mod deadlines;
mod queue;

use self::bootstrap::{bootstrap_worker, WorkerBootstrap};
use self::commands::{handle_scheduler_command, SchedulerCommandAction};
use self::deadlines::{
    handle_inactivity_deadline_elapsed, handle_start_deadline_elapsed, refresh_inactivity_deadline,
    WorkerDeadlineAction,
};
use self::queue::{start_next_queued_turn, QueueStartContext, QueueStartOutcome};

pub(super) async fn session_worker(
    host_weak: Weak<SessionSchedulerWorkerHost>,
    session: Session,
    mut rx: mpsc::Receiver<SchedulerCommand>,
) {
    // Keep only a weak host reference so background workers do not keep daemon
    // assembly state alive after tests or shutdown drop the owner.
    let Some(host) = host_weak.upgrade() else {
        return;
    };
    let mut session = session;
    let Some(bootstrap) = bootstrap_worker(&host, &session).await else {
        return;
    };
    let WorkerBootstrap {
        store,
        order_seq_state,
        mut queue,
        mut event_head_rx,
        workdir,
        session_root_kind,
    } = bootstrap;
    let mut running: Option<RunningTurn> = None;
    let mut running_inactivity_timeout: Option<Duration> = None;
    let mut running_inactivity_deadline: Option<TokioInstant> = None;
    let mut running_start_deadline: Option<TokioInstant> = None;
    let mut suspend_queue = false;
    drop(host);

    loop {
        if running.is_none() && !suspend_queue {
            match start_next_queued_turn(QueueStartContext {
                host_weak: &host_weak,
                session: &mut session,
                store: &store,
                queue: &mut queue,
                workdir: &workdir,
                session_root_kind: &session_root_kind,
                order_seq_state: &order_seq_state,
                running: &mut running,
                running_inactivity_timeout: &mut running_inactivity_timeout,
                running_inactivity_deadline: &mut running_inactivity_deadline,
                running_start_deadline: &mut running_start_deadline,
            })
            .await
            {
                QueueStartOutcome::Idle => {}
                QueueStartOutcome::StartedOrFailed => continue,
                QueueStartOutcome::StopWorker => break,
            }
        }

        tokio::select! {
            cmd = rx.recv() => {
                if matches!(
                    handle_scheduler_command(
                        cmd,
                        &host_weak,
                        session.id,
                        &mut queue,
                        &mut running,
                        &mut running_start_deadline,
                        &mut suspend_queue,
                    )
                    .await,
                    SchedulerCommandAction::Break
                ) {
                    break;
                }
            }
            _ = async {
                if let Some(turn) = running.as_mut() {
                    let _ = (&mut turn.handle.done).await;
                }
            }, if running.is_some() => {
                if let Some(turn) = running.take() {
                    running_start_deadline = None;
                    let Some(host) = host_weak.upgrade() else {
                        break;
                    };
                    let Some(finalized) = host.handle_provider_exit(session.id, turn).await else {
                        break;
                    };
                    suspend_queue = !finalized;
                    if !host.set_running(session.id, false).await {
                        break;
                    }
                } else if let Some(host) = host_weak.upgrade() {
                    if !host.set_running(session.id, false).await {
                        break;
                    }
                } else {
                    break;
                }
            }
            changed = event_head_rx.changed(), if running.is_some() => {
                let Some(host) = host_weak.upgrade() else {
                    break;
                };
                if changed.is_err() {
                    event_head_rx = host.subscribe_session_event_head(session.id).await;
                }
                refresh_inactivity_deadline(
                    running_inactivity_timeout,
                    &mut running_inactivity_deadline,
                );
            }
            _ = async {
                if let Some(deadline) = running_start_deadline {
                    tokio::time::sleep_until(deadline).await;
                }
            }, if running.is_some() && running_start_deadline.is_some() => {
                if matches!(
                    handle_start_deadline_elapsed(
                        &host_weak,
                        session.id,
                        &mut running,
                        &mut running_start_deadline,
                    )
                    .await,
                    WorkerDeadlineAction::Break
                ) {
                    break;
                }
            }
            _ = async {
                if let Some(deadline) = running_inactivity_deadline {
                    tokio::time::sleep_until(deadline).await;
                }
            }, if running.is_some() && running_inactivity_deadline.is_some() => {
                if matches!(
                    handle_inactivity_deadline_elapsed(
                        &host_weak,
                        session.id,
                        &mut running,
                        &mut running_start_deadline,
                        &mut suspend_queue,
                    )
                    .await,
                    WorkerDeadlineAction::Break
                ) {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Weak;
    use std::time::Duration;

    use chrono::Utc;
    use tokio::sync::mpsc;

    use ctx_core::ids::{SessionId, TaskId, WorkspaceId, WorktreeId};
    use ctx_core::models::{ExecutionEnvironment, Session, SessionStatus};

    use super::*;
    use crate::daemon::scheduler::host::SessionSchedulerWorkerHost;

    fn test_session() -> Session {
        let now = Utc::now();
        Session {
            id: SessionId::new(),
            task_id: TaskId::new(),
            workspace_id: WorkspaceId::new(),
            worktree_id: WorktreeId::new(),
            execution_environment: ExecutionEnvironment::Host,
            parent_session_id: None,
            relationship: None,
            provider_id: "fake".to_string(),
            model_id: "fake-model".to_string(),
            reasoning_effort: None,
            title: "test".to_string(),
            agent_role: "default".to_string(),
            status: SessionStatus::Active,
            provider_session_ref: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[tokio::test]
    async fn session_worker_exits_when_owner_host_is_dropped() {
        let (_tx, rx) = mpsc::channel(1);

        tokio::time::timeout(
            Duration::from_secs(1),
            session_worker(
                Weak::<SessionSchedulerWorkerHost>::new(),
                test_session(),
                rx,
            ),
        )
        .await
        .expect("worker should exit when its weak owner host cannot upgrade");
    }
}
