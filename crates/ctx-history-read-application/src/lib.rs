//! Transport-neutral reads and structured read models over one pinned Core generation.
//!
//! This crate owns query-domain parsing, selector resolution, search planning,
//! lexical/semantic ranking, bounded result contracts, and structured DTO
//! composition. Process lifecycle, refresh execution, concrete semantic
//! services, writers, and terminal rendering remain in the outer composition
//! layer.

mod application;
mod compact_presentation;
mod event_read_model;
mod filters;
mod generation;
mod health;
mod json;
mod lineage;
mod list;
mod locate;
mod presentation;
mod search;
mod search_read_model;
mod selector;
mod semantic;
mod show;
mod show_read_model;

#[cfg(test)]
mod application_tests;

pub use application::{
    execute_search, execute_search_observed, plan_search, ObservedSearchApplicationError,
    PinnedHistoryQuery, PlannedSearch, SearchApplicationError, SearchApplicationReadModelInput,
    SearchApplicationRequest, SearchApplicationResult, SearchQueryResult,
};
pub use compact_presentation::{
    normalize_uuid_prefix, reference_needs_retained_peer, CompactPresentationProjection,
    UuidPrefixError,
};
pub use event_read_model::{
    event_query_completion_read_model, event_query_event_read_model, event_query_page_read_model,
    event_query_receipt, event_query_wire_request, mcp_event_query_core_record_bytes,
    render_event_read_model, EventContentProjection, EventQueryCompletionReadModel,
    EventQueryCompletionUsage, EventQueryFreshness, EventQueryFrontier, EventQueryPageReadModel,
    EventQueryPageUsage, EventQueryReceipt, EventQueryWireRequest, EVENT_QUERY_PAGE_BYTES,
    EVENT_QUERY_PAGE_ITEMS, EVENT_QUERY_SCHEMA_VERSION, MAX_EVENT_QUERY_WIRE_RECORD_BYTES,
};
pub use filters::{
    normalize_source_identity_filter, normalize_source_identity_filters, parse_since_filter,
    SourceIdentityFilterArgs, SourceIdentityFilterError, SourceIdentityFilters,
};
pub use generation::{
    GenerationRead, GenerationReadAuthorityError, GenerationReadError, GenerationReadPort,
    GenerationReadReceipt, GenerationReadRequest, GenerationReadTarget, RetainedPeerRead,
};
pub use health::{
    history_health_report, HistoryDataCoverage, HistoryHealthReport, HistoryRootCoverage,
};
pub use json::{event_copy_json, timestamp_json};
pub use lineage::{
    copied_lineage_read_model, copied_lineage_relationship_summary, copied_lineage_summary,
};
pub use list::{
    decode_event_range_cursor, encode_event_range_cursor, event_range_selection,
    execute_list_events_page, execute_list_events_stream, parse_event_query_uuid,
    validated_event_limit, ListEventsApplicationError, ListEventsApplicationResult,
    ListEventsError, ListEventsPageRequest, ListEventsRequest, ListEventsResult,
    ListEventsStreamCallback, ListEventsStreamCompletion, ListEventsStreamControl,
    ListEventsStreamPage, ListEventsStreamResult, DEFAULT_EVENT_QUERY_LIMIT,
    MAX_EVENT_QUERY_CURSOR_CHARS, MAX_EVENT_QUERY_LIMIT,
};
pub use locate::{
    execute_locate, locate_read_model, LocateApplicationError, LocateApplicationRequest,
    LocateApplicationResult, LocateRequest, LocateResult,
};
pub use presentation::{
    search_snippet_fragment, SearchPresentation, SearchPresentationHydrationBudget,
    SearchPresentationRetentionBudgetExceeded, MAX_SEARCH_RESULTS,
    SEARCH_PRESENTATION_HYDRATION_BUDGET, SEARCH_PRESENTATION_MAX_RETAINED_SNIPPET_BYTES,
    SEARCH_SNIPPET_MAX_BYTES, SEARCH_SNIPPET_MAX_CHARS,
};
pub use search::{
    collect_search_hits, collect_search_hits_using, normalize_search_request,
    resolve_search_backend, search_filters, search_filters_with_refs, unsupported_semantic_scope,
    validate_search_request, ActiveSessionExclusion, NormalizedSearchQuery, SearchBackend,
    SearchCollection, SearchDiversificationDecision, SearchDiversificationStatus,
    SearchEventMetadata, SearchExecutionError, SearchExecutionResult, SearchFailurePhase,
    SearchHit, SearchLexicalDiagnostics, SearchLexicalExhaustionDiagnostics, SearchPolicy,
    SearchRequest, SearchResultWindow, SearchStopReason, SearchWorkReceipt,
    SemanticFallbackDiagnostics,
};
pub use search_read_model::{
    phase_attribution, render_search_json, search_json, search_result_json, search_snippet,
    semantic_diagnostics_read_model, SearchJsonInput, SearchRenderMetrics, SearchResultCommands,
};
pub use selector::{
    resolve_core_event, resolve_core_event_with_refs, resolve_session, resolve_session_with_refs,
    resolve_show_session, resolve_show_session_with_refs, validate_ctx_id,
    validate_session_selector, CompactRefMap, CompactRefNamespace, CompactRefResolveError,
    CompactRefResolver, MissingLookupError, MissingLookupKind, SelectorError,
    MAX_COMPACT_REF_HEX_LEN, MIN_COMPACT_REF_HEX_LEN,
};
pub use semantic::{
    HistorySemanticBatch, HistorySemanticError, HistorySemanticPort, HistorySemanticQuery,
    SemanticAvailability, SemanticReason,
};
pub use show::{
    execute_show_event, execute_show_session_page, execute_show_session_stream,
    ContentQueryLimitError, EncodedCoreQueryLimitError, EventWindowBudget, EventWindowLimitError,
    SessionEventMode, ShowEventApplicationRequest, ShowEventApplicationResult, ShowEventRequest,
    ShowEventResult, ShowReadApplicationError, ShowReadModelProjection,
    ShowSessionApplicationRequest, ShowSessionApplicationResult, ShowSessionEvent, ShowSessionPage,
    ShowSessionPageRequest, ShowSessionReadModels, ShowSessionStreamCallback,
    ShowSessionStreamControl, ShowSessionStreamPage, ShowSessionStreamRequest,
    ShowSessionStreamResult, ShowSessionStreamStart, SHOW_SESSION_PAGE_ITEMS,
};
pub use show_read_model::{
    decode_session_event_cursor, encode_session_event_cursor, event_window_read_model,
    event_window_value, event_window_with_lineage_read_model,
    paginated_session_transcript_read_model, render_event_read_model_values,
    render_show_event_read_model, retain_structured_session_page, session_transcript_read_model,
    ReadModelLimitError, SessionPageReadModel, StructuredOutputFormat, StructuredTranscriptMode,
};
