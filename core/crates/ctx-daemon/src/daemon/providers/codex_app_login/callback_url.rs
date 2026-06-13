use anyhow::Context;
use url::Url;

pub(super) fn expected_callback_from_auth_url(auth_url: &str) -> Option<String> {
    let parsed = Url::parse(auth_url).ok()?;
    let redirect = parsed
        .query_pairs()
        .find_map(|(key, value)| (key == "redirect_uri").then_some(value.into_owned()))?;
    let callback = Url::parse(&redirect).ok()?;
    let host = callback.host_str()?;
    if !is_loopback_host(host) {
        return None;
    }
    Some(callback.to_string())
}

fn is_loopback_host(host: &str) -> bool {
    let host = host.trim().to_ascii_lowercase();
    if host == "localhost" {
        return true;
    }
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return ip.is_loopback();
    }
    false
}

fn normalized_host(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

pub(super) fn validate_callback_url(
    callback_url: &str,
    expected_callback: Option<&str>,
) -> anyhow::Result<()> {
    let callback = Url::parse(callback_url)
        .with_context(|| format!("invalid callback_url: {callback_url}"))?;
    if callback.scheme() != "http" {
        anyhow::bail!("callback_url must use http scheme");
    }
    let host = callback
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("callback_url must include host"))?;
    if !is_loopback_host(host) {
        anyhow::bail!("callback_url host must be loopback");
    }
    if callback.port().is_none() {
        anyhow::bail!("callback_url must include explicit port");
    }
    if !callback.path().starts_with("/auth/callback") {
        anyhow::bail!("callback_url path must start with /auth/callback");
    }
    if callback.query().is_none() {
        anyhow::bail!("callback_url must include query parameters");
    }
    let Some(expected_raw) = expected_callback else {
        return Ok(());
    };
    let expected = Url::parse(expected_raw)
        .with_context(|| format!("invalid expected callback URL: {expected_raw}"))?;
    let expected_host = expected
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("expected callback URL must include host"))?;
    let same_host = normalized_host(host) == normalized_host(expected_host);
    let same_loopback = is_loopback_host(host) && is_loopback_host(expected_host);
    if !same_host && !same_loopback {
        anyhow::bail!("callback_url host mismatch");
    }
    if callback.scheme() != expected.scheme() {
        anyhow::bail!("callback_url scheme mismatch");
    }
    if callback.port() != expected.port() {
        anyhow::bail!("callback_url port mismatch");
    }
    if callback.path() != expected.path() {
        anyhow::bail!("callback_url path mismatch");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expected_callback_extracts_loopback_redirect_uri() {
        let auth_url = "https://auth.example.test/login?redirect_uri=http%3A%2F%2Flocalhost%3A4317%2Fauth%2Fcallback%3Fcode%3Dabc";

        assert_eq!(
            expected_callback_from_auth_url(auth_url).as_deref(),
            Some("http://localhost:4317/auth/callback?code=abc")
        );
    }

    #[test]
    fn expected_callback_rejects_non_loopback_redirect_uri() {
        let auth_url = "https://auth.example.test/login?redirect_uri=http%3A%2F%2Fexample.com%3A4317%2Fauth%2Fcallback%3Fcode%3Dabc";

        assert!(expected_callback_from_auth_url(auth_url).is_none());
    }

    #[test]
    fn validate_callback_accepts_loopback_alias_for_expected_host() {
        validate_callback_url(
            "http://127.0.0.1:4317/auth/callback?code=abc",
            Some("http://localhost:4317/auth/callback?code=original"),
        )
        .unwrap();
    }

    #[test]
    fn validate_callback_rejects_path_mismatch() {
        let err = validate_callback_url(
            "http://127.0.0.1:4317/auth/other?code=abc",
            Some("http://localhost:4317/auth/callback?code=original"),
        )
        .unwrap_err();

        assert!(err.to_string().contains("path must start with"));
    }

    #[test]
    fn validate_callback_rejects_non_http_scheme() {
        let err = validate_callback_url(
            "https://127.0.0.1:4317/auth/callback?code=abc",
            Some("http://localhost:4317/auth/callback?code=original"),
        )
        .unwrap_err();

        assert!(err.to_string().contains("must use http scheme"));
    }
}
