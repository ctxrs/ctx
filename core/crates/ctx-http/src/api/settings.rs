use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;

use ctx_daemon::daemon::SettingsHandle;
use ctx_settings_model as user_settings;
use ctx_settings_service::route_contract::{SettingsRouteError, SettingsRouteErrorKind};

fn settings_route_status(error: SettingsRouteError) -> StatusCode {
    match error.kind() {
        SettingsRouteErrorKind::Forbidden => StatusCode::FORBIDDEN,
        SettingsRouteErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

pub(super) async fn get_settings(
    State(state): State<SettingsHandle>,
) -> Result<Json<user_settings::PublicSettings>, StatusCode> {
    state
        .settings_snapshot_for_response()
        .await
        .map(Json)
        .map_err(settings_route_status)
}

pub(super) async fn update_settings(
    State(state): State<SettingsHandle>,
    Json(req): Json<user_settings::UpdateSettingsReq>,
) -> Result<Json<user_settings::PublicSettings>, StatusCode> {
    state
        .update_settings_for_request(req)
        .await
        .map(Json)
        .map_err(settings_route_status)
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    use ctx_settings_service::EXECUTION_POLICY_TEST_ENV_LOCK;

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, previous }
        }

        fn remove(key: &'static str) -> Self {
            let previous = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(value) = &self.previous {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    #[tokio::test]
    async fn update_settings_rejects_host_execution_when_sandbox_only_policy_is_set() {
        let _env_guard = EXECUTION_POLICY_TEST_ENV_LOCK.lock().await;
        let _policy = EnvVarGuard::set("CTX_HOST_EXECUTION_POLICY", "sandbox_only");
        let _mode = EnvVarGuard::remove("CTX_EXECUTION_MODE");
        let fixture = crate::test_support::TestDaemonFixture::new("http://127.0.0.1:4310").await;
        let req = serde_json::from_value::<user_settings::UpdateSettingsReq>(json!({
            "execution": {
                "mode": "host"
            }
        }))
        .expect("settings update request");

        let err = update_settings(State(fixture.settings()), Json(req))
            .await
            .expect_err("sandbox-only policy should reject host execution settings update");

        assert_eq!(err, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn update_settings_persists_and_returns_public_settings() {
        let _env_guard = EXECUTION_POLICY_TEST_ENV_LOCK.lock().await;
        let _policy = EnvVarGuard::remove("CTX_HOST_EXECUTION_POLICY");
        let fixture = crate::test_support::TestDaemonFixture::new("http://127.0.0.1:4310").await;
        let endpoint = "https://telemetry.example/functions/v1/telemetry";
        let req = serde_json::from_value::<user_settings::UpdateSettingsReq>(json!({
            "telemetry": {
                "enabled": false,
                "endpoint": endpoint
            }
        }))
        .expect("settings update request");

        let Json(public) = update_settings(State(fixture.settings()), Json(req))
            .await
            .expect("settings update should succeed");

        let telemetry = public.telemetry.expect("public telemetry settings");
        assert!(!telemetry.enabled);
        assert_eq!(telemetry.endpoint, endpoint);
        let persisted = fixture
            .settings()
            .load_settings()
            .await
            .expect("persisted settings");
        let persisted_telemetry = persisted.telemetry.expect("persisted telemetry settings");
        assert!(!persisted_telemetry.enabled);
        assert_eq!(persisted_telemetry.endpoint, endpoint);
    }

    #[tokio::test]
    async fn update_settings_returns_internal_server_error_for_invalid_host_execution_policy() {
        let _env_guard = EXECUTION_POLICY_TEST_ENV_LOCK.lock().await;
        let _policy = EnvVarGuard::set("CTX_HOST_EXECUTION_POLICY", "invalid");
        let fixture = crate::test_support::TestDaemonFixture::new("http://127.0.0.1:4310").await;
        let req = serde_json::from_value::<user_settings::UpdateSettingsReq>(json!({
            "telemetry": {
                "enabled": true,
                "endpoint": "https://telemetry.example/functions/v1/telemetry"
            }
        }))
        .expect("settings update request");

        let err = update_settings(State(fixture.settings()), Json(req))
            .await
            .expect_err("invalid host execution policy should fail settings update");

        assert_eq!(err, StatusCode::INTERNAL_SERVER_ERROR);
    }
}
