mod server;
mod transport;

pub const DAEMON_SEMANTIC_QUERY_SCHEMA_VERSION: u64 = 2;

#[cfg(all(test, unix))]
pub(crate) use server::bind_daemon_query_listener;
#[cfg(test)]
pub(crate) use server::{
    ctx_authenticated_request_handler, start_daemon_query_service_with_request_timeout,
    start_daemon_source_refresh_service_with_request_timeout,
};
pub(crate) use server::{
    ctx_authenticated_request_handler_with_lifecycle, start_daemon_query_service,
    start_daemon_source_refresh_service, DaemonLifecycleState, DaemonQueryActivity,
    DaemonQueryService,
};
#[cfg(all(feature = "test-support", not(test)))]
pub(crate) use transport::write_daemon_service_endpoint;
#[cfg(test)]
pub(crate) use transport::{
    daemon_query_endpoint_path, read_daemon_query_endpoint, read_daemon_query_endpoint_identity,
    remove_daemon_query_endpoint_if_matches, write_daemon_query_endpoint,
    write_daemon_service_endpoint,
};
pub use transport::{
    daemon_query_request, daemon_service_endpoint_path, daemon_source_refresh_request,
    read_daemon_service_endpoint_identity, DaemonIpcService, DaemonQueryEndpoint,
    DaemonQueryServiceUnavailable, DaemonSourceRefreshServiceUnavailable,
};

pub(crate) fn daemon_query_service_transport_supported() -> bool {
    cfg!(any(unix, windows))
}
