use std::{
    io::Read,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::{de::Deserializer, Deserialize};
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
const MAX_USER_PROFILE_FIELDS: usize = 32;
const MAX_USER_PROFILE_DEPTH: usize = 8;
const MAX_USER_PROFILE_NODES: usize = 256;
const MAX_USER_PROFILE_STRING_BYTES: usize = 16 * 1024;
const MAX_USER_PROFILE_KEY_BYTES: usize = 256;
const REQUIRED_PERMISSION: &str = "ctx-pro:access";

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
    #[serde(default, deserialize_with = "deserialize_optional_present_string")]
    pub(super) organization_id: Option<String>,
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
            .field("organization_id", &self.organization_id.as_deref())
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
struct WorkOsUser {
    #[serde(rename = "object")]
    _object: WorkOsUserObject,
    id: String,
    // WorkOS user profiles contain non-security display and metadata fields.
    // Keep those bounded separately while parsing the identity fields above
    // with their exact documented types.
    #[serde(flatten)]
    ignored: std::collections::BTreeMap<String, Value>,
}

#[derive(Clone, Deserialize)]
enum WorkOsUserObject {
    #[serde(rename = "user")]
    User,
}

#[derive(Deserialize)]
struct OAuthError {
    error: String,
}

#[derive(Debug, Deserialize)]
struct AccessClaims {
    sub: String,
    sid: String,
    client_id: String,
    #[serde(default, deserialize_with = "deserialize_optional_present_string")]
    org_id: Option<String>,
    exp: i64,
    iat: i64,
    #[serde(default)]
    nbf: Option<i64>,
    #[serde(default)]
    aud: Option<Value>,
    #[serde(default)]
    scope: Option<Value>,
    #[serde(default, deserialize_with = "deserialize_optional_permissions")]
    permissions: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkOsTokenDisposition {
    BootstrapPending,
    OrganizationBound,
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
        let tokens = self.refresh_inner(refresh_token, None)?;
        if self.validate_tokens(&tokens)? != WorkOsTokenDisposition::OrganizationBound {
            bail!("authentication_invalid: WorkOS token refresh has no organization");
        }
        Ok(tokens)
    }

    pub(super) fn refresh_for_organization(
        &self,
        refresh_token: &str,
        organization_id: &str,
    ) -> Result<WorkOsTokens> {
        validate_identifier(organization_id, "organization")?;
        let tokens = self.refresh_inner(refresh_token, Some(organization_id))?;
        self.validate_refreshed_organization(&tokens, organization_id)?;
        Ok(tokens)
    }

    fn validate_refreshed_organization(
        &self,
        tokens: &WorkOsTokens,
        organization_id: &str,
    ) -> Result<()> {
        if self.validate_tokens(&tokens)? != WorkOsTokenDisposition::OrganizationBound
            || tokens.organization_id.as_deref() != Some(organization_id)
        {
            bail!("authentication_invalid: WorkOS token refresh selected another organization");
        }
        Ok(())
    }

    fn refresh_inner(
        &self,
        refresh_token: &str,
        organization_id: Option<&str>,
    ) -> Result<WorkOsTokens> {
        validate_secret(refresh_token, "refresh token")?;
        let url = self.endpoint("/user_management/authenticate")?;
        let form = refresh_form(
            refresh_token,
            self.config.client_id.as_str(),
            organization_id,
        );
        let response = self
            .agent
            .post(url.as_str())
            .set("content-type", "application/x-www-form-urlencoded")
            .send_form(&form)
            .map_err(|error| safe_http_error(error, "WorkOS token refresh"))?;
        require_json(&response, "WorkOS token refresh")?;
        let tokens: WorkOsTokens = read_json(response, "WorkOS token refresh")?;
        Ok(tokens)
    }

    pub(super) fn validate_access_token(&self, access_token: &str) -> Result<()> {
        let claims = claims(access_token)?;
        validate_identifier(&claims.sub, "subject")?;
        validate_identifier(&claims.sid, "session")?;
        validate_identifier(
            claims
                .org_id
                .as_deref()
                .ok_or_else(|| anyhow!("authentication_invalid: WorkOS organization is missing"))?,
            "organization",
        )?;
        validate_claim_time_and_audience(&claims, &self.config.client_id)?;
        validate_scope(claims.scope.as_ref())?;
        validate_permissions(claims.permissions.as_deref(), true)
    }

    pub(super) fn access_token_expiration(&self, access_token: &str) -> Result<i64> {
        self.validate_access_token(access_token)?;
        Ok(claims(access_token)?.exp)
    }

    pub(super) fn bootstrap_pending(&self, tokens: &WorkOsTokens) -> Result<bool> {
        Ok(self.validate_tokens(tokens)? == WorkOsTokenDisposition::BootstrapPending)
    }

    fn validate_tokens(&self, tokens: &WorkOsTokens) -> Result<WorkOsTokenDisposition> {
        validate_secret(&tokens.access_token, "access token")?;
        validate_secret(&tokens.refresh_token, "refresh token")?;
        if let Some(organization_id) = tokens.organization_id.as_deref() {
            validate_identifier(organization_id, "organization")?;
        }
        validate_identifier(&tokens.user.id, "user")?;
        if !tokens.user_profile_is_bounded() {
            bail!("authentication_invalid: WorkOS user profile is outside allowed bounds");
        }
        if tokens
            .authentication_method
            .as_ref()
            .is_some_and(|value| value.len() > 128)
        {
            bail!("authentication_invalid: WorkOS authentication method is invalid");
        }
        let claims = claims(&tokens.access_token)?;
        validate_identifier(&claims.sub, "subject")?;
        validate_identifier(&claims.sid, "session")?;
        validate_claim_time_and_audience(&claims, &self.config.client_id)?;
        validate_scope(claims.scope.as_ref())?;
        if claims.sub != tokens.user.id || claims.org_id != tokens.organization_id {
            bail!("authentication_invalid: WorkOS token identity is inconsistent");
        }
        match tokens.organization_id.as_deref() {
            Some(organization_id) => {
                validate_identifier(organization_id, "organization")?;
                validate_permissions(claims.permissions.as_deref(), true)?;
                Ok(WorkOsTokenDisposition::OrganizationBound)
            }
            None => {
                validate_permissions(claims.permissions.as_deref(), false)?;
                Ok(WorkOsTokenDisposition::BootstrapPending)
            }
        }
    }

    fn endpoint(&self, path: &str) -> Result<Url> {
        self.config
            .api_origin
            .join(path)
            .context("invalid_request: invalid WorkOS route")
    }
}

fn refresh_form<'a>(
    refresh_token: &'a str,
    client_id: &'a str,
    organization_id: Option<&'a str>,
) -> Vec<(&'a str, &'a str)> {
    let mut form = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", client_id),
    ];
    if let Some(organization_id) = organization_id {
        form.push(("organization_id", organization_id));
    }
    form
}

impl WorkOsTokens {
    fn ignored_fields(&self) -> usize {
        self.user.ignored.len()
    }

    fn user_profile_is_bounded(&self) -> bool {
        if self.ignored_fields() > MAX_USER_PROFILE_FIELDS {
            return false;
        }
        let mut remaining_nodes = MAX_USER_PROFILE_NODES;
        self.user.ignored.iter().all(|(key, value)| {
            key.len() <= MAX_USER_PROFILE_KEY_BYTES
                && bounded_json_value(value, 0, &mut remaining_nodes)
        })
    }
}

fn bounded_json_value(value: &Value, depth: usize, remaining_nodes: &mut usize) -> bool {
    if depth > MAX_USER_PROFILE_DEPTH || *remaining_nodes == 0 {
        return false;
    }
    *remaining_nodes -= 1;
    match value {
        Value::String(value) => value.len() <= MAX_USER_PROFILE_STRING_BYTES,
        Value::Array(values) => values
            .iter()
            .all(|value| bounded_json_value(value, depth + 1, remaining_nodes)),
        Value::Object(values) => values.iter().all(|(key, value)| {
            key.len() <= MAX_USER_PROFILE_KEY_BYTES
                && bounded_json_value(value, depth + 1, remaining_nodes)
        }),
        Value::Null | Value::Bool(_) | Value::Number(_) => true,
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

fn validate_claim_time_and_audience(claims: &AccessClaims, client_id: &str) -> Result<()> {
    let now = unix_time()?;
    if claims.exp <= now - 60
        || claims.iat > now + 60
        || claims.nbf.is_some_and(|nbf| nbf > now + 60)
    {
        bail!("authentication_expired: WorkOS access token is expired or not active");
    }
    // AuthKit access tokens bind the application with `client_id`; `aud` is
    // optional. Require the documented client binding and, when present, a
    // matching audience as defense in depth.
    if claims.client_id != client_id
        || claims
        .aud
        .as_ref()
        .is_some_and(|aud| !audience_contains(aud, client_id))
    {
        bail!("authentication_invalid: WorkOS access token audience does not match ctx");
    }
    Ok(())
}

fn validate_permissions(permissions: Option<&[String]>, organization_bound: bool) -> Result<()> {
    let permissions = permissions.unwrap_or_default();
    if permissions.len() > 128
        || permissions.iter().any(|permission| {
            permission.len() < 3
                || permission.len() > 128
                || !permission
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_lowercase)
                || !permission.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'_' | b'.' | b':' | b'-')
                })
        })
        || permissions
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != permissions.len()
        || (organization_bound && !permissions.iter().any(|value| value == REQUIRED_PERMISSION))
        || (!organization_bound && !permissions.is_empty())
    {
        bail!("authentication_invalid: WorkOS access token permissions are invalid");
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

fn deserialize_optional_present_string<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    String::deserialize(deserializer).map(Some)
}

fn deserialize_optional_permissions<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    Vec::<String>::deserialize(deserializer).map(Some)
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

    fn token_response() -> Value {
        serde_json::json!({
            "access_token": "access.secret",
            "refresh_token": "refresh-secret",
            "organization_id": "org_123",
            "authentication_method": "Password",
            "user": {
                "object": "user",
                "id": "user_123",
                "first_name": "Ada",
                "last_name": "Lovelace",
                "profile_picture_url": null,
                "email": "ada@example.test",
                "email_verified": true,
                "external_id": null,
                "metadata": {},
                "last_sign_in_at": "2026-07-23T12:00:00.000Z",
                "locale": "en-US",
                "created_at": "2026-07-23T12:00:00.000Z",
                "updated_at": "2026-07-23T12:00:00.000Z"
            }
        })
    }

    fn jwt(claims: Value) -> String {
        format!(
            "e30.{}.signature",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap())
        )
    }

    #[test]
    fn parses_documented_token_response() {
        let tokens: WorkOsTokens = serde_json::from_value(token_response()).unwrap();

        assert_eq!(tokens.organization_id.as_deref(), Some("org_123"));
        assert_eq!(tokens.user.id, "user_123");
        assert_eq!(tokens.ignored_fields(), 11);
    }

    #[test]
    fn parses_a_bounded_zero_organization_bootstrap_response() {
        let mut response = token_response();
        response.as_object_mut().unwrap().remove("organization_id");
        let tokens: WorkOsTokens = serde_json::from_value(response).unwrap();
        assert!(tokens.organization_id.is_none());
    }

    #[test]
    fn rejects_null_organization_instead_of_treating_it_as_bootstrap_pending() {
        let mut response = token_response();
        response["organization_id"] = Value::Null;
        assert!(serde_json::from_value::<WorkOsTokens>(response).is_err());
    }

    #[test]
    fn rejects_unknown_top_level_token_field() {
        let mut response = token_response();
        response["unexpected"] = Value::Bool(true);

        let error = serde_json::from_value::<WorkOsTokens>(response).unwrap_err();

        assert!(error.to_string().contains("unknown field `unexpected`"));
    }

    #[test]
    fn rejects_non_string_user_object() {
        for invalid in [Value::Null, Value::Bool(true)] {
            let mut response = token_response();
            response["user"]["object"] = invalid;

            assert!(serde_json::from_value::<WorkOsTokens>(response).is_err());
        }
    }

    #[test]
    fn rejects_missing_user_object() {
        let mut response = token_response();
        response["user"].as_object_mut().unwrap().remove("object");

        assert!(serde_json::from_value::<WorkOsTokens>(response).is_err());
    }

    #[test]
    fn rejects_wrong_user_object_literal() {
        let mut response = token_response();
        response["user"]["object"] = Value::String("organization".to_owned());

        assert!(serde_json::from_value::<WorkOsTokens>(response).is_err());
    }

    #[test]
    fn token_debug_redacts_secrets() {
        let tokens: WorkOsTokens = serde_json::from_value(token_response()).unwrap();
        let debug = format!("{tokens:?}");

        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("access.secret"));
        assert!(!debug.contains("refresh-secret"));
    }

    #[test]
    fn rejects_excessive_user_profile_depth_and_size() {
        let mut deeply_nested = Value::Null;
        for _ in 0..=MAX_USER_PROFILE_DEPTH {
            deeply_nested = serde_json::json!({"nested": deeply_nested});
        }
        let mut deep_response = token_response();
        deep_response["user"]["metadata"] = deeply_nested;
        let deep_tokens: WorkOsTokens = serde_json::from_value(deep_response).unwrap();
        assert!(!deep_tokens.user_profile_is_bounded());

        let mut wide_response = token_response();
        let user = wide_response["user"].as_object_mut().unwrap();
        for index in 0..=MAX_USER_PROFILE_FIELDS {
            user.insert(format!("field_{index}"), Value::Null);
        }
        let wide_tokens: WorkOsTokens = serde_json::from_value(wide_response).unwrap();
        assert!(!wide_tokens.user_profile_is_bounded());
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
            "client_id":"client_123456",
            "iat":now, "exp":now+300, "aud":"client_123456", "scope":"openid profile",
            "permissions":[REQUIRED_PERMISSION]
        }));
        client.validate_access_token(&token).unwrap();
    }

    #[test]
    fn distinguishes_bootstrap_pending_from_organization_bound_tokens() {
        let client = WorkOsDeviceClient::new(WorkOsConfig {
            api_origin: Url::parse("https://api.workos.com/").unwrap(),
            client_id: "client_123456".to_owned(),
        })
        .unwrap();
        let now = unix_time().unwrap();
        let mut pending_response = token_response();
        pending_response
            .as_object_mut()
            .unwrap()
            .remove("organization_id");
        pending_response["access_token"] = Value::String(jwt(serde_json::json!({
            "sub":"user_123", "sid":"session_123",
            "client_id":"client_123456",
            "iat":now, "exp":now+300, "aud":"client_123456"
        })));
        let pending: WorkOsTokens = serde_json::from_value(pending_response).unwrap();
        assert!(client.bootstrap_pending(&pending).unwrap());

        let mut bound_response = token_response();
        bound_response["access_token"] = Value::String(jwt(serde_json::json!({
            "sub":"user_123", "sid":"session_123", "org_id":"org_123",
            "client_id":"client_123456",
            "iat":now, "exp":now+300, "aud":"client_123456",
            "permissions":[REQUIRED_PERMISSION]
        })));
        let bound: WorkOsTokens = serde_json::from_value(bound_response).unwrap();
        assert!(!client.bootstrap_pending(&bound).unwrap());
    }

    #[test]
    fn bootstrap_pending_rejects_org_permissions_and_bound_tokens_require_access() {
        let client = WorkOsDeviceClient::new(WorkOsConfig {
            api_origin: Url::parse("https://api.workos.com/").unwrap(),
            client_id: "client_123456".to_owned(),
        })
        .unwrap();
        let now = unix_time().unwrap();
        for permissions in [
            serde_json::json!([REQUIRED_PERMISSION]),
            serde_json::json!(["ctx-pro:other"]),
        ] {
            let mut response = token_response();
            response.as_object_mut().unwrap().remove("organization_id");
            response["access_token"] = Value::String(jwt(serde_json::json!({
                "sub":"user_123", "sid":"session_123",
                "client_id":"client_123456",
                "iat":now, "exp":now+300, "aud":"client_123456",
                "permissions":permissions
            })));
            let tokens: WorkOsTokens = serde_json::from_value(response).unwrap();
            assert!(client.bootstrap_pending(&tokens).is_err());
        }

        let mut response = token_response();
        response["access_token"] = Value::String(jwt(serde_json::json!({
            "sub":"user_123", "sid":"session_123", "org_id":"org_123",
            "client_id":"client_123456",
            "iat":now, "exp":now+300, "aud":"client_123456",
            "permissions":[]
        })));
        let tokens: WorkOsTokens = serde_json::from_value(response).unwrap();
        assert!(client.bootstrap_pending(&tokens).is_err());
    }

    #[test]
    fn organization_bootstrap_refresh_selects_the_server_returned_organization() {
        assert_eq!(
            refresh_form(
                "refresh-secret",
                "client_123456",
                Some("org_server_selected")
            ),
            [
                ("grant_type", "refresh_token"),
                ("refresh_token", "refresh-secret"),
                ("client_id", "client_123456"),
                ("organization_id", "org_server_selected"),
            ]
        );
        assert_eq!(
            refresh_form("refresh-secret", "client_123456", None),
            [
                ("grant_type", "refresh_token"),
                ("refresh_token", "refresh-secret"),
                ("client_id", "client_123456"),
            ]
        );

        let client = WorkOsDeviceClient::new(WorkOsConfig {
            api_origin: Url::parse("https://api.workos.com/").unwrap(),
            client_id: "client_123456".to_owned(),
        })
        .unwrap();
        let now = unix_time().unwrap();
        let mut response = token_response();
        response["access_token"] = Value::String(jwt(serde_json::json!({
            "sub":"user_123", "sid":"session_123", "org_id":"org_123",
            "client_id":"client_123456",
            "iat":now, "exp":now+300, "aud":"client_123456",
            "permissions":[REQUIRED_PERMISSION]
        })));
        let tokens: WorkOsTokens = serde_json::from_value(response).unwrap();
        client
            .validate_refreshed_organization(&tokens, "org_123")
            .unwrap();
        assert!(client
            .validate_refreshed_organization(&tokens, "org_other")
            .is_err());
    }

    #[test]
    fn accepts_undocumented_missing_audience_but_rejects_wrong_audience_and_malformed_scope() {
        let client = WorkOsDeviceClient::new(WorkOsConfig {
            api_origin: Url::parse("https://api.workos.com/").unwrap(),
            client_id: "client_123456".to_owned(),
        })
        .unwrap();
        let now = unix_time().unwrap();
        client
            .validate_access_token(&jwt(serde_json::json!({
                "sub":"user_123","sid":"session_123","org_id":"org_123",
                "client_id":"client_123456",
                "iat":now,"exp":now+300,"permissions":[REQUIRED_PERMISSION]
            })))
            .unwrap();
        for claims in [
            serde_json::json!({"sub":"user_123","sid":"session_123","org_id":"org_123","client_id":"other_123","iat":now,"exp":now+300,"permissions":[REQUIRED_PERMISSION]}),
            serde_json::json!({"sub":"user_123","sid":"session_123","org_id":"org_123","client_id":"client_123456","iat":now,"exp":now+300,"aud":"other_123","permissions":[REQUIRED_PERMISSION]}),
            serde_json::json!({"sub":"user_123","sid":"session_123","org_id":"org_123","client_id":"client_123456","iat":now,"exp":now+300,"aud":"client_123456","scope":{"bad":true},"permissions":[REQUIRED_PERMISSION]}),
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
