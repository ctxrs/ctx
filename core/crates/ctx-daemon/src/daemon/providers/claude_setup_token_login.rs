use ctx_provider_accounts::{
    ClaudeLoginRouteError, ClaudeLoginRouteErrorKind, ClaudeLoginStartRouteRequest,
    ClaudeLoginStartRouteResponse, ClaudeLoginStatusRouteResponse,
};

use crate::daemon::providers::login_deps::ProviderLoginDeps;
use crate::daemon::providers::{login_sessions, StartedLoginSession};
use crate::daemon::ProviderAccountsHandle;

mod auth_url;
mod runtime;
mod session;

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum ClaudeSetupTokenLoginStartErrorKind {
    BadRequest,
    Internal,
}

#[derive(Debug)]
struct ClaudeSetupTokenLoginStartError {
    kind: ClaudeSetupTokenLoginStartErrorKind,
    message: String,
}

impl ClaudeSetupTokenLoginStartError {
    fn from_runtime_error(err: anyhow::Error) -> Self {
        let message = format!("{err:#}");
        let kind = if message.contains("runtime_command_") {
            ClaudeSetupTokenLoginStartErrorKind::BadRequest
        } else {
            ClaudeSetupTokenLoginStartErrorKind::Internal
        };
        Self::from_message(kind, message)
    }

    fn from_internal_error(err: anyhow::Error) -> Self {
        Self::new(ClaudeSetupTokenLoginStartErrorKind::Internal, err)
    }

    fn new(kind: ClaudeSetupTokenLoginStartErrorKind, err: anyhow::Error) -> Self {
        Self::from_message(kind, format!("{err:#}"))
    }

    fn from_message(kind: ClaudeSetupTokenLoginStartErrorKind, message: String) -> Self {
        Self {
            kind,
            message: ctx_observability::logs::redact_sensitive(&message),
        }
    }

    fn kind(&self) -> ClaudeSetupTokenLoginStartErrorKind {
        self.kind
    }

    fn route_safe_message(&self) -> &str {
        &self.message
    }
}

impl ProviderAccountsHandle {
    pub async fn start_claude_login_for_route(
        &self,
        request: ClaudeLoginStartRouteRequest,
    ) -> Result<ClaudeLoginStartRouteResponse, ClaudeLoginRouteError> {
        start_claude_setup_token_login(
            ProviderLoginDeps::from_accounts_handle(self),
            request.into_label(),
        )
        .await
        .map(claude_login_start_route_response)
        .map_err(claude_login_start_route_error)
    }

    pub async fn claude_login_status_for_route(
        &self,
        login_id: &str,
    ) -> Result<ClaudeLoginStatusRouteResponse, ClaudeLoginRouteError> {
        login_sessions::claude_login_status(self.providers(), login_id)
            .await
            .map(Into::into)
            .ok_or_else(claude_login_not_found_route_error)
    }
}

fn claude_login_start_route_response(
    session: StartedLoginSession,
) -> ClaudeLoginStartRouteResponse {
    ClaudeLoginStartRouteResponse::new(session.login_id, session.auth_url)
}

fn claude_login_not_found_route_error() -> ClaudeLoginRouteError {
    ClaudeLoginRouteError::not_found("login not found")
}

fn claude_login_start_route_error(error: ClaudeSetupTokenLoginStartError) -> ClaudeLoginRouteError {
    let kind = match error.kind() {
        ClaudeSetupTokenLoginStartErrorKind::BadRequest => ClaudeLoginRouteErrorKind::BadRequest,
        ClaudeSetupTokenLoginStartErrorKind::Internal => ClaudeLoginRouteErrorKind::Internal,
    };
    ClaudeLoginRouteError::new(kind, error.route_safe_message().to_string())
}

async fn start_claude_setup_token_login(
    deps: ProviderLoginDeps,
    label: Option<String>,
) -> Result<StartedLoginSession, ClaudeSetupTokenLoginStartError> {
    let runtime = super::login_runtime::resolve_claude_login_runtime(deps.data_root())
        .await
        .map_err(ClaudeSetupTokenLoginStartError::from_runtime_error)?;
    let login = session::start_claude_login_process(&runtime)
        .await
        .map_err(ClaudeSetupTokenLoginStartError::from_internal_error)?;
    let auth_url = login.auth_url.clone();
    let login_session =
        login_sessions::start_claude_login_session(deps.providers(), auth_url).await;

    let login_id = login_session.login_id.clone();
    tokio::spawn(async move {
        session::monitor_claude_login(deps, login_id, label, login).await;
    });

    Ok(login_session)
}

#[cfg(test)]
mod route_tests {
    use ctx_provider_accounts as provider_accounts;

    use super::*;

    #[test]
    fn claude_login_route_missing_status_preserves_not_found_message() {
        let error = claude_login_not_found_route_error();

        assert_eq!(error.kind(), ClaudeLoginRouteErrorKind::NotFound);
        assert_eq!(error.message(), "login not found");
    }

    #[test]
    fn claude_login_route_start_error_maps_status_classes() {
        let cases = [
            (
                ClaudeSetupTokenLoginStartErrorKind::BadRequest,
                ClaudeLoginRouteErrorKind::BadRequest,
            ),
            (
                ClaudeSetupTokenLoginStartErrorKind::Internal,
                ClaudeLoginRouteErrorKind::Internal,
            ),
        ];

        for (source, expected) in cases {
            let error = claude_login_start_route_error(
                ClaudeSetupTokenLoginStartError::from_message(source, "boom".to_string()),
            );
            assert_eq!(error.kind(), expected);
            assert_eq!(error.message(), "boom");
        }
    }

    #[test]
    fn claude_login_route_start_response_omits_absent_auth_url() {
        let payload =
            serde_json::to_value(claude_login_start_route_response(StartedLoginSession {
                login_id: "login-1".to_string(),
                auth_url: None,
                device_code: None,
            }))
            .unwrap();

        assert_eq!(payload["login_id"].as_str(), Some("login-1"));
        assert!(payload.get("auth_url").is_none());
    }

    #[test]
    fn claude_login_route_start_response_preserves_auth_url() {
        let payload =
            serde_json::to_value(claude_login_start_route_response(StartedLoginSession {
                login_id: "login-2".to_string(),
                auth_url: Some("https://claude.ai/oauth/authorize".to_string()),
                device_code: None,
            }))
            .unwrap();

        assert_eq!(
            payload["auth_url"].as_str(),
            Some("https://claude.ai/oauth/authorize")
        );
    }

    #[test]
    fn claude_login_status_route_response_matches_provider_account_wire_shape() {
        let status = provider_accounts::ClaudeLoginStatus {
            login_id: "claude-login".to_string(),
            auth_url: None,
            status: "pending".to_string(),
            account_id: None,
            error: None,
        };

        assert_eq!(
            serde_json::to_value(ClaudeLoginStatusRouteResponse::from(status.clone())).unwrap(),
            serde_json::to_value(status).unwrap()
        );
    }
}
