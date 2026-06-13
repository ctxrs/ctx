use axum::http::StatusCode;
use axum::Json;

use ctx_provider_install::{ProviderInstallJsonRouteError, ProviderInstallJsonRouteErrorStatus};

pub(in crate::api::provider_launch) fn provider_install_error_response(
    error: ProviderInstallJsonRouteError,
) -> (StatusCode, Json<serde_json::Value>) {
    let status = match error.status() {
        ProviderInstallJsonRouteErrorStatus::BadRequest => StatusCode::BAD_REQUEST,
        ProviderInstallJsonRouteErrorStatus::Forbidden => StatusCode::FORBIDDEN,
    };
    (status, Json(error.body().clone()))
}

#[cfg(test)]
mod tests {
    use super::provider_install_error_response;
    use axum::http::StatusCode;
    use ctx_provider_install::{
        ProviderInstallJsonRouteError, ProviderInstallJsonRouteErrorStatus,
    };

    #[test]
    fn provider_install_error_response_maps_disabled_install_targets_to_forbidden() {
        let (status, body) = provider_install_error_response(ProviderInstallJsonRouteError::new(
            ProviderInstallJsonRouteErrorStatus::Forbidden,
            serde_json::json!({
                "error": "host provider installs are disabled by daemon policy",
                "code": "install_target_disabled",
            }),
        ));

        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body.0["code"], "install_target_disabled");
    }
}
