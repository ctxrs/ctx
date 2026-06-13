use super::types::RegisterMobileDeviceReq;
use super::*;

pub(in crate::api) async fn list_mobile_devices_for_profile(
    State(state): State<MobileStoreHandle>,
    mobile_auth: Option<Extension<MobileAuthContext>>,
    Path(id): Path<String>,
) -> Result<Json<Vec<MobileDeviceRegistration>>, StatusCode> {
    if mobile_auth.is_some() {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let devices = state
        .list_mobile_devices_for_profile_for_route_params(MobileConnectionProfileRouteParams::new(
            id,
        ))
        .await
        .map_err(|error| mobile_access_status_code(&error))?;
    Ok(Json(devices))
}

pub(in crate::api) async fn register_mobile_device(
    State(state): State<MobileStoreHandle>,
    auth: Option<Extension<MobileAuthContext>>,
    Json(req): Json<RegisterMobileDeviceReq>,
) -> Result<Json<MobileDeviceRegistration>, (StatusCode, Json<ApiErrorResp>)> {
    let Some(Extension(mobile_auth)) = auth else {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(ApiErrorResp {
                error: "mobile token required".into(),
            }),
        ));
    };
    let device = state
        .register_mobile_device_for_route(
            mobile_auth,
            RegisterMobileDeviceForRouteRequest {
                device_id: req.device_id,
                device_label: req.device_label,
                platform: req.platform,
                push_token: req.push_token,
                push_provider: req.push_provider,
                public_key: req.public_key,
                app_version: req.app_version,
            },
        )
        .await
        .map_err(mobile_access_api_error)?;
    Ok(Json(device))
}
