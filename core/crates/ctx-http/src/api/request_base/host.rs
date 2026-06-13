use url::Url;

pub(in crate::api) fn is_loopback_host(host: &str) -> bool {
    let normalized = host.trim().trim_matches('[').trim_matches(']');
    normalized.eq_ignore_ascii_case("localhost")
        || normalized.eq_ignore_ascii_case("tauri.localhost")
        || normalized == "127.0.0.1"
        || normalized == "::1"
}

pub(super) fn is_safe_request_base_host(host: &str) -> bool {
    let trimmed = host.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return false;
    }
    let parsed = match Url::parse(&format!("http://{trimmed}")) {
        Ok(url) => url,
        Err(_) => return false,
    };
    parsed.host_str().map(is_loopback_host).unwrap_or(false)
}
