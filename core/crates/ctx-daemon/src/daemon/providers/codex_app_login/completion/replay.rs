use std::time::Duration;

use url::Url;

use super::super::{CodexLoginCompleteError, CodexLoginCompleteErrorKind};

pub(in crate::daemon::providers::codex_app_login) enum CallbackReplayError {
    InvalidCallbackUrl(String),
    BuildClient(String),
    Request(String),
    NonSuccess(reqwest::StatusCode),
}

impl CallbackReplayError {
    pub(in crate::daemon::providers::codex_app_login) fn should_restore_completion_token(
        &self,
    ) -> bool {
        matches!(self, Self::Request(_) | Self::NonSuccess(_))
    }

    pub(in crate::daemon::providers::codex_app_login) fn into_route_error(
        self,
    ) -> CodexLoginCompleteError {
        match self {
            Self::InvalidCallbackUrl(err) => CodexLoginCompleteError::new(
                CodexLoginCompleteErrorKind::BadRequest,
                format!("invalid callback_url: {err}"),
            ),
            Self::BuildClient(err) => CodexLoginCompleteError::new(
                CodexLoginCompleteErrorKind::Internal,
                format!("failed to build callback replay client: {err}"),
            ),
            Self::Request(err) => CodexLoginCompleteError::new(
                CodexLoginCompleteErrorKind::BadGateway,
                format!("failed to replay callback: {err}"),
            ),
            Self::NonSuccess(status) => CodexLoginCompleteError::new(
                CodexLoginCompleteErrorKind::BadGateway,
                format!("callback replay returned {status}"),
            ),
        }
    }
}

fn callback_replay_client(callback_url: &str) -> Result<reqwest::Client, CallbackReplayError> {
    let parsed_callback = Url::parse(callback_url)
        .map_err(|err| CallbackReplayError::InvalidCallbackUrl(err.to_string()))?;
    let callback_host = parsed_callback
        .host_str()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let builder = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none());
    let builder = if callback_host == "localhost" {
        builder.resolve(
            "localhost",
            std::net::SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 0),
        )
    } else {
        builder
    };
    builder
        .build()
        .map_err(|err| CallbackReplayError::BuildClient(err.to_string()))
}

pub(in crate::daemon::providers::codex_app_login) async fn replay_codex_callback(
    callback_url: &str,
) -> Result<u16, CallbackReplayError> {
    let client = callback_replay_client(callback_url)?;
    let response = client
        .get(callback_url)
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|err| CallbackReplayError::Request(err.to_string()))?;
    let status = response.status();
    if !status.is_success() {
        return Err(CallbackReplayError::NonSuccess(status));
    }
    Ok(status.as_u16())
}
