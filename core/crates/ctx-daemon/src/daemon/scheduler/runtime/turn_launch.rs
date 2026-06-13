use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::Value;
use tokio::sync::Mutex;

use ctx_core::ids::{MessageId, RunId, TurnId};
use ctx_core::models::{ExecutionEnvironment, Session};
use ctx_providers::adapters::{ProviderAdapter, TurnInput};
use ctx_session_tools::order_seq::OrderSeqState;
use ctx_store::Store;

use super::event_loop::{spawn_turn_event_loop_for_session, TurnEventLoopSpawnRequest};
use super::provider_spawn::{spawn_provider_turn, ProviderTurnSpawnRequest};
use super::running_turn::{build_running_turn, RunningTurnParts};
use super::turn_channels::TurnRuntimeChannels;
use crate::daemon::scheduler::host::{ProviderTurnLaunchHost, WorkerLifecycleHost};
use crate::daemon::scheduler::lifecycle::RunningTurn;

pub(super) struct TurnLaunchRequest<'a> {
    pub(super) provider_launch: &'a ProviderTurnLaunchHost,
    pub(super) lifecycle: &'a WorkerLifecycleHost,
    pub(super) store: &'a Store,
    pub(super) session: &'a Session,
    pub(super) adapter: Arc<dyn ProviderAdapter>,
    pub(super) turn_input: TurnInput,
    pub(super) workdir: &'a Path,
    pub(super) provider_env: HashMap<String, String>,
    pub(super) perf_run_id: Option<String>,
    pub(super) run_id: RunId,
    pub(super) turn_id: TurnId,
    pub(super) message_id: MessageId,
    pub(super) mcp_token: Option<String>,
    pub(super) workdir_str: &'a str,
    pub(super) full_model_id: &'a str,
    pub(super) execution_environment: ExecutionEnvironment,
    pub(super) session_root_kind: &'a str,
    pub(super) workdir_root: PathBuf,
    pub(super) workdir_canonical: Option<PathBuf>,
    pub(super) provider_session_ref: Option<String>,
    pub(super) codex_home: Option<PathBuf>,
    pub(super) context_window_metrics: Option<Value>,
    pub(super) order_seq_state: Arc<Mutex<OrderSeqState>>,
    pub(super) start_deadline_duration: Duration,
}

pub(super) async fn launch_running_turn(request: TurnLaunchRequest<'_>) -> Result<RunningTurn> {
    let TurnLaunchRequest {
        provider_launch,
        lifecycle,
        store,
        session,
        adapter,
        turn_input,
        workdir,
        provider_env,
        perf_run_id,
        run_id,
        turn_id,
        message_id,
        mcp_token,
        workdir_str,
        full_model_id,
        execution_environment,
        session_root_kind,
        workdir_root,
        workdir_canonical,
        provider_session_ref,
        codex_home,
        context_window_metrics,
        order_seq_state,
        start_deadline_duration,
    } = request;

    let TurnRuntimeChannels {
        ev_tx,
        ev_rx,
        events_done_tx,
        events_done_rx,
        start_progress_tx,
        start_progress_rx,
    } = TurnRuntimeChannels::new();
    let event_tx = ev_tx.clone();
    let run_started_at = Instant::now();

    let handle = spawn_provider_turn(ProviderTurnSpawnRequest {
        provider_launch,
        lifecycle,
        store,
        session,
        adapter: Arc::clone(&adapter),
        turn_input,
        workdir,
        provider_env,
        event_tx: ev_tx,
        perf_run_id: perf_run_id.clone(),
        run_id,
        turn_id,
        message_id,
        mcp_token: mcp_token.as_deref(),
        run_started_at,
        workdir_str,
        full_model_id,
        execution_environment,
        session_root_kind,
    })
    .await?;

    spawn_turn_event_loop_for_session(TurnEventLoopSpawnRequest {
        host_weak: provider_launch.event_loop_host_weak(),
        store: store.clone(),
        session,
        full_model_id,
        session_root_kind,
        execution_environment_label: execution_environment.as_str(),
        perf_run_id,
        workdir_root,
        workdir_canonical,
        workdir_str: workdir_str.to_string(),
        run_started_at,
        run_id,
        turn_id,
        message_id,
        provider_session_ref,
        codex_home,
        context_window_metrics,
        ev_rx,
        events_done_tx,
        start_progress_tx,
        order_seq_state,
    });

    Ok(build_running_turn(RunningTurnParts {
        adapter,
        handle,
        run_id,
        turn_id,
        message_id,
        session,
        full_model_id,
        execution_environment_label: execution_environment.as_str(),
        session_root_kind,
        event_tx,
        events_done_rx,
        start_progress_rx,
        start_deadline_duration,
        mcp_token,
    }))
}
