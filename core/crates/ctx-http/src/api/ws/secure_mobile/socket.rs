use axum::extract::ws::{Message as WsMessage, WebSocket};
use futures::StreamExt;

use super::super::workspace_stream;
use super::context::decode_mobile_secure_client_message;
use ctx_core::ids::WorkspaceId;
use ctx_daemon::daemon::WorkspaceStreamHandle;
use ctx_mobile_access_service::route_contract::MobileSecureStreamContext;

pub(super) async fn handle_mobile_secure_ws(
    socket: WebSocket,
    workspace_stream: WorkspaceStreamHandle,
    workspace_id: WorkspaceId,
    stream_context: MobileSecureStreamContext,
) -> Result<(), anyhow::Error> {
    let (sender, mut receiver) = socket.split();

    let labels = workspace_stream::WorkspaceStreamLabels {
        ready_queue_label: "ready_secure",
        subscribe_resolution_log: "workspace stream subscribe resolution failed (secure)",
        replay_list_metric: "ctx_http.replay_session_events_secure.list",
        replay_send_metric: None,
        replay_queue_label: "replay_secure",
        replay_failure_log: "workspace stream replay failed (secure)",
        lagged_log: "workspace stream lagged (secure)",
        event_queue_label: "event_secure",
    };
    let Some((mut runtime, mut rx)) = workspace_stream::initialize_workspace_stream(
        &workspace_stream,
        workspace_id,
        labels.ready_queue_label,
    )
    .await
    else {
        return Ok(());
    };

    let send_task = super::send_loop::spawn_mobile_secure_send_loop(
        sender,
        workspace_id,
        stream_context.device_id.clone(),
        stream_context.key.clone(),
        &runtime,
    );

    let recv_loop = async {
        loop {
            tokio::select! {
                msg = receiver.next() => {
                    match msg {
                        Some(Ok(WsMessage::Text(text))) => {
                            let Some(message) =
                                decode_mobile_secure_client_message(&stream_context, &text)?
                            else {
                                continue;
                            };
                                if workspace_stream::handle_workspace_stream_subscription(
                                    &workspace_stream,
                                workspace_id,
                                message,
                                &mut rx,
                                &mut runtime,
                                &labels,
                            )
                            .await
                            .is_err()
                            {
                                break;
                            }
                        }
                        Some(Ok(WsMessage::Close(_))) => break,
                        Some(Ok(_)) => {}
                        Some(Err(_)) => break,
                        None => break,
                    }
                }
                event = rx.recv() => {
                    match event {
                        Ok(event) => {
                            let burst = workspace_stream::take_workspace_stream_receiver_burst(&mut rx, event);
                            if workspace_stream::handle_workspace_stream_receiver_burst(
                                &workspace_stream,
                                workspace_id,
                                burst,
                                &mut runtime,
                                &labels,
                            )
                            .await
                            .is_err()
                            {
                                break;
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(lagged)) => {
                            if workspace_stream::handle_workspace_stream_lagged(
                                &workspace_stream,
                                workspace_id,
                                lagged,
                                &mut runtime,
                                &labels,
                            )
                            .await
                            .is_err()
                            {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            }
        }
        Ok::<(), anyhow::Error>(())
    };

    let (send_task, recv_result) =
        super::super::async_util::race_join_handle(send_task, recv_loop).await;

    workspace_stream::notify_workspace_stream_shutdown(&runtime).await;
    if let Some(send_task) = send_task {
        let _ = send_task.await;
    }
    workspace_stream::release_workspace_stream(&workspace_stream, &runtime).await;

    recv_result.unwrap_or(Ok(()))
}
