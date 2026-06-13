use url::Url;

pub(in crate::api) fn public_route_url(base_url: &str, route_path: &str) -> Option<String> {
    let mut base = Url::parse(base_url).ok()?;
    let normalized_base_path = match base.path().trim_end_matches('/') {
        "" => "/".to_string(),
        path => format!("{path}/"),
    };
    base.set_path(&normalized_base_path);
    base.join(route_path.trim_start_matches('/'))
        .ok()
        .map(|url| url.to_string())
}

pub(in crate::api) fn public_websocket_url(base_url: &str, route_path: &str) -> Option<String> {
    let mut url =
        public_route_url(base_url, route_path).and_then(|joined| Url::parse(&joined).ok())?;
    match url.scheme() {
        "http" => {
            url.set_scheme("ws").ok()?;
        }
        "https" => {
            url.set_scheme("wss").ok()?;
        }
        _ => return None,
    }
    Some(url.to_string())
}
