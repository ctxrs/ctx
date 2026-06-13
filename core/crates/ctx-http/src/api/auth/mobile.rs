use axum::http::StatusCode;

use ctx_daemon::daemon::AuthHandle;
use ctx_mobile_access_service::MobileAuthContext;

#[path = "mobile/tokens.rs"]
mod tokens;
use tokens::hash_api_token;

pub(super) async fn verify_mobile_api_token(
    state: &AuthHandle,
    token: &str,
) -> Result<Option<MobileAuthContext>, StatusCode> {
    let hash = hash_api_token(token);
    state
        .verify_mobile_api_token_hash(&hash)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}
