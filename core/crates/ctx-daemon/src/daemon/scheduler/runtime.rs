use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::Mutex;

use ctx_core::models::Session;
use ctx_session_tools::order_seq::OrderSeqState;

use super::host::{TurnRuntimeHost, WorkerLifecycleHost};

mod event_loop;
mod execution_plan;
mod helpers;
mod provider_env;
mod provider_launch;
mod provider_setup;
mod provider_spawn;
mod running_turn;
#[cfg(test)]
mod tests;
mod tool_runtime;
mod turn_channels;
mod turn_context;
mod turn_failure;
mod turn_input;
mod turn_launch;
mod turn_start;

use self::provider_launch::prepare_provider_launch_environment;
use self::provider_setup::{prepare_provider_turn_runtime, ProviderTurnRuntimeSetupRequest};
use self::turn_context::{prepare_turn_runtime_context, TurnRuntimeContext};
use self::turn_input::prepare_turn_input;
use self::turn_launch::{launch_running_turn, TurnLaunchRequest};
use self::turn_start::{prepare_turn_start, PrepareTurnStartRequest};
use super::lifecycle::RunningTurn;
use super::QueuedMessage;

pub async fn start_turn(
    turn_runtime: &TurnRuntimeHost,
    lifecycle: &WorkerLifecycleHost,
    session: &Session,
    workdir: &Path,
    session_root_kind: &str,
    queued: QueuedMessage,
    order_seq_state: Arc<Mutex<OrderSeqState>>,
) -> Result<RunningTurn> {
    let provider_launch = turn_runtime.provider_launch_host();
    let TurnRuntimeContext {
        store,
        workdir_root,
        workdir_canonical,
        workdir_str,
        execution_environment,
        full_model_id,
    } = prepare_turn_runtime_context(turn_runtime, session, workdir).await?;

    let turn_start = prepare_turn_start(PrepareTurnStartRequest {
        turn_runtime,
        store: &store,
        session,
        workdir_str: &workdir_str,
        full_model_id: &full_model_id,
        execution_environment,
        session_root_kind,
        queued,
    })
    .await?;
    let message = turn_start.message;
    let message_id = turn_start.message_id;
    let perf_run_id = turn_start.perf_run_id;
    let run_id = turn_start.run_id;
    let turn_id = turn_start.turn_id;
    let provider_session_ref = turn_start.provider_session_ref;
    let context_window_metrics = turn_start.context_window_metrics;

    let provider_runtime = prepare_provider_turn_runtime(ProviderTurnRuntimeSetupRequest {
        turn_runtime,
        provider_launch,
        lifecycle,
        store: &store,
        session,
        run_id,
        turn_id,
        message_id,
        workdir_str: &workdir_str,
        full_model_id: &full_model_id,
        execution_environment,
        session_root_kind,
    })
    .await?;
    let mut provider_env = provider_runtime.provider_env;
    let runtime_provider_id = provider_runtime.runtime_provider_id;
    let adapter = provider_runtime.adapter;

    let turn_input =
        prepare_turn_input(&store, session, &message, &full_model_id, &mut provider_env).await?;
    let launch_environment = prepare_provider_launch_environment(
        provider_launch,
        session,
        &runtime_provider_id,
        workdir,
        &mut provider_env,
    )
    .await?;
    let mcp_token = launch_environment.mcp_token;
    let codex_home = launch_environment.codex_home;
    let start_deadline_duration = launch_environment.start_deadline_duration;

    launch_running_turn(TurnLaunchRequest {
        provider_launch,
        lifecycle,
        store: &store,
        session,
        adapter,
        turn_input,
        workdir,
        provider_env,
        perf_run_id: perf_run_id.clone(),
        run_id,
        turn_id,
        message_id,
        mcp_token,
        workdir_str: &workdir_str,
        full_model_id: &full_model_id,
        execution_environment,
        session_root_kind,
        workdir_root,
        workdir_canonical,
        provider_session_ref,
        codex_home,
        context_window_metrics,
        order_seq_state,
        start_deadline_duration,
    })
    .await
}
