use super::*;

#[allow(dead_code)]
pub(super) async fn session_create_call(
    client: &reqwest::Client,
    daemon_url: &str,
    arguments: &Value,
) -> Result<Value> {
    let kind = arguments.get("kind").and_then(|v| v.as_str()).unwrap_or("");
    if kind != "web" {
        anyhow::bail!("unsupported session kind: {kind}");
    }
    let target = arguments.get("target").context("missing target")?;
    let url = target
        .get("url")
        .and_then(|v| v.as_str())
        .context("missing target.url")?;
    let session_id = ctx_env_opt("SESSION_ID").context("missing session context")?;
    let viewport = arguments.get("viewport");
    let fps = arguments.get("fps");

    let mut body = serde_json::Map::new();
    body.insert("url".to_string(), json!(url));
    body.insert("session_id".to_string(), json!(session_id));
    if let Some(viewport) = viewport {
        body.insert("viewport".to_string(), viewport.clone());
    }
    if let Some(fps) = fps {
        body.insert("fps".to_string(), fps.clone());
    }

    let mut response = daemon_post_json(
        client,
        daemon_url,
        "/api/sessions/web",
        &Value::Object(body),
    )
    .await?;
    map_interactive_session(&mut response);
    Ok(response)
}

#[allow(dead_code)]
pub(super) async fn session_list_call(
    client: &reqwest::Client,
    daemon_url: &str,
    arguments: &Value,
) -> Result<Value> {
    let session_id = ctx_env_opt("SESSION_ID").context("missing session context")?;
    if let Some(kind) = arguments.get("kind").and_then(|v| v.as_str()) {
        if kind != "web" {
            return Ok(json!([]));
        }
    }
    let mut response = daemon_get_json(
        client,
        daemon_url,
        &format!("/api/sessions/web?session_id={session_id}"),
    )
    .await?;
    map_interactive_sessions(&mut response);
    Ok(response)
}

#[allow(dead_code)]
pub(super) async fn session_info_call(
    client: &reqwest::Client,
    daemon_url: &str,
    arguments: &Value,
) -> Result<Value> {
    let session_ref = arguments
        .get("session_ref")
        .and_then(|v| v.as_str())
        .context("missing session_ref")?;
    let session_id = session_id_for_ref(session_ref).context("unknown session_ref")?;
    daemon_get_json(
        client,
        daemon_url,
        &format!("/api/sessions/web/{session_id}"),
    )
    .await
    .map(|mut response| {
        map_interactive_session(&mut response);
        response
    })
}

#[allow(dead_code)]
pub(super) async fn session_run_call(
    client: &reqwest::Client,
    daemon_url: &str,
    arguments: &Value,
    is_eval: bool,
) -> Result<Value> {
    let session_ref = arguments
        .get("session_ref")
        .and_then(|v| v.as_str())
        .context("missing session_ref")?;
    let session_id = session_id_for_ref(session_ref).context("unknown session_ref")?;
    let mut body = serde_json::Map::new();
    if let Some(code) = arguments.get("code") {
        body.insert("code".to_string(), code.clone());
    }
    if let Some(script_path) = arguments.get("script_path") {
        body.insert("script_path".to_string(), script_path.clone());
    }
    if let Some(timeout_ms) = arguments.get("timeout_ms") {
        body.insert("timeout_ms".to_string(), timeout_ms.clone());
    }
    let endpoint = if is_eval { "eval" } else { "run" };
    daemon_post_json(
        client,
        daemon_url,
        &format!("/api/sessions/web/{session_id}/{endpoint}"),
        &Value::Object(body),
    )
    .await
}

#[allow(dead_code)]
pub(super) async fn session_close_call(
    client: &reqwest::Client,
    _daemon_url: &str,
    arguments: &Value,
) -> Result<Value> {
    let session_ref = arguments
        .get("session_ref")
        .and_then(|v| v.as_str())
        .context("missing session_ref")?;
    let session_id = session_id_for_ref(session_ref).context("unknown session_ref")?;
    let access = resolve_daemon_access()?;
    let url = format!(
        "{}/api/sessions/web/{}/close",
        access.daemon_url.trim_end_matches('/'),
        session_id
    );
    let req = client.post(url).bearer_auth(access.auth_token);
    let res = req.send().await?.error_for_status()?;
    if res.status().as_u16() == 204 {
        return Ok(json!({"closed": true}));
    }
    Ok(res.json::<Value>().await?)
}
