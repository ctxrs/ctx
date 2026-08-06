mod compact_presentation;
mod compact_ref;
mod copied_lineage;
mod locate;
mod render;
mod search;
mod shared;
mod show;

pub(crate) use locate::run_locate;
#[cfg(test)]
use search::mcp_search;
pub(crate) use search::{
    mcp_search_with_compact, run_search, validate_explicit_semantic_scope, SourceSearchRequest,
};
pub(crate) use shared::{
    active_generation_race_error_json, generation_query_authority_error_json,
    is_active_generation_race,
};
#[cfg(test)]
pub(crate) use show::mcp_show_event;
pub(crate) use show::{mcp_show_event_with_compact, mcp_show_session_with_compact, run_show};

pub(crate) fn event_origin_json(origin: &ctx_history_core::EventOrigin) -> serde_json::Value {
    match origin {
        ctx_history_core::EventOrigin::Unknown => serde_json::json!({"kind": "unknown"}),
        ctx_history_core::EventOrigin::UniqueToSession => {
            serde_json::json!({"kind": "unique_to_session"})
        }
        ctx_history_core::EventOrigin::CopiedFromAncestor {
            ancestor_session_id,
            ancestor_event_id,
            proof,
        } => serde_json::json!({
            "kind": "copied_from_ancestor",
            "ancestor_session_id": ancestor_session_id.as_uuid(),
            "ancestor_event_id": ancestor_event_id.as_uuid(),
            "proof": proof,
        }),
    }
}

#[cfg(test)]
use std::path::{Path, PathBuf};

#[cfg(test)]
use ctx_history_core::CaptureProvider;
#[cfg(test)]
use ctx_history_index::{CoreEventRecord, EventRecord, EventSearchCandidate};

#[cfg(test)]
use crate::{
    config,
    semantic::{
        PinnedSourceBackedGeneration, SemanticNotReady, SourceBackedRefreshMode,
        SourceBackedRefreshObservation,
    },
    RefreshArg, SearchBackendArg,
};

#[cfg(test)]
use render::{enforce_json_output_limit, pretty_json_stdout_bytes, stdout_body_bytes};
#[cfg(test)]
use search::{
    collect_search_hits_with_backend, collect_search_hits_with_backend_using, index_search_filters,
    refresh_for_search, refresh_for_search_with, search_context_observation,
    search_existing_generation, shape_search_result_window, source_backed_refresh_mode,
};
#[cfg(test)]
use shared::{index_root, open_index};
#[cfg(test)]
use show::resolve_show_session;

include!("source_index/tests.rs");
