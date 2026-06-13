use serde_json::json;

use ctx_core::ids::{MessageId, RunId, TurnId};
use ctx_core::models::Session;

use crate::daemon::scheduler::host::WorkerLifecycleHost;
use crate::daemon::scheduler::terminal::{
    finalize_failed_turn_with_host, FailedTurnTerminalization,
};

pub(super) async fn emit_turn_start_failed(
    lifecycle: &WorkerLifecycleHost,
    session: &Session,
    run_id: RunId,
    turn_id: TurnId,
    message_id: MessageId,
    err: &anyhow::Error,
) {
    let error_message = err.to_string();
    let _ = finalize_failed_turn_with_host(
        lifecycle,
        session.id,
        Some(run_id),
        turn_id,
        message_id,
        FailedTurnTerminalization {
            message: &error_message,
            reason: Some("start_failed"),
            details: None,
            kind: Some(json!("start_failed")),
        },
    )
    .await;
}
