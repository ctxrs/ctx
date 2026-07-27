use std::{
    io::Read,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, bail, Context, Result};
use ctx_pro_host_protocol::SignedEntitlement;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use url::Url;

use super::request_identity::new_idempotency_key;

const MAX_API_RESPONSE_BYTES: u64 = 96 * 1024;
const MAX_ACCESS_TOKEN_BYTES: usize = 16 * 1024;
const MAX_URL_BYTES: usize = 4096;
const MAX_TIMESTAMP: i64 = 253_402_300_799;
const MAX_CHECKOUT_LIFETIME_SECONDS: i64 = 7 * 24 * 60 * 60;
const MAX_RETRY_AFTER_SECONDS: u64 = 30 * 60;
const COMMERCIAL_ACCEPT: &str = "application/json, application/problem+json";

#[derive(Debug, Clone)]
pub(super) struct CommercialApiConfig {
    pub(super) origin: Url,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CommercialState {
    pub(super) subject: String,
    pub(super) account_id: String,
    pub(super) access_state: String,
    pub(super) access_deadline_unix: Option<i64>,
    pub(super) billing: BillingState,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BillingState {
    pub(super) customer_associated: bool,
    pub(super) subscription_status: Option<String>,
    pub(super) trial_end_unix: Option<i64>,
    pub(super) current_period_end_unix: Option<i64>,
    pub(super) cancel_at_period_end: bool,
    pub(super) canceled_at_unix: Option<i64>,
    pub(super) latest_invoice_status: Option<String>,
    pub(super) latest_payment_state: String,
}

impl CommercialState {
    pub(super) fn grants_access(&self) -> bool {
        matches!(
            self.access_state.as_str(),
            "trial" | "active" | "canceling_paid"
        ) && self.access_deadline_unix.is_some()
    }

    fn validate(&self) -> Result<()> {
        validate_identifier(&self.subject, "subject")?;
        validate_identifier(&self.account_id, "account")?;
        if !matches!(
            self.access_state.as_str(),
            "active" | "canceling_paid" | "locked" | "none" | "trial"
        ) {
            bail!("invalid_response: commercial access state is invalid");
        }
        validate_timestamp(self.access_deadline_unix, "access deadline")?;
        let grants_access = matches!(
            self.access_state.as_str(),
            "active" | "canceling_paid" | "trial"
        );
        if (grants_access && self.access_deadline_unix.is_none())
            || (self.access_state == "none" && self.access_deadline_unix.is_some())
        {
            bail!("invalid_response: commercial access deadline is inconsistent");
        }
        self.billing.validate()
    }
}

impl BillingState {
    fn validate(&self) -> Result<()> {
        for (value, label) in [
            (self.subscription_status.as_deref(), "subscription status"),
            (self.latest_invoice_status.as_deref(), "invoice status"),
        ] {
            if value.is_some_and(|value| {
                value.is_empty()
                    || value.len() > 64
                    || !value
                        .bytes()
                        .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
            }) {
                bail!("invalid_response: commercial {label} is invalid");
            }
        }
        if !matches!(
            self.latest_payment_state.as_str(),
            "failed" | "open" | "paid" | "refunded" | "unknown"
        ) {
            bail!("invalid_response: commercial payment state is invalid");
        }
        for (value, label) in [
            (self.trial_end_unix, "trial end"),
            (self.current_period_end_unix, "period end"),
            (self.canceled_at_unix, "cancellation time"),
        ] {
            validate_timestamp(value, label)?;
        }
        if !self.customer_associated
            && (self.subscription_status.is_some()
                || self.trial_end_unix.is_some()
                || self.current_period_end_unix.is_some()
                || self.cancel_at_period_end
                || self.canceled_at_unix.is_some()
                || self.latest_invoice_status.is_some()
                || self.latest_payment_state != "unknown")
        {
            bail!("invalid_response: commercial billing state is inconsistent");
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CheckoutResult {
    pub(super) kind: String,
    #[serde(default)]
    pub(super) url: Option<String>,
    #[serde(default)]
    pub(super) expires_at_unix: Option<i64>,
    #[serde(default)]
    pub(super) state: Option<CommercialState>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PortalResult {
    pub(super) kind: String,
    pub(super) url: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApiSuccess<T> {
    api_version: String,
    request_id: String,
    data: T,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApiFailure {
    api_version: String,
    request_id: String,
    error: ApiError,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApiError {
    code: String,
    message: String,
    retryable: bool,
}

#[derive(Debug, thiserror::Error)]
enum CommercialApiFailure {
    #[error("service_unavailable: {operation} failed")]
    Transport { operation: String },
    #[error("service_unavailable: commercial API returned transient HTTP {status}")]
    Proxy {
        status: u16,
        retry_after: Option<Duration>,
    },
    #[error(
        "invalid_response: commercial API error response is not contracted JSON (HTTP {status})"
    )]
    InvalidResponse { status: u16 },
    #[error("{code}: {message} (HTTP {status})")]
    Response {
        code: String,
        message: &'static str,
        status: u16,
        retryable: bool,
        retry_after: Option<Duration>,
    },
}

impl CommercialApiFailure {
    fn retryable_during_checkout(&self) -> bool {
        match self {
            Self::Transport { .. } | Self::Proxy { .. } => true,
            Self::InvalidResponse { .. } => false,
            Self::Response {
                code, retryable, ..
            } => {
                if matches!(
                    code.as_str(),
                    "authentication_required"
                        | "billing_conflict"
                        | "commercial_access_locked"
                        | "commercial_identity_conflict"
                        | "dependency_invalid_response"
                        | "idempotency_key_invalid"
                        | "idempotency_key_required"
                        | "invalid_request"
                        | "invalid_response"
                        | "invalid_json"
                        | "invalid_installation_public_key"
                        | "method_not_allowed"
                        | "not_found"
                        | "request_body_too_large"
                        | "unsupported_media_type"
                ) {
                    return false;
                }
                *retryable
            }
        }
    }
}

pub(super) struct CommercialApiClient {
    config: CommercialApiConfig,
    agent: ureq::Agent,
}

impl CommercialApiClient {
    pub(super) fn new(config: CommercialApiConfig) -> Result<Self> {
        validate_origin(&config.origin)?;
        Ok(Self {
            config,
            agent: ureq::AgentBuilder::new()
                .redirects(0)
                .timeout_connect(Duration::from_secs(10))
                .timeout_read(Duration::from_secs(20))
                .timeout_write(Duration::from_secs(10))
                .build(),
        })
    }

    pub(super) fn account(&self, access_token: &str) -> Result<CommercialState> {
        let state: CommercialState = self.get("/v1/account", access_token)?;
        state.validate()?;
        Ok(state)
    }

    pub(super) fn checkout(&self, access_token: &str) -> Result<CheckoutResult> {
        let result: CheckoutResult =
            self.post("/v1/billing/checkout", access_token, &EmptyRequest {})?;
        result.validate()?;
        Ok(result)
    }

    pub(super) fn portal(&self, access_token: &str) -> Result<PortalResult> {
        let result: PortalResult =
            self.post("/v1/billing/portal", access_token, &EmptyRequest {})?;
        if result.kind != "portal_created" {
            bail!("invalid_response: commercial API returned an invalid portal result");
        }
        validate_https_url(&result.url, "billing portal")?;
        Ok(result)
    }

    pub(super) fn entitlement(
        &self,
        access_token: &str,
        installation_public_key_base64url: &str,
    ) -> Result<SignedEntitlement> {
        if installation_public_key_base64url.len() != 43
            || !installation_public_key_base64url
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            bail!("invalid_request: installation public key is invalid");
        }
        self.post(
            "/v1/entitlements",
            access_token,
            &EntitlementRequest {
                installation_public_key_base64url,
            },
        )
    }

    pub(super) fn origin(&self) -> &str {
        self.config.origin.as_str()
    }

    fn get<T: DeserializeOwned>(&self, path: &str, access_token: &str) -> Result<T> {
        validate_access_token(access_token)?;
        let url = self.endpoint(path)?;
        let response = self
            .agent
            .get(url.as_str())
            .set("accept", COMMERCIAL_ACCEPT)
            .set("authorization", &format!("Bearer {access_token}"))
            .call();
        self.finish(response, "commercial API request")
    }

    fn post<T: DeserializeOwned, B: Serialize>(
        &self,
        path: &str,
        access_token: &str,
        body: &B,
    ) -> Result<T> {
        validate_access_token(access_token)?;
        let url = self.endpoint(path)?;
        let body =
            serde_json::to_vec(body).context("invalid_request: encode commercial request")?;
        if body.len() > 16 * 1024 {
            bail!("invalid_request: commercial request is too large");
        }
        let response = self
            .agent
            .post(url.as_str())
            .set("accept", COMMERCIAL_ACCEPT)
            .set("authorization", &format!("Bearer {access_token}"))
            .set("content-type", "application/json")
            .set("idempotency-key", &new_idempotency_key("cli")?)
            .send_bytes(&body);
        self.finish(response, "commercial API request")
    }

    fn finish<T: DeserializeOwned>(
        &self,
        response: std::result::Result<ureq::Response, ureq::Error>,
        operation: &str,
    ) -> Result<T> {
        match response {
            Ok(response) => {
                require_success_json(&response)?;
                let success: ApiSuccess<T> = read_success_json(response)?;
                validate_envelope(&success.api_version, &success.request_id)?;
                Ok(success.data)
            }
            Err(ureq::Error::Status(status, response)) => {
                let retry_after = parse_retry_after(response.header("retry-after"));
                match classify_error_response(status, response.header("content-type")) {
                    ErrorResponseKind::Contracted => {
                        let failure = read_json(response, "commercial API error")
                            .and_then(|failure| typed_api_failure(status, retry_after, failure));
                        match failure {
                            Ok(failure) => Err(failure.into()),
                            Err(_) => Err(malformed_error_failure(status, retry_after).into()),
                        }
                    }
                    ErrorResponseKind::TransientProxy => Err(CommercialApiFailure::Proxy {
                        status,
                        retry_after,
                    }
                    .into()),
                    ErrorResponseKind::Invalid => {
                        Err(CommercialApiFailure::InvalidResponse { status }.into())
                    }
                }
            }
            Err(ureq::Error::Transport(_)) => Err(CommercialApiFailure::Transport {
                operation: operation.to_owned(),
            }
            .into()),
        }
    }

    fn endpoint(&self, path: &str) -> Result<Url> {
        self.config
            .origin
            .join(path)
            .context("invalid_request: invalid commercial API route")
    }
}

impl CheckoutResult {
    fn validate(&self) -> Result<()> {
        match self.kind.as_str() {
            "checkout_created" => {
                let url = self
                    .url
                    .as_deref()
                    .ok_or_else(|| anyhow!("invalid_response: Checkout result has no URL"))?;
                validate_https_url(url, "Checkout")?;
                self.validate_poll_expiry()?;
                if self.state.is_some() {
                    bail!("invalid_response: Checkout result unexpectedly contains account state");
                }
            }
            "checkout_pending" => {
                if self.url.is_some() || self.state.is_some() {
                    bail!("invalid_response: pending Checkout result contains unexpected data");
                }
                self.validate_poll_expiry()?;
            }
            "already_subscribed" => {
                if self.url.is_some() || self.expires_at_unix.is_some() {
                    bail!("invalid_response: subscription result unexpectedly contains Checkout");
                }
                self.state
                    .as_ref()
                    .ok_or_else(|| {
                        anyhow!("invalid_response: subscription result has no account state")
                    })?
                    .validate()?;
            }
            _ => bail!("invalid_response: commercial API returned an invalid Checkout result"),
        }
        Ok(())
    }

    fn validate_poll_expiry(&self) -> Result<()> {
        let expires_at = self
            .expires_at_unix
            .ok_or_else(|| anyhow!("invalid_response: Checkout result has no expiry"))?;
        let now = unix_time()?;
        if expires_at <= now || expires_at > now + MAX_CHECKOUT_LIFETIME_SECONDS {
            bail!("invalid_response: Checkout expiry is outside allowed bounds");
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct EmptyRequest {}

#[derive(Serialize)]
struct EntitlementRequest<'a> {
    installation_public_key_base64url: &'a str,
}

fn validate_origin(origin: &Url) -> Result<()> {
    if origin.scheme() != "https"
        || origin.host_str().is_none()
        || origin.path() != "/"
        || origin.query().is_some()
        || origin.fragment().is_some()
        || !origin.username().is_empty()
        || origin.password().is_some()
    {
        bail!("invalid_request: commercial API must be an HTTPS origin");
    }
    Ok(())
}

fn validate_access_token(token: &str) -> Result<()> {
    if token.is_empty()
        || token.len() > MAX_ACCESS_TOKEN_BYTES
        || token.bytes().any(|byte| byte.is_ascii_control())
    {
        bail!("authentication_required: commercial access token is unavailable");
    }
    Ok(())
}

fn validate_identifier(value: &str, label: &str) -> Result<()> {
    if value.len() < 3
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        bail!("invalid_response: commercial {label} identifier is invalid");
    }
    Ok(())
}

fn validate_timestamp(value: Option<i64>, label: &str) -> Result<()> {
    if value.is_some_and(|value| value <= 0 || value > MAX_TIMESTAMP) {
        bail!("invalid_response: commercial {label} is invalid");
    }
    Ok(())
}

fn unix_time() -> Result<i64> {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("invalid_response: system clock is before Unix epoch")?
            .as_secs(),
    )
    .context("invalid_response: system time is invalid")
}

fn validate_envelope(version: &str, request_id: &str) -> Result<()> {
    if version != "v1"
        || request_id.is_empty()
        || request_id.len() > 128
        || !request_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("invalid_response: commercial API response identity is invalid");
    }
    Ok(())
}

fn validate_error(error: &ApiError) -> Result<()> {
    if error.code.is_empty()
        || error.code.len() > 64
        || !error
            .code
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
        || error.message.is_empty()
        || error.message.len() > 512
    {
        bail!("invalid_response: commercial API error is malformed");
    }
    Ok(())
}

fn commercial_error_message(error: &ApiError) -> &'static str {
    match error.code.as_str() {
        "authentication_required" => "sign in again with `ctx pro`",
        "billing_conflict" => {
            "multiple active subscriptions need attention; run `ctx pro manage` to resolve them"
        }
        "commercial_access_locked" => "an active trial or subscription is required",
        "commercial_identity_conflict" => {
            "the billing customer belongs to a different signed-in account; rerun `ctx pro` with the original account"
        }
        "rate_limited" => "the commercial service is rate limited; retry shortly",
        _ if error.retryable => "the commercial service is temporarily unavailable",
        _ => "the commercial service rejected the request",
    }
}

pub(super) fn is_retryable_checkout_failure(error: &anyhow::Error) -> bool {
    if let Some(failure) = error.downcast_ref::<CommercialApiFailure>() {
        return failure.retryable_during_checkout();
    }
    let message = error.to_string();
    message.starts_with("service_unavailable:") || message.starts_with("rate_limited:")
}

pub(super) fn checkout_retry_after(error: &anyhow::Error) -> Option<Duration> {
    error
        .downcast_ref::<CommercialApiFailure>()
        .and_then(|failure| match failure {
            CommercialApiFailure::Proxy { retry_after, .. } => *retry_after,
            CommercialApiFailure::Response { retry_after, .. }
                if failure.retryable_during_checkout() =>
            {
                *retry_after
            }
            CommercialApiFailure::Transport { .. }
            | CommercialApiFailure::InvalidResponse { .. }
            | CommercialApiFailure::Response { .. } => None,
        })
}

fn parse_retry_after(value: Option<&str>) -> Option<Duration> {
    let seconds = value?
        .trim()
        .parse::<u64>()
        .ok()?
        .min(MAX_RETRY_AFTER_SECONDS);
    Some(Duration::from_secs(seconds))
}

fn require_success_json(response: &ureq::Response) -> Result<()> {
    if !media_type_is(response.header("content-type"), "application/json") {
        bail!("invalid_response: commercial API response is not JSON");
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ErrorResponseKind {
    Contracted,
    TransientProxy,
    Invalid,
}

fn classify_error_response(status: u16, content_type: Option<&str>) -> ErrorResponseKind {
    if media_type_is(content_type, "application/problem+json")
        || media_type_is(content_type, "application/json")
    {
        ErrorResponseKind::Contracted
    } else if matches!(status, 429 | 502 | 503 | 504) {
        ErrorResponseKind::TransientProxy
    } else {
        ErrorResponseKind::Invalid
    }
}

fn media_type_is(value: Option<&str>, expected: &str) -> bool {
    value
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case(expected))
}

fn typed_api_failure(
    status: u16,
    retry_after: Option<Duration>,
    failure: ApiFailure,
) -> Result<CommercialApiFailure> {
    validate_envelope(&failure.api_version, &failure.request_id)?;
    validate_error(&failure.error)?;
    let message = commercial_error_message(&failure.error);
    Ok(CommercialApiFailure::Response {
        code: failure.error.code,
        message,
        status,
        retryable: failure.error.retryable,
        retry_after,
    })
}

fn malformed_error_failure(status: u16, retry_after: Option<Duration>) -> CommercialApiFailure {
    if matches!(status, 429 | 502 | 503 | 504) {
        CommercialApiFailure::Proxy {
            status,
            retry_after,
        }
    } else {
        CommercialApiFailure::InvalidResponse { status }
    }
}

fn read_success_json<T: DeserializeOwned>(response: ureq::Response) -> Result<T> {
    match read_json(response, "commercial API response") {
        Ok(value) => Ok(value),
        Err(error) if error.to_string().starts_with("service_unavailable:") => {
            bail!("service_unavailable: read commercial API response")
        }
        Err(_) => bail!("invalid_response: commercial API response is malformed"),
    }
}

fn read_json<T: DeserializeOwned>(response: ureq::Response, label: &str) -> Result<T> {
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(MAX_API_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("service_unavailable: read {label}"))?;
    if bytes.len() as u64 > MAX_API_RESPONSE_BYTES {
        bail!("invalid_response: {label} is too large");
    }
    serde_json::from_slice(&bytes).with_context(|| format!("invalid_response: parse {label}"))
}

pub(super) fn validate_https_url(value: &str, label: &str) -> Result<Url> {
    if value.is_empty() || value.len() > MAX_URL_BYTES {
        bail!("invalid_response: {label} URL is invalid");
    }
    let parsed =
        Url::parse(value).with_context(|| format!("invalid_response: invalid {label} URL"))?;
    if parsed.scheme() != "https"
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        bail!("invalid_response: {label} URL must be HTTPS");
    }
    Ok(parsed)
}

#[cfg(test)]
#[path = "commercial_api_tests.rs"]
mod tests;
