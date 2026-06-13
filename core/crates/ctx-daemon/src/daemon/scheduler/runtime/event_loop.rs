use super::*;
use crate::daemon::scheduler::host::TurnEventLoopHost;
use crate::daemon::scheduler::TurnStartProgress;
use ctx_core::ids::{MessageId, RunId, TurnId};
use ctx_core::models::Session;
use ctx_providers::events::NormalizedEvent;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Weak;
use std::time::Instant;
use tokio::sync::{mpsc, oneshot};

mod assistant;
mod dispatch;
mod driver;
mod failure;
mod processor;
mod provider_events;
mod state;
mod telemetry;
mod terminal;
mod tools;

pub(super) struct TurnEventLoop {
    pub(super) host_weak: Weak<TurnEventLoopHost>,
    pub(super) store: ctx_store::Store,
    pub(super) session_id: ctx_core::ids::SessionId,
    pub(super) task_id: ctx_core::ids::TaskId,
    pub(super) workspace_id: ctx_core::ids::WorkspaceId,
    pub(super) worktree_id: ctx_core::ids::WorktreeId,
    pub(super) provider_id: String,
    pub(super) model_id: String,
    pub(super) session_root_kind: String,
    pub(super) execution_environment_label: String,
    pub(super) perf_run_id: Option<String>,
    pub(super) workdir_root: PathBuf,
    pub(super) workdir_canonical: Option<PathBuf>,
    pub(super) workdir_str: String,
    pub(super) run_started_at: Instant,
    pub(super) run_id: RunId,
    pub(super) turn_id: TurnId,
    pub(super) message_id: MessageId,
    pub(super) provider_session_ref: Option<String>,
    pub(super) codex_home: Option<PathBuf>,
    pub(super) context_window_metrics: Option<Value>,
    pub(super) ev_rx: mpsc::Receiver<NormalizedEvent>,
    pub(super) events_done_tx: oneshot::Sender<()>,
    pub(super) start_progress_tx: tokio::sync::watch::Sender<TurnStartProgress>,
    pub(super) order_seq_state: Arc<Mutex<OrderSeqState>>,
}

impl TurnEventLoop {
    fn host(&self) -> Option<Arc<TurnEventLoopHost>> {
        self.host_weak.upgrade()
    }
}

pub(super) struct TurnEventLoopSpawnRequest<'a> {
    pub(super) host_weak: Weak<TurnEventLoopHost>,
    pub(super) store: ctx_store::Store,
    pub(super) session: &'a Session,
    pub(super) full_model_id: &'a str,
    pub(super) session_root_kind: &'a str,
    pub(super) execution_environment_label: &'a str,
    pub(super) perf_run_id: Option<String>,
    pub(super) workdir_root: PathBuf,
    pub(super) workdir_canonical: Option<PathBuf>,
    pub(super) workdir_str: String,
    pub(super) run_started_at: Instant,
    pub(super) run_id: RunId,
    pub(super) turn_id: TurnId,
    pub(super) message_id: MessageId,
    pub(super) provider_session_ref: Option<String>,
    pub(super) codex_home: Option<PathBuf>,
    pub(super) context_window_metrics: Option<Value>,
    pub(super) ev_rx: mpsc::Receiver<NormalizedEvent>,
    pub(super) events_done_tx: oneshot::Sender<()>,
    pub(super) start_progress_tx: tokio::sync::watch::Sender<TurnStartProgress>,
    pub(super) order_seq_state: Arc<Mutex<OrderSeqState>>,
}

pub(super) fn spawn_turn_event_loop_for_session(request: TurnEventLoopSpawnRequest<'_>) {
    spawn_turn_event_loop(TurnEventLoop {
        host_weak: request.host_weak,
        store: request.store,
        session_id: request.session.id,
        task_id: request.session.task_id,
        workspace_id: request.session.workspace_id,
        worktree_id: request.session.worktree_id,
        provider_id: request.session.provider_id.clone(),
        model_id: request.full_model_id.to_string(),
        session_root_kind: request.session_root_kind.to_string(),
        execution_environment_label: request.execution_environment_label.to_string(),
        perf_run_id: request.perf_run_id,
        workdir_root: request.workdir_root,
        workdir_canonical: request.workdir_canonical,
        workdir_str: request.workdir_str,
        run_started_at: request.run_started_at,
        run_id: request.run_id,
        turn_id: request.turn_id,
        message_id: request.message_id,
        provider_session_ref: request.provider_session_ref,
        codex_home: request.codex_home,
        context_window_metrics: request.context_window_metrics,
        ev_rx: request.ev_rx,
        events_done_tx: request.events_done_tx,
        start_progress_tx: request.start_progress_tx,
        order_seq_state: request.order_seq_state,
    });
}

pub(super) fn spawn_turn_event_loop(ctx: TurnEventLoop) {
    tokio::spawn(async move {
        run_turn_event_loop(ctx).await;
    });
}

async fn run_turn_event_loop(ctx: TurnEventLoop) {
    driver::run_turn_event_loop(ctx).await;
}

#[cfg(test)]
mod tests;
