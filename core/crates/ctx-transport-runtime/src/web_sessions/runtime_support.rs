use super::*;

pub(super) async fn build_run_payload(
    handle: &WebSessionHandle,
    req: WebSessionRunRequest,
) -> Result<serde_json::Value> {
    let mut payload = serde_json::Map::new();
    if let Some(code) = req.code {
        payload.insert("code".to_string(), serde_json::Value::String(code));
    }
    if let Some(script_path) = req.script_path {
        let resolved = resolve_script_path(handle, &script_path).await?;
        payload.insert(
            "script_path".to_string(),
            serde_json::Value::String(resolved.to_string_lossy().to_string()),
        );
    }
    if let Some(timeout_ms) = req.timeout_ms {
        payload.insert(
            "timeout_ms".to_string(),
            serde_json::Value::Number(timeout_ms.into()),
        );
    }
    Ok(serde_json::Value::Object(payload))
}

pub(super) async fn resolve_script_path(
    handle: &WebSessionHandle,
    script_path: &str,
) -> Result<PathBuf> {
    let candidate = PathBuf::from(script_path);
    let work_dir = handle.work_dir().await;
    if candidate.is_absolute() {
        if work_dir.is_none() {
            return Ok(candidate);
        }
        anyhow::bail!("script_path must be relative to work_dir");
    }
    let work_dir = work_dir.context("script_path requires work_dir")?;
    let canonical_work_dir = work_dir
        .canonicalize()
        .with_context(|| format!("failed to resolve work_dir {}", work_dir.display()))?;
    let joined = work_dir.join(candidate);
    let canonical = joined
        .canonicalize()
        .with_context(|| format!("failed to resolve script_path {script_path}"))?;
    if !canonical.starts_with(&canonical_work_dir) {
        anyhow::bail!("script_path must be inside work_dir");
    }
    Ok(canonical)
}

pub(super) fn build_stream_path(id: &str) -> String {
    format!("/sessions/web/{id}/view")
}

pub(super) fn build_stream_connect_path(id: &str, stream_token: &str) -> String {
    format!("/sessions/web/{id}/view?token={stream_token}")
}

pub(super) fn build_signal_connect_path(id: &str, stream_token: &str) -> String {
    format!("/sessions/web/{id}/signal?token={stream_token}")
}

pub(super) fn allocate_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0").context("binding port")?;
    let port = listener.local_addr()?.port();
    Ok(port)
}

pub(super) async fn log_stream<R: tokio::io::AsyncRead + Unpin>(mut reader: R, label: &str) {
    use tokio::io::AsyncReadExt;

    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let chunk = String::from_utf8_lossy(&buf[..n]);
                for line in chunk.split('\n') {
                    if !line.trim().is_empty() {
                        tracing::info!("[{label}] {line}");
                    }
                }
            }
        }
    }
}
