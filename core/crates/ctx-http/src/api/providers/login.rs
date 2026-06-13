use super::*;
use crate::api::MobileAuthContext;
use axum::Extension;

mod browser;
#[cfg(test)]
mod callback;
mod claude;
mod codex;
mod kimi;
mod mistral;

pub(crate) use browser::{
    get_amp_login, get_gemini_login, get_qwen_login, start_amp_login, start_gemini_login,
    start_qwen_login,
};
#[cfg(test)]
pub(super) use callback::{expected_callback_from_auth_url, validate_callback_url};
pub(crate) use claude::{get_claude_login, start_claude_login};
pub(crate) use codex::{complete_codex_login, get_codex_login, start_codex_login};
pub(crate) use kimi::{get_kimi_login, start_kimi_login};
pub(crate) use mistral::{get_mistral_login, start_mistral_login};

pub(super) fn reject_mobile_auth(
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

pub(super) fn provider_login_route_error(
    error: ProviderLoginRouteError,
) -> (StatusCode, Json<ApiErrorResp>) {
    let status = match error.kind() {
        ProviderLoginRouteErrorKind::NotFound => StatusCode::NOT_FOUND,
        ProviderLoginRouteErrorKind::BadGateway => StatusCode::BAD_GATEWAY,
    };
    (
        status,
        Json(ApiErrorResp {
            error: error.message().to_string(),
        }),
    )
}
