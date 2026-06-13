mod headers;
mod host;
mod resolve;
mod urls;

pub(super) use host::is_loopback_host;
pub(super) use resolve::resolve_request_base_url;
pub(super) use urls::{public_route_url, public_websocket_url};

#[cfg(test)]
mod tests;
