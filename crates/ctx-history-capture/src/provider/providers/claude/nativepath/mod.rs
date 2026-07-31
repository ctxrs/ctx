//! Provider-owned Claude Code discovery, parsing, and source-backed decoding.

mod privacy;
mod record;
mod rows;
mod source;
pub(crate) mod source_backed;
pub(crate) use source_backed::registration::register as register_source_backed_route;
