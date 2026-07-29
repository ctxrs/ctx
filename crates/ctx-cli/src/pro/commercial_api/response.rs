use std::io::Read as _;

use super::*;

pub(super) fn validate_envelope(version: &str, request_id: &str) -> Result<()> {
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

pub(super) fn commercial_error_message(error: &ApiError) -> &'static str {
    if let Some(message) = referral::commercial_error_message(&error.code) {
        return message;
    }
    match error.code.as_str() {
        "authentication_required" => "sign in again with `ctx pro`",
        "billing_conflict" => {
            "multiple active subscriptions need attention; run `ctx pro manage` to resolve them"
        }
        "commercial_access_locked" => "an active trial or subscription is required",
        "commercial_identity_conflict" => {
            "the billing customer belongs to a different signed-in account; rerun `ctx pro` with the original account"
        }
        "not_found" => "the requested commercial resource was not found",
        "rate_limited" => "the commercial service is rate limited; retry shortly",
        _ if error.retryable => "the commercial service is temporarily unavailable",
        _ => "the commercial service rejected the request",
    }
}

pub(in crate::pro) fn commercial_http_error(error: ureq::Error, operation: &str) -> anyhow::Error {
    match error {
        ureq::Error::Status(status, response) => commercial_status_failure(status, response).into(),
        ureq::Error::Transport(_) => CommercialApiFailure::Transport {
            operation: operation.to_owned(),
        }
        .into(),
    }
}

fn commercial_status_failure(status: u16, response: ureq::Response) -> CommercialApiFailure {
    let retry_after = parse_retry_after(response.header("retry-after"));
    match classify_error_response(status, response.header("content-type")) {
        ErrorResponseKind::Contracted => read_json(response, "commercial API error")
            .and_then(|failure| typed_api_failure(status, retry_after, failure))
            .unwrap_or_else(|_| malformed_error_failure(status, retry_after)),
        ErrorResponseKind::TransientProxy => CommercialApiFailure::Proxy {
            status,
            retry_after,
        },
        ErrorResponseKind::Invalid => CommercialApiFailure::InvalidResponse { status },
    }
}

pub(super) fn commercial_retry_after(error: &anyhow::Error) -> Option<Duration> {
    error
        .downcast_ref::<CommercialApiFailure>()
        .and_then(CommercialApiFailure::retry_after)
}

pub(in crate::pro) fn is_retryable_checkout_failure(error: &anyhow::Error) -> bool {
    if let Some(failure) = error.downcast_ref::<CommercialApiFailure>() {
        return failure.is_retryable();
    }
    let message = error.to_string();
    message.starts_with("service_unavailable:") || message.starts_with("rate_limited:")
}

pub(super) fn parse_retry_after(value: Option<&str>) -> Option<Duration> {
    let seconds = value?
        .trim()
        .parse::<u64>()
        .ok()?
        .min(MAX_RETRY_AFTER_SECONDS);
    Some(Duration::from_secs(seconds))
}

pub(super) fn require_success_json(response: &ureq::Response) -> Result<()> {
    if !media_type_is(response.header("content-type"), "application/json") {
        bail!("invalid_response: commercial API response is not JSON");
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ErrorResponseKind {
    Contracted,
    TransientProxy,
    Invalid,
}

pub(super) fn classify_error_response(
    status: u16,
    content_type: Option<&str>,
) -> ErrorResponseKind {
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

pub(super) fn media_type_is(value: Option<&str>, expected: &str) -> bool {
    value
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case(expected))
}

pub(super) fn typed_api_failure(
    status: u16,
    retry_after: Option<Duration>,
    failure: ApiFailure,
) -> Result<CommercialApiFailure> {
    validate_envelope(&failure.api_version, &failure.request_id)?;
    validate_error(&failure.error)?;
    let message = commercial_error_message(&failure.error);
    let Some(code) = public_commercial_error_code(&failure.error.code) else {
        return Ok(CommercialApiFailure::InvalidResponse { status });
    };
    Ok(CommercialApiFailure::Response {
        code,
        message,
        status,
        retryable: failure.error.retryable,
        retry_after,
    })
}

fn public_commercial_error_code(code: &str) -> Option<String> {
    if let Some(code) = referral::public_commercial_error_code(code) {
        return Some(code.to_owned());
    }
    match code {
        "authentication_required"
        | "billing_conflict"
        | "commercial_access_locked"
        | "commercial_identity_conflict"
        | "not_found"
        | "rate_limited"
        | "anonymous_trial_already_consumed"
        | "anonymous_trial_identity_ambiguous"
        | "anonymous_trial_installation_limit" => Some(code.to_owned()),
        "dependency_timeout" | "dependency_unavailable" | "service_unavailable" => {
            Some("service_unavailable".to_owned())
        }
        "dependency_invalid_response" | "invalid_response" => Some("invalid_response".to_owned()),
        "idempotency_conflict"
        | "idempotency_key_invalid"
        | "idempotency_key_required"
        | "invalid_json"
        | "invalid_installation_public_key"
        | "invalid_request"
        | "method_not_allowed"
        | "request_body_too_large"
        | "unsupported_media_type" => Some("invalid_request".to_owned()),
        _ => None,
    }
}

pub(super) fn malformed_error_failure(
    status: u16,
    retry_after: Option<Duration>,
) -> CommercialApiFailure {
    if matches!(status, 429 | 502 | 503 | 504) {
        CommercialApiFailure::Proxy {
            status,
            retry_after,
        }
    } else {
        CommercialApiFailure::InvalidResponse { status }
    }
}

pub(super) fn read_success_json<T: DeserializeOwned>(response: ureq::Response) -> Result<T> {
    match read_json(response, "commercial API response") {
        Ok(value) => Ok(value),
        Err(error) if error.to_string().starts_with("service_unavailable:") => {
            bail!("service_unavailable: read commercial API response")
        }
        Err(_) => bail!("invalid_response: commercial API response is malformed"),
    }
}

fn read_json<T: DeserializeOwned>(response: ureq::Response, label: &str) -> Result<T> {
    let mut bytes = Zeroizing::new(Vec::with_capacity(MAX_API_RESPONSE_BYTES as usize + 1));
    response
        .into_reader()
        .take(MAX_API_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("service_unavailable: read {label}"))?;
    if bytes.len() as u64 > MAX_API_RESPONSE_BYTES {
        bytes.zeroize();
        bail!("invalid_response: {label} is too large");
    }
    let parsed =
        serde_json::from_slice(&bytes).with_context(|| format!("invalid_response: parse {label}"));
    bytes.zeroize();
    parsed
}
