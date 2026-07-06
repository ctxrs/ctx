mod filters;
mod model;
mod packet;
mod query;
mod ranking;
mod results;
mod search;
mod snippets;
mod source;

pub use packet::{
    SearchPacket, SearchPacketResult, SearchResultScope, SEARCH_PACKET_SCHEMA_VERSION,
};
pub use query::{
    PacketOptions, ProviderSessionFilter, Result, SearchError, SearchFilters, SearchResultMode,
    DEFAULT_RESULT_LIMIT, DEFAULT_SNIPPET_CHARS, MAX_RESULT_LIMIT,
};
pub use search::{search_packet, search_packet_terms};
pub use snippets::{display_snippet, event_preview_text};

#[cfg(test)]
mod tests;
