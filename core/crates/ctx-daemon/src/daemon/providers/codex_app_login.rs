use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::Context;
use ctx_core::provider_policy::CODEX_APP_SERVER_ARGS;
use ctx_provider_accounts::{
    CodexLoginCompleteRouteRequest, CodexLoginCompleteRouteResponse, CodexLoginRouteError,
    CodexLoginRouteErrorKind, CodexLoginStartRouteRequest, CodexLoginStartRouteResponse,
    CodexLoginStatusRouteResponse,
};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::daemon::providers::login_deps::ProviderLoginDeps;
use crate::daemon::providers::{accounts, login_sessions};
use crate::daemon::ProviderAccountsHandle;

mod app_server;
mod callback_url;
mod completion;
mod process;

const CODEX_LOGIN_RPC_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug)]
struct CodexLoginStartError {
    message: String,
}

impl CodexLoginStartError {
    fn from_error(err: anyhow::Error) -> Self {
        Self {
            message: ctx_observability::logs::redact_sensitive(&err.to_string()),
        }
    }

    fn route_safe_message(&self) -> &str {
        &self.message
    }
}

#[derive(Debug)]
struct StartedCodexLoginSession {
    account_id: String,
    auth_url: String,
    expected_callback_url: Option<String>,
    completion_token: String,
}

impl From<login_sessions::StartedCodexLoginSession> for StartedCodexLoginSession {
    fn from(session: login_sessions::StartedCodexLoginSession) -> Self {
        Self {
            account_id: session.account_id,
            auth_url: session.auth_url,
            expected_callback_url: session.expected_callback_url,
            completion_token: session.completion_token,
        }
    }
}

#[derive(Debug)]
struct CodexLoginCompleteResponse {
    accepted: bool,
    status_code: u16,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum CodexLoginCompleteErrorKind {
    BadRequest,
    NotFound,
    Conflict,
    Unauthorized,
    BadGateway,
    Internal,
}

#[derive(Debug)]
struct CodexLoginCompleteError {
    kind: CodexLoginCompleteErrorKind,
    message: String,
}

impl ProviderAccountsHandle {
    pub async fn start_codex_login_for_route(
        &self,
        request: CodexLoginStartRouteRequest,
    ) -> Result<CodexLoginStartRouteResponse, CodexLoginRouteError> {
        start_codex_app_server_login(
            ProviderLoginDeps::from_accounts_handle(self),
            request.into_label(),
        )
        .await
        .map(codex_login_start_route_response)
        .map_err(codex_login_start_route_error)
    }

    pub async fn codex_login_status_for_route(
        &self,
        account_id: &str,
    ) -> Result<CodexLoginStatusRouteResponse, CodexLoginRouteError> {
        login_sessions::codex_login_status(self.providers(), account_id)
            .await
            .map(Into::into)
            .ok_or_else(codex_login_not_found_route_error)
    }

    pub async fn complete_codex_login_for_route(
        &self,
        account_id: &str,
        request: CodexLoginCompleteRouteRequest,
    ) -> Result<CodexLoginCompleteRouteResponse, CodexLoginRouteError> {
        let (callback_url, completion_token) = request.into_parts();
        complete_codex_app_server_login(
            ProviderLoginDeps::from_accounts_handle(self),
            account_id,
            callback_url,
            completion_token,
        )
        .await
        .map(codex_login_complete_route_response)
        .map_err(codex_login_complete_route_error)
    }
}

fn codex_login_start_route_response(
    session: StartedCodexLoginSession,
) -> CodexLoginStartRouteResponse {
    CodexLoginStartRouteResponse::new(
        session.account_id,
        session.auth_url,
        session.expected_callback_url,
        session.completion_token,
    )
}

fn codex_login_complete_route_response(
    response: CodexLoginCompleteResponse,
) -> CodexLoginCompleteRouteResponse {
    CodexLoginCompleteRouteResponse::new(response.accepted, response.status_code)
}

fn codex_login_not_found_route_error() -> CodexLoginRouteError {
    CodexLoginRouteError::not_found("login not found")
}

fn codex_login_start_route_error(error: CodexLoginStartError) -> CodexLoginRouteError {
    CodexLoginRouteError::internal(error.route_safe_message().to_string())
}

fn codex_login_complete_route_error(error: CodexLoginCompleteError) -> CodexLoginRouteError {
    let kind = match error.kind() {
        CodexLoginCompleteErrorKind::BadRequest => CodexLoginRouteErrorKind::BadRequest,
        CodexLoginCompleteErrorKind::NotFound => CodexLoginRouteErrorKind::NotFound,
        CodexLoginCompleteErrorKind::Conflict => CodexLoginRouteErrorKind::Conflict,
        CodexLoginCompleteErrorKind::Unauthorized => CodexLoginRouteErrorKind::Unauthorized,
        CodexLoginCompleteErrorKind::BadGateway => CodexLoginRouteErrorKind::BadGateway,
        CodexLoginCompleteErrorKind::Internal => CodexLoginRouteErrorKind::Internal,
    };
    CodexLoginRouteError::new(kind, error.route_safe_message().to_string())
}

impl CodexLoginCompleteError {
    fn new(kind: CodexLoginCompleteErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    fn kind(&self) -> CodexLoginCompleteErrorKind {
        self.kind
    }

    fn route_safe_message(&self) -> &str {
        &self.message
    }
}

async fn start_codex_app_server_login(
    deps: ProviderLoginDeps,
    label: Option<String>,
) -> Result<StartedCodexLoginSession, CodexLoginStartError> {
    let prepared = accounts::prepare_codex_login_start(deps.data_root(), label)
        .await
        .map_err(CodexLoginStartError::from_error)?;
    let login = match process::start_codex_login_process(&prepared.account_dir, &prepared.codex_bin)
        .await
    {
        Ok(login) => login,
        Err(err) => {
            let _ = tokio::fs::remove_dir_all(&prepared.account_dir).await;
            return Err(CodexLoginStartError::from_error(err));
        }
    };
    let started_login = login_sessions::start_codex_login_session(
        deps.providers(),
        prepared.account_id,
        login.auth_url.clone(),
        callback_url::expected_callback_from_auth_url(&login.auth_url),
    )
    .await;

    let account_id = started_login.account_id.clone();
    tokio::spawn(async move {
        process::monitor_codex_login(deps, account_id, prepared.label, login).await;
    });

    Ok(started_login.into())
}

async fn complete_codex_app_server_login(
    deps: ProviderLoginDeps,
    account_id: &str,
    callback_url: String,
    completion_token: String,
) -> Result<CodexLoginCompleteResponse, CodexLoginCompleteError> {
    let expected_callback =
        login_sessions::claim_codex_login_callback(deps.providers(), account_id, &completion_token)
            .await
            .map_err(claim_error_response)?;

    if let Err(err) =
        callback_url::validate_callback_url(&callback_url, Some(expected_callback.as_str()))
    {
        login_sessions::restore_codex_login_completion_token(
            deps.providers(),
            account_id,
            &completion_token,
        )
        .await;
        return Err(CodexLoginCompleteError::new(
            CodexLoginCompleteErrorKind::BadRequest,
            err.to_string(),
        ));
    }

    let status_code = match completion::replay_codex_callback(&callback_url).await {
        Ok(status_code) => status_code,
        Err(err) => {
            if err.should_restore_completion_token() {
                login_sessions::restore_codex_login_completion_token(
                    deps.providers(),
                    account_id,
                    &completion_token,
                )
                .await;
            }
            return Err(err.into_route_error());
        }
    };

    Ok(CodexLoginCompleteResponse {
        accepted: true,
        status_code,
    })
}

fn claim_error_response(
    err: login_sessions::CodexLoginCallbackClaimError,
) -> CodexLoginCompleteError {
    match err {
        login_sessions::CodexLoginCallbackClaimError::NotFound => {
            CodexLoginCompleteError::new(CodexLoginCompleteErrorKind::NotFound, "login not found")
        }
        login_sessions::CodexLoginCallbackClaimError::NotPending => CodexLoginCompleteError::new(
            CodexLoginCompleteErrorKind::Conflict,
            "login is not pending",
        ),
        login_sessions::CodexLoginCallbackClaimError::InvalidCompletionToken => {
            CodexLoginCompleteError::new(
                CodexLoginCompleteErrorKind::Unauthorized,
                "invalid completion token",
            )
        }
        login_sessions::CodexLoginCallbackClaimError::MissingExpectedCallback => {
            CodexLoginCompleteError::new(
                CodexLoginCompleteErrorKind::Conflict,
                "login is missing expected callback metadata",
            )
        }
    }
}

#[cfg(test)]
mod route_tests {
    use ctx_managed_installs as installer;
    use ctx_provider_accounts as provider_accounts;

    use super::*;
    use crate::test_support::TestDaemon;

    #[test]
    fn codex_login_route_missing_status_preserves_not_found_message() {
        let error = codex_login_not_found_route_error();

        assert_eq!(error.kind(), CodexLoginRouteErrorKind::NotFound);
        assert_eq!(error.message(), "login not found");
    }

    #[test]
    fn codex_login_route_start_error_maps_to_internal() {
        let error = codex_login_start_route_error(CodexLoginStartError::from_error(
            anyhow::anyhow!("failed to start Codex"),
        ));

        assert_eq!(error.kind(), CodexLoginRouteErrorKind::Internal);
        assert_eq!(error.message(), "failed to start Codex");
    }

    #[test]
    fn codex_login_route_complete_error_maps_all_status_classes() {
        let cases = [
            (
                CodexLoginCompleteErrorKind::BadRequest,
                CodexLoginRouteErrorKind::BadRequest,
            ),
            (
                CodexLoginCompleteErrorKind::NotFound,
                CodexLoginRouteErrorKind::NotFound,
            ),
            (
                CodexLoginCompleteErrorKind::Conflict,
                CodexLoginRouteErrorKind::Conflict,
            ),
            (
                CodexLoginCompleteErrorKind::Unauthorized,
                CodexLoginRouteErrorKind::Unauthorized,
            ),
            (
                CodexLoginCompleteErrorKind::BadGateway,
                CodexLoginRouteErrorKind::BadGateway,
            ),
            (
                CodexLoginCompleteErrorKind::Internal,
                CodexLoginRouteErrorKind::Internal,
            ),
        ];

        for (source, expected) in cases {
            let error =
                codex_login_complete_route_error(CodexLoginCompleteError::new(source, "boom"));
            assert_eq!(error.kind(), expected);
            assert_eq!(error.message(), "boom");
        }
    }

    #[test]
    fn codex_login_route_start_response_omits_absent_expected_callback() {
        let payload =
            serde_json::to_value(codex_login_start_route_response(StartedCodexLoginSession {
                account_id: "acct-1".to_string(),
                auth_url: "https://example.test/auth".to_string(),
                expected_callback_url: None,
                completion_token: "token-1".to_string(),
            }))
            .unwrap();

        assert_eq!(payload["account_id"].as_str(), Some("acct-1"));
        assert_eq!(
            payload["auth_url"].as_str(),
            Some("https://example.test/auth")
        );
        assert_eq!(payload["completion_token"].as_str(), Some("token-1"));
        assert!(payload.get("expected_callback_url").is_none());
    }

    #[test]
    fn codex_login_route_start_response_preserves_expected_callback() {
        let payload =
            serde_json::to_value(codex_login_start_route_response(StartedCodexLoginSession {
                account_id: "acct-2".to_string(),
                auth_url: "https://example.test/auth".to_string(),
                expected_callback_url: Some("http://localhost:1234/auth/callback".to_string()),
                completion_token: "token-2".to_string(),
            }))
            .unwrap();

        assert_eq!(
            payload["expected_callback_url"].as_str(),
            Some("http://localhost:1234/auth/callback")
        );
    }

    #[test]
    fn codex_login_route_complete_response_preserves_shape() {
        let payload = serde_json::to_value(codex_login_complete_route_response(
            CodexLoginCompleteResponse {
                accepted: true,
                status_code: 200,
            },
        ))
        .unwrap();

        assert_eq!(payload["accepted"].as_bool(), Some(true));
        assert_eq!(payload["status_code"].as_u64(), Some(200));
    }

    #[test]
    fn codex_login_status_route_response_matches_provider_account_wire_shape() {
        let status = provider_accounts::CodexLoginStatus {
            account_id: "codex-account".to_string(),
            auth_url: "https://chat.openai.com/oauth/authorize".to_string(),
            expected_callback_url: None,
            completion_token: None,
            status: "pending".to_string(),
            error: None,
        };

        assert_eq!(
            serde_json::to_value(CodexLoginStatusRouteResponse::from(status.clone())).unwrap(),
            serde_json::to_value(status).unwrap()
        );
    }

    #[tokio::test]
    async fn codex_login_start_rejects_config_parse_error_without_session() {
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
            .start_codex_login_for_route(CodexLoginStartRouteRequest::default())
            .await
            .expect_err("config parse failure should fail before session creation");

        assert_eq!(err.kind(), CodexLoginRouteErrorKind::Internal);
        assert!(err.message().contains("agent server config"));
        assert!(daemon.provider_login_session_caches_empty().await);
    }
}
