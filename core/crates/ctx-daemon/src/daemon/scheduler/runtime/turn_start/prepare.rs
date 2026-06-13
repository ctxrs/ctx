use anyhow::Result;
use chrono::Utc;
use ctx_core::ids::{MessageId, RunId, TurnId};
use ctx_core::models::{
    ExecutionEnvironment, Message, MessageDelivery, Session, SessionTurnStatus,
};

use crate::daemon::scheduler::host::TurnRuntimeHost;
use crate::daemon::scheduler::QueuedMessage;

use super::super::helpers::compute_context_window_metrics;
use super::events::{emit_provider_run_started_event, ProviderRunStartedEvent};
use super::metrics::record_queue_wait_metric;

pub(in crate::daemon::scheduler::runtime) struct PreparedTurnStart {
    pub(in crate::daemon::scheduler::runtime) message: Message,
    pub(in crate::daemon::scheduler::runtime) message_id: MessageId,
    pub(in crate::daemon::scheduler::runtime) perf_run_id: Option<String>,
    pub(in crate::daemon::scheduler::runtime) run_id: RunId,
    pub(in crate::daemon::scheduler::runtime) turn_id: TurnId,
    pub(in crate::daemon::scheduler::runtime) provider_session_ref: Option<String>,
    pub(in crate::daemon::scheduler::runtime) context_window_metrics: Option<serde_json::Value>,
}

pub(in crate::daemon::scheduler::runtime) struct PrepareTurnStartRequest<'a> {
    pub(in crate::daemon::scheduler::runtime) turn_runtime: &'a TurnRuntimeHost,
    pub(in crate::daemon::scheduler::runtime) store: &'a ctx_store::Store,
    pub(in crate::daemon::scheduler::runtime) session: &'a Session,
    pub(in crate::daemon::scheduler::runtime) workdir_str: &'a str,
    pub(in crate::daemon::scheduler::runtime) full_model_id: &'a str,
    pub(in crate::daemon::scheduler::runtime) execution_environment: ExecutionEnvironment,
    pub(in crate::daemon::scheduler::runtime) session_root_kind: &'a str,
    pub(in crate::daemon::scheduler::runtime) queued: QueuedMessage,
}

pub(in crate::daemon::scheduler::runtime) async fn prepare_turn_start(
    request: PrepareTurnStartRequest<'_>,
) -> Result<PreparedTurnStart> {
    let PrepareTurnStartRequest {
        turn_runtime,
        store,
        session,
        workdir_str,
        full_model_id,
        execution_environment,
        session_root_kind,
        queued,
    } = request;
    let QueuedMessage {
        mut message,
        enqueued_at,
        run_id: perf_run_id,
    } = queued;
    let message_id = message.id;
    let queue_wait_ms = enqueued_at.elapsed().as_millis() as u64;
    record_queue_wait_metric(
        turn_runtime,
        session,
        full_model_id,
        execution_environment.as_str(),
        session_root_kind,
        perf_run_id.clone(),
        queue_wait_ms,
    )
    .await;
    let run_id = message.run_id.get_or_insert_with(RunId::new).to_owned();
    let turn_id = message.turn_id.get_or_insert_with(TurnId::new).to_owned();

    emit_provider_run_started_event(ProviderRunStartedEvent {
        host: turn_runtime,
        session,
        run_id,
        turn_id,
        workdir_str,
        full_model_id,
        execution_environment: execution_environment.as_str(),
        session_root_kind,
    });

    if message.delivered_at.is_none() {
        store.mark_message_delivered(message.id).await?;
        message.delivery = MessageDelivery::Immediate;
        message.delivered_at = Some(Utc::now());
    }
    store
        .update_session_turn_status(
            session.id,
            turn_id,
            SessionTurnStatus::Starting,
            None,
            None,
            Utc::now(),
        )
        .await?;

    let context_window_metrics =
        compute_context_window_metrics(&session.provider_id, full_model_id, &message.content);

    Ok(PreparedTurnStart {
        message,
        message_id,
        perf_run_id,
        run_id,
        turn_id,
        provider_session_ref: session.provider_session_ref.clone(),
        context_window_metrics,
    })
}
