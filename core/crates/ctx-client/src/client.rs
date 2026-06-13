use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use reqwest::Method;
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::config::{normalize_base_url, resolve_daemon_config, DaemonConfig};

pub struct Client {
    pub(crate) base_url: String,
    pub(crate) auth_token: Option<String>,
    pub(crate) http: reqwest::Client,
}

impl Client {
    pub fn new(config: DaemonConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .context("building http client")?;
        Ok(Self {
            base_url: normalize_base_url(&config.base_url)?,
            auth_token: config.auth_token,
            http,
        })
    }

    pub fn from_env() -> Result<Self> {
        Self::new(resolve_daemon_config()?)
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub(crate) fn url_for(&self, path: &str) -> Result<String> {
        if !path.starts_with('/') {
            return Err(anyhow!("path must start with '/'"));
        }
        Ok(format!("{}{}", self.base_url, path))
    }

    pub(crate) async fn request_json<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&impl Serialize>,
    ) -> Result<T> {
        let url = self.url_for(path)?;
        let mut req = self.http.request(method, url);
        if let Some(token) = &self.auth_token {
            req = req.bearer_auth(token);
        }
        if let Some(value) = body {
            req = req.json(value);
        }
        let resp = req.send().await.context("sending request")?;
        let status = resp.status();
        let text = resp.text().await.context("reading response body")?;
        if !status.is_success() {
            let snippet = text.trim();
            let msg = if snippet.is_empty() {
                format!("request failed with status {}", status.as_u16())
            } else {
                format!(
                    "request failed with status {}: {}",
                    status.as_u16(),
                    snippet
                )
            };
            return Err(anyhow!(msg));
        }
        if text.trim().is_empty() {
            return Err(anyhow!("empty response body from {path}"));
        }
        serde_json::from_str(&text).with_context(|| format!("parsing JSON response from {path}"))
    }

    pub(crate) async fn request_empty(
        &self,
        method: Method,
        path: &str,
        body: Option<&impl Serialize>,
    ) -> Result<()> {
        let url = self.url_for(path)?;
        let mut req = self.http.request(method, url);
        if let Some(token) = &self.auth_token {
            req = req.bearer_auth(token);
        }
        if let Some(value) = body {
            req = req.json(value);
        }
        let resp = req.send().await.context("sending request")?;
        let status = resp.status();
        let text = resp.text().await.context("reading response body")?;
        if !status.is_success() {
            let snippet = text.trim();
            let msg = if snippet.is_empty() {
                format!("request failed with status {}", status.as_u16())
            } else {
                format!(
                    "request failed with status {}: {}",
                    status.as_u16(),
                    snippet
                )
            };
            return Err(anyhow!(msg));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;

    use reqwest::Method;
    use serde_json::json;

    use super::*;
    use crate::{Health, WorkspaceActiveSnapshotParams, WorkspaceArchivedPageParams};
    use ctx_core::ids::{SessionId, TaskId, WorkspaceId};
    use ctx_core::models::{SessionEventsPage, WorkspaceIndexCursor};

    #[derive(Debug)]
    struct CapturedRequest {
        method: String,
        target: String,
        headers: HashMap<String, String>,
        body: Vec<u8>,
    }

    struct TestServer {
        base_url: String,
        requests: mpsc::Receiver<CapturedRequest>,
        handle: thread::JoinHandle<()>,
    }

    impl TestServer {
        fn spawn(status_line: &str, body: String, content_type: Option<&str>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
            let addr = listener.local_addr().expect("read local addr");
            let (tx, rx) = mpsc::channel();
            let status_line = status_line.to_string();
            let content_type = content_type.map(str::to_string);
            let handle = thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept request");
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("set read timeout");
                let request = read_request(&mut stream);
                tx.send(request).expect("send captured request");

                let mut response = format!(
                    "HTTP/1.1 {status_line}\r\nContent-Length: {}\r\nConnection: close\r\n",
                    body.len()
                );
                if let Some(content_type) = content_type {
                    response.push_str(&format!("Content-Type: {content_type}\r\n"));
                }
                response.push_str("\r\n");
                response.push_str(&body);
                stream
                    .write_all(response.as_bytes())
                    .expect("write response");
            });
            Self {
                base_url: format!("http://{addr}"),
                requests: rx,
                handle,
            }
        }

        fn next_request(&self) -> CapturedRequest {
            self.requests
                .recv_timeout(Duration::from_secs(2))
                .expect("receive captured request")
        }

        fn finish(self) {
            self.handle.join().expect("join test server");
        }
    }

    fn read_request(stream: &mut std::net::TcpStream) -> CapturedRequest {
        let mut buf = Vec::new();
        let mut chunk = [0_u8; 4096];
        let mut header_end = None;
        let mut content_length = 0_usize;

        loop {
            let read = stream.read(&mut chunk).expect("read request");
            if read == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..read]);
            if let Some(idx) = header_end {
                if buf.len() >= idx + content_length {
                    break;
                }
            } else if let Some(idx) = find_header_end(&buf) {
                header_end = Some(idx);
                content_length = parse_content_length(&buf[..idx]);
                if buf.len() >= idx + content_length {
                    break;
                }
            }
        }

        let header_end = header_end.expect("complete request headers");
        let headers_text = String::from_utf8(buf[..header_end].to_vec()).expect("utf8 headers");
        let mut lines = headers_text.split("\r\n");
        let request_line = lines.next().expect("request line");
        let mut request_parts = request_line.split_whitespace();
        let method = request_parts.next().expect("request method").to_string();
        let target = request_parts.next().expect("request target").to_string();

        let mut headers = HashMap::new();
        for line in lines {
            if line.is_empty() {
                continue;
            }
            if let Some((name, value)) = line.split_once(':') {
                headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
            }
        }

        let body = buf[header_end..header_end + content_length].to_vec();
        CapturedRequest {
            method,
            target,
            headers,
            body,
        }
    }

    fn find_header_end(buf: &[u8]) -> Option<usize> {
        buf.windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|idx| idx + 4)
    }

    fn parse_content_length(headers: &[u8]) -> usize {
        let text = String::from_utf8(headers.to_vec()).expect("utf8 header parse");
        text.lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                if name.trim().eq_ignore_ascii_case("content-length") {
                    value.trim().parse::<usize>().ok()
                } else {
                    None
                }
            })
            .unwrap_or(0)
    }

    fn test_client(base_url: String, auth_token: Option<&str>) -> Client {
        Client::new(DaemonConfig {
            base_url,
            auth_token: auth_token.map(str::to_string),
        })
        .expect("build test client")
    }

    fn health_body(base_url: &str) -> String {
        json!({
            "version": "1.0.0",
            "daemon_version": "1.0.0",
            "pid": 42,
            "data_root": "/tmp/ctx",
            "daemon_url": base_url,
            "auth_required": true,
            "compatibility": {
                "desktop_exact_version": "1.0.0",
                "desktop_build_id": "build-1",
                "desktop_dev_instance_id": "dev-1",
                "mobile_api_min": 1,
                "mobile_api_max": 1
            }
        })
        .to_string()
    }

    fn public_health_body() -> String {
        json!({
            "version": "1.0.0",
            "daemon_version": "1.0.0",
            "auth_required": true,
            "compatibility": {
                "desktop_exact_version": "1.0.0",
                "desktop_build_id": "build-1",
                "desktop_dev_instance_id": "dev-1",
                "mobile_api_min": 1,
                "mobile_api_max": 1
            }
        })
        .to_string()
    }

    fn active_snapshot_body(workspace_id: WorkspaceId) -> String {
        json!({
            "workspace_id": workspace_id,
            "snapshot_rev": 7,
            "archived_rev": 3,
            "active": {
                "tasks": [],
                "total_count": 0
            }
        })
        .to_string()
    }

    fn archived_page_body(workspace_id: WorkspaceId) -> String {
        json!({
            "workspace_id": workspace_id,
            "archived_rev": 9,
            "tasks": [],
            "total_archived": 0
        })
        .to_string()
    }

    fn session_events_body(session_id: SessionId) -> String {
        json!({
            "session_id": session_id,
            "events": [],
            "has_more": false
        })
        .to_string()
    }

    #[test]
    fn url_for_uses_normalized_base_url() {
        let client = test_client("https://example.com/base/".to_string(), None);
        let url = client.url_for("/api/health").unwrap();
        assert_eq!(url, "https://example.com/base/api/health");
        assert_eq!(client.base_url(), "https://example.com/base");
    }

    #[test]
    fn url_for_rejects_paths_without_leading_slash() {
        let client = test_client("https://example.com".to_string(), None);
        let err = client.url_for("api/health").unwrap_err();
        assert!(err.to_string().contains("path must start with '/'"));
    }

    #[tokio::test]
    async fn auth_request_json_includes_bearer_token() {
        let server = TestServer::spawn(
            "200 OK",
            health_body("http://daemon"),
            Some("application/json"),
        );
        let client = test_client(server.base_url.clone(), Some("secret-token"));

        let health: Health = client
            .request_json(Method::GET, "/api/health", None::<&()>)
            .await
            .unwrap();
        let request = server.next_request();
        server.finish();

        assert_eq!(health.pid, Some(42));
        assert_eq!(request.method, "GET");
        assert_eq!(request.target, "/api/health");
        assert!(request.body.is_empty());
        assert_eq!(
            request.headers.get("authorization").map(String::as_str),
            Some("Bearer secret-token")
        );
    }

    #[tokio::test]
    async fn auth_request_json_omits_authorization_without_token() {
        let server = TestServer::spawn(
            "200 OK",
            health_body("http://daemon"),
            Some("application/json"),
        );
        let client = test_client(server.base_url.clone(), None);

        client
            .request_json::<Health>(Method::GET, "/api/health", None::<&()>)
            .await
            .unwrap();
        let request = server.next_request();
        server.finish();

        assert_eq!(request.target, "/api/health");
        assert!(!request.headers.contains_key("authorization"));
    }

    #[tokio::test]
    async fn public_health_payload_parses_without_sensitive_fields() {
        let server = TestServer::spawn("200 OK", public_health_body(), Some("application/json"));
        let client = test_client(server.base_url.clone(), None);

        let health: Health = client
            .request_json(Method::GET, "/api/health", None::<&()>)
            .await
            .unwrap();
        server.next_request();
        server.finish();

        assert!(health.auth_required);
        assert_eq!(health.pid, None);
        assert_eq!(health.data_root, None);
        assert_eq!(health.daemon_url, None);
        assert_eq!(health.compatibility.mobile_api_max, 1);
    }

    #[tokio::test]
    async fn error_request_json_includes_status_and_body_text() {
        let server = TestServer::spawn(
            "502 Bad Gateway",
            "upstream failed".to_string(),
            Some("text/plain"),
        );
        let client = test_client(server.base_url.clone(), None);

        let err = client
            .request_json::<Health>(Method::GET, "/api/health", None::<&()>)
            .await
            .unwrap_err();
        let request = server.next_request();
        server.finish();

        assert_eq!(request.target, "/api/health");
        assert!(err
            .to_string()
            .contains("request failed with status 502: upstream failed"));
    }

    #[tokio::test]
    async fn error_request_json_includes_status_without_body_text() {
        let server = TestServer::spawn("404 Not Found", String::new(), Some("text/plain"));
        let client = test_client(server.base_url.clone(), None);

        let err = client
            .request_json::<Health>(Method::GET, "/api/health", None::<&()>)
            .await
            .unwrap_err();
        let request = server.next_request();
        server.finish();

        assert_eq!(request.target, "/api/health");
        assert_eq!(err.to_string(), "request failed with status 404");
    }

    #[tokio::test]
    async fn error_request_json_rejects_empty_success_body() {
        let server = TestServer::spawn("200 OK", String::new(), Some("application/json"));
        let client = test_client(server.base_url.clone(), None);

        let err = client
            .request_json::<Health>(Method::GET, "/api/health", None::<&()>)
            .await
            .unwrap_err();
        let request = server.next_request();
        server.finish();

        assert_eq!(request.target, "/api/health");
        assert_eq!(err.to_string(), "empty response body from /api/health");
    }

    #[tokio::test]
    async fn query_get_workspace_active_snapshot_adds_limit() {
        let workspace_id = WorkspaceId::new();
        let server = TestServer::spawn(
            "200 OK",
            active_snapshot_body(workspace_id),
            Some("application/json"),
        );
        let client = test_client(server.base_url.clone(), None);

        client
            .get_workspace_active_snapshot(
                workspace_id,
                &WorkspaceActiveSnapshotParams { limit: Some(25) },
            )
            .await
            .unwrap();
        let request = server.next_request();
        server.finish();

        assert_eq!(request.method, "GET");
        assert_eq!(
            request.target,
            format!(
                "/api/workspaces/{}/active_snapshot?limit=25",
                workspace_id.0
            )
        );
    }

    #[tokio::test]
    async fn query_get_workspace_active_snapshot_omits_empty_query() {
        let workspace_id = WorkspaceId::new();
        let server = TestServer::spawn(
            "200 OK",
            active_snapshot_body(workspace_id),
            Some("application/json"),
        );
        let client = test_client(server.base_url.clone(), None);

        client
            .get_workspace_active_snapshot(workspace_id, &WorkspaceActiveSnapshotParams::default())
            .await
            .unwrap();
        let request = server.next_request();
        server.finish();

        assert_eq!(
            request.target,
            format!("/api/workspaces/{}/active_snapshot", workspace_id.0)
        );
    }

    #[tokio::test]
    async fn query_archived_task_summaries_encodes_cursor_and_limit() {
        let workspace_id = WorkspaceId::new();
        let task_id = TaskId::new();
        let cursor: WorkspaceIndexCursor = serde_json::from_value(json!({
            "sort_at": "2026-03-10T12:34:56Z",
            "task_id": task_id
        }))
        .unwrap();
        let encoded_sort_at =
            url::form_urlencoded::byte_serialize(cursor.sort_at.to_rfc3339().as_bytes())
                .collect::<String>();
        let server = TestServer::spawn(
            "200 OK",
            archived_page_body(workspace_id),
            Some("application/json"),
        );
        let client = test_client(server.base_url.clone(), None);

        client
            .list_workspace_archived_task_summaries(
                workspace_id,
                &WorkspaceArchivedPageParams {
                    limit: Some(50),
                    cursor: Some(cursor),
                },
            )
            .await
            .unwrap();
        let request = server.next_request();
        server.finish();

        assert_eq!(
            request.target,
            format!(
                "/api/workspaces/{}/archived_task_summaries?limit=50&cursor_sort_at={encoded_sort_at}&cursor_task_id={}",
                workspace_id.0, task_id.0
            )
        );
    }

    #[tokio::test]
    async fn query_session_events_includes_after_seq_limit_and_tail() {
        let session_id = SessionId::new();
        let server = TestServer::spawn(
            "200 OK",
            session_events_body(session_id),
            Some("application/json"),
        );
        let client = test_client(server.base_url.clone(), None);

        let page: SessionEventsPage = client
            .get_session_events(session_id, Some(42), Some(10), Some(5))
            .await
            .unwrap();
        let request = server.next_request();
        server.finish();

        assert!(!page.has_more);
        assert_eq!(
            request.target,
            format!(
                "/api/sessions/{}/events?after_seq=42&limit=10&tail=5",
                session_id.0
            )
        );
    }
}
