use axum::http::{header, HeaderMap};
use url::Url;

use super::headers::{header_first_value, parse_forwarded_header};
use super::host::is_safe_request_base_host;

pub(in crate::api) fn resolve_request_base_url(
    headers: &HeaderMap,
    fallback: &str,
    public_base_url: Option<&str>,
) -> Option<String> {
    if let Some(public_base_url) = public_base_url {
        let trimmed = public_base_url.trim().trim_end_matches('/');
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    let fallback = fallback.trim_end_matches('/');
    let fallback_url = Url::parse(fallback).ok();
    let fallback_base = fallback_url.as_ref().and_then(|url| {
        let host = url.host_str()?;
        let host = match url.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_string(),
        };
        if is_safe_request_base_host(&host) && matches!(url.scheme(), "http" | "https") {
            Some(format!("{}://{}", url.scheme(), host.trim_end_matches('/')))
        } else {
            None
        }
    });

    let (forwarded_proto, forwarded_host) = headers
        .get(header::FORWARDED)
        .and_then(|value| value.to_str().ok())
        .map(parse_forwarded_header)
        .unwrap_or((None, None));

    let proto = forwarded_proto
        .or_else(|| header_first_value(headers, "x-forwarded-proto"))
        .unwrap_or_else(|| {
            fallback_url
                .as_ref()
                .map(|url| url.scheme().to_string())
                .unwrap_or_else(|| "http".to_string())
        })
        .trim()
        .trim_end_matches(':')
        .to_ascii_lowercase();
    let host = forwarded_host
        .or_else(|| header_first_value(headers, "x-forwarded-host"))
        .or_else(|| header_first_value(headers, header::HOST.as_str()));

    match host {
        Some(host)
            if is_safe_request_base_host(&host) && matches!(proto.as_str(), "http" | "https") =>
        {
            Some(format!("{}://{}", proto, host.trim_end_matches('/')))
        }
        Some(_) => None,
        None => fallback_base,
    }
}
