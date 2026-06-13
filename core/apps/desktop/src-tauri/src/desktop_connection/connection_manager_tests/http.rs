use super::*;

#[test]
fn daemon_request_error_includes_method_and_url_context() {
    let manager = ConnectionManager::default();
    manager.set_local_attached(
        "http://127.0.0.1:65535".to_string(),
        "token".to_string(),
        None,
        LocalConnectionSource::ExistingCompatibleDaemon,
    );
    let err = manager
        .daemon_request(DesktopDaemonRequest {
            method: "GET".to_string(),
            path: "/api/health".to_string(),
            body: None,
            headers: Vec::new(),
        })
        .expect_err("request should fail on closed port");
    let message = format!("{err:#}");
    assert!(
        message.contains("sending request GET http://127.0.0.1:65535/api/health"),
        "expected method/url context in error, got: {message}"
    );
}

#[test]
fn daemon_request_reuses_connection_http_client() {
    reset_connection_http_client_build_count();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind test listener");
    let addr = listener.local_addr().expect("listener addr");
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().expect("accept request");
            let mut buf = [0_u8; 1024];
            let _ = std::io::Read::read(&mut stream, &mut buf);
            std::io::Write::write_all(
                &mut stream,
                b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
            )
            .expect("write response");
        }
    });

    let manager = ConnectionManager::default();
    manager.set_local_attached(
        format!("http://{}", addr),
        "token".to_string(),
        None,
        LocalConnectionSource::EnvOverride,
    );

    for _ in 0..2 {
        let response = manager
            .daemon_request(DesktopDaemonRequest {
                method: "GET".to_string(),
                path: "/api/health".to_string(),
                body: None,
                headers: Vec::new(),
            })
            .expect("daemon request succeeds");
        assert_eq!(response.status, 200);
    }

    server.join().expect("join test server");
    assert_eq!(connection_http_client_build_count(), 1);
}

#[test]
fn blob_upload_error_uses_daemon_message() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind test listener");
    let addr = listener.local_addr().expect("listener addr");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept request");
        let mut buf = [0_u8; 2048];
        let _ = std::io::Read::read(&mut stream, &mut buf);
        std::io::Write::write_all(
            &mut stream,
            b"HTTP/1.1 413 Payload Too Large\r\nContent-Length: 56\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"error\":\"Image attachments must be 25 MiB or smaller.\"}",
        )
        .expect("write response");
    });

    let manager = ConnectionManager::default();
    manager.set_local_attached(
        format!("http://{}", addr),
        "token".to_string(),
        None,
        LocalConnectionSource::EnvOverride,
    );

    let err = manager
        .upload_blob(
            vec![1, 2, 3],
            "image/png".to_string(),
            Some("x.png".to_string()),
        )
        .expect_err("blob upload should fail with daemon message");
    let message = format!("{err:#}");
    assert!(
        message.contains(
            "image attachment upload failed (413 Payload Too Large): Image attachments must be 25 MiB or smaller."
        ),
        "expected daemon upload error message, got: {message}"
    );

    server.join().expect("join test server");
}

#[test]
fn daemon_request_rejects_authorization_header_override() {
    let manager = ConnectionManager::default();
    manager.set_local_attached(
        "http://127.0.0.1:65535".to_string(),
        "token".to_string(),
        None,
        LocalConnectionSource::ExistingCompatibleDaemon,
    );

    let err = manager
        .daemon_request(DesktopDaemonRequest {
            method: "GET".to_string(),
            path: "/api/health".to_string(),
            body: None,
            headers: vec![("Authorization".to_string(), "Bearer other".to_string())],
        })
        .expect_err("authorization override should be rejected");
    assert!(
        format!("{err:#}").contains("cannot override authorization header"),
        "expected authorization override rejection, got: {err:#}"
    );
}

#[test]
#[cfg(unix)]
fn remote_daemon_requests_fail_closed_when_ssh_auth_token_is_missing() {
    let tunnel = spawn_tokio_sleep_child();
    let tunnel_pid = tunnel.id();

    let manager = ConnectionManager::default();
    manager.set_ssh(
        "http://127.0.0.1:65522".to_string(),
        None,
        tunnel,
        "example.test".to_string(),
        Some("dev".to_string()),
        22,
        Some("/tmp/ctx".to_string()),
        SshRuntimeMetadata {
            managed_ctx_bin: "~/.ctx/bin/ctx".to_string(),
            active_ctx_bin: Some("~/.ctx/bin/ctx".to_string()),
            ssh_password_once: None,
            admin_password_once: None,
        },
    );

    let request_err = manager
        .daemon_request(DesktopDaemonRequest {
            method: "GET".to_string(),
            path: "/api/health".to_string(),
            body: None,
            headers: Vec::new(),
        })
        .expect_err("ssh daemon request should fail without auth token");
    assert!(
        format!("{request_err:#}").contains("remote desktop daemon auth token is missing"),
        "expected missing ssh auth token error, got: {request_err:#}"
    );

    let upload_err = manager
        .upload_blob(vec![1, 2, 3], "application/octet-stream".to_string(), None)
        .expect_err("ssh blob upload should fail without auth token");
    assert!(
        format!("{upload_err:#}").contains("remote desktop daemon auth token is missing"),
        "expected missing ssh auth token error, got: {upload_err:#}"
    );

    manager.disconnect();
    assert!(
        wait_for_pid_exit(tunnel_pid, Duration::from_secs(3)),
        "ssh tunnel should be cleaned up during test teardown"
    );
}
