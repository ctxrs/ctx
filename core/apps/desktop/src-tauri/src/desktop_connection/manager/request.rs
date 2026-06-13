use super::*;

fn blob_upload_failure_message(status: reqwest::StatusCode, body: &str) -> String {
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(body.trim()) {
        for key in ["error", "message"] {
            if let Some(message) = parsed.get(key).and_then(|value| value.as_str()) {
                let message = message.trim();
                if !message.is_empty() {
                    return message.to_string();
                }
            }
        }
    }
    if status == reqwest::StatusCode::PAYLOAD_TOO_LARGE {
        return "Image attachments must be 25 MiB or smaller.".to_string();
    }
    let trimmed = body.trim();
    if !trimmed.is_empty() {
        return trimmed.to_string();
    }
    status
        .canonical_reason()
        .map(|reason| reason.to_string())
        .unwrap_or_else(|| "the daemon rejected the image attachment upload".to_string())
}

fn is_disallowed_forward_header(name: &str) -> bool {
    name.trim().eq_ignore_ascii_case("authorization")
}

impl ConnectionManager {
    pub(crate) fn daemon_request(&self, req: DesktopDaemonRequest) -> Result<DesktopHttpResponse> {
        self.daemon_request_for_scope(DEFAULT_CONNECTION_SCOPE, req)
    }

    pub(crate) fn daemon_request_for_scope(
        &self,
        scope: &str,
        req: DesktopDaemonRequest,
    ) -> Result<DesktopHttpResponse> {
        if !req.path.starts_with("/api/") {
            return Err(anyhow!("only /api/* paths are supported"));
        }

        let (base_url, token, client) = {
            let guard = self
                .0
                .lock()
                .map_err(|e| anyhow!("connection manager lock poisoned: {e}"))?;
            let scoped = guard.scope(scope);
            let active = scoped
                .active
                .ok_or_else(|| anyhow!("not connected (open a workspace first)"))?;
            match active {
                ActiveConnection::Local(c) => (
                    c.base_url.clone(),
                    Some(c.token.clone()),
                    get_connection_http_client(&c.http_client)?,
                ),
                ActiveConnection::Ssh(c) => (
                    c.base_url.clone(),
                    c.token.clone(),
                    get_connection_http_client(&c.http_client)?,
                ),
            }
        };
        if token
            .as_deref()
            .map(|value| value.trim().is_empty())
            .unwrap_or(true)
        {
            return Err(anyhow!(
                "remote desktop daemon auth token is missing; reconnect the remote daemon"
            ));
        }

        let url = format!("{}{}", base_url.trim_end_matches('/'), req.path);
        let method = req.method.trim().to_uppercase();
        let mut builder = match method.as_str() {
            "GET" => client.get(&url),
            "POST" => client.post(&url),
            "DELETE" => client.delete(&url),
            "PUT" => client.put(&url),
            "PATCH" => client.patch(&url),
            other => return Err(anyhow!("unsupported method: {other}")),
        }
        .timeout(Duration::from_secs(10 * 60));

        if let Some(t) = token.as_deref() {
            if !t.trim().is_empty() {
                builder = builder.bearer_auth(t);
            }
        }
        for (k, v) in req.headers {
            if is_disallowed_forward_header(&k) {
                return Err(anyhow!(
                    "desktop daemon request cannot override authorization header"
                ));
            }
            builder = builder.header(k, v);
        }
        if let Some(body) = req.body {
            builder = builder.body(body);
        }
        let res = builder
            .send()
            .with_context(|| format!("sending request {method} {url}"))?;
        let status = res.status().as_u16();
        let content_type = res
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let body = res.text().unwrap_or_default();
        Ok(DesktopHttpResponse {
            status,
            body,
            content_type,
        })
    }

    #[cfg(test)]
    pub(crate) fn upload_blob(
        &self,
        bytes: Vec<u8>,
        mime_type: String,
        name: Option<String>,
    ) -> Result<serde_json::Value> {
        self.upload_blob_for_scope(DEFAULT_CONNECTION_SCOPE, bytes, mime_type, name)
    }

    pub(crate) fn upload_blob_for_scope(
        &self,
        scope: &str,
        bytes: Vec<u8>,
        mime_type: String,
        name: Option<String>,
    ) -> Result<serde_json::Value> {
        let (base_url, token, client) = {
            let guard = self
                .0
                .lock()
                .map_err(|e| anyhow!("connection manager lock poisoned: {e}"))?;
            let scoped = guard.scope(scope);
            let active = scoped
                .active
                .ok_or_else(|| anyhow!("not connected (open a workspace first)"))?;
            match active {
                ActiveConnection::Local(c) => (
                    c.base_url.clone(),
                    Some(c.token.clone()),
                    get_connection_http_client(&c.http_client)?,
                ),
                ActiveConnection::Ssh(c) => (
                    c.base_url.clone(),
                    c.token.clone(),
                    get_connection_http_client(&c.http_client)?,
                ),
            }
        };
        if token
            .as_deref()
            .map(|value| value.trim().is_empty())
            .unwrap_or(true)
        {
            return Err(anyhow!(
                "remote desktop daemon auth token is missing; reconnect the remote daemon"
            ));
        }
        let url = format!("{}/api/blobs", base_url.trim_end_matches('/'));
        let mut part = reqwest::blocking::multipart::Part::bytes(bytes);
        if let Some(n) = name.as_deref().filter(|s| !s.trim().is_empty()) {
            part = part.file_name(n.to_string());
        }
        part = part
            .mime_str(&mime_type)
            .context("invalid mime_type for multipart")?;
        let form = reqwest::blocking::multipart::Form::new().part("file", part);
        let mut req = client
            .post(&url)
            .timeout(Duration::from_secs(60))
            .multipart(form);
        if let Some(t) = token.as_deref() {
            if !t.trim().is_empty() {
                req = req.bearer_auth(t);
            }
        }
        let res = req
            .send()
            .with_context(|| format!("uploading image attachment to {url}"))?;
        let status = res.status();
        let body = res.text().unwrap_or_default();
        if !status.is_success() {
            let message = blob_upload_failure_message(status, &body);
            return Err(anyhow!(
                "image attachment upload failed ({status}): {message}"
            ));
        }
        Ok(serde_json::from_str(&body).context("parsing blob upload response")?)
    }
}
