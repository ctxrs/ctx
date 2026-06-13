use super::*;
pub(super) use ctx_desktop_ipc::{
    DesktopConnectionInfo, DesktopConnectionIntent, DesktopConnectionKind,
    DesktopRemoteDaemonUpdateState,
};

mod commands;
mod http_client;
mod lifecycle;
mod manager;
mod types;

pub(crate) use commands::*;
#[cfg(test)]
use http_client::{connection_http_client_build_count, reset_connection_http_client_build_count};
#[cfg(test)]
use lifecycle::cleanup_active_connection;
pub(crate) use manager::ConnectionManager;
pub(crate) use types::{
    LocalConnectionSource, SshConnectionTarget, SshRuntimeMetadata, DEFAULT_CONNECTION_SCOPE,
};

#[cfg(test)]
mod connection_manager_tests;
