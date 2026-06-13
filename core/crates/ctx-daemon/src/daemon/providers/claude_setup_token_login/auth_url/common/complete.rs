use super::*;

pub(in crate::daemon::providers::claude_setup_token_login) fn auth_url_looks_complete(
    auth_url: &str,
) -> bool {
    let parsed = match Url::parse(auth_url) {
        Ok(value) => value,
        Err(_) => return false,
    };
    let maybe_redirect = parsed
        .query_pairs()
        .find_map(|(key, value)| (key == "redirect_uri").then_some(value.into_owned()));
    let Some(redirect_uri) = maybe_redirect else {
        return true;
    };
    let redirect = match Url::parse(&redirect_uri) {
        Ok(value) => value,
        Err(_) => return false,
    };
    let Some(host) = redirect.host_str() else {
        return false;
    };
    if !is_loopback_host(host) {
        return false;
    }
    redirect.port().is_some()
}
