use std::path::{Path as StdPath, PathBuf};

use anyhow::Context;
use ctx_observability::logs;
use ctx_provider_accounts::{
    CursorLoginRouteError, CursorLoginRouteErrorKind, CursorLoginStartRouteRequest,
    CursorLoginStartRouteResponse, CursorLoginStatusRouteResponse,
};

use crate::daemon::providers::login_deps::ProviderLoginDeps;
use crate::daemon::providers::{login_runtime, login_sessions, StartedLoginSession};
use crate::daemon::ProviderAccountsHandle;

mod auth_url;
mod capture;
mod output;
mod session;
#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CursorProcessLoginStartErrorKind {
    RuntimeCommandBadRequest,
    InternalStartup,
}

#[derive(Debug)]
struct CursorProcessLoginStartError {
    kind: CursorProcessLoginStartErrorKind,
    message: String,
}

impl CursorProcessLoginStartError {
    fn from_runtime_error(err: anyhow::Error) -> Self {
        let raw_message = err.to_string();
        let kind = if raw_message.contains("runtime_command_") {
            CursorProcessLoginStartErrorKind::RuntimeCommandBadRequest
        } else {
            CursorProcessLoginStartErrorKind::InternalStartup
        };
        let message = logs::redact_sensitive(&raw_message);
        Self { kind, message }
    }

    fn kind(&self) -> CursorProcessLoginStartErrorKind {
        self.kind
    }

    fn route_safe_message(&self) -> &str {
        &self.message
    }
}

impl ProviderAccountsHandle {
    pub async fn start_cursor_login_for_route(
        &self,
        request: CursorLoginStartRouteRequest,
    ) -> Result<CursorLoginStartRouteResponse, CursorLoginRouteError> {
        start_cursor_process_login(
            ProviderLoginDeps::from_accounts_handle(self),
            request.into_label(),
        )
        .await
        .map(cursor_login_start_route_response)
        .map_err(cursor_login_start_route_error)
    }

    pub async fn cursor_login_status_for_route(
        &self,
        login_id: &str,
    ) -> Result<CursorLoginStatusRouteResponse, CursorLoginRouteError> {
        login_sessions::cursor_login_status(self.providers(), login_id)
            .await
            .map(Into::into)
            .ok_or_else(cursor_login_not_found_route_error)
    }
}

fn cursor_login_start_route_response(
    session: StartedLoginSession,
) -> CursorLoginStartRouteResponse {
    CursorLoginStartRouteResponse::new(session.login_id, session.auth_url)
}

fn cursor_login_not_found_route_error() -> CursorLoginRouteError {
    CursorLoginRouteError::not_found("login not found")
}

fn cursor_login_start_route_error(error: CursorProcessLoginStartError) -> CursorLoginRouteError {
    let kind = match error.kind() {
        CursorProcessLoginStartErrorKind::RuntimeCommandBadRequest => {
            CursorLoginRouteErrorKind::BadRequest
        }
        CursorProcessLoginStartErrorKind::InternalStartup => CursorLoginRouteErrorKind::Internal,
    };
    CursorLoginRouteError::new(kind, error.route_safe_message().to_string())
}

async fn start_cursor_process_login(
    deps: ProviderLoginDeps,
    label: Option<String>,
) -> Result<StartedLoginSession, CursorProcessLoginStartError> {
    let cursor_runtime = login_runtime::resolve_cursor_login_runtime(deps.data_root())
        .await
        .map_err(CursorProcessLoginStartError::from_runtime_error)?;
    let login_session = login_sessions::start_cursor_login_session(deps.providers()).await;

    let login_id = login_session.login_id.clone();
    tokio::spawn(async move {
        session::monitor_cursor_login(deps, cursor_runtime, login_id, label).await;
    });

    Ok(login_session)
}

#[cfg(test)]
mod route_tests {
    use ctx_provider_accounts as provider_accounts;

    use super::*;
    use crate::test_support::TestDaemon;
    use ctx_managed_installs as installer;

    #[test]
    fn cursor_login_route_missing_status_preserves_not_found_message() {
        let error = cursor_login_not_found_route_error();

        assert_eq!(error.kind(), CursorLoginRouteErrorKind::NotFound);
        assert_eq!(error.message(), "login not found");
    }

    #[test]
    fn cursor_login_route_start_error_maps_status_classes() {
        let cases = [
            (
                CursorProcessLoginStartErrorKind::RuntimeCommandBadRequest,
                CursorLoginRouteErrorKind::BadRequest,
            ),
            (
                CursorProcessLoginStartErrorKind::InternalStartup,
                CursorLoginRouteErrorKind::Internal,
            ),
        ];

        for (source, expected) in cases {
            let error = cursor_login_start_route_error(CursorProcessLoginStartError {
                kind: source,
                message: "boom".to_string(),
            });
            assert_eq!(error.kind(), expected);
            assert_eq!(error.message(), "boom");
        }
    }

    #[test]
    fn cursor_login_route_start_response_omits_absent_auth_url() {
        let payload =
            serde_json::to_value(cursor_login_start_route_response(StartedLoginSession {
                login_id: "login-1".to_string(),
                auth_url: None,
                device_code: None,
            }))
            .unwrap();

        assert_eq!(payload["login_id"].as_str(), Some("login-1"));
        assert!(payload.get("auth_url").is_none());
    }

    #[test]
    fn cursor_login_route_start_response_preserves_auth_url() {
        let payload =
            serde_json::to_value(cursor_login_start_route_response(StartedLoginSession {
                login_id: "login-2".to_string(),
                auth_url: Some("https://cursor.com/login/device?code=test".to_string()),
                device_code: None,
            }))
            .unwrap();

        assert_eq!(
            payload["auth_url"].as_str(),
            Some("https://cursor.com/login/device?code=test")
        );
    }

    #[test]
    fn cursor_login_status_route_response_matches_provider_account_wire_shape() {
        let status = provider_accounts::CursorLoginStatus {
            login_id: "cursor-login".to_string(),
            auth_url: None,
            status: "pending".to_string(),
            account_id: None,
            error: None,
        };

        assert_eq!(
            serde_json::to_value(CursorLoginStatusRouteResponse::from(status.clone())).unwrap(),
            serde_json::to_value(status).unwrap()
        );
    }

    #[tokio::test]
    async fn cursor_login_start_rejects_missing_runtime_without_session() {
        let temp = tempfile::tempdir().expect("tempdir");
        let daemon =
            TestDaemon::new_for_test(temp.path().to_path_buf(), "http://127.0.0.1:0".to_string())
                .await
                .expect("test daemon");

        let err = daemon
            .provider_accounts_handle_for_test()
            .start_cursor_login_for_route(CursorLoginStartRouteRequest::default())
            .await
            .expect_err("missing runtime should fail before session creation");

        assert_eq!(err.kind(), CursorLoginRouteErrorKind::BadRequest);
        assert!(err
            .message()
            .contains("runtime_command_missing: provider=cursor-login"));
        assert!(daemon.provider_login_session_caches_empty().await);
    }

    #[tokio::test]
    async fn cursor_login_start_rejects_config_parse_error_without_session() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cfg_path = installer::agent_server_config_path(temp.path());
        tokio::fs::create_dir_all(cfg_path.parent().expect("config parent"))
            .await
            .expect("create config parent");
        tokio::fs::write(&cfg_path, b"{not-json")
            .await
            .expect("write malformed config");
        let daemon =
            TestDaemon::new_for_test(temp.path().to_path_buf(), "http://127.0.0.1:0".to_string())
                .await
                .expect("test daemon");

        let err = daemon
            .provider_accounts_handle_for_test()
            .start_cursor_login_for_route(CursorLoginStartRouteRequest::default())
            .await
            .expect_err("config parse failure should fail before session creation");

        assert_eq!(err.kind(), CursorLoginRouteErrorKind::Internal);
        assert!(err.message().contains("agent server config"));
        assert!(daemon.provider_login_session_caches_empty().await);
    }
}
