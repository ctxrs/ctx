use super::ApiErrorResp;
use axum::body::Bytes;
use axum::Json;
use http::StatusCode;

pub(super) fn parse_json_body<T: serde::de::DeserializeOwned>(
    body: Bytes,
) -> Result<T, (StatusCode, Json<ApiErrorResp>)> {
    if body.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiErrorResp {
                error: "missing request body".into(),
            }),
        ));
    }
    serde_json::from_slice(&body).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(ApiErrorResp {
                error: "invalid json body".into(),
            }),
        )
    })
}
