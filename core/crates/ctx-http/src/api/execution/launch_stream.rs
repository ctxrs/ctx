use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::broadcast;

use ctx_execution_runtime::{
    ExecutionLaunchSnapshot, ExecutionLaunchState, ExecutionLaunchStreamEvent,
};

use ctx_daemon::daemon::ExecutionLaunchHandle;

#[derive(Debug, Deserialize)]
pub(in crate::api) struct ExecutionLaunchStatusQuery {
    job_id: String,
}

pub(in crate::api) async fn launch_status(
    State(state): State<ExecutionLaunchHandle>,
    Query(query): Query<ExecutionLaunchStatusQuery>,
) -> Result<Json<ExecutionLaunchSnapshot>, StatusCode> {
    let job_id = query.job_id.trim();
    if job_id.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let snapshot = state
        .launch_status(job_id)
        .await
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(snapshot))
}

pub(in crate::api) async fn launch_stream_ws(
    ws: WebSocketUpgrade,
    State(state): State<ExecutionLaunchHandle>,
    Query(query): Query<ExecutionLaunchStatusQuery>,
) -> impl IntoResponse {
    let job_id = query.job_id.trim().to_string();
    if job_id.is_empty() {
        return StatusCode::BAD_REQUEST.into_response();
    }

    let Some((snapshot, rx)) = state.subscribe_launch(&job_id).await else {
        return StatusCode::NOT_FOUND.into_response();
    };

    ws.on_upgrade(move |socket| async move {
        if let Err(err) = handle_launch_stream_ws(socket, snapshot, rx).await {
            tracing::debug!("execution launch stream ended: {err:#}");
        }
    })
    .into_response()
}

async fn handle_launch_stream_ws(
    socket: WebSocket,
    snapshot: ExecutionLaunchSnapshot,
    mut rx: broadcast::Receiver<ExecutionLaunchStreamEvent>,
) -> anyhow::Result<()> {
    let (mut sender, mut receiver) = socket.split();

    send_event(
        &mut sender,
        &ExecutionLaunchStreamEvent::LaunchSnapshot {
            snapshot: snapshot.clone(),
        },
    )
    .await?;

    if !matches!(snapshot.state, ExecutionLaunchState::Running) {
        return Ok(());
    }

    loop {
        tokio::select! {
            incoming = receiver.next() => {
                match incoming {
                    Some(Ok(WsMessage::Close(_))) | None => break,
                    Some(Ok(WsMessage::Ping(payload))) => {
                        let _ = sender.send(WsMessage::Pong(payload)).await;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                }
            }
            event = rx.recv() => {
                match event {
                    Ok(event) => {
                        let is_terminal = matches!(event, ExecutionLaunchStreamEvent::LaunchComplete { .. } | ExecutionLaunchStreamEvent::LaunchError { .. });
                        send_event(&mut sender, &event).await?;
                        if is_terminal {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        break;
                    }
                }
            }
        }
    }

    Ok(())
}

async fn send_event<S>(sender: &mut S, event: &ExecutionLaunchStreamEvent) -> anyhow::Result<()>
where
    S: futures::Sink<WsMessage, Error = axum::Error> + Unpin,
{
    let raw = serde_json::to_string(event)?;
    sender.send(WsMessage::Text(raw)).await?;
    Ok(())
}
