use super::*;
use axum::{extract::Query, routing::get, Json, Router};
use std::collections::HashMap;
use std::sync::OnceLock;
use tokio::sync::Mutex;

struct ScopedEnvVar {
    key: &'static str,
    previous: Option<String>,
}

impl ScopedEnvVar {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, previous }
    }
}

impl Drop for ScopedEnvVar {
    fn drop(&mut self) {
        if let Some(value) = &self.previous {
            unsafe {
                std::env::set_var(self.key, value);
            }
        } else {
            unsafe {
                std::env::remove_var(self.key);
            }
        }
    }
}

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

#[tokio::test]
async fn session_list_call_requests_current_session_scope() -> Result<()> {
    let _guard = env_lock().lock().await;
    let current_session_id = "00000000-0000-0000-0000-000000000111";
    let foreign_session_id = "00000000-0000-0000-0000-000000000222";
    let _session_id = ScopedEnvVar::set("CTX_SESSION_ID", current_session_id);
    let _mcp_token = ScopedEnvVar::set("CTX_MCP_TOKEN", "scoped-mcp-token");

    let app = Router::new().route(
        "/api/sessions/web",
        get(move |Query(query): Query<HashMap<String, String>>| async move {
            let sessions = if query.get("session_id").map(String::as_str)
                == Some(current_session_id)
            {
                json!([
                    {
                        "id": "sess-own",
                        "kind": "web",
                        "session_id": current_session_id,
                        "worktree_id": "00000000-0000-0000-0000-000000000333",
                        "status": "running",
                        "created_at": "2026-01-01T00:00:00Z",
                        "updated_at": "2026-01-01T00:00:00Z",
                        "last_activity": "2026-01-01T00:00:00Z",
                        "url": "https://own.example",
                        "viewport": {"width": 1280, "height": 720},
                        "fps": 30,
                        "viewers": 0,
                        "stream_path": "/sessions/web/sess-own/view?token=own-token",
                        "stream_url": "http://127.0.0.1:0/sessions/web/sess-own/view?token=own-token"
                    }
                ])
            } else {
                json!([
                    {
                        "id": "sess-own",
                        "kind": "web",
                        "session_id": current_session_id,
                        "worktree_id": "00000000-0000-0000-0000-000000000333",
                        "status": "running",
                        "created_at": "2026-01-01T00:00:00Z",
                        "updated_at": "2026-01-01T00:00:00Z",
                        "last_activity": "2026-01-01T00:00:00Z",
                        "url": "https://own.example",
                        "viewport": {"width": 1280, "height": 720},
                        "fps": 30,
                        "viewers": 0,
                        "stream_path": "/sessions/web/sess-own/view?token=own-token",
                        "stream_url": "http://127.0.0.1:0/sessions/web/sess-own/view?token=own-token"
                    },
                    {
                        "id": "sess-foreign",
                        "kind": "web",
                        "session_id": foreign_session_id,
                        "worktree_id": "00000000-0000-0000-0000-000000000444",
                        "status": "running",
                        "created_at": "2026-01-01T00:00:00Z",
                        "updated_at": "2026-01-01T00:00:00Z",
                        "last_activity": "2026-01-01T00:00:00Z",
                        "url": "https://foreign.example",
                        "viewport": {"width": 1280, "height": 720},
                        "fps": 30,
                        "viewers": 0,
                        "stream_path": "/sessions/web/sess-foreign/view?token=foreign-token",
                        "stream_url": "http://127.0.0.1:0/sessions/web/sess-foreign/view?token=foreign-token"
                    }
                ])
            };
            Json(sessions)
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let _daemon_url = ScopedEnvVar::set("CTX_DAEMON_URL", &format!("http://{addr}"));
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = reqwest::Client::new();
    let response =
        web_sessions::session_list_call(&client, &format!("http://{addr}"), &json!({"kind":"web"}))
            .await?;

    let sessions = response
        .as_array()
        .context("session_list response should be an array")?;
    assert_eq!(
        sessions.len(),
        1,
        "foreign web sessions leaked through ctx-mcp"
    );
    let session = sessions[0]
        .as_object()
        .context("session entry should be an object")?;
    assert_eq!(session.get("url"), Some(&json!("https://own.example")));
    assert!(session
        .get("session_ref")
        .and_then(|v| v.as_str())
        .is_some());
    assert!(!session.contains_key("id"));
    assert!(!session.contains_key("session_id"));
    assert!(!session.contains_key("worktree_id"));

    Ok(())
}
