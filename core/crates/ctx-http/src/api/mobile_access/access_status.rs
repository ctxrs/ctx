use super::*;
pub(in crate::api) async fn get_mobile_access_status(
    State(state): State<MobileRuntimeHandle>,
    mobile_auth: Option<Extension<MobileAuthContext>>,
) -> Result<Json<MobileAccessStatus>, StatusCode> {
    if mobile_auth.is_some() {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let snapshot = state
        .mobile_access_status()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(mobile_access_status_from_snapshot(snapshot)))
}
