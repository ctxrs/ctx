mod filters;
mod model;
mod packet;
mod query;
mod ranking;
mod results;
mod search;
mod snippets;
mod source;
mod sql_compatibility;

pub use packet::{
    SearchPacket, SearchPacketResult, SearchResultScope, SemanticEventHit,
    SEARCH_PACKET_SCHEMA_VERSION,
};
pub use query::{
    PacketOptions, ProviderSessionFilter, Result, SearchError, SearchFilters, SearchResultMode,
    DEFAULT_RESULT_LIMIT, DEFAULT_SNIPPET_CHARS, MAX_RESULT_LIMIT,
};
pub use search::{search_packet, search_packet_terms, semantic_event_search_packet};
pub use snippets::{display_snippet, event_preview_text};
pub use sql_compatibility::{sql_compatibility_path, SqlCompatibility, SqlCompatibilityResult};

#[cfg(test)]
mod tests;
