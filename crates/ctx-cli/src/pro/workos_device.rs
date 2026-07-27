use std::{
    io::Read,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::Deserialize;
use serde_json::Value;
use url::Url;
use zeroize::Zeroize as _;

const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
const MAX_RESPONSE_BYTES: u64 = 96 * 1024;
const MAX_TOKEN_BYTES: usize = 16 * 1024;
const MAX_DEVICE_CODE_BYTES: usize = 1024;
const MAX_USER_CODE_BYTES: usize = 64;
const MAX_DEVICE_LIFETIME_SECONDS: u64 = 15 * 60;
const MAX_POLL_INTERVAL_SECONDS: u64 = 30;

#[derive(Debug, Clone)]
pub(super) struct WorkOsConfig {
    pub(super) api_origin: Url,
    pub(super) client_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DeviceAuthorization {
    device_code: String,
    pub(super) user_code: String,
    pub(super) verification_uri: String,
    pub(super) verification_uri_complete: String,
    expires_in: u64,
    interval: u64,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WorkOsTokens {
    pub(super) access_token: String,
    pub(super) refresh_token: String,
    pub(super) organization_id: String,
    #[serde(default)]
    authentication_method: Option<String>,
    user: WorkOsUser,
}

impl std::fmt::Debug for WorkOsTokens {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkOsTokens")
            .field("access_token", &"[REDACTED]")
            .field("refresh_token", &"[REDACTED]")
            .field("organization_id", &self.organization_id)
            .finish_non_exhaustive()
    }
}

impl Drop for WorkOsTokens {
    fn drop(&mut self) {
        self.access_token.zeroize();
        self.refresh_token.zeroize();
    }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkOsUser {
    id: String,
    #[serde(flatten)]
    ignored: std::collections::BTreeMap<String, Value>,
}

#[derive(Deserialize)]
struct OAuthError {
    error: String,
}

#[derive(Debug, Deserialize)]
struct AccessClaims {
    sub: String,
    sid: String,
    org_id: String,
    exp: i64,
    iat: i64,
    #[serde(default)]
    nbf: Option<i64>,
    #[serde(default)]
    aud: Option<Value>,
    #[serde(default)]
    scope: Option<Value>,
}

pub(super) struct WorkOsDeviceClient {
    config: WorkOsConfig,
    agent: ureq::Agent,
}

impl WorkOsDeviceClient {
    pub(super) fn new(config: WorkOsConfig) -> Result<Self> {
        validate_config(&config)?;
        Ok(Self {
            config,
            agent: ureq::AgentBuilder::new()
                .redirects(0)
                .timeout_connect(Duration::from_secs(10))
                .timeout_read(Duration::from_secs(15))
                .timeout_write(Duration::from_secs(10))
                .build(),
        })
    }

    pub(super) fn begin(&self) -> Result<DeviceAuthorization> {
        let url = self.endpoint("/user_management/authorize/device")?;
        let response = self
            .agent
            .post(url.as_str())
            .set("content-type", "application/x-www-form-urlencoded")
            .send_form(&[("client_id", self.config.client_id.as_str())])
            .map_err(|error| safe_http_error(error, "WorkOS device authorization"))?;
        require_json(&response, "WorkOS device authorization")?;
        let value: DeviceAuthorization = read_json(response, "WorkOS device authorization")?;
        validate_device_authorization(&value)?;
        Ok(value)
    }

    pub(super) fn poll(&self, device: &DeviceAuthorization) -> Result<WorkOsTokens> {
        validate_device_authorization(device)?;
        let url = self.endpoint("/user_management/authenticate")?;
        let deadline = Instant::now() + Duration::from_secs(device.expires_in);
        let mut interval = device.interval;
        loop {
            if Instant::now() >= deadline {
                bail!("authentication_expired: WorkOS device authorization expired");
            }
            let result = self
                .agent
                .post(url.as_str())
                .set("content-type", "application/x-www-form-urlencoded")
                .send_form(&[
                    ("grant_type", DEVICE_GRANT),
                    ("device_code", device.device_code.as_str()),
                    ("client_id", self.config.client_id.as_str()),
                ]);
            match result {
                Ok(response) => {
                    require_json(&response, "WorkOS token response")?;
                    let tokens: WorkOsTokens = read_json(response, "WorkOS token response")?;
                    self.validate_tokens(&tokens)?;
                    return Ok(tokens);
                }
                Err(ureq::Error::Status(status, response)) => {
                    require_json(&response, "WorkOS token error")?;
                    let error: OAuthError = read_json(response, "WorkOS token error")?;
                    match error.error.as_str() {
                        "authorization_pending" if status == 400 => {}
                        "slow_down" if status == 400 => {
                            interval = interval.saturating_add(1).min(MAX_POLL_INTERVAL_SECONDS);
                        }
                        "access_denied" => {
                            bail!("authentication_denied: WorkOS sign-in was denied")
                        }
                        "expired_token" => {
                            bail!("authentication_expired: WorkOS device authorization expired")
                        }
                        _ => bail!("authentication_failed: WorkOS device authorization failed"),
                    }
                }
                Err(error) => return Err(safe_http_error(error, "WorkOS token request")),
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            thread::sleep(Duration::from_secs(interval).min(remaining));
        }
    }

    pub(super) fn refresh(&self, refresh_token: &str) -> Result<WorkOsTokens> {
        validate_secret(refresh_token, "refresh token")?;
        let url = self.endpoint("/user_management/authenticate")?;
        let response = self
            .agent
            .post(url.as_str())
            .set("content-type", "application/x-www-form-urlencoded")
            .send_form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
                ("client_id", self.config.client_id.as_str()),
            ])
            .map_err(|error| safe_http_error(error, "WorkOS token refresh"))?;
        require_json(&response, "WorkOS token refresh")?;
        let tokens: WorkOsTokens = read_json(response, "WorkOS token refresh")?;
        self.validate_tokens(&tokens)?;
        Ok(tokens)
    }

    pub(super) fn validate_access_token(&self, access_token: &str) -> Result<()> {
        let claims = claims(access_token)?;
        validate_identifier(&claims.sub, "subject")?;
        validate_identifier(&claims.sid, "session")?;
        validate_identifier(&claims.org_id, "organization")?;
        let now = unix_time()?;
        if claims.exp <= now - 60
            || claims.iat > now + 60
            || claims.nbf.is_some_and(|nbf| nbf > now + 60)
        {
            bail!("authentication_expired: WorkOS access token is expired or not active");
        }
        if claims
            .aud
            .as_ref()
            .is_some_and(|aud| !audience_contains(aud, &self.config.client_id))
        {
            bail!("authentication_invalid: WorkOS access token audience does not match ctx");
        }
        validate_scope(claims.scope.as_ref())
    }

    pub(super) fn access_token_expiration(&self, access_token: &str) -> Result<i64> {
        self.validate_access_token(access_token)?;
        Ok(claims(access_token)?.exp)
    }

    fn validate_tokens(&self, tokens: &WorkOsTokens) -> Result<()> {
        validate_secret(&tokens.access_token, "access token")?;
        validate_secret(&tokens.refresh_token, "refresh token")?;
        validate_identifier(&tokens.organization_id, "organization")?;
        validate_identifier(&tokens.user.id, "user")?;
        if tokens.ignored_fields() > 32 {
            bail!("authentication_invalid: WorkOS token response has too many user fields");
        }
        if tokens
            .authentication_method
            .as_ref()
            .is_some_and(|value| value.len() > 128)
        {
            bail!("authentication_invalid: WorkOS authentication method is invalid");
        }
        let claims = claims(&tokens.access_token)?;
        self.validate_access_token(&tokens.access_token)?;
        if claims.org_id != tokens.organization_id || claims.sub != tokens.user.id {
            bail!("authentication_invalid: WorkOS token identity is inconsistent");
        }
        Ok(())
    }

    fn endpoint(&self, path: &str) -> Result<Url> {
        self.config
            .api_origin
            .join(path)
            .context("invalid_request: invalid WorkOS route")
    }
}

impl WorkOsTokens {
    fn ignored_fields(&self) -> usize {
        self.user.ignored.len()
    }
}

fn validate_config(config: &WorkOsConfig) -> Result<()> {
    if config.api_origin.scheme() != "https"
        || config.api_origin.host_str().is_none()
        || config.api_origin.path() != "/"
        || config.api_origin.query().is_some()
        || config.api_origin.fragment().is_some()
        || !config.api_origin.username().is_empty()
        || config.api_origin.password().is_some()
    {
        bail!("invalid_request: WorkOS API must be an HTTPS origin");
    }
    validate_identifier(&config.client_id, "WorkOS client")
}

fn validate_device_authorization(value: &DeviceAuthorization) -> Result<()> {
    if value.device_code.is_empty()
        || value.device_code.len() > MAX_DEVICE_CODE_BYTES
        || value.user_code.is_empty()
        || value.user_code.len() > MAX_USER_CODE_BYTES
        || value.expires_in == 0
        || value.expires_in > MAX_DEVICE_LIFETIME_SECONDS
        || value.interval == 0
        || value.interval > MAX_POLL_INTERVAL_SECONDS
    {
        bail!("authentication_invalid: WorkOS device authorization is outside allowed bounds");
    }
    for candidate in [&value.verification_uri, &value.verification_uri_complete] {
        let parsed = Url::parse(candidate)
            .context("authentication_invalid: invalid WorkOS verification URL")?;
        if parsed.scheme() != "https"
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
        {
            bail!("authentication_invalid: WorkOS verification URL must be HTTPS");
        }
    }
    Ok(())
}

fn claims(token: &str) -> Result<AccessClaims> {
    validate_secret(token, "access token")?;
    let mut segments = token.split('.');
    let (_header, payload, signature) = (segments.next(), segments.next(), segments.next());
    if segments.next().is_some() || payload.is_none() || signature.is_none_or(str::is_empty) {
        bail!("authentication_invalid: WorkOS access token is malformed");
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(payload.unwrap_or_default())
        .context("authentication_invalid: WorkOS access token payload is malformed")?;
    serde_json::from_slice(&bytes)
        .context("authentication_invalid: WorkOS access token claims are malformed")
}

fn validate_scope(scope: Option<&Value>) -> Result<()> {
    let Some(scope) = scope else {
        return Ok(());
    };
    let valid = match scope {
        Value::String(value) => {
            value.len() <= 4096 && value.split_ascii_whitespace().all(valid_scope)
        }
        Value::Array(values) => {
            values.len() <= 64
                && values
                    .iter()
                    .all(|value| value.as_str().is_some_and(valid_scope))
        }
        _ => false,
    };
    if !valid {
        bail!("authentication_invalid: WorkOS access token scope is malformed");
    }
    Ok(())
}

fn valid_scope(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:-/".contains(&byte))
}

fn audience_contains(value: &Value, expected: &str) -> bool {
    value.as_str() == Some(expected)
        || value.as_array().is_some_and(|items| {
            items.len() <= 16
                && items.iter().all(Value::is_string)
                && items.iter().any(|item| item.as_str() == Some(expected))
        })
}

fn validate_identifier(value: &str, label: &str) -> Result<()> {
    if value.len() < 3
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        bail!("authentication_invalid: {label} identifier is invalid");
    }
    Ok(())
}

fn validate_secret(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAX_TOKEN_BYTES
        || value.bytes().any(|byte| byte.is_ascii_control())
    {
        bail!("authentication_invalid: WorkOS {label} is invalid");
    }
    Ok(())
}

fn unix_time() -> Result<i64> {
    Ok(i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("authentication_invalid: system clock is before Unix epoch")?
            .as_secs(),
    )?)
}

fn read_json<T: for<'de> Deserialize<'de>>(response: ureq::Response, label: &str) -> Result<T> {
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("service_unavailable: read {label}"))?;
    if bytes.len() as u64 > MAX_RESPONSE_BYTES {
        bail!("invalid_response: {label} is too large");
    }
    serde_json::from_slice(&bytes).with_context(|| format!("invalid_response: parse {label}"))
}

fn require_json(response: &ureq::Response, label: &str) -> Result<()> {
    let content_type = response.header("content-type").unwrap_or_default();
    if content_type
        .split(';')
        .next()
        .is_none_or(|value| value.trim() != "application/json")
    {
        bail!("invalid_response: {label} is not JSON");
    }
    Ok(())
}

fn safe_http_error(error: ureq::Error, operation: &str) -> anyhow::Error {
    match error {
        ureq::Error::Status(status, _) => {
            anyhow!("authentication_failed: {operation} returned status {status}")
        }
        ureq::Error::Transport(_) => anyhow!("service_unavailable: {operation} failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jwt(claims: Value) -> String {
        format!(
            "e30.{}.signature",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap())
        )
    }

    #[test]
    fn validates_claim_identity_audience_and_scope() {
        let client = WorkOsDeviceClient::new(WorkOsConfig {
            api_origin: Url::parse("https://api.workos.com/").unwrap(),
            client_id: "client_123456".to_owned(),
        })
        .unwrap();
        let now = unix_time().unwrap();
        let token = jwt(serde_json::json!({
            "sub":"user_123", "sid":"session_123", "org_id":"org_123",
            "iat":now, "exp":now+300, "aud":"client_123456", "scope":"openid profile"
        }));
        client.validate_access_token(&token).unwrap();
    }

    #[test]
    fn rejects_wrong_audience_and_malformed_scope() {
        let client = WorkOsDeviceClient::new(WorkOsConfig {
            api_origin: Url::parse("https://api.workos.com/").unwrap(),
            client_id: "client_123456".to_owned(),
        })
        .unwrap();
        let now = unix_time().unwrap();
        for claims in [
            serde_json::json!({"sub":"user_123","sid":"session_123","org_id":"org_123","iat":now,"exp":now+300,"aud":"other_123"}),
            serde_json::json!({"sub":"user_123","sid":"session_123","org_id":"org_123","iat":now,"exp":now+300,"scope":{"bad":true}}),
        ] {
            assert!(client.validate_access_token(&jwt(claims)).is_err());
        }
    }

    #[test]
    fn bounds_device_authorization() {
        let value = DeviceAuthorization {
            device_code: "device".to_owned(),
            user_code: "ABCD-EFGH".to_owned(),
            verification_uri: "https://auth.example/device".to_owned(),
            verification_uri_complete: "https://auth.example/device?user_code=ABCD-EFGH".to_owned(),
            expires_in: 300,
            interval: 5,
        };
        validate_device_authorization(&value).unwrap();
    }
}
