#[derive(Debug, Clone)]
enum DaemonQueryEndpoint {
    #[cfg(unix)]
    Unix { path: PathBuf, token: String },
    #[cfg(not(unix))]
    #[allow(dead_code)]
    Unsupported,
}

impl DaemonQueryEndpoint {
    fn token(&self) -> &str {
        match self {
            #[cfg(unix)]
            Self::Unix { token, .. } => token,
            #[cfg(not(unix))]
            Self::Unsupported => "",
        }
    }
}

fn daemon_query_endpoint_path(data_root: &Path) -> PathBuf {
    daemon_root_path(data_root).join(DAEMON_QUERY_ENDPOINT_FILE)
}

#[cfg(unix)]
fn write_daemon_query_endpoint(data_root: &Path, endpoint: &DaemonQueryEndpoint) -> Result<()> {
    let value = match endpoint {
        DaemonQueryEndpoint::Unix { path, token } => compact_json(json!({
            "schema_version": 1,
            "transport": "unix",
            "path": path,
            "token": token,
            "pid": process::id(),
        })),
    };
    write_private_json_file(&daemon_query_endpoint_path(data_root), &value)
}

#[cfg(not(unix))]
#[allow(dead_code)]
fn write_daemon_query_endpoint(_data_root: &Path, _endpoint: &DaemonQueryEndpoint) -> Result<()> {
    Err(anyhow!(
        "daemon query service is not supported on this platform"
    ))
}

fn remove_daemon_query_endpoint(data_root: &Path) {
    let _ = fs::remove_file(daemon_query_endpoint_path(data_root));
}

fn read_daemon_query_endpoint(data_root: &Path) -> Result<Option<DaemonQueryEndpoint>> {
    let path = daemon_query_endpoint_path(data_root);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("read daemon query endpoint {}", path.display()));
        }
    };
    let value: Value = serde_json::from_str(&text)
        .with_context(|| format!("parse daemon query endpoint {}", path.display()))?;
    if value.get("schema_version").and_then(Value::as_u64) != Some(1) {
        return Ok(None);
    }
    read_daemon_query_endpoint_value(value)
}

#[cfg(unix)]
fn read_daemon_query_endpoint_value(value: Value) -> Result<Option<DaemonQueryEndpoint>> {
    let Some(token) = value
        .get("token")
        .and_then(Value::as_str)
        .filter(|token| token.len() >= 32)
        .map(str::to_owned)
    else {
        return Ok(None);
    };
    match value.get("transport").and_then(Value::as_str) {
        Some("unix") => {
            let path = value.get("path").and_then(Value::as_str).map(PathBuf::from);
            Ok(path.map(|path| DaemonQueryEndpoint::Unix { path, token }))
        }
        _ => Ok(None),
    }
}

#[cfg(not(unix))]
fn read_daemon_query_endpoint_value(_value: Value) -> Result<Option<DaemonQueryEndpoint>> {
    Ok(None)
}

fn daemon_query_request(
    data_root: &Path,
    mut request: Value,
    timeout: StdDuration,
    max_response_bytes: u64,
) -> Result<Option<Value>> {
    let Some(endpoint) = read_daemon_query_endpoint(data_root)? else {
        return Ok(None);
    };
    request["token"] = Value::String(endpoint.token().to_owned());
    let request = format!("{}\n", serde_json::to_string(&compact_json(request))?);
    let body = daemon_query_roundtrip(&endpoint, request.as_bytes(), timeout, max_response_bytes)?;
    let response: Value = serde_json::from_str(&body).context("parse daemon query response")?;
    Ok(Some(response))
}

#[cfg(unix)]
fn daemon_query_roundtrip(
    endpoint: &DaemonQueryEndpoint,
    request: &[u8],
    timeout: StdDuration,
    max_response_bytes: u64,
) -> Result<String> {
    let DaemonQueryEndpoint::Unix { path, .. } = endpoint;
    if !path.exists() {
        return Err(anyhow!("daemon query socket does not exist"));
    }
    let mut stream =
        UnixStream::connect(path).with_context(|| format!("connect daemon query socket {}", path.display()))?;
    stream
        .set_read_timeout(Some(timeout))
        .context("set daemon query read timeout")?;
    stream
        .set_write_timeout(Some(timeout))
        .context("set daemon query write timeout")?;
    stream
        .write_all(request)
        .context("write daemon query request")?;
    let _ = stream.shutdown(Shutdown::Write);
    let mut body = String::new();
    stream
        .take(max_response_bytes)
        .read_to_string(&mut body)
        .context("read daemon query response")?;
    Ok(body)
}

#[cfg(not(unix))]
fn daemon_query_roundtrip(
    _endpoint: &DaemonQueryEndpoint,
    _request: &[u8],
    _timeout: StdDuration,
    _max_response_bytes: u64,
) -> Result<String> {
    Err(anyhow!("daemon query service is not supported on this platform"))
}

#[cfg(unix)]
fn read_daemon_query_request<S: Read>(stream: &mut S, max_bytes: usize) -> Result<String> {
    let mut body = Vec::new();
    let mut byte = [0u8; 1];
    while body.len() < max_bytes {
        let read = stream
            .read(&mut byte)
            .context("read daemon query request")?;
        if read == 0 {
            break;
        }
        if byte[0] == b'\n' {
            break;
        }
        body.push(byte[0]);
    }
    if body.len() >= max_bytes {
        return Err(anyhow!("daemon query request is too large"));
    }
    String::from_utf8(body).context("daemon query request is not UTF-8")
}
