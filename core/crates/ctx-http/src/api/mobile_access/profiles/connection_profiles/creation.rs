use super::*;

pub(in crate::api) async fn create_mobile_connection_profile(
    State(state): State<MobileStoreHandle>,
    mobile_auth: Option<Extension<MobileAuthContext>>,
    Json(req): Json<CreateMobileConnectionProfileReq>,
) -> Result<Json<CreateMobileConnectionProfileResp>, (StatusCode, Json<ApiErrorResp>)> {
    if mobile_auth.is_some() {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(ApiErrorResp {
                error: "desktop auth required".into(),
            }),
        ));
    }
    let result = state
        .create_mobile_connection_profile_for_route(CreateMobileConnectionProfileForRouteRequest {
            label: req.label,
            base_url: req.base_url,
            scopes: req.scopes,
        })
        .await
        .map_err(mobile_access_api_error)?;
    Ok(Json(CreateMobileConnectionProfileResp {
        profile: result.profile,
        token: result.token,
        qr_payload: result.qr_payload,
    }))
}
