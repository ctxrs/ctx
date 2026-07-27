mod eligibility;
mod encoding;
mod identity;
#[cfg(test)]
mod parity_tests;
mod prepared;
mod query;
mod semantic_document;
mod snapshot;
mod storage;

pub use identity::{EventEmbeddingDocument, EventSearchHit};
pub use snapshot::SemanticProjectionSnapshot;

pub(crate) use eligibility::{
    semantic_searchable_document_count_for_event,
    semantic_searchable_document_count_from_stored_event,
};
pub(crate) use query::{fts_match_clauses, fts_match_query};
pub(crate) use storage::{
    adjust_semantic_searchable_item_stats, delete_record_search_projection,
    detect_event_search_projection_capabilities, event_scriptgram_table_ready,
    event_search_lookup_table_ready, insert_event_search_projection_for_event,
    populate_event_search_projection_from_query, rebuild_event_search_lookup_projection,
    rebuild_search_projection, record_scriptgram_table_ready,
    refresh_semantic_searchable_item_stats, upsert_event_search_projection_for_event,
    upsert_record_search_projection, EventSearchProjectionCapabilities,
};
