use axum::http::{header, HeaderMap, HeaderValue};

use super::*;

#[test]
fn resolve_request_base_url_accepts_loopback_host_headers() {
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, HeaderValue::from_static("127.0.0.1:4455"));
    assert_eq!(
        resolve_request_base_url(&headers, "http://127.0.0.1:4321", None),
        Some("http://127.0.0.1:4455".to_string())
    );
}

#[test]
fn resolve_request_base_url_rejects_non_loopback_host_headers() {
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, HeaderValue::from_static("evil.example"));
    assert_eq!(
        resolve_request_base_url(&headers, "http://127.0.0.1:4321", None),
        None
    );
}

#[test]
fn resolve_request_base_url_rejects_non_http_forwarded_proto() {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::FORWARDED,
        HeaderValue::from_static("proto=javascript;host=127.0.0.1:4455"),
    );
    assert_eq!(
        resolve_request_base_url(&headers, "http://127.0.0.1:4321", None),
        None
    );
}

#[test]
fn resolve_request_base_url_accepts_forwarded_loopback_origin() {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::FORWARDED,
        HeaderValue::from_static("proto=https;host=tauri.localhost:3000"),
    );
    assert_eq!(
        resolve_request_base_url(&headers, "http://127.0.0.1:4321", None),
        Some("https://tauri.localhost:3000".to_string())
    );
}

#[test]
fn resolve_request_base_url_uses_loopback_fallback_without_request_host() {
    let headers = HeaderMap::new();
    assert_eq!(
        resolve_request_base_url(&headers, "http://127.0.0.1:4321", None),
        Some("http://127.0.0.1:4321".to_string())
    );
}

#[test]
fn resolve_request_base_url_prefers_configured_public_base_url() {
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, HeaderValue::from_static("127.0.0.1:4455"));
    headers.insert(
        header::FORWARDED,
        HeaderValue::from_static("proto=https;host=proxy.example"),
    );
    assert_eq!(
        resolve_request_base_url(
            &headers,
            "http://127.0.0.1:4321",
            Some("https://proxy.example/ctx"),
        ),
        Some("https://proxy.example/ctx".to_string())
    );
}

#[test]
fn public_route_url_preserves_path_prefix_and_query() {
    assert_eq!(
        public_route_url(
            "https://proxy.example/ctx",
            "/sessions/web/sess-1/view?token=stream-token",
        ),
        Some("https://proxy.example/ctx/sessions/web/sess-1/view?token=stream-token".to_string(),)
    );
}

#[test]
fn public_websocket_url_preserves_path_prefix_and_query() {
    assert_eq!(
        public_websocket_url(
            "https://proxy.example/ctx",
            "/sessions/web/sess-1/signal?token=signal-token",
        ),
        Some("wss://proxy.example/ctx/sessions/web/sess-1/signal?token=signal-token".to_string(),)
    );
}
