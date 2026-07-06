mod analysis;
mod error;
mod fast_events;
mod filters;
mod hits;
mod limits;
mod ranking;
mod results;
mod search;
mod sources;
mod text;
mod types;

#[cfg(test)]
mod tests;

pub(crate) use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

pub(crate) use chrono::Utc;
pub(crate) use ctx_history_core::{
    utc_now, Artifact, ContextCitation, ContextCitationType, ContextLinks, ContextPagination,
    ContextTruncation, Event, EventType, FileTouched, HistoryRecord, RedactionState, Run, Session,
    Summary, VcsChange, Visibility,
};
pub(crate) use ctx_history_store::{EventSearchHit, FileTouchScope, Store};
pub(crate) use uuid::Uuid;

pub(crate) use analysis::*;
pub use error::*;
pub(crate) use fast_events::*;
pub(crate) use filters::*;
pub(crate) use hits::*;
pub use limits::{
    DEFAULT_RESULT_LIMIT, DEFAULT_SNIPPET_CHARS, MAX_RESULT_LIMIT, SEARCH_PACKET_SCHEMA_VERSION,
};
pub(crate) use limits::{
    FILTERED_SEARCH_MAX_PAGES, FILTERED_SEARCH_PAGE_SIZE, LARGE_EVENT_CORPUS_THRESHOLD,
};
pub(crate) use ranking::*;
pub(crate) use results::*;
pub use search::{search_packet, search_packet_terms};
pub(crate) use sources::*;
pub use text::{display_snippet, event_preview_text};
pub(crate) use text::{
    event_text, event_weight, joined, local_snippet, matched_snippet, matches_terms, non_blank,
    query_terms, search_snippet,
};
pub(crate) use types::{Candidate, CandidateSearch, HitMetadata, RecordContext, SearchSection};
pub use types::{
    PacketOptions, ProviderSessionFilter, SearchFilters, SearchPacket, SearchPacketResult,
    SearchResultMode, SearchResultScope,
};
