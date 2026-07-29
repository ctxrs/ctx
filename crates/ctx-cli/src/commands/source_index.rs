mod locate;
mod render;
mod search;
mod shared;
mod show;

pub(crate) use locate::run_locate;
pub(crate) use search::{mcp_search, run_search, SourceSearchRequest};
pub(crate) use show::{mcp_show_event, mcp_show_session, run_show};

#[cfg(test)]
use std::path::{Path, PathBuf};

#[cfg(test)]
use ctx_history_core::CaptureProvider;
#[cfg(test)]
use ctx_history_index::{EventRecord, EventSearchCandidate};

#[cfg(test)]
use crate::{
    complete_content::ContentPolicy,
    config,
    semantic::{
        PinnedSourceBackedGeneration, SourceBackedRefreshMode, SourceBackedRefreshObservation,
        SourceBackedSemanticNotReady,
    },
    RefreshArg, SearchBackendArg,
};

#[cfg(test)]
use render::{
    enforce_json_output_limit, locate_event_text_output_bytes, locate_session_text_output_bytes,
    pretty_json_stdout_bytes, stdout_body_bytes,
};
#[cfg(test)]
use search::{
    collect_search_hits_with_backend, collect_search_hits_with_backend_using, index_search_filters,
    refresh_for_search, refresh_for_search_with, search_existing_generation,
    search_existing_generation_with_hydrator, search_with_hydration_retry_with,
    shape_search_result_window, source_backed_refresh_mode, RefreshOutcome,
};
#[cfg(test)]
use shared::{index_root, open_index};
#[cfg(test)]
use show::{resolve_complete_contents, resolve_show_session};

include!("source_index/tests.rs");
