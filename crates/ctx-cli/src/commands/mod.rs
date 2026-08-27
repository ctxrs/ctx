pub(crate) mod doctor;
mod history_health;
pub(crate) mod import;
pub(crate) mod index;
pub(crate) mod list;
pub(crate) mod locate;
pub(crate) mod search;
pub(crate) mod semantic;
pub(crate) mod setup;
pub(crate) mod show;
/// Final-host compatibility names for MCP/cross-product adapters. Command
/// execution itself lives in `ctx-history-cli`.
pub(crate) mod source_index {
    pub(crate) use ctx_history_cli::{
        generation_query_authority_error_json, mcp_search_with_compact, mcp_show_event_application,
        mcp_show_session_application, normalize_mcp_search_request,
        validate_explicit_semantic_scope, McpSearchError, ShowApplicationError,
        SourceSearchRequest,
    };
}
pub(crate) mod sources;
pub(crate) mod stats;
pub(crate) mod status;
