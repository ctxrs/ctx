use axum::extract::ws::{CloseFrame, Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use futures::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::{
    protocol::CloseFrame as TungsteniteCloseFrame, Message as TungsteniteMessage,
};

use ctx_daemon::daemon::WebSessionRouteHandle;
use ctx_transport_runtime::web_sessions::WebSessionAccessError;

use super::super::web_sessions::WebSessionStreamAccessQuery;

pub(crate) async fn web_session_signal(
    State(state): State<WebSessionRouteHandle>,
    Path(id): Path<String>,
    Query(query): Query<WebSessionStreamAccessQuery>,
    ws: WebSocketUpgrade,
) -> Result<Response, StatusCode> {
    state
        .authorize_web_session_signal_bridge(&id, query.token.as_deref())
        .await
        .map_err(web_session_access_status)?;
    let session_id = id.clone();
    Ok(ws.on_upgrade(move |socket| async move {
        handle_web_session_socket(socket, state, session_id).await;
    }))
}

fn web_session_access_status(error: WebSessionAccessError) -> StatusCode {
    match error {
        WebSessionAccessError::MissingToken | WebSessionAccessError::Unauthorized => {
            StatusCode::UNAUTHORIZED
        }
        WebSessionAccessError::NotFound => StatusCode::NOT_FOUND,
    }
}

async fn handle_web_session_socket(
    socket: WebSocket,
    state: WebSessionRouteHandle,
    session_id: String,
) {
    let (upstream, mut viewer_guard) =
        match state.connect_web_session_signal_bridge(session_id).await {
            Ok(parts) => parts,
            Err(_) => {
                let _ = socket.close().await;
                return;
            }
        };

    let (mut client_tx, mut client_rx) = socket.split();
    let (mut up_tx, mut up_rx) = upstream.split();

    let client_to_up = tokio::spawn(async move {
        while let Some(Ok(msg)) = client_rx.next().await {
            let out = match msg {
                WsMessage::Text(text) => TungsteniteMessage::Text(text.into()),
                WsMessage::Binary(bytes) => TungsteniteMessage::Binary(bytes.into()),
                WsMessage::Ping(bytes) => TungsteniteMessage::Ping(bytes.into()),
                WsMessage::Pong(bytes) => TungsteniteMessage::Pong(bytes.into()),
                WsMessage::Close(frame) => {
                    let frame = frame.map(|frame| TungsteniteCloseFrame {
                        code: frame.code.into(),
                        reason: frame.reason.to_string().into(),
                    });
                    TungsteniteMessage::Close(frame)
                }
            };
            if up_tx.send(out).await.is_err() {
                break;
            }
        }
    });

    let up_to_client = tokio::spawn(async move {
        while let Some(Ok(msg)) = up_rx.next().await {
            let out = match msg {
                TungsteniteMessage::Text(text) => WsMessage::Text(text.to_string()),
                TungsteniteMessage::Binary(bytes) => WsMessage::Binary(bytes.to_vec()),
                TungsteniteMessage::Ping(bytes) => WsMessage::Ping(bytes.to_vec()),
                TungsteniteMessage::Pong(bytes) => WsMessage::Pong(bytes.to_vec()),
                TungsteniteMessage::Close(frame) => {
                    let frame = frame.map(|frame| CloseFrame {
                        code: frame.code.into(),
                        reason: frame.reason.to_string().into(),
                    });
                    WsMessage::Close(frame)
                }
                TungsteniteMessage::Frame(_) => continue,
            };
            if client_tx.send(out).await.is_err() {
                break;
            }
        }
    });

    tokio::select! {
        _ = client_to_up => {},
        _ = up_to_client => {},
    };

    viewer_guard.release().await;
}
