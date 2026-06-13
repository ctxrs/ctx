mod artifacts;
mod providers;
mod session_controls;
mod sessions;
mod telemetry;
mod workspaces;

use anyhow::{anyhow, Context, Result};
use reqwest::Method;
use url::Url;

use crate::client::Client;
use crate::types::Health;

impl Client {
    pub(crate) fn websocket_url_for_path(&self, stream_path: &str) -> Result<String> {
        let mut url = Url::parse(&self.base_url)
            .with_context(|| format!("invalid base url: {}", self.base_url))?;
        let scheme = match url.scheme() {
            "http" => "ws",
            "https" => "wss",
            other => return Err(anyhow!("unsupported base url scheme: {other}")),
        };
        url.set_scheme(scheme)
            .map_err(|_| anyhow!("failed to set websocket scheme"))?;
        let prefix = url.path().trim_end_matches('/').to_string();
        let (path, query) = match stream_path.split_once('?') {
            Some((path, query)) => (path, Some(query)),
            None => (stream_path, None),
        };
        let path = if prefix.is_empty() {
            path.to_string()
        } else {
            format!("{prefix}{path}")
        };
        url.set_path(&path);
        url.set_query(query);
        Ok(url.to_string())
    }

    pub async fn get_health(&self) -> Result<Health> {
        self.request_json(Method::GET, "/api/health", None::<&()>)
            .await
    }
}

#[cfg(test)]
mod tests {
    use ctx_core::ids::{TerminalId, WorkspaceId};
    use ctx_core::models::TerminalSession;

    use crate::{Client, DaemonConfig};

    #[test]
    fn workspace_stream_url_builds_without_token_query() {
        let client = Client::new(DaemonConfig {
            base_url: "https://example.com/base/".to_string(),
            auth_token: Some("secret-token".to_string()),
        })
        .unwrap();
        let workspace_id = WorkspaceId::new();
        let url = client.workspace_stream_url(workspace_id).unwrap();
        assert_eq!(
            url,
            format!(
                "wss://example.com/base/api/workspaces/{}/active_snapshot/stream",
                workspace_id.0
            )
        );
        assert!(!url.contains("token="));
    }

    #[tokio::test]
    async fn terminal_stream_url_uses_terminal_scoped_stream_path() {
        let terminal_id = TerminalId::new();
        let stream_path = format!(
            "/api/terminals/{}/stream?token=terminal-secret",
            terminal_id.0
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let app = axum::Router::new().route(
                &format!("/base/api/terminals/{}/stream_token", terminal_id.0),
                axum::routing::post(move || {
                    let stream_path = stream_path.clone();
                    async move {
                        axum::Json(serde_json::json!({
                            "stream_path": stream_path,
                            "expires_at": "2026-04-23T00:00:00Z"
                        }))
                    }
                }),
            );
            axum::serve(listener, app).await.unwrap();
        });
        let client = Client::new(DaemonConfig {
            base_url: format!("http://{addr}/base/"),
            auth_token: Some("secret-token".to_string()),
        })
        .unwrap();
        let terminal: TerminalSession = serde_json::from_value(serde_json::json!({
            "id": terminal_id.0,
            "workspace_id": WorkspaceId::new().0,
            "task_id": null,
            "session_id": null,
            "worktree_id": null,
            "cwd": "/tmp",
            "shell": "/bin/bash",
            "title": "bash",
            "status": "running",
            "exit_code": null,
            "stream_path": format!(
                "/api/terminals/{}/stream",
                terminal_id.0
            ),
            "created_at": "2026-04-23T00:00:00Z",
            "updated_at": "2026-04-23T00:00:00Z"
        }))
        .unwrap();
        let url = client.terminal_stream_url(&terminal).await.unwrap();
        assert_eq!(
            url,
            format!(
                "ws://{addr}/base/api/terminals/{}/stream?token=terminal-secret",
                terminal_id.0
            )
        );
    }

    #[test]
    fn websocket_urls_reject_unsupported_base_scheme() {
        let client = Client {
            base_url: "ftp://example.com".to_string(),
            auth_token: None,
            http: reqwest::Client::new(),
        };

        let workspace_err = client.workspace_stream_url(WorkspaceId::new()).unwrap_err();
        assert!(workspace_err
            .to_string()
            .contains("unsupported base url scheme: ftp"));
    }
}
