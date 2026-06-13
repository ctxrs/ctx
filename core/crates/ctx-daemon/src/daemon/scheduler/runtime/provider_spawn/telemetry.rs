use std::time::Instant;

use anyhow::Error;
use ctx_core::ids::{MessageId, RunId, TurnId};
use ctx_core::models::{ExecutionEnvironment, Session};
use serde_json::json;

use crate::daemon::scheduler::host::{
    ProviderRunFailedOpsEvent, ProviderTurnLaunchHost, WorkerLifecycleHost,
};

use super::super::super::terminal::{finalize_failed_turn_with_host, FailedTurnTerminalization};

pub(super) async fn record_provider_spawn_metric(
    provider_launch: &ProviderTurnLaunchHost,
    perf_run_id: Option<String>,
    session: &Session,
    full_model_id: &str,
    execution_environment: ExecutionEnvironment,
    session_root_kind: &str,
    spawn_started_at: Instant,
) {
    provider_launch
        .record_provider_spawn_metric(
            perf_run_id,
            session,
            full_model_id,
            execution_environment,
            session_root_kind,
            spawn_started_at,
        )
        .await;
}

pub(super) struct ProviderStartFailure<'a> {
    pub(super) session: &'a Session,
    pub(super) run_id: RunId,
    pub(super) turn_id: TurnId,
    pub(super) message_id: MessageId,
    pub(super) mcp_token: Option<&'a str>,
    pub(super) run_started_at: Instant,
    pub(super) workdir_str: &'a str,
    pub(super) full_model_id: &'a str,
    pub(super) execution_environment: ExecutionEnvironment,
    pub(super) session_root_kind: &'a str,
    pub(super) err: &'a Error,
}

pub(super) async fn handle_provider_start_failure(
    provider_launch: &ProviderTurnLaunchHost,
    lifecycle: &WorkerLifecycleHost,
    failure: ProviderStartFailure<'_>,
) {
    let ProviderStartFailure {
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
        err,
    } = failure;
    if let Some(token) = mcp_token {
        lifecycle.revoke_turn_mcp_token_value(token).await;
    }
    let duration_ms = run_started_at.elapsed().as_millis() as u64;
    provider_launch
        .emit_provider_call_telemetry(
            session,
            full_model_id,
            execution_environment,
            session_root_kind,
            false,
            duration_ms,
        )
        .await;
    provider_launch.emit_provider_run_failed_event(ProviderRunFailedOpsEvent {
        session,
        run_id,
        turn_id,
        workdir_str,
        full_model_id,
        execution_environment,
        session_root_kind,
        err,
    });
    let error_message = err.to_string();
    let _ = finalize_failed_turn_with_host(
        lifecycle,
        session.id,
        Some(run_id),
        turn_id,
        message_id,
        FailedTurnTerminalization {
            message: &error_message,
            reason: Some("provider_start_failed"),
            details: None,
            kind: Some(json!("provider_start_failed")),
        },
    )
    .await;
}
