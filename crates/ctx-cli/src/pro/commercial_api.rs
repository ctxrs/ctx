use std::{
    fmt,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, bail, Context, Result};
use ctx_pro_host_protocol::SignedEntitlement;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use url::Url;
use zeroize::{Zeroize as _, ZeroizeOnDrop, Zeroizing};

use super::request_identity::new_idempotency_key;

mod referral;
mod response;
mod urls;

pub(super) use referral::{ReferralCreateResult, ReferralPayoutResult, ReferralStatusResult};
#[cfg(test)]
use response::{
    classify_error_response, commercial_error_message, malformed_error_failure, media_type_is,
    parse_retry_after, typed_api_failure, ErrorResponseKind,
};
pub(super) use response::{commercial_http_error, is_retryable_checkout_failure};
use response::{read_success_json, require_success_json, validate_envelope};

const MAX_API_RESPONSE_BYTES: u64 = 96 * 1024;
const MAX_ACCESS_TOKEN_BYTES: usize = 16 * 1024;
const MAX_TRIAL_ACCESS_TOKEN_BYTES: usize = 2 * 1024;
const MAX_URL_BYTES: usize = 4096;
const MAX_TIMESTAMP: i64 = 253_402_300_799;
const MAX_CHECKOUT_LIFETIME_SECONDS: i64 = 7 * 24 * 60 * 60;
const MAX_RETRY_AFTER_SECONDS: u64 = 30 * 60;
const MAX_TRIAL_CHALLENGE_LIFETIME_SECONDS: i64 = 10 * 60;
pub(super) const COMMERCIAL_ACCEPT: &str = "application/json, application/problem+json";
const STAGING_ACCESS_ORIGIN: &str = "https://pro-staging.ctx.rs/";
const MAX_ACCESS_HEADER_BYTES: usize = 4096;
const CLOUDFLARE_ACCESS_CLIENT_ID_HEADER: &str = "CF-Access-Client-Id";
const CLOUDFLARE_ACCESS_CLIENT_SECRET_HEADER: &str = "CF-Access-Client-Secret";

#[derive(Clone)]
pub(super) struct CloudflareAccessCredentials {
    client_id: Zeroizing<String>,
    client_secret: Zeroizing<String>,
}

impl CloudflareAccessCredentials {
    pub(super) fn new(client_id: String, client_secret: String) -> Result<Self> {
        validate_access_header_value(&client_id)?;
        validate_access_header_value(&client_secret)?;
        Ok(Self {
            client_id: Zeroizing::new(client_id),
            client_secret: Zeroizing::new(client_secret),
        })
    }
}

impl fmt::Debug for CloudflareAccessCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CloudflareAccessCredentials")
            .field("client_id", &"[REDACTED]")
            .field("client_secret", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone)]
pub(super) struct CommercialApiConfig {
    origin: Url,
    staging_access: Option<CloudflareAccessCredentials>,
}

impl fmt::Debug for CommercialApiConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CommercialApiConfig")
            .field("origin", &self.origin)
            .field(
                "staging_access",
                &self.staging_access.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

impl CommercialApiConfig {
    pub(super) fn new(
        origin: Url,
        staging_access: Option<CloudflareAccessCredentials>,
    ) -> Result<Self> {
        validate_origin(&origin)?;
        if staging_access.is_some() && origin.as_str() != STAGING_ACCESS_ORIGIN {
            bail!(
                "invalid_request: staging Cloudflare Access credentials require the fixed staging origin"
            );
        }
        Ok(Self {
            origin,
            staging_access,
        })
    }

    pub(super) fn origin(&self) -> &Url {
        &self.origin
    }

    pub(super) fn apply_transport_credentials(
        &self,
        request: ureq::Request,
        url: &Url,
    ) -> Result<ureq::Request> {
        if url.origin() != self.origin.origin() {
            bail!("invalid_request: commercial request escaped its fixed origin");
        }
        let Some(access) = &self.staging_access else {
            return Ok(request);
        };
        if self.origin.as_str() != STAGING_ACCESS_ORIGIN {
            bail!(
                "invalid_request: staging Cloudflare Access credentials require the fixed staging origin"
            );
        }
        Ok(request
            .set(
                CLOUDFLARE_ACCESS_CLIENT_ID_HEADER,
                access.client_id.as_str(),
            )
            .set(
                CLOUDFLARE_ACCESS_CLIENT_SECRET_HEADER,
                access.client_secret.as_str(),
            ))
    }
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkOsBootstrapResult {
    organization_id: String,
}

#[derive(Deserialize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
pub(super) struct TrialChallenge {
    pub(super) challenge_id: String,
    pub(super) challenge_base64url: String,
    pub(super) expires_at_unix: i64,
    pub(super) trial_activation_token: String,
}

#[derive(Deserialize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
pub(super) struct TrialActivation {
    pub(super) disposition: String,
    #[zeroize(skip)]
    pub(super) entitlement: SignedEntitlement,
    pub(super) trial_access_token: String,
    pub(super) trial_deadline_unix: i64,
    #[serde(default)]
    pub(super) referral_claim_token: Option<String>,
}

#[derive(Deserialize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
pub(super) struct TrialRefresh {
    #[zeroize(skip)]
    pub(super) entitlement: SignedEntitlement,
    pub(super) trial_access_token: String,
    pub(super) trial_deadline_unix: i64,
    #[serde(default)]
    pub(super) referral_claim_token: Option<String>,
}

impl fmt::Debug for TrialChallenge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrialChallenge")
            .field("challenge_id", &self.challenge_id)
            .field("challenge_base64url", &self.challenge_base64url)
            .field("expires_at_unix", &self.expires_at_unix)
            .field("trial_activation_token", &"[REDACTED]")
            .finish()
    }
}

impl fmt::Debug for TrialActivation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrialActivation")
            .field("disposition", &self.disposition)
            .field("entitlement", &"[REDACTED]")
            .field("trial_access_token", &"[REDACTED]")
            .field("trial_deadline_unix", &self.trial_deadline_unix)
            .finish()
    }
}

impl fmt::Debug for TrialRefresh {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrialRefresh")
            .field("entitlement", &"[REDACTED]")
            .field("trial_access_token", &"[REDACTED]")
            .field("trial_deadline_unix", &self.trial_deadline_unix)
            .finish()
    }
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
    fn is_retryable(&self) -> bool {
        match self {
            Self::Transport { .. } | Self::Proxy { .. } => true,
            Self::InvalidResponse { .. } => false,
            Self::Response {
                code,
                status,
                retryable,
                ..
            } => {
                if matches!(*status, 401 | 403 | 404 | 409)
                    || matches!(
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
                    )
                    || referral::is_never_retryable_error_code(code)
                {
                    return false;
                }
                *retryable
            }
        }
    }

    fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::Proxy { retry_after, .. } => *retry_after,
            Self::Response { retry_after, .. } if self.is_retryable() => *retry_after,
            Self::Transport { .. } | Self::InvalidResponse { .. } | Self::Response { .. } => None,
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
        if config.staging_access.is_some() && config.origin.as_str() != STAGING_ACCESS_ORIGIN {
            bail!(
                "invalid_request: staging Cloudflare Access credentials require the fixed staging origin"
            );
        }
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

    pub(super) fn checkout(
        &self,
        access_token: &str,
        referral_claim_token: Option<&str>,
    ) -> Result<CheckoutResult> {
        referral::validate_optional_claim_token(referral_claim_token)?;
        let result: CheckoutResult = self.post(
            "/v1/billing/checkout",
            access_token,
            &CheckoutRequest {
                referral_claim_token,
            },
        )?;
        result.validate()?;
        Ok(result)
    }

    pub(super) fn portal(&self, access_token: &str) -> Result<PortalResult> {
        let result: PortalResult =
            self.post("/v1/billing/portal", access_token, &EmptyRequest {})?;
        if result.kind != "portal_created" {
            bail!("invalid_response: commercial API returned an invalid portal result");
        }
        urls::validate_portal_url(&result.url)?;
        Ok(result)
    }

    pub(super) fn bootstrap_workos_personal_organization(
        &self,
        access_token: &str,
    ) -> Result<String> {
        let result: WorkOsBootstrapResult =
            self.post("/v1/identity/bootstrap", access_token, &EmptyRequest {})?;
        validate_identifier(&result.organization_id, "organization")?;
        Ok(result.organization_id)
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

    pub(super) fn trial_challenge(
        &self,
        request: TrialChallengeRequest<'_>,
    ) -> Result<TrialChallenge> {
        validate_fixed_base64url(
            request.installation_public_key_base64url,
            "installation public key",
        )?;
        referral::validate_optional_codename(request.referral_codename)?;
        let challenge: TrialChallenge =
            self.post_authorized("/v1/trials/challenge", None, &request)?;
        challenge.validate()?;
        Ok(challenge)
    }

    pub(super) fn activate_trial(
        &self,
        trial_activation_token: &str,
        challenge_id: &str,
        installation_public_key_base64url: &str,
        evidence: &serde_json::Value,
    ) -> Result<TrialActivation> {
        validate_identifier(challenge_id, "trial challenge")?;
        validate_fixed_base64url(installation_public_key_base64url, "installation public key")?;
        let authorization = trial_authorization(trial_activation_token)?;
        let activation: TrialActivation = self.post_authorized(
            "/v1/trials/activate",
            Some(authorization),
            &TrialActivationRequest {
                schema_version: 1,
                challenge_id,
                installation_public_key_base64url,
                evidence,
            },
        )?;
        activation.validate()?;
        Ok(activation)
    }

    pub(super) fn refresh_trial(
        &self,
        access_token: &str,
        installation_public_key_base64url: &str,
    ) -> Result<TrialRefresh> {
        validate_fixed_base64url(installation_public_key_base64url, "installation public key")?;
        let authorization = trial_authorization(access_token)?;
        let refresh: TrialRefresh = self.post_authorized(
            "/v1/trials/refresh",
            Some(authorization),
            &TrialRefreshRequest {
                schema_version: 1,
                installation_public_key_base64url,
            },
        )?;
        refresh.validate()?;
        Ok(refresh)
    }

    pub(super) fn config(&self) -> &CommercialApiConfig {
        &self.config
    }

    fn get<T: DeserializeOwned>(&self, path: &str, access_token: &str) -> Result<T> {
        let url = self.endpoint(path)?;
        let response = {
            let authorization = bearer_authorization(access_token)?;
            let request = self.config.apply_transport_credentials(
                self.agent
                    .get(url.as_str())
                    .set("accept", COMMERCIAL_ACCEPT)
                    .set("authorization", authorization.as_str()),
                &url,
            )?;
            request.call()
        };
        self.finish(response, "commercial API request")
    }

    fn post<T: DeserializeOwned, B: Serialize>(
        &self,
        path: &str,
        access_token: &str,
        body: &B,
    ) -> Result<T> {
        let authorization = bearer_authorization(access_token)?;
        self.post_authorized(path, Some(authorization), body)
    }

    fn post_authorized<T: DeserializeOwned, B: Serialize>(
        &self,
        path: &str,
        authorization: Option<Zeroizing<String>>,
        body: &B,
    ) -> Result<T> {
        let url = self.endpoint(path)?;
        let mut body =
            serde_json::to_vec(body).context("invalid_request: encode commercial request")?;
        if body.len() > 16 * 1024 {
            body.zeroize();
            bail!("invalid_request: commercial request is too large");
        }
        let mut request = self.config.apply_transport_credentials(
            self.agent
                .post(url.as_str())
                .set("accept", COMMERCIAL_ACCEPT)
                .set("content-type", "application/json")
                .set("idempotency-key", &new_idempotency_key("cli")?),
            &url,
        )?;
        if let Some(authorization) = authorization.as_deref() {
            validate_authorization_header(authorization)?;
            request = request.set("authorization", authorization);
        }
        let response = request.send_bytes(&body);
        body.zeroize();
        drop(authorization);
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
            Err(error) => Err(commercial_http_error(error, operation)),
        }
    }

    fn endpoint(&self, path: &str) -> Result<Url> {
        self.config
            .origin
            .join(path)
            .context("invalid_request: invalid commercial API route")
    }
}

impl TrialChallenge {
    fn validate(&self) -> Result<()> {
        validate_identifier(&self.challenge_id, "trial challenge")?;
        validate_fixed_base64url(&self.challenge_base64url, "trial challenge")?;
        validate_trial_token(&self.trial_activation_token)?;
        let now = unix_time()?;
        if self.expires_at_unix <= now
            || self.expires_at_unix > now.saturating_add(MAX_TRIAL_CHALLENGE_LIFETIME_SECONDS)
        {
            bail!("invalid_response: trial challenge expiry is outside allowed bounds");
        }
        Ok(())
    }
}

impl TrialActivation {
    fn validate(&self) -> Result<()> {
        if !matches!(
            self.disposition.as_str(),
            "trial_started" | "trial_existing"
        ) {
            bail!("invalid_response: trial activation disposition is invalid");
        }
        validate_trial_token(&self.trial_access_token)?;
        referral::validate_optional_claim_token(self.referral_claim_token.as_deref())?;
        validate_timestamp(Some(self.trial_deadline_unix), "trial deadline")
    }
}

impl TrialRefresh {
    fn validate(&self) -> Result<()> {
        validate_trial_token(&self.trial_access_token)?;
        referral::validate_optional_claim_token(self.referral_claim_token.as_deref())?;
        validate_timestamp(Some(self.trial_deadline_unix), "trial deadline")
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
                urls::validate_checkout_url(url)?;
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
struct CheckoutRequest<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    referral_claim_token: Option<&'a str>,
}

#[derive(Serialize)]
struct EntitlementRequest<'a> {
    installation_public_key_base64url: &'a str,
}

#[derive(Serialize)]
pub(super) struct TrialChallengeRequest<'a> {
    pub(super) schema_version: u16,
    pub(super) channel: &'a str,
    pub(super) target: &'a str,
    pub(super) current_version: Option<&'a str>,
    pub(super) protocol_version: u16,
    pub(super) protocol_fingerprint: &'a str,
    pub(super) installation_public_key_base64url: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) referral_codename: Option<&'a str>,
}

#[derive(Serialize)]
struct TrialActivationRequest<'a> {
    schema_version: u16,
    challenge_id: &'a str,
    installation_public_key_base64url: &'a str,
    evidence: &'a serde_json::Value,
}

#[derive(Serialize)]
struct TrialRefreshRequest<'a> {
    schema_version: u16,
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

fn validate_access_header_value(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAX_ACCESS_HEADER_BYTES
        || !value.bytes().all(|byte| byte.is_ascii_graphic())
    {
        bail!("invalid_request: staging Cloudflare Access credential is invalid");
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

fn validate_authorization_header(value: &str) -> Result<()> {
    if value.len() > MAX_ACCESS_TOKEN_BYTES
        || value.bytes().any(|byte| byte.is_ascii_control())
        || !(value.starts_with("Bearer ") || value.starts_with("CtxTrial "))
    {
        bail!("authentication_required: commercial access token is unavailable");
    }
    Ok(())
}

fn bearer_authorization(token: &str) -> Result<Zeroizing<String>> {
    validate_access_token(token)?;
    Ok(Zeroizing::new(format!("Bearer {token}")))
}

fn trial_authorization(token: &str) -> Result<Zeroizing<String>> {
    validate_trial_token(token)?;
    Ok(Zeroizing::new(format!("CtxTrial {token}")))
}

fn validate_trial_token(token: &str) -> Result<()> {
    if invalid_trial_token(token) {
        bail!("invalid_response: anonymous trial credential is invalid");
    }
    Ok(())
}

fn invalid_trial_token(token: &str) -> bool {
    token.len() < 16
        || token.len() > MAX_TRIAL_ACCESS_TOKEN_BYTES
        || token.bytes().any(|byte| {
            !(byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~'))
        })
}

fn validate_fixed_base64url(value: &str, label: &str) -> Result<()> {
    if value.len() != 43
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("invalid_request: {label} is invalid");
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

pub(super) fn unix_time() -> Result<i64> {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("invalid_response: system clock is before Unix epoch")?
            .as_secs(),
    )
    .context("invalid_response: system time is invalid")
}

pub(super) fn commercial_retry_after(error: &anyhow::Error) -> Option<Duration> {
    response::commercial_retry_after(error)
}

pub(super) fn checkout_retry_after(error: &anyhow::Error) -> Option<Duration> {
    commercial_retry_after(error)
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
