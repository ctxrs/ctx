use super::*;
use crate::web_sessions::runtime_support::resolve_script_path;

#[tokio::test]
async fn worker_readiness_rejects_unauthenticated_health_endpoint() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let mut buf = [0_u8; 1024];
                let _ = stream.read(&mut buf).await;
                let _ = stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok",
                    )
                    .await;
            });
        }
    });

    let manager = WebSessionManager::new();
    let err = manager
        .await_worker_ready(port, "worker-secret")
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("must reject unauthenticated loopback access"),
        "unexpected error: {err:#}"
    );

    server.abort();
}

fn test_session_info() -> WebSessionInfo {
    let now = Utc::now();
    WebSessionInfo {
        id: "sess-1".to_string(),
        kind: "web".to_string(),
        session_id: None,
        worktree_id: None,
        status: WebSessionStatus::Running,
        created_at: now,
        updated_at: now,
        last_activity: now,
        url: "https://example.com".to_string(),
        viewport: WebSessionViewport {
            width: 1280,
            height: 720,
        },
        fps: 30,
        viewers: 0,
        stream_path: build_stream_path("sess-1"),
        stream_url: None,
    }
}

fn test_handle(work_dir: Option<PathBuf>) -> WebSessionHandle {
    let now = Utc::now();
    WebSessionHandle {
        info: test_session_info(),
        stream_tokens: Arc::new(Mutex::new(HashMap::new())),
        worker_auth_secret: "worker-secret".to_string(),
        runtime: Arc::new(Mutex::new(WebSessionRuntime {
            status: WebSessionStatus::Running,
            updated_at: now,
            last_activity: now,
            viewers: 0,
            worker_port: 4321,
            child: None,
            work_dir,
        })),
        run_lock: Arc::new(Mutex::new(())),
    }
}

#[test]
fn web_session_paths_separate_stable_and_tokenized_routes() {
    assert_eq!(build_stream_path("sess-1"), "/sessions/web/sess-1/view");
    assert_eq!(
        build_stream_connect_path("sess-1", "stream-token"),
        "/sessions/web/sess-1/view?token=stream-token"
    );
    assert_eq!(
        build_signal_connect_path("sess-1", "stream-token"),
        "/sessions/web/sess-1/signal?token=stream-token"
    );
}

#[test]
fn rendered_view_uses_tokenized_signal_path() {
    let html = render_web_session_view(
        &test_session_info(),
        "/sessions/web/sess-1/signal?token=stream-token",
    );
    assert!(html.contains("/sessions/web/sess-1/signal?token=stream-token"));
}

#[test]
fn rendered_view_accepts_absolute_signal_websocket_url() {
    let html = render_web_session_view(
        &test_session_info(),
        "wss://proxy.example/ctx/sessions/web/sess-1/signal?token=stream-token",
    );
    assert!(html.contains("wss://proxy.example/ctx/sessions/web/sess-1/signal?token=stream-token"));
    assert!(html.contains("signalEndpoint.startsWith('ws://')"));
}

#[test]
fn web_session_handle_exposes_worker_auth_secret() {
    let handle = test_handle(None);
    assert_eq!(handle.worker_auth_secret(), "worker-secret");
}

#[tokio::test]
async fn web_session_stream_tokens_are_scoped_and_single_use() {
    let handle = test_handle(None);

    let (view_path, _) = handle.issue_view_connect_path().await;
    let view_token = view_path
        .split("token=")
        .nth(1)
        .expect("missing view token");
    assert!(handle.consume_view_token(view_token).await);
    assert!(!handle.consume_view_token(view_token).await);

    let (signal_path, _) = handle.issue_signal_connect_path().await;
    let signal_token = signal_path
        .split("token=")
        .nth(1)
        .expect("missing signal token");
    assert!(!handle.consume_view_token(signal_token).await);
    assert!(handle.consume_signal_token(signal_token).await);
    assert!(!handle.consume_signal_token(signal_token).await);
}

#[tokio::test]
async fn closing_missing_session_returns_not_found_error() {
    let manager = WebSessionManager::new();
    let err = manager.close("missing-session").await.unwrap_err();
    assert!(format!("{err:#}").contains("session not found"));
}

#[tokio::test]
async fn resolve_script_path_rejects_absolute_paths() {
    let dir = std::env::temp_dir().join(format!("ctx-web-session-test-{}", Uuid::new_v4()));
    tokio::fs::create_dir_all(&dir).await.unwrap();
    let handle = test_handle(Some(dir.clone()));
    let absolute = dir.join("script.js");
    tokio::fs::write(&absolute, "console.log('hi');")
        .await
        .unwrap();

    let err = resolve_script_path(&handle, absolute.to_str().unwrap())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("relative to work_dir"));
    let _ = tokio::fs::remove_dir_all(&dir).await;
}

#[tokio::test]
async fn resolve_script_path_allows_absolute_paths_without_work_dir() {
    let absolute = std::env::temp_dir().join(format!("ctx-web-session-test-{}.js", Uuid::new_v4()));
    let handle = test_handle(None);

    let resolved = resolve_script_path(&handle, absolute.to_str().unwrap())
        .await
        .unwrap();
    assert_eq!(resolved, absolute);
}

#[tokio::test]
async fn resolve_script_path_accepts_relative_paths_inside_work_dir() {
    let dir = std::env::temp_dir().join(format!("ctx-web-session-test-{}", Uuid::new_v4()));
    let nested = dir.join("scripts");
    tokio::fs::create_dir_all(&nested).await.unwrap();
    let script = nested.join("script.js");
    tokio::fs::write(&script, "console.log('hi');")
        .await
        .unwrap();
    let handle = test_handle(Some(dir.clone()));

    let resolved = resolve_script_path(&handle, "scripts/script.js")
        .await
        .unwrap();
    assert_eq!(resolved, script.canonicalize().unwrap());
    let _ = tokio::fs::remove_dir_all(&dir).await;
}

#[tokio::test]
async fn resolve_script_path_accepts_paths_inside_symlinked_work_dir() {
    let root = tempfile::tempdir().unwrap();
    let real = root.path().join("real");
    let linked = root.path().join("linked");
    let nested = real.join("scripts");
    tokio::fs::create_dir_all(&nested).await.unwrap();
    tokio::fs::write(nested.join("script.js"), "console.log('hi');")
        .await
        .unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&real, &linked).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(&real, &linked).unwrap();

    let handle = test_handle(Some(linked.clone()));
    let resolved = resolve_script_path(&handle, "scripts/script.js")
        .await
        .unwrap();
    assert_eq!(
        resolved,
        real.join("scripts/script.js").canonicalize().unwrap()
    );
}
