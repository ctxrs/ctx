use super::*;
use ctx_mobile_access_service::route_contract::DisableMobileAccessError;

pub(in crate::api) async fn disable_mobile_access(
    State(state): State<MobileRuntimeHandle>,
    mobile_auth: Option<Extension<MobileAuthContext>>,
    Json(_req): Json<EnableMobileAccessReq>,
) -> Result<StatusCode, (StatusCode, Json<ApiErrorResp>)> {
    if mobile_auth.is_some() {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(ApiErrorResp {
                error: "desktop auth required".into(),
            }),
        ));
    }

    state
        .disable_mobile_access_for_route(EnableMobileAccessRequest {})
        .await
        .map_err(disable_mobile_access_error)?;
    Ok(StatusCode::NO_CONTENT)
}

fn disable_mobile_access_error(
    error: DisableMobileAccessError,
) -> (StatusCode, Json<ApiErrorResp>) {
    let message = match error {
        DisableMobileAccessError::ReadConfig => "failed to read mobile access config",
        DisableMobileAccessError::DisableConfig => "failed to disable mobile access config",
        DisableMobileAccessError::ClearPairingTokens => "failed to clear pairing tokens",
        DisableMobileAccessError::DeleteConfig => "failed to delete mobile access config",
        DisableMobileAccessError::DeleteConnectionProfile => {
            "failed to delete mobile connection profile"
        }
    };
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ApiErrorResp {
            error: message.into(),
        }),
    )
}
