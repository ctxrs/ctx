use ctx_mobile_access_service::{
    route_contract::{
        DisableMobileAccessError, EnableMobileAccessRequest, EnableMobileAccessResult,
        MobileAccessRouteError, MobileAccessRouteErrorKind,
    },
};
use ctx_store::Store;
use ctx_transport_runtime::mobile_tunnel::MobileTunnelManager;
use url::Url;

pub fn mobile_public_url_is_allowed(url: &Url) -> bool {
    if url.scheme() == "https" {
        return true;
    }
    if url.scheme() != "http" {
        return false;
    }
    if std::env::var_os("CTX_MOBILE_TUNNEL_ALLOW_INSECURE_LOOPBACK").is_none() {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .map(|addr| addr.is_loopback())
            .unwrap_or(false)
}

pub(super) async fn enable_mobile_access_for_route(
    _store: &Store,
    _mobile_tunnel: &MobileTunnelManager,
    _daemon_url: &str,
    _auth_token_configured: bool,
    _request: EnableMobileAccessRequest,
) -> Result<EnableMobileAccessResult, MobileAccessRouteError> {
    Err(MobileAccessRouteError::new(
        MobileAccessRouteErrorKind::Forbidden,
        "managed mobile access is not included in the public ADE export",
    ))
}

pub(super) async fn disable_mobile_access_for_route(
    store: &Store,
    mobile_tunnel: &MobileTunnelManager,
    _request: EnableMobileAccessRequest,
) -> Result<(), DisableMobileAccessError> {
    super::disable_mobile_access_runtime(store, mobile_tunnel).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    static ENV_LOCK: StdMutex<()> = StdMutex::new(());

    #[test]
    fn mobile_public_url_requires_https_unless_explicit_loopback_test_flag_is_set() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let previous = std::env::var("CTX_MOBILE_TUNNEL_ALLOW_INSECURE_LOOPBACK").ok();
        std::env::remove_var("CTX_MOBILE_TUNNEL_ALLOW_INSECURE_LOOPBACK");

        assert!(mobile_public_url_is_allowed(
            &Url::parse("https://example.com/t/id").unwrap()
        ));
        assert!(!mobile_public_url_is_allowed(
            &Url::parse("http://127.0.0.1:8790/t/id").unwrap()
        ));

        std::env::set_var("CTX_MOBILE_TUNNEL_ALLOW_INSECURE_LOOPBACK", "1");
        assert!(mobile_public_url_is_allowed(
            &Url::parse("http://127.0.0.1:8790/t/id").unwrap()
        ));
        assert!(mobile_public_url_is_allowed(
            &Url::parse("http://localhost:8790/t/id").unwrap()
        ));
        assert!(!mobile_public_url_is_allowed(
            &Url::parse("http://example.com/t/id").unwrap()
        ));

        match previous {
            Some(value) => std::env::set_var("CTX_MOBILE_TUNNEL_ALLOW_INSECURE_LOOPBACK", value),
            None => std::env::remove_var("CTX_MOBILE_TUNNEL_ALLOW_INSECURE_LOOPBACK"),
        }
    }
}
