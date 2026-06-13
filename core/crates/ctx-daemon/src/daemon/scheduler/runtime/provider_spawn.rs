use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use tokio::sync::mpsc;

use ctx_core::ids::{MessageId, RunId, TurnId};
use ctx_core::models::{ExecutionEnvironment, Session};
use ctx_providers::adapters::{ProviderAdapter, RunHandle, TurnInput};
use ctx_providers::events::NormalizedEvent;
use ctx_store::Store;

use crate::daemon::scheduler::host::{ProviderTurnLaunchHost, WorkerLifecycleHost};

mod adapter;
mod hooks;
mod telemetry;

pub(super) use adapter::prepare_provider_adapter_for_turn;
use hooks::build_provider_run_hooks;
use telemetry::{
    handle_provider_start_failure, record_provider_spawn_metric, ProviderStartFailure,
};

pub(super) struct ProviderTurnSpawnRequest<'a> {
    pub(super) provider_launch: &'a ProviderTurnLaunchHost,
    pub(super) lifecycle: &'a WorkerLifecycleHost,
    pub(super) store: &'a Store,
    pub(super) session: &'a Session,
    pub(super) adapter: Arc<dyn ProviderAdapter>,
    pub(super) turn_input: TurnInput,
    pub(super) workdir: &'a Path,
    pub(super) provider_env: HashMap<String, String>,
    pub(super) event_tx: mpsc::Sender<NormalizedEvent>,
    pub(super) perf_run_id: Option<String>,
    pub(super) run_id: RunId,
    pub(super) turn_id: TurnId,
    pub(super) message_id: MessageId,
    pub(super) mcp_token: Option<&'a str>,
    pub(super) run_started_at: Instant,
    pub(super) workdir_str: &'a str,
    pub(super) full_model_id: &'a str,
    pub(super) execution_environment: ExecutionEnvironment,
    pub(super) session_root_kind: &'a str,
}

pub(super) async fn spawn_provider_turn(
    request: ProviderTurnSpawnRequest<'_>,
) -> Result<RunHandle> {
    let ProviderTurnSpawnRequest {
        provider_launch,
        lifecycle,
        store,
        session,
        adapter,
        turn_input,
        workdir,
        provider_env,
        event_tx,
        perf_run_id,
        run_id,
        turn_id,
        message_id,
        mcp_token,
        run_started_at,
        workdir_str,
        full_model_id,
        execution_environment,
        session_root_kind,
    } = request;
    let spawn_started_at = Instant::now();
    let provider_run_hooks = build_provider_run_hooks(
        provider_launch,
        store,
        session,
        execution_environment,
        session_root_kind,
    );
    let handle = match adapter
        .run(
            turn_input,
            workdir.to_path_buf(),
            provider_env,
            event_tx,
            provider_run_hooks,
        )
        .await
    {
        Ok(handle) => {
            record_provider_spawn_metric(
                provider_launch,
                perf_run_id,
                session,
                full_model_id,
                execution_environment,
                session_root_kind,
                spawn_started_at,
            )
            .await;
            handle
        }
        Err(err) => {
            handle_provider_start_failure(
                provider_launch,
                lifecycle,
                ProviderStartFailure {
                    session,
                    run_id,
                    turn_id,
                    message_id,
                    mcp_token,
                    run_started_at,
                    workdir_str,
                    full_model_id,
                    execution_environment,
                    session_root_kind,
                    err: &err,
                },
            )
            .await;
            return Err(err);
        }
    };

    Ok(handle)
}
