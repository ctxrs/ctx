//! Experimental in-repo Rust SDK for ctx agent history.
//!
//! This SDK is intentionally not published. The local backend shells out to the
//! `ctx` CLI and adapts its private JSON into the public `agent-history-v1` envelope.

mod backend;
mod client;
mod error;
mod fixtures;
mod local_cli;
mod normalize;
mod options;

pub use backend::{AgentHistoryBackend, HostedBackendConfig, LocalBackendConfig};
pub use client::AgentHistoryClient;
pub use error::AgentHistoryError;
pub use fixtures::fixture_path;
pub use options::{
    ImportOptions, InitOptions, SearchOptions, SearchRefresh, ShowEventOptions, ShowSessionOptions,
};

pub use ctx_protocol::{
    AgentHistoryEnvelope, AgentHistoryErrorBody, AgentHistoryErrorCode, AgentHistoryEvent,
    AgentHistoryOperation, AgentHistoryStatus, BackendInfo, BackendKind, EventResult, Freshness,
    ImportResult, LocationResult, ProviderSource, SearchHit, SearchResult, SessionResult,
    SourceLocation, Totals, CONTRACT_VERSION, SCHEMA_VERSION,
};

#[cfg(test)]
mod tests;
