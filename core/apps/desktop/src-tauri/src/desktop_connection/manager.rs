use super::*;

mod core;
mod local;
mod request;
mod ssh;

pub(super) use super::http_client::get_connection_http_client;
pub(super) use super::lifecycle::{
    build_ssh_connection, cleanup_active_connection, cleanup_active_connection_result_for_restart,
    should_preserve_local_handoff,
};
pub(super) use super::types::{
    log_local_connection_established, log_ssh_connection_established,
    same_local_daemon_for_ownership, ActiveConnection, ConnectionIntent, ConnectionState,
    LocalConnection, LocalConnectionOwnership, LocalConnectionSource, SshConnection,
    SshConnectionTarget, SshRemoteUpdateStatus, SshRuntimeMetadata, DEFAULT_CONNECTION_SCOPE,
};

pub(crate) struct ConnectionManager(pub(super) std::sync::Mutex<ConnectionState>);

impl Default for ConnectionManager {
    fn default() -> Self {
        Self(std::sync::Mutex::new(ConnectionState::default()))
    }
}
