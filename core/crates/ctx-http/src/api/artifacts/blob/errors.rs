use super::*;
use ctx_session_artifacts::{BlobReadError, ImageBlobStoreError};

pub(super) fn blob_upload_api_error(
    status: StatusCode,
    error: impl Into<String>,
) -> (StatusCode, Json<ApiErrorResp>) {
    (
        status,
        Json(ApiErrorResp {
            error: error.into(),
        }),
    )
}

pub(super) fn blob_upload_store_error(
    error: ImageBlobStoreError,
) -> (StatusCode, Json<ApiErrorResp>) {
    match error {
        ImageBlobStoreError::PayloadTooLarge => blob_upload_api_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            SESSION_IMAGE_BLOB_TOO_LARGE_MESSAGE,
        ),
        ImageBlobStoreError::UnsupportedMediaType => blob_upload_api_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Only image attachments are supported.",
        ),
        ImageBlobStoreError::Internal => blob_upload_api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to store image attachment.",
        ),
    }
}

pub(super) fn blob_read_status(error: BlobReadError) -> StatusCode {
    match error {
        BlobReadError::NotFound => StatusCode::NOT_FOUND,
        BlobReadError::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

pub(super) fn blob_upload_multipart_rejection_error(
    status: StatusCode,
) -> (StatusCode, Json<ApiErrorResp>) {
    if status == StatusCode::PAYLOAD_TOO_LARGE {
        return blob_upload_api_error(status, SESSION_IMAGE_BLOB_TOO_LARGE_MESSAGE);
    }
    blob_upload_api_error(
        StatusCode::BAD_REQUEST,
        "Image attachment upload was not valid multipart form data.",
    )
}
