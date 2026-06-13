use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::{header, StatusCode};
use axum::response::Response;
use axum::Json;
use ctx_session_artifacts::{
    StoredImageBlob, SESSION_IMAGE_BLOB_MAX_BYTES, SESSION_IMAGE_BLOB_MULTIPART_MAX_BYTES,
    SESSION_IMAGE_BLOB_TOO_LARGE_MESSAGE,
};
use serde::Serialize;
use tokio_util::io::ReaderStream;

use super::super::errors::ApiErrorResp;
use ctx_daemon::daemon::BlobHandle;

#[path = "blob/errors.rs"]
mod errors;
#[path = "blob/upload.rs"]
mod upload;

use errors::{
    blob_read_status, blob_upload_api_error, blob_upload_multipart_rejection_error,
    blob_upload_store_error,
};
use upload::parse_blob_upload_file;

#[derive(Debug, Serialize)]
pub(in crate::api) struct BlobUploadResp {
    pub(in crate::api) blob_id: String,
    pub(in crate::api) sha256: String,
    pub(in crate::api) bytes: i64,
    pub(in crate::api) mime_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(in crate::api) name: Option<String>,
}

impl From<StoredImageBlob> for BlobUploadResp {
    fn from(blob: StoredImageBlob) -> Self {
        Self {
            blob_id: blob.blob_id,
            sha256: blob.sha256,
            bytes: blob.bytes,
            mime_type: blob.mime_type,
            name: blob.name,
        }
    }
}

pub(super) const MAX_BLOB_BYTES: usize = SESSION_IMAGE_BLOB_MAX_BYTES;
pub(in crate::api) const MAX_BLOB_MULTIPART_BODY_BYTES: usize =
    SESSION_IMAGE_BLOB_MULTIPART_MAX_BYTES;

pub(in crate::api) async fn upload_blob(
    State(state): State<BlobHandle>,
    req: Request,
) -> Result<Json<BlobUploadResp>, (StatusCode, Json<ApiErrorResp>)> {
    let file = parse_blob_upload_file(req, &state).await?;
    let resp = state
        .store_image_blob(&file.bytes, &file.mime_type, file.name.as_deref())
        .await
        .map(BlobUploadResp::from)
        .map_err(blob_upload_store_error)?;
    Ok(Json(resp))
}

pub(in crate::api) async fn get_blob(
    State(state): State<BlobHandle>,
    Path(id): Path<String>,
) -> Result<Response, StatusCode> {
    let opened = state
        .open_blob_for_read(&id)
        .await
        .map_err(blob_read_status)?;
    let stream = ReaderStream::new(opened.file);
    let mut resp = Response::new(Body::from_stream(stream));
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        opened
            .mime_type
            .parse()
            .unwrap_or_else(|_| header::HeaderValue::from_static("application/octet-stream")),
    );
    if let Some(name) = opened.name {
        let value = format!("inline; filename=\"{}\"", name.replace('"', ""));
        if let Ok(v) = value.parse() {
            resp.headers_mut().insert(header::CONTENT_DISPOSITION, v);
        }
    }
    Ok(resp)
}
