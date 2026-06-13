use super::*;

pub(in crate::api) async fn enable_mobile_access(
    State(state): State<MobileRuntimeHandle>,
    mobile_auth: Option<Extension<MobileAuthContext>>,
    Json(_req): Json<EnableMobileAccessReq>,
) -> Result<Json<EnableMobileAccessResp>, (StatusCode, Json<ApiErrorResp>)> {
    if mobile_auth.is_some() {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(ApiErrorResp {
                error: "desktop auth required".into(),
            }),
        ));
    }
    let result = state
        .enable_mobile_access_for_route(EnableMobileAccessRequest {})
        .await
        .map_err(mobile_access_api_error)?;

    Ok(Json(EnableMobileAccessResp {
        status: mobile_access_status_from_snapshot(result.status),
        qr_payload: result.qr_payload,
        pairing_expires_at: result.pairing_expires_at.to_rfc3339(),
    }))
}
