use anyhow::{anyhow, Context, Result};
use reqwest::{header, multipart, Method};

use ctx_core::ids::{ArtifactId, SessionId};
use ctx_core::models::Artifact;

use crate::client::Client;
use crate::types::BlobUploadResp;

impl Client {
    pub async fn list_session_artifacts(&self, session_id: SessionId) -> Result<Vec<Artifact>> {
        let path = format!("/api/sessions/{}/artifacts", session_id.0);
        self.request_json(Method::GET, &path, None::<&()>).await
    }

    pub async fn get_artifact_bytes(
        &self,
        session_id: SessionId,
        artifact_id: ArtifactId,
        range: Option<(u64, u64)>,
    ) -> Result<Vec<u8>> {
        let path = format!("/api/sessions/{}/artifacts/{}", session_id.0, artifact_id.0);
        let url = self.url_for(&path)?;
        let mut req = self.http.request(Method::GET, url);
        if let Some(token) = &self.auth_token {
            req = req.bearer_auth(token);
        }
        if let Some((start, end)) = range {
            let value = format!("bytes={start}-{end}");
            req = req.header(header::RANGE, value);
        }
        let resp = req.send().await.context("sending request")?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.context("reading response body")?;
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
        let bytes = resp.bytes().await.context("reading response body")?;
        Ok(bytes.to_vec())
    }

    pub async fn upload_blob(
        &self,
        bytes: Vec<u8>,
        mime_type: &str,
        name: Option<&str>,
    ) -> Result<BlobUploadResp> {
        let url = self.url_for("/api/blobs")?;
        let mut req = self.http.request(Method::POST, url);
        if let Some(token) = &self.auth_token {
            req = req.bearer_auth(token);
        }

        let mut part = multipart::Part::bytes(bytes)
            .mime_str(mime_type)
            .context("invalid blob mime type")?;
        if let Some(name) = name {
            part = part.file_name(name.to_string());
        }
        let form = multipart::Form::new().part("file", part);
        let resp = req
            .multipart(form)
            .send()
            .await
            .context("sending request")?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.context("reading response body")?;
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
        let text = resp.text().await.context("reading response body")?;
        let resp = if text.trim().is_empty() {
            return Err(anyhow!("empty response when uploading blob"));
        } else {
            serde_json::from_str::<BlobUploadResp>(&text).context("decoding blob response")?
        };
        Ok(resp)
    }

    pub async fn get_blob(&self, blob_id: &str) -> Result<Vec<u8>> {
        let path = format!("/api/blobs/{blob_id}");
        let url = self.url_for(&path)?;
        let mut req = self.http.request(Method::GET, url);
        if let Some(token) = &self.auth_token {
            req = req.bearer_auth(token);
        }
        let resp = req.send().await.context("sending request")?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.context("reading response body")?;
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
        let bytes = resp.bytes().await.context("reading response body")?;
        Ok(bytes.to_vec())
    }
}
