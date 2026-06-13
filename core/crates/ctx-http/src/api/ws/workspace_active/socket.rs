use axum::extract::ws::{Message as WsMessage, WebSocket};
use futures::StreamExt;

use super::super::workspace_stream;
use ctx_core::ids::WorkspaceId;
use ctx_core::models::WorkspaceActiveSnapshotClientMessage;
use ctx_daemon::daemon::WorkspaceStreamHandle;

pub(super) async fn handle_workspace_active_snapshot_ws(
    socket: WebSocket,
    state: WorkspaceStreamHandle,
    workspace_id: WorkspaceId,
) {
    let (sender, mut receiver) = socket.split();
    let labels = workspace_stream::WorkspaceStreamLabels {
        ready_queue_label: "ready",
        subscribe_resolution_log: "workspace stream subscribe resolution failed",
        replay_list_metric: "ctx_http.replay_session_events_active.list",
        replay_send_metric: Some("ctx_http.replay_session_events_active.send"),
        replay_queue_label: "replay",
        replay_failure_log: "workspace stream replay failed",
        lagged_log: "workspace stream lagged",
        event_queue_label: "event",
    };
    let Some((mut runtime, mut rx)) = workspace_stream::initialize_workspace_stream(
        &state,
        workspace_id,
        labels.ready_queue_label,
    )
    .await
    else {
        return;
    };

    let send_task =
        super::send_loop::spawn_workspace_active_send_loop(sender, workspace_id, &runtime);
    let recv_loop = async {
        loop {
            tokio::select! {
                msg = receiver.next() => {
                    match msg {
                        Some(Ok(WsMessage::Text(text))) => {
                            if let Ok(message) =
                                serde_json::from_str::<WorkspaceActiveSnapshotClientMessage>(&text)
                            {
                                if workspace_stream::handle_workspace_stream_subscription(
                                    &state,
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
                        }
                        Some(Ok(WsMessage::Binary(bytes))) => {
                            if let Ok(text) = String::from_utf8(bytes.to_vec()) {
                                if let Ok(message) =
                                    serde_json::from_str::<WorkspaceActiveSnapshotClientMessage>(&text)
                                {
                                    if workspace_stream::handle_workspace_stream_subscription(
                                        &state,
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
                            }
                        }
                        Some(Ok(WsMessage::Close(_))) => break,
                        Some(Ok(_)) => {}
                        Some(Err(_)) => break,
                        None => break,
                    }
                }
                event = rx.recv() => {
                    let event = match event {
                        Ok(event) => event,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(lagged)) => {
                            if workspace_stream::handle_workspace_stream_lagged(
                                &state,
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
                            continue;
                        }
                        Err(_) => break,
                    };
                    let burst = workspace_stream::take_workspace_stream_receiver_burst(&mut rx, event);
                    if workspace_stream::handle_workspace_stream_receiver_burst(
                        &state,
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
            }
        }
        Ok::<(), ()>(())
    };

    let (send_task, _recv_result) =
        super::super::async_util::race_join_handle(send_task, recv_loop).await;

    workspace_stream::notify_workspace_stream_shutdown(&runtime).await;
    if let Some(send_task) = send_task {
        let _ = send_task.await;
    }

    workspace_stream::release_workspace_stream(&state, &runtime).await;
}
