//! Provider-owned Claude Code discovery, parsing, and source-backed hydration.

mod checkpoint;
mod privacy;
mod reader;
mod record;
mod rows;
mod source;
pub(crate) mod source_backed;

pub(crate) use checkpoint::ParseCheckpoint;
pub(crate) use reader::ClaudeNativeScanner;
pub(crate) use rows::{ClaudeEventKind, ClaudeRetainedRow, ClaudeSessionMetadata};
pub(crate) use source::{
    discover_projects, revalidate_discovered_source, ClaudeNativePathError,
    DiscoveredClaudeSession, SessionLayout,
};
