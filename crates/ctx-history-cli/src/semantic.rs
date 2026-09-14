//! Daemon-owned semantic and refresh adapters exposed without final CLI types.

pub use ctx_daemon_cli::{
    coordinate_source_backed_refresh, coordinate_source_backed_refresh_with_retained_peer,
    SemanticNotReady, SemanticQueryAdapter,
};
