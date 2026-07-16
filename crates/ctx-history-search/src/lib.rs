mod filters;
mod model;
mod packet;
mod query;
mod ranking;
mod results;
mod search;
mod snippets;
mod source;

pub use ctx_protocol::{
    SearchClause, SearchExecutionConsumption, SearchExecutionLimits, SearchQuery,
    SearchRequestEnvelope, SearchSemanticCandidate, SearchSemanticCompleteness,
    SearchSemanticCoverage, SearchSemanticDiagnostics, SearchSemanticInput, SearchSemanticPolicy,
    SearchSemanticReadiness, SearchSemanticSkipReason,
};
pub use packet::{
    SearchExecutionDiagnostics, SearchPacket, SearchPacketResult, SearchResultScope,
    SemanticEventHit, SEARCH_PACKET_SCHEMA_VERSION,
};
pub use query::{
    PacketOptions, ProviderSessionFilter, Result, SearchError, SearchFilters, SearchResultMode,
    DEFAULT_RESULT_LIMIT, DEFAULT_SNIPPET_CHARS, MAX_RESULT_LIMIT,
    SEARCH_BUDGET_EXHAUSTED_ERROR_CODE,
};
pub use search::{
    search_packet, search_packet_envelope, search_packet_file_filter, search_packet_query,
    search_packet_terms,
};
pub use snippets::{display_snippet, event_preview_text};

#[cfg(test)]
mod tests;
