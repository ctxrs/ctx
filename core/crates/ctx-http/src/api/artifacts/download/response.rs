use std::io::SeekFrom;

use axum::body::Body;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

mod headers;

pub(in crate::api) struct SessionArtifactDownloadMetadata<'a> {
    pub(in crate::api) size: u64,
    pub(in crate::api) etag: Option<&'a str>,
    pub(in crate::api) last_modified: Option<&'a str>,
    pub(in crate::api) mime_type: &'a str,
    pub(in crate::api) name: Option<&'a str>,
}

pub(in crate::api) async fn build_session_artifact_download_response(
    request_headers: HeaderMap,
    mut file: File,
    metadata: SessionArtifactDownloadMetadata<'_>,
) -> Result<Response, StatusCode> {
    let range_decision = match headers::session_artifact_range_decision(&request_headers, &metadata)
    {
        headers::SessionArtifactRangeDecision::NotModified(response) => return Ok(response),
        headers::SessionArtifactRangeDecision::RangeNotSatisfiable(response) => {
            return Ok(response);
        }
        headers::SessionArtifactRangeDecision::Full => None,
        headers::SessionArtifactRangeDecision::Partial { start, end } => Some((start, end)),
    };

    let (status, body, content_length, content_range) = if let Some((start, end)) = range_decision {
        file.seek(SeekFrom::Start(start))
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let len = end.saturating_sub(start).saturating_add(1);
        let stream = ReaderStream::new(file.take(len));
        (
            StatusCode::PARTIAL_CONTENT,
            Body::from_stream(stream),
            len,
            Some(format!("bytes {start}-{end}/{}", metadata.size)),
        )
    } else {
        let stream = ReaderStream::new(file);
        (
            StatusCode::OK,
            Body::from_stream(stream),
            metadata.size,
            None,
        )
    };

    let mut resp = Response::new(body);
    *resp.status_mut() = status;
    headers::apply_session_artifact_download_headers(
        resp.headers_mut(),
        &metadata,
        content_length,
        content_range.as_deref(),
    );
    Ok(resp)
}
