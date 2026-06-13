use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use base64::Engine;
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, warn};
use url::Url;

const TUNNEL_SECRET_HEADER: &str = "x-ctx-tunnel-secret";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MobileTunnelState {
    Idle,
    Running,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct MobileTunnelStatus {
    pub state: MobileTunnelState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StartMobileTunnelConfig {
    pub relay_base_url: String,
    pub tunnel_id: String,
    pub tunnel_secret: String,
    pub public_base_url: String,
    pub local_daemon_url: String,
}

#[derive(Default, Clone)]
pub struct MobileTunnelManager {
    inner: Arc<Mutex<MobileTunnelInner>>,
}

#[derive(Default)]
struct MobileTunnelInner {
    active: Option<ActiveTunnel>,
    last_error: Option<String>,
}

struct ActiveTunnel {
    base_url: String,
    stop_tx: oneshot::Sender<()>,
    handle: tokio::task::JoinHandle<()>,
}

impl MobileTunnelManager {
    pub async fn stop(&self) {
        let active = {
            let mut inner = self.inner.lock().await;
            inner.last_error = None;
            inner.active.take()
        };
        if let Some(active) = active {
            let _ = active.stop_tx.send(());
            active.handle.abort();
        }
    }

    pub async fn status(&self) -> MobileTunnelStatus {
        let inner = self.inner.lock().await;
        if let Some(active) = inner.active.as_ref() {
            return MobileTunnelStatus {
                state: MobileTunnelState::Running,
                base_url: Some(active.base_url.clone()),
                last_error: None,
            };
        }
        if let Some(err) = inner.last_error.as_ref() {
            return MobileTunnelStatus {
                state: MobileTunnelState::Error,
                base_url: None,
                last_error: Some(err.clone()),
            };
        }
        MobileTunnelStatus {
            state: MobileTunnelState::Idle,
            base_url: None,
            last_error: None,
        }
    }

    pub async fn start(&self, cfg: StartMobileTunnelConfig) -> Result<String> {
        self.stop().await;

        let (stop_tx, stop_rx) = oneshot::channel();
        let base_url = cfg.public_base_url.clone();
        let manager = self.clone();

        let handle = tokio::spawn(async move {
            if let Err(err) = tunnel_client(cfg, stop_rx).await {
                manager.set_error(err.to_string()).await;
            }
        });

        {
            let mut inner = self.inner.lock().await;
            inner.last_error = None;
            inner.active = Some(ActiveTunnel {
                base_url: base_url.clone(),
                stop_tx,
                handle,
            });
        }

        Ok(base_url)
    }

    async fn set_error(&self, err: String) {
        let mut inner = self.inner.lock().await;
        inner.active = None;
        inner.last_error = Some(err);
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RelayToClient {
    HttpRequest {
        id: String,
        method: String,
        path: String,
        headers: Vec<(String, String)>,
        body_b64: String,
    },
    WsOpen {
        id: String,
        path: String,
        headers: Vec<(String, String)>,
    },
    WsMessage {
        id: String,
        is_binary: bool,
        data: String,
    },
    WsClose {
        id: String,
        code: Option<u16>,
        reason: Option<String>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientToRelay {
    HttpResponse {
        id: String,
        status: u16,
        headers: Vec<(String, String)>,
        body_b64: String,
    },
    WsOpenResult {
        id: String,
        ok: bool,
        #[serde(default)]
        error: Option<String>,
    },
    WsMessage {
        id: String,
        is_binary: bool,
        data: String,
    },
    WsClosed {
        id: String,
        #[serde(default)]
        code: Option<u16>,
        #[serde(default)]
        reason: Option<String>,
    },
}

async fn tunnel_client(
    cfg: StartMobileTunnelConfig,
    mut stop_rx: oneshot::Receiver<()>,
) -> Result<()> {
    ensure_rustls_crypto_provider();

    let relay_ws_url =
        build_relay_ws_url(&cfg.relay_base_url, &cfg.tunnel_id).context("building relay ws url")?;
    let relay_secret_header =
        http::HeaderValue::from_str(&cfg.tunnel_secret).context("building relay secret header")?;

    let local_http = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .context("building local http client")?;

    let mut backoff = Duration::from_millis(200);
    let max_backoff = Duration::from_secs(5);

    loop {
        info!("mobile tunnel connecting to {relay_ws_url}");
        let mut req = match relay_ws_url.as_str().into_client_request() {
            Ok(req) => req,
            Err(err) => return Err(err).context("building relay ws request"),
        };
        req.headers_mut()
            .insert(TUNNEL_SECRET_HEADER, relay_secret_header.clone());
        let connect = tokio_tungstenite::connect_async(req);
        let connected = tokio::select! {
            res = connect => res,
            _ = &mut stop_rx => return Ok(()),
        };

        let (ws_stream, _) = match connected {
            Ok(v) => v,
            Err(err) => {
                warn!("mobile tunnel connect failed: {err}");
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {},
                    _ = &mut stop_rx => return Ok(()),
                }
                backoff = (backoff * 2).min(max_backoff);
                continue;
            }
        };

        backoff = Duration::from_millis(200);
        let (mut ws_tx, mut ws_rx) = ws_stream.split();
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<ClientToRelay>();
        let streams: Arc<Mutex<HashMap<String, mpsc::UnboundedSender<Message>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let streams_for_read = streams.clone();
        let write_task = tokio::spawn(async move {
            while let Some(msg) = out_rx.recv().await {
                let text = match serde_json::to_string(&msg) {
                    Ok(t) => t,
                    Err(err) => {
                        warn!("failed to serialize tunnel message: {err}");
                        continue;
                    }
                };
                if ws_tx.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
        });

        loop {
            tokio::select! {
                _ = &mut stop_rx => {
                    write_task.abort();
                    return Ok(());
                }
                msg = ws_rx.next() => {
                    let Some(msg) = msg else { break; };
                    let Ok(msg) = msg else { break; };
                    match msg {
                        Message::Text(text) => {
                            let parsed: RelayToClient = match serde_json::from_str(&text) {
                                Ok(v) => v,
                                Err(err) => {
                                    warn!("invalid relay message: {err}");
                                    continue;
                                }
                            };
                            handle_relay_message(&cfg, &local_http, &out_tx, &streams_for_read, parsed).await;
                        }
                        Message::Close(_) => break,
                        _ => {}
                    }
                }
            }
        }

        write_task.abort();
        warn!("mobile tunnel disconnected; reconnecting...");
    }
}

async fn handle_relay_message(
    cfg: &StartMobileTunnelConfig,
    client: &reqwest::Client,
    out_tx: &mpsc::UnboundedSender<ClientToRelay>,
    streams: &Arc<Mutex<HashMap<String, mpsc::UnboundedSender<Message>>>>,
    msg: RelayToClient,
) {
    match msg {
        RelayToClient::HttpRequest {
            id,
            method,
            path,
            headers,
            body_b64,
        } => {
            let res = proxy_http(cfg, client, &method, &path, &headers, &body_b64).await;
            let (status, headers, body) = match res {
                Ok(v) => v,
                Err(err) => {
                    let _ = out_tx.send(ClientToRelay::HttpResponse {
                        id,
                        status: 502,
                        headers: vec![("content-type".to_string(), "text/plain".to_string())],
                        body_b64: BASE64.encode(err.to_string().as_bytes()),
                    });
                    return;
                }
            };
            let _ = out_tx.send(ClientToRelay::HttpResponse {
                id,
                status,
                headers,
                body_b64: BASE64.encode(body),
            });
        }
        RelayToClient::WsOpen { id, path, headers } => {
            let result =
                proxy_ws_open(cfg, &id, &path, &headers, out_tx.clone(), streams.clone()).await;
            match result {
                Ok(()) => {
                    let _ = out_tx.send(ClientToRelay::WsOpenResult {
                        id,
                        ok: true,
                        error: None,
                    });
                }
                Err(err) => {
                    let _ = out_tx.send(ClientToRelay::WsOpenResult {
                        id,
                        ok: false,
                        error: Some(err.to_string()),
                    });
                }
            }
        }
        RelayToClient::WsMessage {
            id,
            is_binary,
            data,
        } => {
            let tx = { streams.lock().await.get(&id).cloned() };
            let Some(tx) = tx else {
                return;
            };
            let msg = if is_binary {
                Message::Binary(BASE64.decode(data.as_bytes()).unwrap_or_default().into())
            } else {
                Message::Text(data.into())
            };
            let _ = tx.send(msg);
        }
        RelayToClient::WsClose { id, code, reason } => {
            let tx = { streams.lock().await.remove(&id) };
            let Some(tx) = tx else {
                return;
            };
            let _ = tx.send(Message::Close(Some(
                tokio_tungstenite::tungstenite::protocol::CloseFrame {
                    code:
                        tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Normal,
                    reason: reason.unwrap_or_default().into(),
                },
            )));
            if let Some(_code) = code {
                // ignore for now
            }
        }
    }
}

async fn proxy_http(
    cfg: &StartMobileTunnelConfig,
    client: &reqwest::Client,
    method: &str,
    path: &str,
    headers: &[(String, String)],
    body_b64: &str,
) -> Result<(u16, Vec<(String, String)>, Vec<u8>)> {
    let base = cfg.local_daemon_url.trim_end_matches('/');
    let url = format!("{base}{path}");
    let method = reqwest::Method::from_bytes(method.as_bytes()).context("invalid method")?;
    let mut req = client.request(method, &url);
    for (k, v) in headers {
        if k.eq_ignore_ascii_case("host") {
            continue;
        }
        req = req.header(k, v);
    }
    let body = BASE64.decode(body_b64.as_bytes()).unwrap_or_default();
    if !body.is_empty() {
        req = req.body(body);
    }

    let resp = req.send().await.context("sending local request")?;
    let status = resp.status().as_u16();
    let mut out_headers = Vec::new();
    for (k, v) in resp.headers().iter() {
        if let Ok(v) = v.to_str() {
            out_headers.push((k.to_string(), v.to_string()));
        }
    }
    let bytes = resp.bytes().await.context("reading local response")?;
    Ok((status, out_headers, bytes.to_vec()))
}

async fn proxy_ws_open(
    cfg: &StartMobileTunnelConfig,
    stream_id: &str,
    path: &str,
    headers: &[(String, String)],
    out_tx: mpsc::UnboundedSender<ClientToRelay>,
    streams: Arc<Mutex<HashMap<String, mpsc::UnboundedSender<Message>>>>,
) -> Result<()> {
    let url = build_local_ws_url(&cfg.local_daemon_url, path).context("building local ws url")?;

    let mut req = url
        .as_str()
        .into_client_request()
        .context("building ws request")?;
    for (k, v) in headers {
        if k.eq_ignore_ascii_case("host") {
            continue;
        }
        let name: http::header::HeaderName = k.parse().context("invalid ws header name")?;
        let value: http::HeaderValue = v.parse().context("invalid ws header value")?;
        req.headers_mut().insert(name, value);
    }

    let (ws_stream, _) = tokio_tungstenite::connect_async(req)
        .await
        .context("connecting local ws")?;
    let (mut ws_tx, mut ws_rx) = ws_stream.split();

    let (in_tx, mut in_rx) = mpsc::unbounded_channel::<Message>();
    {
        let mut map = streams.lock().await;
        map.insert(stream_id.to_string(), in_tx);
    }

    let stream_id = stream_id.to_string();
    let out_tx_for_read = out_tx.clone();
    let streams_for_read = streams.clone();
    tokio::spawn(async move {
        let mut closed = None::<(Option<u16>, Option<String>)>;
        while let Some(Ok(msg)) = ws_rx.next().await {
            match msg {
                Message::Text(text) => {
                    let _ = out_tx_for_read.send(ClientToRelay::WsMessage {
                        id: stream_id.clone(),
                        is_binary: false,
                        data: text.to_string(),
                    });
                }
                Message::Binary(bytes) => {
                    let _ = out_tx_for_read.send(ClientToRelay::WsMessage {
                        id: stream_id.clone(),
                        is_binary: true,
                        data: BASE64.encode(bytes),
                    });
                }
                Message::Close(frame) => {
                    closed = Some(
                        frame
                            .map(|f| (Some(f.code.into()), Some(f.reason.to_string())))
                            .unwrap_or((None, None)),
                    );
                    break;
                }
                _ => {}
            }
        }

        let (code, reason) = closed.unwrap_or((None, None));
        let _ = out_tx_for_read.send(ClientToRelay::WsClosed {
            id: stream_id.clone(),
            code,
            reason,
        });
        let mut map = streams_for_read.lock().await;
        map.remove(&stream_id);
    });

    tokio::spawn(async move {
        while let Some(msg) = in_rx.recv().await {
            if ws_tx.send(msg).await.is_err() {
                break;
            }
        }
    });

    Ok(())
}

fn build_relay_ws_url(relay_base_url: &str, tunnel_id: &str) -> Result<String> {
    let mut base = Url::parse(relay_base_url).context("relay_base_url must be a valid URL")?;
    let scheme = match base.scheme() {
        "https" => "wss",
        "http" => "ws",
        other => anyhow::bail!("unsupported relay base url scheme: {other}"),
    };
    base.set_scheme(scheme).ok();
    base.set_path(&format!("/connect/{tunnel_id}"));
    base.set_query(None);
    Ok(base.to_string())
}

fn ensure_rustls_crypto_provider() {
    if let Err(err) = rustls::crypto::aws_lc_rs::default_provider().install_default() {
        tracing::debug!("rustls crypto provider already installed or unavailable: {err:?}");
    }
}

fn build_local_ws_url(local_daemon_url: &str, path: &str) -> Result<Url> {
    let mut base = Url::parse(local_daemon_url).context("local_daemon_url must be a valid URL")?;
    let scheme = match base.scheme() {
        "https" => "wss",
        "http" => "ws",
        other => anyhow::bail!("unsupported local daemon url scheme: {other}"),
    };
    base.set_scheme(scheme).ok();
    base.set_path("");
    base.set_query(None);

    let joined = base.join(path.trim_start_matches('/'))?;
    Ok(joined)
}

static BASE64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::STANDARD;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_ws_url_does_not_include_secret_query() {
        let url = build_relay_ws_url("https://relay.example.test", "tunnel-1")
            .expect("relay websocket url");
        assert_eq!(url, "wss://relay.example.test/connect/tunnel-1");
        assert!(!url.contains("secret"));
    }
}
