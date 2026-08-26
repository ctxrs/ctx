mod active_session_env;
mod compact_presentation;
mod copied_lineage;
mod locate;
mod render;
mod search;
mod shared;
mod show;

pub(crate) use active_session_env::detected_active_session;
pub(crate) use compact_presentation::open_generation_read;
pub use copied_lineage::copied_lineage_summary;
pub use locate::run_locate;
#[cfg(test)]
use search::mcp_search;
pub use search::{
    mcp_search_with_compact, normalize_mcp_search_request, run_search,
    validate_explicit_semantic_scope, McpSearchError, McpSearchExecutionFailure,
    SourceSearchRequest,
};
pub use shared::generation_query_authority_error_json;
pub use show::{
    mcp_show_event_application, mcp_show_session_application, run_show, ShowApplicationError,
};

#[cfg(test)]
use std::path::{Path, PathBuf};

#[cfg(test)]
use ctx_history_core::CaptureProvider;
#[cfg(test)]
use ctx_history_index::{CoreEventRecord, EventRecord};

#[cfg(test)]
use crate::{config, semantic::SemanticNotReady, RefreshMode};

#[cfg(test)]
type RefreshArg = RefreshMode;
#[cfg(test)]
type SearchBackendArg = ctx_history_read_application::SearchBackend;

#[cfg(test)]
use render::{enforce_json_output_limit, pretty_json_stdout_bytes, stdout_body_bytes};
#[cfg(test)]
use search::{
    collect_search_hits_with_backend, collect_search_hits_with_backend_using, index_search_filters,
    refresh_for_search, refresh_for_search_with, search_context_observation,
    search_existing_generation, source_backed_refresh_mode,
};
#[cfg(test)]
use shared::{externalize_query_error, index_root, open_index};
#[cfg(test)]
use show::resolve_show_session;

#[cfg(test)]
mod tests;
