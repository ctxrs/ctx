use super::*;

use axum::body::Bytes;
use axum::extract::{FromRequest, Multipart};
use ctx_daemon::daemon::BlobHandle;
use ctx_session_artifacts::infer_session_upload_blob_mime_type;

pub(super) struct ParsedBlobUploadFile {
    pub(super) bytes: Bytes,
    pub(super) mime_type: String,
    pub(super) name: Option<String>,
}

pub(super) async fn parse_blob_upload_file(
    req: Request,
    state: &BlobHandle,
) -> Result<ParsedBlobUploadFile, (StatusCode, Json<ApiErrorResp>)> {
    let mut multipart = Multipart::from_request(req, state)
        .await
        .map_err(|rejection| blob_upload_multipart_rejection_error(rejection.status()))?;
    let mut file_name: Option<String> = None;
    let mut mime_type: Option<String> = None;
    let mut bytes: Option<Bytes> = None;

    while let Some(field) = multipart.next_field().await.map_err(|_| {
        blob_upload_api_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            SESSION_IMAGE_BLOB_TOO_LARGE_MESSAGE,
        )
    })? {
        let name = field.name().map(|s| s.to_string()).unwrap_or_default();
        if name != "file" {
            continue;
        }
        let mut field = field;
        file_name = field.file_name().map(|s| s.to_string());
        mime_type = field.content_type().map(|s| s.to_string());
        let mut field_bytes = Vec::new();
        while let Some(chunk) = field.chunk().await.map_err(|_| {
            blob_upload_api_error(
                StatusCode::PAYLOAD_TOO_LARGE,
                SESSION_IMAGE_BLOB_TOO_LARGE_MESSAGE,
            )
        })? {
            if field_bytes.len().saturating_add(chunk.len()) > MAX_BLOB_BYTES {
                return Err(blob_upload_api_error(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    SESSION_IMAGE_BLOB_TOO_LARGE_MESSAGE,
                ));
            }
            field_bytes.extend_from_slice(&chunk);
        }
        bytes = Some(Bytes::from(field_bytes));
        break;
    }

    let Some(bytes) = bytes else {
        return Err(blob_upload_api_error(
            StatusCode::BAD_REQUEST,
            "Image attachment upload requires a file field.",
        ));
    };
    let mime_type = infer_session_upload_blob_mime_type(file_name.as_deref(), mime_type);
    Ok(ParsedBlobUploadFile {
        bytes,
        mime_type,
        name: file_name,
    })
}
