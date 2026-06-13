use super::*;
use ctx_desktop_ipc::DesktopCodexLoginRelayReq;

pub(super) fn is_loopback_host_name(host: &str) -> bool {
    let value = host.trim().to_ascii_lowercase();
    if value == "localhost" {
        return true;
    }
    value
        .parse::<std::net::IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

fn relay_bind_addr(host: &str, port: u16) -> Result<std::net::SocketAddr> {
    let normalized = host.trim().to_ascii_lowercase();
    if normalized == "localhost" {
        // Codex currently listens for the real callback on 127.0.0.1 while advertising
        // `localhost` in the OAuth redirect URI. Binding the relay on 127.0.0.1 would
        // shadow Codex and cause the daemon replay to loop back into the relay instead of
        // reaching Codex. Bind the relay on ::1 so browser callbacks that resolve
        // `localhost` to IPv6 still succeed without stealing the IPv4 listener.
        return Ok(std::net::SocketAddr::new(
            std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST),
            port,
        ));
    }

    let ip = normalized
        .parse::<std::net::IpAddr>()
        .with_context(|| format!("callback_url host is not a loopback IP: {host}"))?;
    if !ip.is_loopback() {
        anyhow::bail!("callback_url host must be loopback");
    }
    Ok(std::net::SocketAddr::new(ip, port))
}

fn read_http_request_target(stream: &mut TcpStream) -> Result<String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .context("setting relay read timeout")?;
    let mut buf = [0u8; 16384];
    let read = stream.read(&mut buf).context("reading callback request")?;
    if read == 0 {
        anyhow::bail!("empty callback request");
    }
    let request = String::from_utf8_lossy(&buf[..read]);
    let first_line = request
        .lines()
        .next()
        .ok_or_else(|| anyhow!("callback request missing request line"))?;
    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default();
    if method != "GET" {
        anyhow::bail!("unsupported callback method: {method}");
    }
    if target.trim().is_empty() {
        anyhow::bail!("callback request missing target path");
    }
    Ok(target.trim().to_string())
}

fn write_http_response(stream: &mut TcpStream, status: &str, message: &str) {
    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>ctx Codex Login</title></head><body><h2>{status}</h2><p>{message}</p><p>You can now return to ctx.</p></body></html>"
    );
    let payload = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        html.len(),
        html
    );
    let _ = stream.write_all(payload.as_bytes());
    let _ = stream.flush();
}

fn callback_url_from_target(
    target: &str,
    expected_path: &str,
    expected_port: u16,
) -> Result<String> {
    let mut url = if target.starts_with("http://") || target.starts_with("https://") {
        Url::parse(target).context("parsing callback request target URL")?
    } else {
        Url::parse(&format!("http://localhost{target}"))
            .context("parsing callback request target path")?
    };
    if url.path() != expected_path {
        anyhow::bail!("callback path mismatch");
    }
    if url.query().is_none() {
        anyhow::bail!("callback query is missing");
    }
    if !is_loopback_host_name(url.host_str().unwrap_or_default()) {
        anyhow::bail!("callback host must be loopback");
    }
    let _ = url.set_scheme("http");
    let _ = url.set_port(Some(expected_port));
    Ok(url.to_string())
}

fn process_codex_login_relay_connection(
    app: &tauri::AppHandle,
    scope: &str,
    mut stream: TcpStream,
    login_id: &str,
    completion_token: &str,
    expected_path: &str,
    expected_port: u16,
) -> Result<()> {
    let target = match read_http_request_target(&mut stream) {
        Ok(target) => target,
        Err(err) => {
            write_http_response(
                &mut stream,
                "400 Bad Request",
                "Invalid callback request. Retry from ctx Settings.",
            );
            return Err(err);
        }
    };
    let callback_url = match callback_url_from_target(&target, expected_path, expected_port) {
        Ok(url) => url,
        Err(err) => {
            write_http_response(
                &mut stream,
                "400 Bad Request",
                "Callback URL validation failed. Retry from ctx Settings.",
            );
            return Err(err);
        }
    };

    let state = app.state::<ConnectionManager>();
    let manager: &ConnectionManager = state.inner();
    ensure_local_connection_for_scope(app, manager, scope).context("ensuring daemon connection")?;
    let body = serde_json::json!({
        "callback_url": callback_url,
        "completion_token": completion_token,
    })
    .to_string();
    let response = manager.daemon_request_for_scope(
        scope,
        DesktopDaemonRequest {
            method: "POST".to_string(),
            path: format!("/api/providers/codex/accounts/login/{login_id}"),
            body: Some(body),
            headers: vec![("Content-Type".to_string(), "application/json".to_string())],
        },
    )?;
    if (200..300).contains(&response.status) {
        write_http_response(
            &mut stream,
            "200 OK",
            "Codex login callback received. Completing sign-in.",
        );
        return Ok(());
    }

    write_http_response(
        &mut stream,
        "502 Bad Gateway",
        "ctx could not complete login relay on the daemon. Use manual callback paste in Settings.",
    );
    anyhow::bail!(
        "daemon callback completion failed with status {}",
        response.status
    )
}

#[tauri::command]
pub(crate) async fn desktop_start_codex_login_relay(
    app: tauri::AppHandle,
    window: tauri::Window,
    req: DesktopCodexLoginRelayReq,
) -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let login_id = req.login_id.trim().to_string();
        if login_id.is_empty() {
            return Err(anyhow!("login_id is required"));
        }
        let completion_token = req.completion_token.trim().to_string();
        if completion_token.is_empty() {
            return Err(anyhow!("completion_token is required"));
        }
        let callback_url = Url::parse(req.callback_url.trim())
            .context("invalid callback_url for relay listener")?;
        if callback_url.scheme() != "http" {
            anyhow::bail!("callback_url must use http");
        }
        let host = callback_url
            .host_str()
            .ok_or_else(|| anyhow!("callback_url missing host"))?
            .to_string();
        if !is_loopback_host_name(&host) {
            anyhow::bail!("callback_url host must be loopback");
        }
        let port = callback_url
            .port()
            .ok_or_else(|| anyhow!("callback_url missing explicit port"))?;
        let expected_path = callback_url.path().to_string();
        if !expected_path.starts_with("/auth/callback") {
            anyhow::bail!("callback_url path must start with /auth/callback");
        }

        let bind_addr = relay_bind_addr(&host, port)?;
        let listener = TcpListener::bind(bind_addr)
            .with_context(|| format!("binding codex callback relay on {bind_addr}"))?;
        listener
            .set_nonblocking(true)
            .context("setting callback relay nonblocking")?;
        let scope = window.label().to_string();

        std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5 * 60);
            loop {
                if Instant::now() >= deadline {
                    break;
                }
                match listener.accept() {
                    Ok((stream, _addr)) => {
                        if let Err(err) = process_codex_login_relay_connection(
                            &app,
                            &scope,
                            stream,
                            &login_id,
                            &completion_token,
                            &expected_path,
                            port,
                        ) {
                            eprintln!("codex login relay failed: {err:#}");
                        }
                        break;
                    }
                    Err(err) if err.kind() == ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(40));
                    }
                    Err(err) => {
                        eprintln!("codex login relay accept failed: {err}");
                        break;
                    }
                }
            }
        });
        Ok(true)
    })
    .await
    .map_err(|e| format!("starting codex relay failed: {e}"))?
    .map_err(to_err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_bind_addr_uses_ipv6_loopback_for_localhost() {
        let addr = relay_bind_addr("localhost", 43123).expect("resolve localhost relay addr");
        assert_eq!(
            addr,
            std::net::SocketAddr::new(std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST), 43123,)
        );
    }

    #[test]
    fn relay_bind_addr_preserves_explicit_ipv4_loopback() {
        let addr = relay_bind_addr("127.0.0.1", 43123).expect("resolve ipv4 relay addr");
        assert_eq!(
            addr,
            std::net::SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 43123,)
        );
    }

    #[test]
    fn callback_url_from_target_preserves_localhost_authority() {
        let callback =
            callback_url_from_target("/auth/callback?code=abc&state=def", "/auth/callback", 1455)
                .expect("build callback URL");
        assert_eq!(
            callback,
            "http://localhost:1455/auth/callback?code=abc&state=def"
        );
    }

    #[test]
    fn ipv6_localhost_relay_can_coexist_with_ipv4_codex_callback_listener() {
        let codex_listener =
            TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("bind ipv4 callback");
        let port = codex_listener
            .local_addr()
            .expect("read ipv4 callback addr")
            .port();
        let relay_addr = relay_bind_addr("localhost", port).expect("resolve localhost relay addr");
        let relay_listener = TcpListener::bind(relay_addr);
        if relay_addr.is_ipv6() && relay_listener.is_err() {
            let err = relay_listener.expect_err("relay bind should fail with an error");
            let kind = err.kind();
            assert!(
                matches!(
                    kind,
                    ErrorKind::AddrNotAvailable
                        | ErrorKind::Unsupported
                        | ErrorKind::PermissionDenied
                ),
                "unexpected ipv6 bind failure kind: {kind:?}"
            );
            return;
        }
        relay_listener.expect("bind localhost relay without shadowing ipv4 listener");
    }
}
