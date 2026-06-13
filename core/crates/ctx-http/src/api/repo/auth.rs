use super::*;

pub(in crate::api::repo) fn reject_mobile_auth(
    mobile_auth: Option<Extension<MobileAuthContext>>,
) -> Result<(), (StatusCode, Json<ApiErrorResp>)> {
    if mobile_auth.is_some() {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(ApiErrorResp {
                error: "desktop auth required".to_string(),
            }),
        ));
    }
    Ok(())
}
