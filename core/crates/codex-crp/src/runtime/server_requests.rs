use serde_json::Value;
use tracing::warn;

use crate::app_server::{AppServerInbound, AppServerRequestId};

use super::emit_unsupported_server_request_notice;
use super::io::CrpEventRouter;
use super::translate::translate_notification;
use super::AppServerSessionState;

pub(super) async fn handle_app_server_event(
    session_state: &mut AppServerSessionState,
    inbound: AppServerInbound,
    router: &CrpEventRouter,
) {
    match inbound {
        AppServerInbound::Notification { method, params } => {
            match translate_notification(session_state, &method, params) {
                Ok(events) => {
                    for (channel, event) in events {
                        super::io::dispatch_event(router, channel, event);
                    }
                }
                Err(err) => warn!(%method, %err, "failed to translate app-server notification"),
            }
        }
        AppServerInbound::Request { id, method, params } => {
            handle_server_request(session_state, router, id, &method, params).await;
        }
    }
}

async fn handle_server_request(
    session_state: &mut AppServerSessionState,
    router: &CrpEventRouter,
    id: AppServerRequestId,
    method: &str,
    params: Value,
) {
    let turn_id = params
        .get("turnId")
        .and_then(Value::as_str)
        .map(|turn_id| session_state.turn_aliases.ensure_crp_turn_id(turn_id));
    emit_unsupported_server_request_notice(
        router,
        &session_state.tracker.session_id,
        turn_id,
        "unsupported_server_request",
        method,
    );
    if let Err(err) = session_state
        .client
        .reject_request(
            id,
            format!("app-server request `{method}` is not supported by codex-crp"),
        )
        .await
    {
        warn!(?err, "failed to reject app-server request");
    }
}
