use std::sync::Arc;

use http::header::{HeaderName, HeaderValue};
use tokio_tungstenite::{
    connect_async, tungstenite::client::IntoClientRequest, MaybeTlsStream, WebSocketStream,
};

use super::{WebSessionManager, WEB_SESSION_WORKER_AUTH_HEADER};

pub type WebSessionSignalUpstream = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

pub struct WebSessionSignalViewerGuard {
    manager: Arc<WebSessionManager>,
    session_id: String,
    released: bool,
}

impl WebSessionSignalViewerGuard {
    pub async fn release(&mut self) {
        if self.released {
            return;
        }
        let _ = self.manager.bump_viewers(&self.session_id, -1).await;
        self.released = true;
    }
}

impl Drop for WebSessionSignalViewerGuard {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        let manager = Arc::clone(&self.manager);
        let session_id = self.session_id.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = manager.bump_viewers(&session_id, -1).await;
            });
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebSessionSignalBridgeError {
    NotFound,
    WorkerRequest,
    WorkerAuthHeader,
    WorkerConnect,
}

impl WebSessionManager {
    pub async fn connect_signal_bridge(
        self: Arc<Self>,
        session_id: String,
    ) -> Result<(WebSessionSignalUpstream, WebSessionSignalViewerGuard), WebSessionSignalBridgeError>
    {
        let handle = self
            .get(&session_id)
            .await
            .ok_or(WebSessionSignalBridgeError::NotFound)?;
        let port = handle.worker_port().await;
        let url = format!("ws://127.0.0.1:{port}/signal");
        let viewer_guard = WebSessionSignalViewerGuard {
            manager: Arc::clone(&self),
            session_id: session_id.clone(),
            released: false,
        };
        let _ = self.bump_viewers(&session_id, 1).await;

        let mut request = match url.into_client_request() {
            Ok(request) => request,
            Err(_) => {
                release_signal_viewer(viewer_guard).await;
                return Err(WebSessionSignalBridgeError::WorkerRequest);
            }
        };
        let header_value: Result<HeaderValue, WebSessionSignalBridgeError> = handle
            .worker_auth_secret()
            .parse()
            .map_err(|_| WebSessionSignalBridgeError::WorkerAuthHeader);
        let header_value = match header_value {
            Ok(header_value) => header_value,
            Err(error) => {
                release_signal_viewer(viewer_guard).await;
                return Err(error);
            }
        };
        request.headers_mut().insert(
            HeaderName::from_static(WEB_SESSION_WORKER_AUTH_HEADER),
            header_value,
        );

        let upstream = match connect_async(request).await {
            Ok((upstream, _)) => upstream,
            Err(_) => {
                release_signal_viewer(viewer_guard).await;
                return Err(WebSessionSignalBridgeError::WorkerConnect);
            }
        };

        Ok((upstream, viewer_guard))
    }
}

async fn release_signal_viewer(mut viewer_guard: WebSessionSignalViewerGuard) {
    viewer_guard.release().await;
}
