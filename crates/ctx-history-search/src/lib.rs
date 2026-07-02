use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use chrono::Utc;
use ctx_history_core::{
    utc_now, Artifact, ContextCitation, ContextCitationType, ContextLinks, ContextPagination,
    ContextTruncation, Event, EventType, FileTouched, HistoryRecord, RedactionState, Run, Session,
    Summary, VcsChange, Visibility,
};
use ctx_history_store::{EventSearchHit, FileTouchScope, Store};
use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

pub const SEARCH_PACKET_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_RESULT_LIMIT: usize = 10;
pub const MAX_RESULT_LIMIT: usize = 200;
pub const DEFAULT_SNIPPET_CHARS: usize = 320;
const LARGE_EVENT_CORPUS_THRESHOLD: i64 = 1_024;
const FILTERED_SEARCH_PAGE_SIZE: usize = 500;
const FILTERED_SEARCH_MAX_PAGES: usize = 20;

#[derive(Debug, Error)]
pub enum SearchError {
    #[error("store error: {0}")]
    Store(#[from] ctx_history_store::StoreError),
}

pub type Result<T> = std::result::Result<T, SearchError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PacketOptions {
    pub limit: usize,
    pub snippet_chars: usize,
    pub filters: SearchFilters,
    pub result_mode: SearchResultMode,
}

impl Default for PacketOptions {
    fn default() -> Self {
        Self {
            limit: DEFAULT_RESULT_LIMIT,
            snippet_chars: DEFAULT_SNIPPET_CHARS,
            filters: SearchFilters::default(),
            result_mode: SearchResultMode::Sessions,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchResultMode {
    Sessions,
    Events,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SearchFilters {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<ctx_history_core::CaptureProvider>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_format: Option<String>,
    #[serde(default, rename = "workspace", skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<chrono::DateTime<Utc>>,
    #[serde(skip_serializing)]
    pub primary_only: bool,
    #[serde(default)]
    pub include_subagents: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_type: Option<EventType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude_provider_session: Option<ProviderSessionFilter>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderSessionFilter {
    pub provider: ctx_history_core::CaptureProvider,
    pub provider_session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SearchPacket {
    pub schema_version: u32,
    pub query: String,
    pub filters: SearchFilters,
    pub generated_at: chrono::DateTime<Utc>,
    pub results: Vec<SearchPacketResult>,
    pub pagination: ContextPagination,
    pub truncation: ContextTruncation,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SearchPacketResult {
    pub record_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_seq: Option<u64>,
    pub title: String,
    pub snippet: String,
    pub rank: f32,
    #[serde(default, skip_serializing_if = "is_default_result_scope")]
    pub result_scope: SearchResultScope,
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub more_matches_in_session: usize,
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub session_importance: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<ctx_history_core::CaptureProvider>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_source_plugin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<chrono::DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_source_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_source_exists: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default)]
    pub why_matched: Vec<String>,
    #[serde(default)]
    pub citations: Vec<ContextCitation>,
    #[serde(default)]
    pub links: ContextLinks,
    #[serde(default)]
    pub visibility: Visibility,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchResultScope {
    Session,
    #[default]
    Event,
}

fn is_default_result_scope(value: &SearchResultScope) -> bool {
    *value == SearchResultScope::Event
}

fn is_zero_usize(value: &usize) -> bool {
    *value == 0
}

fn is_zero_f32(value: &f32) -> bool {
    *value == 0.0
}

#[derive(Debug, Clone)]
struct Candidate {
    record: HistoryRecord,
    context: RecordContext,
    score: f32,
    why_matched: Vec<String>,
    citations: Vec<ContextCitation>,
    primary_hit: Option<HitMetadata>,
}

#[derive(Debug, Clone, Default)]
struct RecordContext {
    sessions: Vec<Session>,
    runs: Vec<Run>,
    events: Vec<Event>,
    artifacts: Vec<Artifact>,
    files_touched: Vec<FileTouched>,
    vcs_changes: Vec<VcsChange>,
    summaries: Vec<Summary>,
    sources: BTreeMap<Uuid, ctx_history_core::CaptureSource>,
}

#[derive(Debug, Clone)]
struct SearchSection {
    reason: &'static str,
    weight: f32,
    text: String,
    citation: ContextCitation,
    hit: HitMetadata,
}

#[derive(Debug, Clone)]
struct HitMetadata {
    time: chrono::DateTime<Utc>,
    provider: Option<ctx_history_core::CaptureProvider>,
    provider_session_id: Option<String>,
    history_source: Option<String>,
    history_source_plugin: Option<String>,
    provider_key: Option<String>,
    source_id: Option<String>,
    source_format: Option<String>,
    session_id: Option<Uuid>,
    parent_session_id: Option<Uuid>,
    root_session_id: Option<Uuid>,
    event_id: Option<Uuid>,
    event_seq: Option<u64>,
    cwd: Option<String>,
    raw_source_path: Option<String>,
    raw_source_exists: Option<bool>,
    cursor: Option<String>,
}

struct CandidateSearch {
    candidates: Vec<Candidate>,
    scan_budget_exhausted: bool,
}

pub fn search_packet(store: &Store, query: &str, options: &PacketOptions) -> Result<SearchPacket> {
    let options = normalized_options(options);
    if let Some(provider) = options.filters.provider {
        if !store.has_provider_data(provider)? {
            return Ok(empty_search_packet(query, &options));
        }
    }
    let file_scope = file_filter_scope(store, &options.filters)?;
    if file_scope.as_ref().is_some_and(FileTouchScope::is_empty) {
        return Ok(empty_search_packet(query, &options));
    }
    if let Some(packet) = fast_event_search_packet(store, query, &options, file_scope.as_ref())? {
        return Ok(packet);
    }
    let CandidateSearch {
        candidates,
        scan_budget_exhausted,
    } = ranked_candidates(store, Some(query), &options, file_scope.as_ref())?;
    let mut truncation = ContextTruncation::default();
    let mut results = Vec::new();

    push_candidate_results(&mut results, &candidates, query, &options);

    let has_more = candidates.len() > results.len() || scan_budget_exhausted;
    if scan_budget_exhausted {
        truncation.truncated = true;
        truncation.omitted_results = 1;
        truncation.reason = Some("scan_budget".to_owned());
    } else if candidates.len() > results.len() {
        truncation.truncated = true;
        truncation.omitted_results = (candidates.len() - results.len()) as u32;
        truncation.reason = Some("limit".to_owned());
    }

    let cursor_offset = results.len();
    Ok(SearchPacket {
        schema_version: SEARCH_PACKET_SCHEMA_VERSION,
        query: query.to_owned(),
        filters: options.filters,
        generated_at: utc_now(),
        results,
        pagination: pagination(Some(cursor_offset), has_more),
        truncation,
    })
}

pub fn search_packet_terms(
    store: &Store,
    query: &str,
    terms: &[String],
    options: &PacketOptions,
) -> Result<SearchPacket> {
    let options = normalized_options(options);
    let search_terms = composed_search_terms(query, terms);
    if search_terms.len() <= 1 {
        return search_packet(
            store,
            search_terms.first().map_or(query, String::as_str),
            &options,
        );
    }

    let mut child_options = options.clone();
    child_options.limit = options
        .limit
        .saturating_mul(2)
        .max(options.limit)
        .min(MAX_RESULT_LIMIT);

    let mut merged_results = Vec::<SearchPacketResult>::new();
    let mut result_index = BTreeMap::<Uuid, usize>::new();
    let mut truncated = false;
    let mut omitted_results = 0_u32;
    for term in &search_terms {
        let packet = search_packet(store, term, &child_options)?;
        truncated |= packet.truncation.truncated;
        omitted_results = omitted_results.saturating_add(packet.truncation.omitted_results);
        for mut result in packet.results {
            push_unique_why(&mut result.why_matched, format!("term:{term}"));
            let result_key = search_result_merge_key(&result, options.result_mode);
            if let Some(index) = result_index.get(&result_key).copied() {
                merge_search_result(&mut merged_results[index], result);
            } else {
                result_index.insert(result_key, merged_results.len());
                merged_results.push(result);
            }
        }
    }

    merged_results.sort_by(compare_search_results);
    let has_more = merged_results.len() > options.limit || truncated;
    if merged_results.len() > options.limit {
        omitted_results =
            omitted_results.saturating_add((merged_results.len() - options.limit) as u32);
        merged_results.truncate(options.limit);
    }
    normalize_search_result_ranks(&mut merged_results);

    let truncation = if has_more {
        ContextTruncation {
            truncated: true,
            reason: Some(if truncated { "source_limit" } else { "limit" }.to_owned()),
            omitted_results: omitted_results.max(1),
        }
    } else {
        ContextTruncation::default()
    };
    let cursor_offset = merged_results.len();

    Ok(SearchPacket {
        schema_version: SEARCH_PACKET_SCHEMA_VERSION,
        query: search_terms.join(" OR "),
        filters: options.filters,
        generated_at: utc_now(),
        results: merged_results,
        pagination: pagination(Some(cursor_offset), has_more),
        truncation,
    })
}

fn composed_search_terms(query: &str, terms: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::<String>::new();
    let mut out = Vec::new();
    for value in std::iter::once(query).chain(terms.iter().map(String::as_str)) {
        let Some(term) = non_blank(value) else {
            continue;
        };
        let key = term.to_lowercase();
        if seen.insert(key) {
            out.push(term);
        }
    }
    out
}

fn search_result_merge_key(result: &SearchPacketResult, result_mode: SearchResultMode) -> Uuid {
    if result_mode == SearchResultMode::Sessions {
        result.session_id.unwrap_or(result.record_id)
    } else {
        result.event_id.unwrap_or(result.record_id)
    }
}

fn merge_search_result(existing: &mut SearchPacketResult, incoming: SearchPacketResult) {
    let incoming_rank = incoming.rank;
    let existing_rank = existing.rank;
    if incoming_rank > existing_rank {
        existing.title = incoming.title.clone();
        existing.snippet = incoming.snippet.clone();
        existing.record_id = incoming.record_id;
        existing.event_id = incoming.event_id;
        existing.event_seq = incoming.event_seq;
        existing.timestamp = incoming.timestamp;
        existing.cwd = incoming.cwd.clone();
        existing.provider = incoming.provider;
        existing.provider_session_id = incoming.provider_session_id.clone();
        existing.history_source = incoming.history_source.clone();
        existing.history_source_plugin = incoming.history_source_plugin.clone();
        existing.provider_key = incoming.provider_key.clone();
        existing.source_id = incoming.source_id.clone();
        existing.source_format = incoming.source_format.clone();
        existing.raw_source_path = incoming.raw_source_path.clone();
        existing.raw_source_exists = incoming.raw_source_exists;
        existing.cursor = incoming.cursor.clone();
    } else {
        existing.history_source = existing
            .history_source
            .clone()
            .or(incoming.history_source.clone());
        existing.history_source_plugin = existing
            .history_source_plugin
            .clone()
            .or(incoming.history_source_plugin.clone());
        existing.provider_key = existing
            .provider_key
            .clone()
            .or(incoming.provider_key.clone());
        existing.source_id = existing.source_id.clone().or(incoming.source_id.clone());
        existing.source_format = existing
            .source_format
            .clone()
            .or(incoming.source_format.clone());
    }
    existing.rank = existing_rank.max(incoming_rank) + 0.08;
    existing.more_matches_in_session = existing
        .more_matches_in_session
        .saturating_add(1)
        .saturating_add(incoming.more_matches_in_session);
    if existing.result_scope == SearchResultScope::Session {
        existing.session_importance =
            session_importance(existing.rank, existing.more_matches_in_session);
    }
    for reason in incoming.why_matched {
        push_unique_why(&mut existing.why_matched, reason);
    }
    for citation in incoming.citations {
        let duplicate = existing.citations.iter().any(|existing_citation| {
            existing_citation.citation_type == citation.citation_type
                && existing_citation.id == citation.id
        });
        if !duplicate {
            existing.citations.push(citation);
        }
    }
}

fn push_unique_why(why_matched: &mut Vec<String>, reason: String) {
    if !why_matched.iter().any(|value| value == &reason) {
        why_matched.push(reason);
    }
}

fn compare_search_results(left: &SearchPacketResult, right: &SearchPacketResult) -> Ordering {
    right
        .rank
        .partial_cmp(&left.rank)
        .unwrap_or(Ordering::Equal)
        .then_with(|| right.timestamp.cmp(&left.timestamp))
        .then_with(|| left.record_id.cmp(&right.record_id))
}

fn push_candidate_results(
    results: &mut Vec<SearchPacketResult>,
    candidates: &[Candidate],
    query: &str,
    options: &PacketOptions,
) {
    let mut clustered_index = BTreeMap::<Uuid, usize>::new();
    for candidate in candidates {
        let mut result = candidate_search_result(candidate, query, options);
        if options.result_mode == SearchResultMode::Sessions {
            let cluster_id = result.session_id.unwrap_or(result.record_id);
            if let Some(index) = clustered_index.get(&cluster_id).copied() {
                let existing = &mut results[index];
                existing.more_matches_in_session =
                    existing.more_matches_in_session.saturating_add(1);
                existing.session_importance =
                    session_importance(existing.rank, existing.more_matches_in_session);
                continue;
            }
            if result.session_id.is_some() {
                result.result_scope = SearchResultScope::Session;
                result.session_importance = session_importance(result.rank, 0);
            }
            clustered_index.insert(cluster_id, results.len());
        }
        results.push(result);
        if results.len() >= options.limit {
            break;
        }
    }
}

fn candidate_search_result(
    candidate: &Candidate,
    query: &str,
    options: &PacketOptions,
) -> SearchPacketResult {
    let display_hit = candidate_display_hit(candidate, &options.filters);
    let record_id = candidate
        .primary_hit
        .as_ref()
        .and_then(|hit| hit.event_id)
        .unwrap_or(candidate.record.id);
    SearchPacketResult {
        record_id,
        session_id: display_hit.as_ref().and_then(|hit| hit.session_id),
        event_id: display_hit.as_ref().and_then(|hit| hit.event_id),
        event_seq: display_hit.as_ref().and_then(|hit| hit.event_seq),
        title: local_snippet(&candidate.record.title, 240),
        snippet: search_snippet(
            &candidate.record,
            &candidate.context,
            query,
            options.snippet_chars,
            &options.filters,
        ),
        rank: candidate.score,
        result_scope: SearchResultScope::Event,
        more_matches_in_session: 0,
        session_importance: 0.0,
        provider: display_hit.as_ref().and_then(|hit| hit.provider),
        provider_session_id: display_hit
            .as_ref()
            .and_then(|hit| hit.provider_session_id.clone()),
        history_source: display_hit
            .as_ref()
            .and_then(|hit| hit.history_source.clone()),
        history_source_plugin: display_hit
            .as_ref()
            .and_then(|hit| hit.history_source_plugin.clone()),
        provider_key: display_hit
            .as_ref()
            .and_then(|hit| hit.provider_key.clone()),
        source_id: display_hit.as_ref().and_then(|hit| hit.source_id.clone()),
        source_format: display_hit
            .as_ref()
            .and_then(|hit| hit.source_format.clone()),
        timestamp: display_hit.as_ref().map(|hit| hit.time),
        cwd: display_hit.as_ref().and_then(|hit| hit.cwd.clone()),
        raw_source_path: display_hit
            .as_ref()
            .and_then(|hit| hit.raw_source_path.clone()),
        raw_source_exists: display_hit.as_ref().and_then(|hit| hit.raw_source_exists),
        cursor: display_hit.as_ref().and_then(|hit| hit.cursor.clone()),
        why_matched: candidate.why_matched.clone(),
        citations: candidate.citations.clone(),
        links: links_for(&candidate.record, options),
        visibility: Visibility::LocalOnly,
    }
}

fn candidate_display_hit(candidate: &Candidate, filters: &SearchFilters) -> Option<HitMetadata> {
    if let Some(hit) = &candidate.primary_hit {
        if hit.event_id.is_some() {
            return Some(hit.clone());
        }
    }
    if let Some(event) = candidate.context.events.iter().find(|event| {
        let hit = event_hit(event, &candidate.context);
        filters
            .provider
            .map_or(true, |provider| hit.provider == Some(provider))
            && filters
                .session
                .map_or(true, |id| hit.session_id == Some(id))
            && hit_matches_history_source_filter(&hit, filters)
    }) {
        return Some(event_hit(event, &candidate.context));
    }
    if let Some(hit) = &candidate.primary_hit {
        if hit.provider.is_some() || hit.session_id.is_some() {
            return Some(hit.clone());
        }
    }
    candidate
        .context
        .sessions
        .iter()
        .find(|session| {
            filters
                .provider
                .map_or(true, |provider| session.provider == provider)
                && filters.session.map_or(true, |id| session.id == id)
                && hit_matches_history_source_filter(
                    &session_hit(session, &candidate.context),
                    filters,
                )
        })
        .or_else(|| candidate.context.sessions.first())
        .map(|session| session_hit(session, &candidate.context))
}

fn fast_event_search_packet(
    store: &Store,
    query: &str,
    options: &PacketOptions,
    file_scope: Option<&FileTouchScope>,
) -> Result<Option<SearchPacket>> {
    if query.trim().is_empty() {
        return Ok(None);
    }
    if has_history_source_filter(&options.filters) {
        return Ok(None);
    }
    if !store.has_at_least_events(LARGE_EVENT_CORPUS_THRESHOLD)? {
        return Ok(None);
    }

    let target_results = options.limit.saturating_add(1);
    let filtered = has_filters(&options.filters);
    let clustered = options.result_mode == SearchResultMode::Sessions;
    let page_size = if clustered {
        FILTERED_SEARCH_PAGE_SIZE.max(target_results.saturating_mul(8).max(50))
    } else if filtered {
        FILTERED_SEARCH_PAGE_SIZE.max(target_results)
    } else {
        target_results
    };
    let mut results = Vec::new();
    let mut clustered_results = Vec::<SearchPacketResult>::new();
    let mut clustered_index = BTreeMap::<Uuid, usize>::new();
    let mut offset = 0_usize;
    let mut pages_scanned = 0_usize;
    let mut scan_budget_exhausted = false;

    loop {
        pages_scanned = pages_scanned.saturating_add(1);
        let hits = store.search_event_hits_page(query, page_size, offset)?;
        let page_len = hits.len();

        for hit in hits {
            if !event_hit_matches_filters(&hit, &options.filters, file_scope) {
                continue;
            }
            if clustered {
                let cluster_id = hit.session_id.unwrap_or(hit.event_id);
                if let Some(index) = clustered_index.get(&cluster_id).copied() {
                    let existing = &mut clustered_results[index];
                    existing.more_matches_in_session =
                        existing.more_matches_in_session.saturating_add(1);
                    existing.session_importance =
                        session_importance(existing.rank, existing.more_matches_in_session);
                } else {
                    let mut result = event_search_result(&hit, query, options.snippet_chars);
                    result.result_scope = if result.session_id.is_some() {
                        SearchResultScope::Session
                    } else {
                        SearchResultScope::Event
                    };
                    result.session_importance = session_importance(result.rank, 0);
                    clustered_index.insert(cluster_id, clustered_results.len());
                    clustered_results.push(result);
                }
                if clustered_results.len() >= target_results {
                    break;
                }
            } else {
                let result = event_search_result(&hit, query, options.snippet_chars);
                results.push(result);
                if results.len() >= target_results {
                    break;
                }
            }
        }

        let enough_results = if clustered {
            clustered_results.len() >= target_results
        } else {
            results.len() >= target_results
        };
        if (!filtered && !clustered) || enough_results || page_len < page_size {
            break;
        }
        if pages_scanned >= FILTERED_SEARCH_MAX_PAGES {
            scan_budget_exhausted = true;
            break;
        }
        let next_offset = offset.saturating_add(page_size);
        if next_offset == offset {
            break;
        }
        offset = next_offset;
    }

    if clustered {
        results = clustered_results;
    }
    let has_more = results.len() > options.limit || scan_budget_exhausted;
    if results.len() > options.limit {
        results.truncate(options.limit);
    }
    normalize_search_result_ranks(&mut results);

    let truncation = if scan_budget_exhausted {
        ContextTruncation {
            truncated: true,
            reason: Some("scan_budget".to_owned()),
            omitted_results: 1,
        }
    } else if has_more {
        ContextTruncation {
            truncated: true,
            reason: Some("limit".to_owned()),
            omitted_results: 1,
        }
    } else {
        ContextTruncation::default()
    };

    let cursor_offset = results.len();
    Ok(Some(SearchPacket {
        schema_version: SEARCH_PACKET_SCHEMA_VERSION,
        query: query.to_owned(),
        filters: options.filters.clone(),
        generated_at: utc_now(),
        results,
        pagination: pagination(Some(cursor_offset), has_more),
        truncation,
    }))
}

fn empty_search_packet(query: &str, options: &PacketOptions) -> SearchPacket {
    SearchPacket {
        schema_version: SEARCH_PACKET_SCHEMA_VERSION,
        query: query.to_owned(),
        filters: options.filters.clone(),
        generated_at: utc_now(),
        results: Vec::new(),
        pagination: pagination(Some(0), false),
        truncation: ContextTruncation::default(),
    }
}

fn event_hit_matches_filters(
    hit: &EventSearchHit,
    filters: &SearchFilters,
    file_scope: Option<&FileTouchScope>,
) -> bool {
    if let Some(session_id) = filters.session {
        if hit.session_id != Some(session_id) {
            return false;
        }
    }
    if event_hit_matches_excluded_provider_session(hit, filters) {
        return false;
    }
    if let Some(provider) = filters.provider {
        if hit.provider != Some(provider) {
            return false;
        }
    }
    if let Some(since) = filters.since {
        if hit.occurred_at < since {
            return false;
        }
    }
    if !event_hit_matches_agent_scope(hit, filters) {
        return false;
    }
    if let Some(event_type) = filters.event_type {
        if hit.event_type != event_type {
            return false;
        }
    }
    if let Some(repo) = filters
        .repo
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let repo = repo.to_lowercase();
        let matches_repo = [
            hit.cwd.as_deref(),
            hit.raw_source_path.as_deref(),
            hit.record_workspace.as_deref(),
        ]
        .into_iter()
        .flatten()
        .any(|value| value.to_lowercase().contains(&repo));
        if !matches_repo {
            return false;
        }
    }
    if let Some(scope) = file_scope {
        if !file_scope_matches_hit(scope, hit) {
            return false;
        }
    }
    true
}

fn event_hit_matches_excluded_provider_session(
    hit: &EventSearchHit,
    filters: &SearchFilters,
) -> bool {
    filters
        .exclude_provider_session
        .as_ref()
        .is_some_and(|excluded| {
            (hit.provider == Some(excluded.provider)
                && hit.session_external_session_id.as_deref()
                    == Some(excluded.provider_session_id.as_str()))
                || excluded_session_tree_matches(
                    excluded,
                    hit.session_id,
                    hit.session_parent_session_id,
                    hit.session_root_session_id,
                )
        })
}

fn hit_matches_excluded_provider_session(hit: &HitMetadata, filters: &SearchFilters) -> bool {
    filters
        .exclude_provider_session
        .as_ref()
        .is_some_and(|excluded| {
            (hit.provider == Some(excluded.provider)
                && hit.provider_session_id.as_deref()
                    == Some(excluded.provider_session_id.as_str()))
                || excluded_session_tree_matches(
                    excluded,
                    hit.session_id,
                    hit.parent_session_id,
                    hit.root_session_id,
                )
        })
}

fn context_has_excluded_provider_session(context: &RecordContext, filters: &SearchFilters) -> bool {
    filters
        .exclude_provider_session
        .as_ref()
        .is_some_and(|excluded| {
            context.sessions.iter().any(|session| {
                (session.provider == excluded.provider
                    && session.external_session_id.as_deref()
                        == Some(excluded.provider_session_id.as_str()))
                    || excluded_session_tree_matches(
                        excluded,
                        Some(session.id),
                        session.parent_session_id,
                        session.root_session_id,
                    )
            })
        })
}

fn excluded_session_tree_matches(
    excluded: &ProviderSessionFilter,
    session_id: Option<Uuid>,
    parent_session_id: Option<Uuid>,
    root_session_id: Option<Uuid>,
) -> bool {
    excluded.session_id.is_some_and(|excluded_session_id| {
        session_id == Some(excluded_session_id)
            || parent_session_id == Some(excluded_session_id)
            || root_session_id == Some(excluded_session_id)
    })
}

fn file_filter_scope(store: &Store, filters: &SearchFilters) -> Result<Option<FileTouchScope>> {
    let Some(file) = filters
        .file
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    Ok(Some(store.file_touch_scope(file)?))
}

fn file_scope_matches_hit(scope: &FileTouchScope, hit: &EventSearchHit) -> bool {
    scope.event_ids.contains(&hit.event_id)
        || hit
            .run_id
            .is_some_and(|run_id| scope.run_ids.contains(&run_id))
        || hit
            .session_id
            .is_some_and(|session_id| scope.session_ids.contains(&session_id))
        || hit
            .history_record_id
            .is_some_and(|record_id| scope.history_record_ids.contains(&record_id))
}

fn event_search_result(
    hit: &EventSearchHit,
    query: &str,
    snippet_chars: usize,
) -> SearchPacketResult {
    let terms = query_terms(query);
    let raw_source_exists = hit
        .raw_source_path
        .as_deref()
        .map(|path| Path::new(path).exists());
    let mut citations = vec![ContextCitation {
        citation_type: ContextCitationType::Event,
        id: hit.event_id,
        label: event_result_label(hit).to_owned(),
        time: hit.occurred_at,
        provider: hit.provider,
        session_id: hit.session_id,
        event_seq: Some(hit.seq),
        raw_source_path: hit.raw_source_path.clone(),
        raw_source_exists,
        cursor: hit.cursor.clone(),
    }];
    if let Some(session_id) = hit.session_id {
        citations.push(ContextCitation {
            citation_type: ContextCitationType::Session,
            id: session_id,
            label: "session".to_owned(),
            time: hit.occurred_at,
            provider: hit.provider,
            session_id: Some(session_id),
            event_seq: None,
            raw_source_path: hit.raw_source_path.clone(),
            raw_source_exists,
            cursor: hit.cursor.clone(),
        });
    }

    SearchPacketResult {
        record_id: hit.event_id,
        session_id: hit.session_id,
        event_id: Some(hit.event_id),
        event_seq: Some(hit.seq),
        title: event_result_title(hit),
        snippet: matched_snippet(&hit.preview, &terms, snippet_chars),
        rank: (-hit.score as f32).max(0.0),
        result_scope: SearchResultScope::Event,
        more_matches_in_session: 0,
        session_importance: 0.0,
        provider: hit.provider,
        provider_session_id: hit.session_external_session_id.clone(),
        history_source: hit.history_source.clone(),
        history_source_plugin: hit.history_source_plugin.clone(),
        provider_key: hit.provider_key.clone(),
        source_id: hit.source_id.clone(),
        source_format: hit.source_format.clone(),
        timestamp: Some(hit.occurred_at),
        cwd: hit.cwd.clone(),
        raw_source_path: hit.raw_source_path.clone(),
        raw_source_exists,
        cursor: hit.cursor.clone(),
        why_matched: vec![event_reason(hit.event_type).to_owned()],
        citations,
        links: ContextLinks::default(),
        visibility: Visibility::LocalOnly,
    }
}

fn event_result_title(hit: &EventSearchHit) -> String {
    let provider = hit
        .provider
        .map(|provider| provider.as_str())
        .unwrap_or("agent");
    let source = hit
        .session_external_session_id
        .as_deref()
        .or_else(|| {
            hit.raw_source_path
                .as_deref()
                .and_then(|path| Path::new(path).file_name().and_then(|value| value.to_str()))
        })
        .map(|value| local_snippet(value, 80));
    match source {
        Some(source) => format!("{provider} {} - {source}", event_result_label(hit)),
        None => format!("{provider} {}", event_result_label(hit)),
    }
}

fn event_result_label(hit: &EventSearchHit) -> &'static str {
    match hit.event_type {
        EventType::Message => match hit.role {
            Some(ctx_history_core::EventRole::User) => "user message",
            Some(ctx_history_core::EventRole::Assistant) => "assistant message",
            Some(ctx_history_core::EventRole::System) => "system message",
            _ => "message",
        },
        EventType::ToolCall => "tool call",
        EventType::ToolOutput => "tool output",
        EventType::CommandStarted => "command started",
        EventType::CommandOutput => "command output",
        EventType::CommandFinished => "command finished",
        EventType::FileTouched => "file touched",
        EventType::VcsChange => "vcs change",
        EventType::Artifact => "artifact",
        EventType::Summary => "summary",
        EventType::Notice => "notice",
    }
}

fn event_reason(event_type: EventType) -> &'static str {
    match event_type {
        EventType::Message => "message",
        EventType::ToolCall => "tool_call",
        EventType::ToolOutput => "tool_output",
        EventType::CommandStarted | EventType::CommandOutput | EventType::CommandFinished => {
            "command_event"
        }
        EventType::FileTouched => "file_touched",
        EventType::VcsChange => "vcs_change",
        EventType::Artifact => "artifact",
        EventType::Summary => "summary",
        EventType::Notice => "notice",
    }
}

fn normalize_search_result_ranks(results: &mut [SearchPacketResult]) {
    let max_rank = results
        .iter()
        .map(|result| result.rank)
        .fold(0.0_f32, f32::max);
    if max_rank <= 0.0 {
        return;
    }
    for result in results.iter_mut() {
        result.rank = (result.rank / max_rank).clamp(0.0, 1.0);
    }
    for result in results.iter_mut() {
        if result.result_scope == SearchResultScope::Session {
            result.session_importance =
                session_importance(result.rank, result.more_matches_in_session);
        } else {
            result.session_importance = 0.0;
        }
    }
}

fn session_importance(rank: f32, more_matches_in_session: usize) -> f32 {
    let coverage_boost = ((more_matches_in_session as f32).ln_1p() * 0.08).min(0.24);
    (rank + coverage_boost).clamp(0.0, 1.0)
}

pub fn display_snippet(input: &str, max_chars: usize) -> String {
    local_snippet(input, max_chars)
}

fn normalized_options(options: &PacketOptions) -> PacketOptions {
    PacketOptions {
        limit: options.limit.clamp(1, MAX_RESULT_LIMIT),
        snippet_chars: options.snippet_chars.clamp(32, 2_000),
        filters: options.filters.clone(),
        result_mode: options.result_mode,
    }
}

fn ranked_candidates(
    store: &Store,
    query: Option<&str>,
    options: &PacketOptions,
    file_scope: Option<&FileTouchScope>,
) -> Result<CandidateSearch> {
    let target_candidates = options.limit.saturating_add(1);
    let filtered = has_filters(&options.filters);
    let terms = query_terms(query.unwrap_or_default());
    let mut candidates = Vec::new();
    let mut seen = BTreeSet::<Uuid>::new();
    let mut scan_budget_exhausted = false;
    let file_only = terms.is_empty() && file_scope.is_some();
    if terms.is_empty() && !file_only {
        return Ok(CandidateSearch {
            candidates,
            scan_budget_exhausted,
        });
    }

    if filtered {
        let page_size = FILTERED_SEARCH_PAGE_SIZE.max(target_candidates);
        let mut offset = 0_usize;
        let mut pages_scanned = 0_usize;
        loop {
            pages_scanned = pages_scanned.saturating_add(1);
            let records = if file_only {
                store.list_records_page(page_size, offset)?
            } else {
                match query {
                    Some(query) if !query.trim().is_empty() => {
                        store.search_records_page(query, page_size, offset)?
                    }
                    _ => Vec::new(),
                }
            };
            let page_len = records.len();

            for record in records {
                if !seen.insert(record.id) {
                    continue;
                }
                if let Some(scope) = file_scope {
                    if !scope.history_record_ids.is_empty()
                        && !scope.history_record_ids.contains(&record.id)
                    {
                        continue;
                    }
                }
                if let Some(candidate) =
                    candidate_for_record(store, record, &terms, &options.filters, file_scope)?
                {
                    candidates.push(candidate);
                }
            }

            if candidates.len() >= target_candidates || page_len < page_size {
                break;
            }
            if pages_scanned >= FILTERED_SEARCH_MAX_PAGES {
                scan_budget_exhausted = true;
                break;
            }
            let next_offset = offset.saturating_add(page_size);
            if next_offset == offset {
                break;
            }
            offset = next_offset;
        }
    } else {
        let fetch_limit = target_candidates;
        let records = if file_only {
            store.list_records(fetch_limit)?
        } else {
            match query {
                Some(query) if !query.trim().is_empty() => {
                    store.search_records(query, fetch_limit)?
                }
                _ => Vec::new(),
            }
        };
        for record in records {
            if !seen.insert(record.id) {
                continue;
            }
            if file_scope.is_some_and(|scope| !scope.history_record_ids.contains(&record.id)) {
                continue;
            }
            if let Some(candidate) =
                candidate_for_record(store, record, &terms, &options.filters, file_scope)?
            {
                candidates.push(candidate);
            }
        }
    }

    normalize_scores(&mut candidates);
    candidates.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| right.record.updated_at.cmp(&left.record.updated_at))
            .then_with(|| left.record.title.cmp(&right.record.title))
            .then_with(|| left.record.id.cmp(&right.record.id))
    });
    if candidates.len() > target_candidates {
        candidates.truncate(target_candidates);
    }
    Ok(CandidateSearch {
        candidates,
        scan_budget_exhausted,
    })
}

fn candidate_for_record(
    store: &Store,
    record: HistoryRecord,
    terms: &[String],
    filters: &SearchFilters,
    file_scope: Option<&FileTouchScope>,
) -> Result<Option<Candidate>> {
    let context = hydrate_record_context(store, record.id, filters.file.as_deref())?;
    if !record_matches_filters(&record, &context, filters, file_scope) {
        return Ok(None);
    }
    let analysis = analyze_record(&record, &context, terms, filters);
    if terms.is_empty() || analysis.score > 0.0 {
        Ok(Some(Candidate {
            record,
            context,
            score: analysis.score,
            why_matched: analysis.why_matched,
            citations: analysis.citations,
            primary_hit: analysis.primary_hit,
        }))
    } else {
        Ok(None)
    }
}

fn hydrate_record_context(
    store: &Store,
    record_id: Uuid,
    file_filter: Option<&str>,
) -> Result<RecordContext> {
    let sessions = store.sessions_for_record(record_id)?;
    let runs = store.runs_for_record(record_id)?;
    let events = store.events_for_record(record_id)?;
    let artifacts = store.artifacts_for_record(record_id)?;
    let files_touched =
        if let Some(file) = file_filter.map(str::trim).filter(|value| !value.is_empty()) {
            store.files_touched_for_record_matching(record_id, file)?
        } else {
            store.files_touched_for_record(record_id)?
        };
    let vcs_changes = store.vcs_changes_for_record(record_id)?;
    let summaries = store.summaries_for_record(record_id)?;
    let mut source_ids = BTreeSet::new();
    for session in &sessions {
        if let Some(id) = session.capture_source_id {
            source_ids.insert(id);
        }
    }
    for run in &runs {
        if let Some(id) = run.source_id {
            source_ids.insert(id);
        }
    }
    for event in &events {
        if let Some(id) = event.capture_source_id {
            source_ids.insert(id);
        }
    }
    for artifact in &artifacts {
        if let Some(id) = artifact.source_id {
            source_ids.insert(id);
        }
    }
    for file in &files_touched {
        if let Some(id) = file.source_id {
            source_ids.insert(id);
        }
    }
    for change in &vcs_changes {
        if let Some(id) = change.source_id {
            source_ids.insert(id);
        }
    }
    for summary in &summaries {
        if let Some(id) = summary.source_id {
            source_ids.insert(id);
        }
    }
    let mut sources = BTreeMap::new();
    for source_id in source_ids {
        if let Ok(source) = store.get_capture_source(source_id) {
            sources.insert(source_id, source);
        }
    }

    Ok(RecordContext {
        sessions,
        runs,
        events,
        artifacts,
        files_touched,
        vcs_changes,
        summaries,
        sources,
    })
}

struct MatchAnalysis {
    score: f32,
    why_matched: Vec<String>,
    citations: Vec<ContextCitation>,
    primary_hit: Option<HitMetadata>,
}

fn analyze_record(
    record: &HistoryRecord,
    context: &RecordContext,
    terms: &[String],
    filters: &SearchFilters,
) -> MatchAnalysis {
    let mut score = 0.0_f32;
    let mut why = Vec::new();
    let mut citations = Vec::new();

    if terms.is_empty() {
        if filters
            .file
            .as_ref()
            .is_some_and(|file| !file.trim().is_empty())
        {
            let mut primary_hit = None;
            for section in search_sections(record, context, filters)
                .into_iter()
                .filter(|section| section.reason == "file_touched")
            {
                if primary_hit.is_none() {
                    primary_hit = Some(section.hit.clone());
                }
                score += section.weight;
                add_match(
                    &mut why,
                    &mut citations,
                    section.reason,
                    section.citation,
                    &section.hit,
                );
            }
            if !why.is_empty() {
                return MatchAnalysis {
                    score,
                    why_matched: why,
                    citations,
                    primary_hit,
                };
            }
        }
        add_match(
            &mut why,
            &mut citations,
            "recent_activity",
            ContextCitation {
                citation_type: ContextCitationType::HistoryRecord,
                id: record.id,
                label: "recent session".to_owned(),
                time: record.updated_at,
                provider: None,
                session_id: None,
                event_seq: None,
                raw_source_path: None,
                raw_source_exists: None,
                cursor: None,
            },
            &empty_hit(record.updated_at),
        );
        return MatchAnalysis {
            score: 1.0,
            why_matched: why,
            citations,
            primary_hit: None,
        };
    }

    let mut primary_hit = None;
    let mut primary_weight = f32::MIN;
    for section in search_sections(record, context, filters) {
        if hit_matches_excluded_provider_session(&section.hit, filters) {
            continue;
        }
        if matches_terms(&section.text, terms) {
            score += section.weight;
            if section.weight > primary_weight {
                primary_weight = section.weight;
                primary_hit = Some(section.hit.clone());
            }
            add_match(
                &mut why,
                &mut citations,
                section.reason,
                section.citation,
                &section.hit,
            );
        }
    }

    MatchAnalysis {
        score,
        why_matched: why,
        citations,
        primary_hit,
    }
}

fn add_match(
    why: &mut Vec<String>,
    citations: &mut Vec<ContextCitation>,
    reason: &str,
    mut citation: ContextCitation,
    hit: &HitMetadata,
) {
    if !why.iter().any(|value| value == reason) {
        why.push(reason.to_owned());
    }
    citation.provider = hit.provider;
    citation.session_id = hit.session_id;
    citation.event_seq = hit.event_seq;
    citation.raw_source_path = hit.raw_source_path.clone();
    citation.raw_source_exists = hit.raw_source_exists;
    citation.cursor = hit.cursor.clone().or_else(|| {
        hit.provider_session_id
            .as_ref()
            .map(|session_id| format!("session:{session_id}"))
    });
    if !citations.iter().any(|existing| {
        existing.citation_type == citation.citation_type && existing.id == citation.id
    }) {
        citations.push(citation);
    }
}

fn search_sections(
    record: &HistoryRecord,
    context: &RecordContext,
    filters: &SearchFilters,
) -> Vec<SearchSection> {
    let mut sections = Vec::new();
    let record_hit = record_context_display_hit(context, filters, record.updated_at);
    sections.push(SearchSection {
        reason: "title",
        weight: 8.0,
        text: record.title.clone(),
        citation: citation(
            ContextCitationType::HistoryRecord,
            record.id,
            "session title",
            record.updated_at,
        ),
        hit: record_hit.clone(),
    });
    let include_record_text = record_text_matches_agent_scope(context, filters)
        && !context_has_excluded_provider_session(context, filters);
    if include_record_text {
        sections.push(SearchSection {
            reason: "primary_user_message",
            weight: 5.0,
            text: record.body.clone(),
            citation: citation(
                ContextCitationType::HistoryRecord,
                record.id,
                "session text",
                record.updated_at,
            ),
            hit: record_hit.clone(),
        });
    }
    if include_record_text {
        for tag in &record.tags {
            sections.push(SearchSection {
                reason: "tag",
                weight: 3.0,
                text: tag.clone(),
                citation: citation(
                    ContextCitationType::HistoryRecord,
                    record.id,
                    "session tag",
                    record.updated_at,
                ),
                hit: record_hit.clone(),
            });
        }
    }
    for session in &context.sessions {
        if !session_matches_agent_scope(session, filters)
            || !source_id_matches_history_source_filter(session.capture_source_id, context, filters)
        {
            continue;
        }
        let hit = session_hit(session, context);
        sections.push(SearchSection {
            reason: "session_metadata",
            weight: 2.5,
            text: joined([
                session.provider.as_str(),
                session.agent_type.as_str(),
                session.status.as_str(),
                session.external_session_id.as_deref().unwrap_or_default(),
                session.external_agent_id.as_deref().unwrap_or_default(),
                session.role_hint.as_deref().unwrap_or_default(),
            ]),
            citation: citation(
                ContextCitationType::Session,
                session.id,
                "session",
                session.started_at,
            ),
            hit,
        });
    }

    for run in &context.runs {
        if !item_matches_agent_scope(run.session_id, run.source_id, context, filters) {
            continue;
        }
        let hit = run_hit(run, context);
        sections.push(SearchSection {
            reason: "run_command",
            weight: if run.exit_code.unwrap_or(0) == 0 {
                3.0
            } else {
                4.0
            },
            text: joined([
                run.run_type.as_str(),
                run.status.as_str(),
                run.cwd.as_deref().unwrap_or_default(),
                run.command_preview.as_deref().unwrap_or_default(),
            ]),
            citation: citation(
                ContextCitationType::Run,
                run.id,
                "run command",
                run.started_at,
            ),
            hit,
        });
    }

    for event in &context.events {
        if !item_matches_agent_scope(event.session_id, event.capture_source_id, context, filters) {
            continue;
        }
        let event_text = event_text(event);
        let hit = event_hit(event, context);
        sections.push(SearchSection {
            reason: match event.event_type {
                ctx_history_core::EventType::Message => "message",
                ctx_history_core::EventType::ToolCall => "tool_call",
                ctx_history_core::EventType::ToolOutput => "tool_output",
                ctx_history_core::EventType::CommandStarted
                | ctx_history_core::EventType::CommandOutput
                | ctx_history_core::EventType::CommandFinished => "command_event",
                _ => "event",
            },
            weight: event_weight(event),
            text: event_text,
            citation: citation(
                ContextCitationType::Event,
                event.id,
                "event",
                event.occurred_at,
            ),
            hit,
        });
    }

    for artifact in &context.artifacts {
        if !item_matches_agent_scope(None, artifact.source_id, context, filters) {
            continue;
        }
        let hit = artifact_hit(artifact, context);
        sections.push(SearchSection {
            reason: "artifact",
            weight: 2.5,
            text: joined([
                artifact.kind.as_str(),
                artifact.media_type.as_deref().unwrap_or_default(),
                artifact.preview_text.as_deref().unwrap_or_default(),
                artifact.blob_path.as_str(),
            ]),
            citation: citation(
                ContextCitationType::Artifact,
                artifact.id,
                "artifact",
                artifact.timestamps.updated_at,
            ),
            hit,
        });
    }

    for file in &context.files_touched {
        let session_id = file.event_id.and_then(|id| {
            context
                .events
                .iter()
                .find(|event| event.id == id)
                .and_then(|event| event.session_id)
        });
        if !item_matches_agent_scope(session_id, file.source_id, context, filters) {
            continue;
        }
        let hit = file_hit(file, context);
        sections.push(SearchSection {
            reason: "file_touched",
            weight: 3.0,
            text: file_touched_search_text(file),
            citation: citation(
                ContextCitationType::File,
                file.id,
                "file touched",
                file.timestamps.updated_at,
            ),
            hit,
        });
    }

    for change in &context.vcs_changes {
        if !item_matches_agent_scope(None, change.source_id, context, filters) {
            continue;
        }
        let parent_change_ids = change.parent_change_ids.join(" ");
        let hit = source_hit(
            change.source_id,
            change.author_time.unwrap_or(change.timestamps.updated_at),
            context,
        );
        sections.push(SearchSection {
            reason: "vcs_change",
            weight: 3.0,
            text: joined([
                change.kind.as_str(),
                change.change_id.as_str(),
                change.branch_or_bookmark.as_deref().unwrap_or_default(),
                change.tree_hash.as_deref().unwrap_or_default(),
                parent_change_ids.as_str(),
            ]),
            citation: citation(
                ContextCitationType::VcsChange,
                change.id,
                "vcs change",
                change.author_time.unwrap_or(change.timestamps.updated_at),
            ),
            hit,
        });
    }

    for summary in &context.summaries {
        if !item_matches_agent_scope(None, summary.source_id, context, filters) {
            continue;
        }
        let hit = source_hit(summary.source_id, summary.timestamps.updated_at, context);
        sections.push(SearchSection {
            reason: "summary",
            weight: 4.0,
            text: summary.text.clone(),
            citation: citation(
                ContextCitationType::Summary,
                summary.id,
                "summary",
                summary.timestamps.updated_at,
            ),
            hit,
        });
    }

    sections
}

fn session_matches_agent_scope(session: &Session, filters: &SearchFilters) -> bool {
    if filters.session == Some(session.id) {
        return true;
    }
    if filters.include_subagents && !filters.primary_only {
        return true;
    }
    session_is_primary(session)
        || (!filters.primary_only
            && session.agent_type == ctx_history_core::AgentType::Unknown
            && session.parent_session_id.is_none())
}

fn session_is_primary(session: &Session) -> bool {
    session.is_primary || session.agent_type == ctx_history_core::AgentType::Primary
}

fn event_hit_matches_agent_scope(hit: &EventSearchHit, filters: &SearchFilters) -> bool {
    if filters.session.is_some() && filters.session == hit.session_id {
        return true;
    }
    if filters.include_subagents && !filters.primary_only {
        return true;
    }
    if hit.session_is_primary == Some(true)
        || hit.agent_type == Some(ctx_history_core::AgentType::Primary)
    {
        return true;
    }
    if filters.primary_only {
        return false;
    }
    hit.session_is_primary.is_none() && hit.agent_type.is_none()
}

fn record_text_matches_agent_scope(context: &RecordContext, filters: &SearchFilters) -> bool {
    if has_history_source_filter(filters) {
        return false;
    }
    context
        .sessions
        .iter()
        .all(|session| session_matches_agent_scope(session, filters))
}

fn item_matches_agent_scope(
    session_id: Option<Uuid>,
    source_id: Option<Uuid>,
    context: &RecordContext,
    filters: &SearchFilters,
) -> bool {
    let item_source_id = source_id.or_else(|| {
        session_id
            .and_then(|id| context.sessions.iter().find(|session| session.id == id))
            .and_then(|session| session.capture_source_id)
    });
    if !source_id_matches_history_source_filter(item_source_id, context, filters) {
        return false;
    }
    associated_session(session_id, source_id, context)
        .map(|session| session_matches_agent_scope(session, filters))
        .unwrap_or(true)
}

fn source_id_matches_history_source_filter(
    source_id: Option<Uuid>,
    context: &RecordContext,
    filters: &SearchFilters,
) -> bool {
    if !has_history_source_filter(filters) {
        return true;
    }
    source_id
        .and_then(|id| context.sources.get(&id))
        .is_some_and(|source| source_matches_history_source_filter(source, filters))
}

fn associated_session(
    session_id: Option<Uuid>,
    source_id: Option<Uuid>,
    context: &RecordContext,
) -> Option<&Session> {
    session_id
        .and_then(|id| context.sessions.iter().find(|session| session.id == id))
        .or_else(|| source_id.and_then(|id| associated_session_for_source(id, context)))
}

fn associated_session_for_source(source_id: Uuid, context: &RecordContext) -> Option<&Session> {
    context
        .sessions
        .iter()
        .find(|session| session.capture_source_id == Some(source_id))
        .or_else(|| {
            let source = context.sources.get(&source_id)?;
            context.sessions.iter().find(|session| {
                session.provider == source.descriptor.provider
                    && session.external_session_id == source.descriptor.external_session_id
            })
        })
}

fn record_context_display_hit(
    context: &RecordContext,
    filters: &SearchFilters,
    time: chrono::DateTime<Utc>,
) -> HitMetadata {
    context
        .sessions
        .iter()
        .find(|session| {
            session_matches_agent_scope(session, filters)
                && filters
                    .provider
                    .map_or(true, |provider| session.provider == provider)
                && filters.session.map_or(true, |id| session.id == id)
                && hit_matches_history_source_filter(&session_hit(session, context), filters)
        })
        .or_else(|| {
            context
                .sessions
                .iter()
                .find(|session| session_matches_agent_scope(session, filters))
        })
        .map(|session| session_hit(session, context))
        .unwrap_or_else(|| empty_hit(time))
}

fn file_touched_search_text(file: &FileTouched) -> String {
    let path = file.path.as_str();
    let old_path = file.old_path.as_deref().unwrap_or_default();
    joined([
        path,
        old_path,
        file.change_kind
            .map(|kind| kind.as_str())
            .unwrap_or_default(),
    ])
}

fn citation(
    citation_type: ContextCitationType,
    id: Uuid,
    label: &str,
    time: chrono::DateTime<Utc>,
) -> ContextCitation {
    ContextCitation {
        citation_type,
        id,
        label: label.to_owned(),
        time,
        provider: None,
        session_id: None,
        event_seq: None,
        raw_source_path: None,
        raw_source_exists: None,
        cursor: None,
    }
}

fn empty_hit(time: chrono::DateTime<Utc>) -> HitMetadata {
    HitMetadata {
        time,
        provider: None,
        provider_session_id: None,
        history_source: None,
        history_source_plugin: None,
        provider_key: None,
        source_id: None,
        source_format: None,
        session_id: None,
        parent_session_id: None,
        root_session_id: None,
        event_id: None,
        event_seq: None,
        cwd: None,
        raw_source_path: None,
        raw_source_exists: None,
        cursor: None,
    }
}

fn session_hit(session: &Session, context: &RecordContext) -> HitMetadata {
    let mut hit = source_hit(session.capture_source_id, session.started_at, context);
    hit.provider = Some(session.provider);
    hit.provider_session_id = session.external_session_id.clone();
    hit.session_id = Some(session.id);
    hit.parent_session_id = session.parent_session_id;
    hit.root_session_id = session.root_session_id;
    if hit.cwd.is_none() {
        hit.cwd = source_for_id(session.capture_source_id, context)
            .and_then(|source| source.descriptor.cwd.clone());
    }
    hit
}

fn run_hit(run: &Run, context: &RecordContext) -> HitMetadata {
    let mut hit = source_hit(run.source_id, run.started_at, context);
    hit.session_id = run.session_id;
    if let Some(session) = run
        .session_id
        .and_then(|id| context.sessions.iter().find(|session| session.id == id))
    {
        if hit.provider.is_none() {
            hit.provider = Some(session.provider);
        }
        if hit.provider_session_id.is_none() {
            hit.provider_session_id = session.external_session_id.clone();
        }
        hit.parent_session_id = session.parent_session_id;
        hit.root_session_id = session.root_session_id;
    }
    if hit.cwd.is_none() {
        hit.cwd = run.cwd.clone();
    }
    hit
}

fn event_hit(event: &Event, context: &RecordContext) -> HitMetadata {
    let mut hit = source_hit(event.capture_source_id, event.occurred_at, context);
    hit.session_id = event.session_id;
    hit.event_id = Some(event.id);
    hit.event_seq = Some(event.seq);
    hit.cursor = event_cursor(event).or(hit.cursor);
    if hit.provider.is_none() {
        if let Some(session) = event
            .session_id
            .and_then(|id| context.sessions.iter().find(|session| session.id == id))
        {
            hit.provider = Some(session.provider);
            if hit.provider_session_id.is_none() {
                hit.provider_session_id = session.external_session_id.clone();
            }
            hit.parent_session_id = session.parent_session_id;
            hit.root_session_id = session.root_session_id;
        }
    }
    hit
}

fn artifact_hit(artifact: &Artifact, context: &RecordContext) -> HitMetadata {
    source_hit(artifact.source_id, artifact.timestamps.updated_at, context)
}

fn file_hit(file: &FileTouched, context: &RecordContext) -> HitMetadata {
    let mut hit = source_hit(file.source_id, file.timestamps.updated_at, context);
    hit.event_id = file.event_id;
    hit.session_id = file.event_id.and_then(|id| {
        context
            .events
            .iter()
            .find(|event| event.id == id)
            .and_then(|event| event.session_id)
    });
    if let Some(session) = hit
        .session_id
        .and_then(|id| context.sessions.iter().find(|session| session.id == id))
    {
        hit.provider = Some(session.provider);
        hit.provider_session_id = session.external_session_id.clone();
        hit.parent_session_id = session.parent_session_id;
        hit.root_session_id = session.root_session_id;
    }
    hit
}

fn source_hit(
    source_id: Option<Uuid>,
    time: chrono::DateTime<Utc>,
    context: &RecordContext,
) -> HitMetadata {
    let Some(source) = source_for_id(source_id, context) else {
        return empty_hit(time);
    };
    let raw_source_path = source.descriptor.raw_source_path.clone();
    let identity = source_history_identity(source);
    let mut hit = HitMetadata {
        time,
        provider: Some(source.descriptor.provider),
        provider_session_id: source.descriptor.external_session_id.clone(),
        history_source: identity.history_source,
        history_source_plugin: identity.history_source_plugin,
        provider_key: identity.provider_key,
        source_id: identity.source_id,
        source_format: identity.source_format,
        session_id: None,
        parent_session_id: None,
        root_session_id: None,
        event_id: None,
        event_seq: None,
        cwd: source.descriptor.cwd.clone(),
        raw_source_exists: raw_source_path
            .as_deref()
            .map(|path| Path::new(path).exists()),
        raw_source_path,
        cursor: source_cursor(source),
    };
    if let Some(session) = associated_session_for_source(source.id, context) {
        hit.provider = Some(session.provider);
        hit.provider_session_id = session.external_session_id.clone();
        hit.session_id = Some(session.id);
        hit.parent_session_id = session.parent_session_id;
        hit.root_session_id = session.root_session_id;
    }
    hit
}

fn source_for_id(
    source_id: Option<Uuid>,
    context: &RecordContext,
) -> Option<&ctx_history_core::CaptureSource> {
    source_id.and_then(|id| context.sources.get(&id))
}

fn source_cursor(source: &ctx_history_core::CaptureSource) -> Option<String> {
    source
        .sync
        .metadata
        .get("cursor")
        .and_then(|cursor| cursor.get("after"))
        .and_then(|after| after.get("cursor"))
        .and_then(|value| value.as_str())
        .map(str::to_owned)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct SourceHistoryIdentity {
    history_source: Option<String>,
    history_source_plugin: Option<String>,
    provider_key: Option<String>,
    source_id: Option<String>,
    source_format: Option<String>,
}

fn source_history_identity(source: &ctx_history_core::CaptureSource) -> SourceHistoryIdentity {
    let metadata = &source.sync.metadata;
    let source_metadata = metadata
        .get("source_metadata")
        .and_then(serde_json::Value::as_object);
    let plugin = source_metadata
        .and_then(|metadata| metadata.get("ctx_history_plugin"))
        .or_else(|| metadata.get("ctx_history_plugin"))
        .and_then(serde_json::Value::as_object);
    let custom = source_metadata
        .and_then(|metadata| metadata.get("ctx_history_jsonl_v1"))
        .or_else(|| metadata.get("ctx_history_jsonl_v1"))
        .and_then(serde_json::Value::as_object);
    let plugin_name = plugin
        .and_then(|plugin| plugin.get("plugin_name"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let plugin_source_id = plugin
        .and_then(|plugin| plugin.get("plugin_source_id"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let history_source = plugin
        .and_then(|plugin| plugin.get("history_source"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            plugin_name
                .as_deref()
                .zip(plugin_source_id.as_deref())
                .map(|(plugin_name, source_id)| format!("{plugin_name}/{source_id}"))
        });
    let provider_key = custom
        .and_then(|custom| custom.get("provider_key"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let source_id = custom
        .and_then(|custom| custom.get("source_id"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let source_format = custom
        .and_then(|custom| custom.get("source_format"))
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            source_metadata
                .and_then(|metadata| metadata.get("source_format"))
                .and_then(serde_json::Value::as_str)
        })
        .or_else(|| {
            metadata
                .get("source_format")
                .and_then(serde_json::Value::as_str)
        })
        .map(str::to_owned);
    SourceHistoryIdentity {
        history_source,
        history_source_plugin: plugin_name,
        provider_key,
        source_id,
        source_format,
    }
}

fn has_history_source_filter(filters: &SearchFilters) -> bool {
    filters
        .history_source
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
        || filters
            .provider_key
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        || filters
            .source_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        || filters
            .source_format
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
}

fn source_matches_history_source_filter(
    source: &ctx_history_core::CaptureSource,
    filters: &SearchFilters,
) -> bool {
    let identity = source_history_identity(source);
    source_identity_matches_history_source_filter(&identity, filters)
}

fn hit_matches_history_source_filter(hit: &HitMetadata, filters: &SearchFilters) -> bool {
    if !has_history_source_filter(filters) {
        return true;
    }
    source_identity_matches_history_source_filter(
        &SourceHistoryIdentity {
            history_source: hit.history_source.clone(),
            history_source_plugin: hit.history_source_plugin.clone(),
            provider_key: hit.provider_key.clone(),
            source_id: hit.source_id.clone(),
            source_format: hit.source_format.clone(),
        },
        filters,
    )
}

fn source_identity_matches_history_source_filter(
    identity: &SourceHistoryIdentity,
    filters: &SearchFilters,
) -> bool {
    if let Some(selector) = filters
        .history_source
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let plugin_match = identity.history_source.as_deref() == Some(selector);
        let provider_source_match = identity
            .provider_key
            .as_deref()
            .zip(identity.source_id.as_deref())
            .is_some_and(|(provider_key, source_id)| {
                selector == format!("{provider_key}/{source_id}")
            });
        if !plugin_match && !provider_source_match {
            return false;
        }
    }
    if let Some(provider_key) = filters
        .provider_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if identity.provider_key.as_deref() != Some(provider_key) {
            return false;
        }
    }
    if let Some(source_id) = filters
        .source_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if identity.source_id.as_deref() != Some(source_id) {
            return false;
        }
    }
    if let Some(source_format) = filters
        .source_format
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if identity.source_format.as_deref() != Some(source_format) {
            return false;
        }
    }
    true
}

fn event_cursor(event: &Event) -> Option<String> {
    event
        .payload
        .get("cursor")
        .and_then(|value| value.as_str())
        .map(str::to_owned)
        .or_else(|| {
            event
                .sync
                .metadata
                .get("cursor")
                .and_then(|value| value.as_str())
                .map(str::to_owned)
        })
}

fn joined<const N: usize>(parts: [&str; N]) -> String {
    parts
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn event_weight(event: &Event) -> f32 {
    match event.event_type {
        ctx_history_core::EventType::Message => 4.0,
        ctx_history_core::EventType::ToolCall | ctx_history_core::EventType::ToolOutput => 3.5,
        ctx_history_core::EventType::CommandStarted
        | ctx_history_core::EventType::CommandOutput
        | ctx_history_core::EventType::CommandFinished => 3.0,
        _ => 2.0,
    }
}

fn event_text(event: &Event) -> String {
    let payload_text = event_preview_text(event);
    let dedupe_key = event.dedupe_key.as_deref().unwrap_or_default();
    joined([
        event.event_type.as_str(),
        event.role.map(|role| role.as_str()).unwrap_or_default(),
        payload_text.as_str(),
        dedupe_key,
    ])
}

pub fn event_preview_text(event: &Event) -> String {
    if matches!(
        event.redaction_state,
        RedactionState::Raw | RedactionState::Withheld
    ) {
        return "raw event payload withheld".to_owned();
    }
    if let Some(preview) = event_payload_preview(&event.payload) {
        return local_snippet(&preview, 900);
    }
    if event.payload.is_object() || event.payload.is_array() {
        return local_snippet(&event.payload.to_string(), 900);
    }
    String::new()
}

fn event_payload_preview(payload: &serde_json::Value) -> Option<String> {
    if let Some(body) = payload.get("body") {
        if let Some(preview) = event_value_preview(body) {
            return Some(preview);
        }
    }
    event_value_preview(payload)
}

fn event_value_preview(value: &serde_json::Value) -> Option<String> {
    if let Some(value) = value.as_str() {
        return non_blank(value);
    }
    let object = value.as_object()?;
    for key in [
        "text",
        "preview",
        "summary",
        "command",
        "output_preview",
        "output",
        "message",
    ] {
        if let Some(value) = object.get(key).and_then(preview_fragment) {
            return Some(value);
        }
    }
    let structured = ["tool", "name", "arguments_preview", "status"]
        .into_iter()
        .filter_map(|key| {
            object
                .get(key)
                .and_then(preview_fragment)
                .map(|value| format!("{key}: {value}"))
        })
        .collect::<Vec<_>>();
    if structured.is_empty() {
        None
    } else {
        Some(structured.join(" | "))
    }
}

fn preview_fragment(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => non_blank(value),
        serde_json::Value::Number(_) | serde_json::Value::Bool(_) => Some(value.to_string()),
        _ => None,
    }
}

fn non_blank(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

fn normalize_scores(candidates: &mut [Candidate]) {
    let max_score = candidates
        .iter()
        .map(|candidate| candidate.score)
        .fold(0.0_f32, f32::max);
    if max_score <= 0.0 {
        return;
    }
    for candidate in candidates {
        candidate.score = (candidate.score / max_score).clamp(0.0, 1.0);
    }
}

fn query_terms(query: &str) -> Vec<String> {
    query
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '-')
        .filter_map(|term| {
            let term = term.trim().to_lowercase();
            if term.is_empty() || !term.chars().any(char::is_alphanumeric) {
                None
            } else {
                Some(term)
            }
        })
        .collect()
}

fn has_filters(filters: &SearchFilters) -> bool {
    filters.session.is_some()
        || filters.provider.is_some()
        || filters
            .repo
            .as_ref()
            .is_some_and(|value| !value.trim().is_empty())
        || filters.since.is_some()
        || filters.primary_only
        || !filters.include_subagents
        || filters.event_type.is_some()
        || filters
            .file
            .as_ref()
            .is_some_and(|value| !value.trim().is_empty())
        || filters.exclude_provider_session.is_some()
        || has_history_source_filter(filters)
}

fn record_matches_filters(
    record: &HistoryRecord,
    context: &RecordContext,
    filters: &SearchFilters,
    file_scope: Option<&FileTouchScope>,
) -> bool {
    if let Some(session_id) = filters.session {
        if !context
            .sessions
            .iter()
            .any(|session| session.id == session_id)
            && !context
                .events
                .iter()
                .any(|event| event.session_id == Some(session_id))
            && !context
                .runs
                .iter()
                .any(|run| run.session_id == Some(session_id))
        {
            return false;
        }
    }

    if let Some(excluded) = &filters.exclude_provider_session {
        let matched_sessions = context
            .sessions
            .iter()
            .filter(|session| {
                (session.provider == excluded.provider
                    && session.external_session_id.as_deref()
                        == Some(excluded.provider_session_id.as_str()))
                    || excluded_session_tree_matches(
                        excluded,
                        Some(session.id),
                        session.parent_session_id,
                        session.root_session_id,
                    )
            })
            .count();
        if matched_sessions > 0 && matched_sessions == context.sessions.len() {
            return false;
        }
    }

    if let Some(provider) = filters.provider {
        let session_match = context
            .sessions
            .iter()
            .any(|session| session.provider == provider);
        let source_match = context
            .sources
            .values()
            .any(|source| source.descriptor.provider == provider);
        if !session_match && !source_match {
            return false;
        }
    }

    if has_history_source_filter(filters)
        && !context
            .sources
            .values()
            .any(|source| source_matches_history_source_filter(source, filters))
    {
        return false;
    }

    if let Some(since) = filters.since {
        let has_recent_event = context
            .events
            .iter()
            .any(|event| event.occurred_at >= since);
        let has_recent_session = context.sessions.iter().any(|session| {
            session.started_at >= since || session.ended_at.is_some_and(|ended| ended >= since)
        });
        if record.updated_at < since && !has_recent_event && !has_recent_session {
            return false;
        }
    }

    if (filters.primary_only || !filters.include_subagents)
        && !context.sessions.is_empty()
        && !context
            .sessions
            .iter()
            .any(|session| session_matches_agent_scope(session, filters))
    {
        return false;
    }

    if let Some(event_type) = filters.event_type {
        if !context
            .events
            .iter()
            .any(|event| event.event_type == event_type)
        {
            return false;
        }
    }

    if let Some(repo) = filters
        .repo
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let repo = repo.to_lowercase();
        let matches_record = record
            .workspace
            .as_deref()
            .is_some_and(|workspace| workspace.to_lowercase().contains(&repo));
        let matches_session = context.sessions.iter().any(|session| {
            session
                .sync
                .metadata
                .get("metadata")
                .and_then(|value| value.as_object())
                .is_some_and(|metadata| {
                    metadata
                        .values()
                        .any(|value| value.to_string().to_lowercase().contains(&repo))
                })
        });
        let matches_source = context.sources.values().any(|source| {
            source
                .descriptor
                .cwd
                .as_deref()
                .is_some_and(|cwd| cwd.to_lowercase().contains(&repo))
        });
        if !matches_record && !matches_session && !matches_source {
            return false;
        }
    }

    if let Some(file) = filters
        .file
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if let Some(scope) = file_scope {
            if !record_context_matches_file_scope(scope, record, context) {
                return false;
            }
        } else if !context.files_touched.iter().any(|touched| {
            touched.path == file
                || touched.path.ends_with(file)
                || touched.old_path.as_deref() == Some(file)
        }) {
            return false;
        }
    }

    true
}

fn record_context_matches_file_scope(
    scope: &FileTouchScope,
    record: &HistoryRecord,
    context: &RecordContext,
) -> bool {
    scope.history_record_ids.contains(&record.id)
        || context.sessions.iter().any(|session| {
            scope.session_ids.contains(&session.id)
                || session
                    .capture_source_id
                    .is_some_and(|source_id| scope.source_ids.contains(&source_id))
        })
        || context.runs.iter().any(|run| {
            scope.run_ids.contains(&run.id)
                || run
                    .session_id
                    .is_some_and(|session_id| scope.session_ids.contains(&session_id))
                || run
                    .source_id
                    .is_some_and(|source_id| scope.source_ids.contains(&source_id))
        })
        || context.events.iter().any(|event| {
            scope.event_ids.contains(&event.id)
                || event
                    .session_id
                    .is_some_and(|session_id| scope.session_ids.contains(&session_id))
                || event
                    .run_id
                    .is_some_and(|run_id| scope.run_ids.contains(&run_id))
                || event
                    .capture_source_id
                    .is_some_and(|source_id| scope.source_ids.contains(&source_id))
        })
        || context.files_touched.iter().any(|file| {
            file.source_id
                .is_some_and(|source_id| scope.source_ids.contains(&source_id))
        })
}

fn matches_terms(value: &str, terms: &[String]) -> bool {
    if terms.is_empty() {
        return false;
    }
    let haystack = value.to_lowercase();
    terms.iter().all(|term| haystack.contains(term))
}

fn search_snippet(
    record: &HistoryRecord,
    context: &RecordContext,
    query: &str,
    max_chars: usize,
    filters: &SearchFilters,
) -> String {
    let terms = query_terms(query);
    for section in search_sections(record, context, filters) {
        if hit_matches_excluded_provider_session(&section.hit, filters) {
            continue;
        }
        if matches_terms(&section.text, &terms) {
            return matched_snippet(&section.text, &terms, max_chars);
        }
    }
    if !record.body.trim().is_empty()
        && record_text_matches_agent_scope(context, filters)
        && !context_has_excluded_provider_session(context, filters)
    {
        return local_snippet(&record.body, max_chars);
    }
    String::new()
}

fn matched_snippet(input: &str, terms: &[String], max_chars: usize) -> String {
    let body = input.trim();
    if body.is_empty() {
        return String::new();
    }
    let lower = body.to_lowercase();
    let start = terms
        .iter()
        .filter_map(|term| lower.find(term))
        .min()
        .unwrap_or(0);
    let start = start.saturating_sub(max_chars / 4);
    let snippet = take_chars_from(body, start, max_chars);
    local_snippet(&snippet, max_chars)
}

fn local_snippet(input: &str, max_chars: usize) -> String {
    truncate_chars(input.trim(), max_chars)
}

fn truncate_chars(input: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (idx, ch) in input.chars().enumerate() {
        if idx >= max_chars {
            out.push_str("...");
            return out;
        }
        out.push(ch);
    }
    out
}

fn take_chars_from(input: &str, start: usize, max_chars: usize) -> String {
    input.chars().skip(start).take(max_chars).collect()
}

fn links_for(_record: &HistoryRecord, _options: &PacketOptions) -> ContextLinks {
    ContextLinks {}
}

fn pagination(cursor_base: Option<usize>, has_more: bool) -> ContextPagination {
    ContextPagination {
        cursor: if has_more {
            cursor_base.map(|value| format!("offset:{value}"))
        } else {
            None
        },
        has_more,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctx_history_core::{
        AgentType, ArtifactKind, CaptureProvider, CaptureSource, CaptureSourceDescriptor,
        CaptureSourceKind, Confidence, EntityTimestamps, EventRole, EventType, Fidelity,
        FileChangeKind, HistoryRecordLink, HistoryRecordLinkTargetType, HistoryRecordLinkType,
        RedactionState, RunStatus, RunType, SessionHistoryArchive, SessionStatus, SummaryKind,
        SyncMetadata, SyncState, VcsChangeKind, VcsHost, VcsKind, VcsWorkspace,
    };

    fn tempdir() -> tempfile::TempDir {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .unwrap()
            .join("target/test-data");
        std::fs::create_dir_all(&root).unwrap();
        tempfile::Builder::new()
            .prefix("ctx-history-search-")
            .tempdir_in(root)
            .unwrap()
    }

    fn fixed_time() -> chrono::DateTime<Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-06-23T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn timestamps() -> EntityTimestamps {
        EntityTimestamps {
            created_at: fixed_time(),
            updated_at: fixed_time(),
        }
    }

    fn sync_metadata() -> SyncMetadata {
        SyncMetadata {
            visibility: Visibility::LocalOnly,
            fidelity: Fidelity::Imported,
            sync_state: SyncState::LocalOnly,
            sync_version: 0,
            deleted_at: None,
            metadata: serde_json::json!({}),
        }
    }

    fn excluded_filter(session_id: Option<Uuid>) -> SearchFilters {
        SearchFilters {
            exclude_provider_session: Some(ProviderSessionFilter {
                provider: CaptureProvider::Codex,
                provider_session_id: "provider-session-1".into(),
                session_id,
            }),
            ..SearchFilters::default()
        }
    }

    fn test_store() -> (tempfile::TempDir, ctx_history_store::Store) {
        let temp = tempdir();
        let path = temp.path().join("work.sqlite");
        let store = ctx_history_store::Store::open(path).unwrap();
        (temp, store)
    }

    #[test]
    fn local_snippets_preserve_transcript_text() {
        let snippet = display_snippet(
            "token=ghp_1234567890abcdef1234567890abcdef and password=hunter2",
            200,
        );

        assert!(snippet.contains("token=ghp_1234567890abcdef1234567890abcdef"));
        assert!(snippet.contains("password=hunter2"));
        assert!(!snippet.contains("[REDACTED"));
    }

    #[test]
    fn withheld_events_do_not_render_payload_previews() {
        let event = Event {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000010").unwrap(),
            seq: 1,
            history_record_id: None,
            session_id: None,
            run_id: None,
            event_type: EventType::Message,
            role: Some(EventRole::Assistant),
            occurred_at: fixed_time(),
            capture_source_id: None,
            payload: serde_json::json!({"text": "secret payload that must not render"}),
            payload_blob_id: None,
            dedupe_key: None,
            redaction_state: RedactionState::Withheld,
            sync: sync_metadata(),
        };

        let preview = event_preview_text(&event);
        assert_eq!(preview, "raw event payload withheld");
        assert!(!preview.contains("secret payload"));
    }

    #[test]
    fn excluded_provider_session_matches_provider_external_id_for_hits() {
        let filters = excluded_filter(None);
        let hit = HitMetadata {
            provider: Some(CaptureProvider::Codex),
            provider_session_id: Some("provider-session-1".into()),
            ..empty_hit(fixed_time())
        };
        assert!(hit_matches_excluded_provider_session(&hit, &filters));

        let event_hit = EventSearchHit {
            event_id: Uuid::parse_str("018f45d0-0000-7000-8000-000000001001").unwrap(),
            history_record_id: None,
            session_id: None,
            session_parent_session_id: None,
            session_root_session_id: None,
            run_id: None,
            seq: 1,
            event_type: EventType::Message,
            role: Some(EventRole::User),
            occurred_at: fixed_time(),
            preview: "synthetic preview".into(),
            score: 1.0,
            provider: Some(CaptureProvider::Codex),
            session_external_session_id: Some("provider-session-1".into()),
            history_source: None,
            history_source_plugin: None,
            provider_key: None,
            source_id: None,
            source_format: None,
            agent_type: Some(AgentType::Primary),
            session_is_primary: Some(true),
            cwd: None,
            raw_source_path: None,
            cursor: None,
            record_title: None,
            record_kind: None,
            record_workspace: None,
        };
        assert!(event_hit_matches_excluded_provider_session(
            &event_hit, &filters
        ));

        let mut different_provider = event_hit;
        different_provider.provider = Some(CaptureProvider::Claude);
        assert!(!event_hit_matches_excluded_provider_session(
            &different_provider,
            &filters
        ));
    }

    #[test]
    fn excluded_provider_session_matches_parent_and_root_session_tree() {
        let excluded_session_id = Uuid::parse_str("018f45d0-0000-7000-8000-000000001100").unwrap();
        let child_session_id = Uuid::parse_str("018f45d0-0000-7000-8000-000000001101").unwrap();
        let grandchild_session_id =
            Uuid::parse_str("018f45d0-0000-7000-8000-000000001102").unwrap();
        let filters = excluded_filter(Some(excluded_session_id));

        let parent_hit = HitMetadata {
            session_id: Some(child_session_id),
            parent_session_id: Some(excluded_session_id),
            ..empty_hit(fixed_time())
        };
        assert!(hit_matches_excluded_provider_session(&parent_hit, &filters));

        let root_event_hit = EventSearchHit {
            event_id: Uuid::parse_str("018f45d0-0000-7000-8000-000000001103").unwrap(),
            history_record_id: None,
            session_id: Some(grandchild_session_id),
            session_parent_session_id: Some(child_session_id),
            session_root_session_id: Some(excluded_session_id),
            run_id: None,
            seq: 1,
            event_type: EventType::Message,
            role: Some(EventRole::Assistant),
            occurred_at: fixed_time(),
            preview: "synthetic preview".into(),
            score: 1.0,
            provider: None,
            session_external_session_id: None,
            history_source: None,
            history_source_plugin: None,
            provider_key: None,
            source_id: None,
            source_format: None,
            agent_type: Some(AgentType::Subagent),
            session_is_primary: Some(false),
            cwd: None,
            raw_source_path: None,
            cursor: None,
            record_title: None,
            record_kind: None,
            record_workspace: None,
        };
        assert!(event_hit_matches_excluded_provider_session(
            &root_event_hit,
            &filters
        ));

        let context = RecordContext {
            sessions: vec![Session {
                id: grandchild_session_id,
                history_record_id: None,
                parent_session_id: Some(child_session_id),
                root_session_id: Some(excluded_session_id),
                capture_source_id: None,
                provider: CaptureProvider::Claude,
                external_session_id: Some("different-provider-session".into()),
                external_agent_id: None,
                agent_type: AgentType::Subagent,
                role_hint: None,
                is_primary: false,
                status: SessionStatus::Imported,
                transcript_blob_id: None,
                started_at: fixed_time(),
                ended_at: None,
                timestamps: timestamps(),
                sync: sync_metadata(),
            }],
            ..RecordContext::default()
        };
        assert!(context_has_excluded_provider_session(&context, &filters));
    }

    #[test]
    fn rich_search_matches_typed_metadata_with_citations_and_redaction() {
        let (_temp, store) = test_store();
        let record = HistoryRecord::new(
            "Plain work",
            "ordinary body without the query",
            vec!["needle-tag".into()],
            "task",
            None,
        );
        store.insert_record(&record).unwrap();

        let artifact = Artifact {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000201").unwrap(),
            kind: ArtifactKind::Markdown,
            blob_hash: "hash-rich-search-artifact".into(),
            blob_path: "blobs/rich-search-artifact".into(),
            byte_size: 32,
            media_type: Some("text/markdown".into()),
            preview_text: Some("needle-artifact /home/example/private/repo".into()),
            redaction_state: RedactionState::SafePreview,
            timestamps: timestamps(),
            source_id: None,
            sync: sync_metadata(),
        };
        store.upsert_artifact(&artifact).unwrap();

        let session = Session {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000202").unwrap(),
            history_record_id: Some(record.id),
            parent_session_id: None,
            root_session_id: None,
            capture_source_id: None,
            provider: CaptureProvider::Codex,
            external_session_id: Some("needle-session".into()),
            external_agent_id: Some("agent-needle".into()),
            agent_type: AgentType::Primary,
            role_hint: Some("needle-role".into()),
            is_primary: true,
            status: SessionStatus::Imported,
            transcript_blob_id: None,
            started_at: fixed_time(),
            ended_at: None,
            timestamps: timestamps(),
            sync: sync_metadata(),
        };
        store.upsert_session(&session).unwrap();

        let run = Run {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000203").unwrap(),
            history_record_id: Some(record.id),
            session_id: Some(session.id),
            run_type: RunType::Command,
            status: RunStatus::Failed,
            started_at: fixed_time(),
            ended_at: Some(fixed_time()),
            exit_code: Some(1),
            cwd: Some("/home/example/private/repo".into()),
            command_preview: Some("cargo test needle-run password=hunter2".into()),
            input_blob_id: None,
            output_blob_id: Some(artifact.id),
            timestamps: timestamps(),
            source_id: None,
            sync: sync_metadata(),
        };
        store.upsert_run(&run).unwrap();

        let event = Event {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000204").unwrap(),
            seq: 1,
            history_record_id: Some(record.id),
            session_id: Some(session.id),
            run_id: Some(run.id),
            event_type: EventType::ToolCall,
            role: Some(EventRole::Assistant),
            occurred_at: fixed_time(),
            capture_source_id: None,
            payload: serde_json::json!({
                "tool": "shell",
                "arguments": "needle-event token=secretvalue"
            }),
            payload_blob_id: None,
            dedupe_key: Some("needle-dedupe".into()),
            redaction_state: RedactionState::SafePreview,
            sync: sync_metadata(),
        };
        store.upsert_event(&event).unwrap();

        let workspace = VcsWorkspace {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000205").unwrap(),
            kind: VcsKind::Git,
            root_path: "/repo".into(),
            repo_fingerprint: "git:needle".into(),
            primary_remote_url_normalized: Some("https://github.com/ctxrs/ctx".into()),
            host: VcsHost::Github,
            owner: Some("ctxrs".into()),
            name: Some("ctx".into()),
            monorepo_subpath: None,
            timestamps: timestamps(),
            source_id: None,
            sync: sync_metadata(),
        };
        let workspace_id = store.upsert_vcs_workspace(&workspace).unwrap();

        let change = VcsChange {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000206").unwrap(),
            vcs_workspace_id: workspace_id,
            kind: VcsChangeKind::GitCommit,
            change_id: "needle-change".into(),
            parent_change_ids: vec!["parent".into()],
            branch_or_bookmark: Some("ctx/needle-branch".into()),
            tree_hash: Some("tree".into()),
            author_time: Some(fixed_time()),
            confidence: Confidence::Explicit,
            timestamps: timestamps(),
            source_id: None,
            sync: sync_metadata(),
        };
        store.upsert_vcs_change(&change).unwrap();

        let file = FileTouched {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000208").unwrap(),
            history_record_id: Some(record.id),
            run_id: Some(run.id),
            event_id: Some(event.id),
            vcs_workspace_id: Some(workspace_id),
            path: "crates/ctx-history-search/src/needle_file.rs".into(),
            change_kind: Some(FileChangeKind::Modified),
            old_path: None,
            line_count_delta: Some(12),
            confidence: Confidence::Explicit,
            timestamps: timestamps(),
            source_id: None,
            sync: sync_metadata(),
        };
        store.upsert_file_touched(&file).unwrap();

        let summary = Summary {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000209").unwrap(),
            history_record_id: Some(record.id),
            session_id: Some(session.id),
            kind: SummaryKind::ImportedProviderSummary,
            model_or_source: Some("codex".into()),
            text: "needle summary password=hunter2".into(),
            citations: Vec::new(),
            timestamps: timestamps(),
            source_id: None,
            sync: sync_metadata(),
        };
        store.upsert_summary(&summary).unwrap();

        for (target_type, target_id, link_type) in [
            (
                HistoryRecordLinkTargetType::VcsChange,
                change.id,
                HistoryRecordLinkType::References,
            ),
            (
                HistoryRecordLinkTargetType::Artifact,
                artifact.id,
                HistoryRecordLinkType::Produced,
            ),
        ] {
            store
                .upsert_history_record_link(&HistoryRecordLink {
                    id: new_link_id(target_id),
                    history_record_id: record.id,
                    target_type,
                    target_id,
                    link_type,
                    confidence: Confidence::Explicit,
                    source_id: None,
                    timestamps: timestamps(),
                    sync: sync_metadata(),
                })
                .unwrap();
        }

        let packet = search_packet(
            &store,
            "needle",
            &PacketOptions {
                limit: 5,
                snippet_chars: 600,
                ..PacketOptions::default()
            },
        )
        .unwrap();

        assert_eq!(packet.results.len(), 1);
        let result = &packet.results[0];
        for reason in [
            "tag",
            "session_metadata",
            "run_command",
            "tool_call",
            "artifact",
            "file_touched",
            "vcs_change",
            "summary",
        ] {
            assert!(
                result.why_matched.iter().any(|value| value == reason),
                "missing why_matched reason {reason}: {:?}",
                result.why_matched
            );
        }
        for removed_reason in ["failed_command", "failed_evidence_output", "pull_request"] {
            assert!(
                !result
                    .why_matched
                    .iter()
                    .any(|value| value == removed_reason),
                "removed reason {removed_reason} leaked into search result: {:?}",
                result.why_matched
            );
        }

        for citation_type in [
            ContextCitationType::HistoryRecord,
            ContextCitationType::Session,
            ContextCitationType::Run,
            ContextCitationType::Event,
            ContextCitationType::Artifact,
            ContextCitationType::File,
            ContextCitationType::VcsChange,
            ContextCitationType::Summary,
        ] {
            assert!(
                result
                    .citations
                    .iter()
                    .any(|citation| citation.citation_type == citation_type),
                "missing citation type {citation_type:?}: {:?}",
                result.citations
            );
        }
        assert_eq!(result.visibility, Visibility::LocalOnly);
        assert!(!result.snippet.contains("hunter2"));
        assert!(!result.snippet.contains("ghp_123456"));
        assert!(!result.snippet.contains("secretvalue"));

        let secret_packet = search_packet(
            &store,
            "hunter2",
            &PacketOptions {
                limit: 1,
                snippet_chars: 600,
                ..PacketOptions::default()
            },
        )
        .unwrap();
        assert!(secret_packet.results.is_empty());

        maybe_write_synthetic_search_smoke_artifact();
    }

    #[test]
    fn nested_provider_body_event_preview_drives_search() {
        let (_temp, store) = test_store();
        let record = HistoryRecord::new(
            "Provider event record",
            "ordinary body without event query",
            Vec::new(),
            "task",
            None,
        );
        store.insert_record(&record).unwrap();
        let session = Session {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000301").unwrap(),
            history_record_id: Some(record.id),
            parent_session_id: None,
            root_session_id: None,
            capture_source_id: None,
            provider: CaptureProvider::Codex,
            external_session_id: Some("codex-session".into()),
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("worker".into()),
            is_primary: true,
            status: SessionStatus::Imported,
            transcript_blob_id: None,
            started_at: fixed_time(),
            ended_at: None,
            timestamps: timestamps(),
            sync: sync_metadata(),
        };
        store.upsert_session(&session).unwrap();
        let event = Event {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000302").unwrap(),
            seq: 1,
            history_record_id: Some(record.id),
            session_id: Some(session.id),
            run_id: None,
            event_type: EventType::ToolCall,
            role: Some(EventRole::Assistant),
            occurred_at: fixed_time(),
            capture_source_id: None,
            payload: serde_json::json!({
                "provider": "codex",
                "body": {
                    "tool": "shell",
                    "name": "exec_command",
                    "arguments_preview": "nested-search-needle token=secretvalue",
                    "arguments": "unsafe-raw-needle password=hunter2"
                }
            }),
            payload_blob_id: None,
            dedupe_key: Some("nested-provider-event".into()),
            redaction_state: RedactionState::SafePreview,
            sync: sync_metadata(),
        };
        store.upsert_event(&event).unwrap();
        store.upsert_record(&record).unwrap();

        let packet = search_packet(
            &store,
            "nested-search-needle",
            &PacketOptions {
                limit: 5,
                snippet_chars: 600,
                ..PacketOptions::default()
            },
        )
        .unwrap();
        assert_eq!(packet.results.len(), 1);
        assert!(packet.results[0]
            .why_matched
            .iter()
            .any(|reason| reason == "tool_call"));
        assert!(packet.results[0]
            .snippet
            .contains("arguments_preview: nested-search-needle token=secretvalue"));
        assert!(!packet.results[0].snippet.contains("unsafe-raw-needle"));
        assert!(!packet.results[0].snippet.contains("hunter2"));

        let unsafe_packet = search_packet(
            &store,
            "unsafe-raw-needle",
            &PacketOptions {
                limit: 5,
                snippet_chars: 600,
                ..PacketOptions::default()
            },
        )
        .unwrap();
        assert!(unsafe_packet.results.is_empty());
    }

    #[test]
    fn large_agent_history_search_returns_event_hits() {
        let (_temp, store) = test_store();
        let record = HistoryRecord::new(
            "Large provider history",
            "single imported agent-history record",
            Vec::new(),
            "agent_history",
            Some("/workspace/ctx".into()),
        );
        store.insert_record(&record).unwrap();

        let session = Session {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000601").unwrap(),
            history_record_id: Some(record.id),
            parent_session_id: None,
            root_session_id: None,
            capture_source_id: None,
            provider: CaptureProvider::Codex,
            external_session_id: Some("large-history-session".into()),
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("primary".into()),
            is_primary: true,
            status: SessionStatus::Imported,
            transcript_blob_id: None,
            started_at: fixed_time(),
            ended_at: None,
            timestamps: timestamps(),
            sync: sync_metadata(),
        };
        store.upsert_session(&session).unwrap();

        let other_record = HistoryRecord::new(
            "Large provider history shard",
            "another imported agent-history record",
            Vec::new(),
            "agent_history",
            Some("/workspace/ctx".into()),
        );
        store.insert_record(&other_record).unwrap();
        let other_session = Session {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000602").unwrap(),
            history_record_id: Some(other_record.id),
            parent_session_id: None,
            root_session_id: None,
            capture_source_id: None,
            provider: CaptureProvider::Codex,
            external_session_id: Some("large-history-session-shard".into()),
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("primary".into()),
            is_primary: true,
            status: SessionStatus::Imported,
            transcript_blob_id: None,
            started_at: fixed_time(),
            ended_at: None,
            timestamps: timestamps(),
            sync: sync_metadata(),
        };
        store.upsert_session(&other_session).unwrap();

        let target_event_id = Uuid::parse_str("018f45d0-0000-7000-8000-0000000006ff").unwrap();
        for index in 0..=(LARGE_EVENT_CORPUS_THRESHOLD as u64) {
            let (event_record_id, event_session) = if index < 512 {
                (other_record.id, other_session.id)
            } else {
                (record.id, session.id)
            };
            let event_id = if index == LARGE_EVENT_CORPUS_THRESHOLD as u64 {
                target_event_id
            } else {
                let mut bytes = *event_session.as_bytes();
                bytes[14] = (index / 256) as u8;
                bytes[15] = index as u8;
                Uuid::from_bytes(bytes)
            };
            let text = if event_id == target_event_id {
                "large-fast-event-needle from one transcript"
            } else {
                "ordinary large history event"
            };
            store
                .upsert_event(&Event {
                    id: event_id,
                    seq: 10_000 + index,
                    history_record_id: Some(event_record_id),
                    session_id: Some(event_session),
                    run_id: None,
                    event_type: EventType::Message,
                    role: Some(EventRole::Assistant),
                    occurred_at: fixed_time() + chrono::Duration::milliseconds(index as i64),
                    capture_source_id: None,
                    payload: serde_json::json!({
                        "cursor": format!("line:{index}"),
                        "body": { "text": text }
                    }),
                    payload_blob_id: None,
                    dedupe_key: Some(format!("large-history-{index}")),
                    redaction_state: RedactionState::SafePreview,
                    sync: sync_metadata(),
                })
                .unwrap();
        }
        store.refresh_search_index().unwrap();

        let packet = search_packet(
            &store,
            "large-fast-event-needle",
            &PacketOptions {
                limit: 5,
                snippet_chars: 200,
                ..PacketOptions::default()
            },
        )
        .unwrap();

        assert_eq!(packet.results.len(), 1);
        let result = &packet.results[0];
        assert_eq!(result.result_scope, SearchResultScope::Session);
        assert_eq!(result.record_id, target_event_id);
        assert_eq!(result.event_id, Some(target_event_id));
        assert_eq!(result.session_id, Some(session.id));
        assert_eq!(result.provider, Some(CaptureProvider::Codex));
        assert_eq!(
            result.snippet,
            "large-fast-event-needle from one transcript"
        );
        assert_eq!(result.why_matched, vec!["message"]);
        assert!(result.citations.iter().any(|citation| {
            citation.citation_type == ContextCitationType::Event
                && citation.id == target_event_id
                && citation.cursor.as_deref() == Some("line:1024")
        }));

        let event_packet = search_packet(
            &store,
            "large-fast-event-needle",
            &PacketOptions {
                limit: 5,
                snippet_chars: 200,
                result_mode: SearchResultMode::Events,
                ..PacketOptions::default()
            },
        )
        .unwrap();
        assert_eq!(event_packet.results.len(), 1);
        assert_eq!(
            event_packet.results[0].result_scope,
            SearchResultScope::Event
        );
        assert_eq!(event_packet.results[0].event_id, Some(target_event_id));
    }

    #[test]
    fn clustered_fast_search_pages_past_dominant_first_session() {
        let (_temp, store) = test_store();
        let dominant_record = HistoryRecord::new(
            "Dominant matching session",
            "dominant record",
            Vec::new(),
            "agent_history",
            Some("/workspace/ctx".into()),
        );
        store.insert_record(&dominant_record).unwrap();
        let dominant_session = Session {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000701").unwrap(),
            history_record_id: Some(dominant_record.id),
            parent_session_id: None,
            root_session_id: None,
            capture_source_id: None,
            provider: CaptureProvider::Codex,
            external_session_id: Some("dominant-session".into()),
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("primary".into()),
            is_primary: true,
            status: SessionStatus::Imported,
            transcript_blob_id: None,
            started_at: fixed_time(),
            ended_at: None,
            timestamps: timestamps(),
            sync: sync_metadata(),
        };
        store.upsert_session(&dominant_session).unwrap();

        let later_record = HistoryRecord::new(
            "Later matching session",
            "later record",
            Vec::new(),
            "agent_history",
            Some("/workspace/ctx".into()),
        );
        store.insert_record(&later_record).unwrap();
        let later_session = Session {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000702").unwrap(),
            history_record_id: Some(later_record.id),
            parent_session_id: None,
            root_session_id: None,
            capture_source_id: None,
            provider: CaptureProvider::Codex,
            external_session_id: Some("later-session".into()),
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("primary".into()),
            is_primary: true,
            status: SessionStatus::Imported,
            transcript_blob_id: None,
            started_at: fixed_time(),
            ended_at: None,
            timestamps: timestamps(),
            sync: sync_metadata(),
        };
        store.upsert_session(&later_session).unwrap();

        for index in 0..=(LARGE_EVENT_CORPUS_THRESHOLD as u64) {
            let (record_id, session_id, text, occurred_at) = if index < 600 {
                (
                    dominant_record.id,
                    dominant_session.id,
                    "cluster-paging-needle dominant hit",
                    fixed_time() + chrono::Duration::milliseconds(2_000 - index as i64),
                )
            } else if index == 600 {
                (
                    later_record.id,
                    later_session.id,
                    "cluster-paging-needle later hit",
                    fixed_time(),
                )
            } else {
                (
                    dominant_record.id,
                    dominant_session.id,
                    "ordinary large history event",
                    fixed_time() - chrono::Duration::milliseconds(index as i64),
                )
            };
            store
                .upsert_event(&Event {
                    id: Uuid::parse_str(&format!("018f45d0-0000-7000-8000-0000001{index:05x}"))
                        .unwrap(),
                    seq: 20_000 + index,
                    history_record_id: Some(record_id),
                    session_id: Some(session_id),
                    run_id: None,
                    event_type: EventType::Message,
                    role: Some(EventRole::Assistant),
                    occurred_at,
                    capture_source_id: None,
                    payload: serde_json::json!({
                        "cursor": format!("line:{index}"),
                        "body": { "text": text }
                    }),
                    payload_blob_id: None,
                    dedupe_key: Some(format!("clustered-paging-{index}")),
                    redaction_state: RedactionState::SafePreview,
                    sync: sync_metadata(),
                })
                .unwrap();
        }
        store.refresh_search_index().unwrap();

        let packet = search_packet(
            &store,
            "cluster-paging-needle",
            &PacketOptions {
                limit: 2,
                snippet_chars: 200,
                ..PacketOptions::default()
            },
        )
        .unwrap();
        let sessions = packet
            .results
            .iter()
            .filter_map(|result| result.session_id)
            .collect::<BTreeSet<_>>();
        assert_eq!(packet.results.len(), 2);
        assert!(sessions.contains(&dominant_session.id));
        assert!(sessions.contains(&later_session.id));
        assert!(!packet.truncation.truncated);
    }

    #[test]
    fn search_filters_and_citations_expose_source_metadata() {
        let (_temp, store) = test_store();
        let record = HistoryRecord::new(
            "Source-backed session",
            "ordinary body",
            Vec::new(),
            "session",
            Some("/workspace/ctx".into()),
        );
        store.insert_record(&record).unwrap();

        let source_id = Uuid::parse_str("018f45d0-0000-7000-8000-000000000401").unwrap();
        let source = CaptureSource {
            id: source_id,
            descriptor: CaptureSourceDescriptor {
                kind: CaptureSourceKind::ProviderImport,
                provider: CaptureProvider::Codex,
                machine_id: "machine-1".into(),
                process_id: None,
                cwd: Some("/workspace/ctx".into()),
                raw_source_path: Some("/definitely/missing/source-filter.jsonl".into()),
                external_session_id: Some("source-filter-session".into()),
            },
            started_at: fixed_time(),
            ended_at: None,
            sync: SyncMetadata {
                metadata: serde_json::json!({
                    "source_format": "codex_session_jsonl",
                    "cursor": {
                        "after": {
                            "stream": "provider:codex:codex_session_jsonl",
                            "cursor": "line:8",
                            "observed_at": "2026-06-23T12:00:00Z"
                        }
                    }
                }),
                ..sync_metadata()
            },
        };
        store.upsert_capture_source(&source).unwrap();

        let session = Session {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000402").unwrap(),
            history_record_id: Some(record.id),
            parent_session_id: None,
            root_session_id: None,
            capture_source_id: Some(source_id),
            provider: CaptureProvider::Codex,
            external_session_id: Some("source-filter-session".into()),
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("primary".into()),
            is_primary: true,
            status: SessionStatus::Imported,
            transcript_blob_id: None,
            started_at: fixed_time(),
            ended_at: None,
            timestamps: timestamps(),
            sync: sync_metadata(),
        };
        store.upsert_session(&session).unwrap();

        let event = Event {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000403").unwrap(),
            seq: 401,
            history_record_id: Some(record.id),
            session_id: Some(session.id),
            run_id: None,
            event_type: EventType::ToolCall,
            role: Some(EventRole::Assistant),
            occurred_at: fixed_time(),
            capture_source_id: Some(source_id),
            payload: serde_json::json!({
                "cursor": "line:8",
                "body": {
                    "tool": "shell",
                    "name": "exec_command",
                    "arguments_preview": "source-filter-needle"
                }
            }),
            payload_blob_id: None,
            dedupe_key: Some("source-filter-event".into()),
            redaction_state: RedactionState::SafePreview,
            sync: sync_metadata(),
        };
        store.upsert_event(&event).unwrap();

        let file = FileTouched {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000404").unwrap(),
            history_record_id: Some(record.id),
            run_id: None,
            event_id: Some(event.id),
            vcs_workspace_id: None,
            path: "crates/search/src/source_filter.rs".into(),
            change_kind: Some(FileChangeKind::Modified),
            old_path: None,
            line_count_delta: Some(1),
            confidence: Confidence::Explicit,
            timestamps: timestamps(),
            source_id: Some(source_id),
            sync: sync_metadata(),
        };
        store.upsert_file_touched(&file).unwrap();
        store.upsert_record(&record).unwrap();

        let packet = search_packet(
            &store,
            "source-filter-needle",
            &PacketOptions {
                limit: 10,
                filters: SearchFilters {
                    provider: Some(CaptureProvider::Codex),
                    repo: Some("ctx".into()),
                    event_type: Some(EventType::ToolCall),
                    file: Some("source_filter.rs".into()),
                    ..SearchFilters::default()
                },
                ..PacketOptions::default()
            },
        )
        .unwrap();

        assert_eq!(packet.results.len(), 1);
        let result = &packet.results[0];
        assert_eq!(result.provider, Some(CaptureProvider::Codex));
        assert_eq!(result.session_id, Some(session.id));
        assert_eq!(result.event_id, Some(event.id));
        assert_eq!(result.event_seq, Some(401));
        assert_eq!(
            result.raw_source_path.as_deref(),
            source.descriptor.raw_source_path.as_deref()
        );
        assert_eq!(result.raw_source_exists, Some(false));
        assert_eq!(result.cursor.as_deref(), Some("line:8"));
        assert!(result.citations.iter().any(|citation| {
            citation.citation_type == ContextCitationType::Event
                && citation.raw_source_path.as_deref()
                    == source.descriptor.raw_source_path.as_deref()
                && citation.raw_source_exists == Some(false)
                && citation.cursor.as_deref() == Some("line:8")
        }));

        let file_only = search_packet(
            &store,
            "",
            &PacketOptions {
                limit: 10,
                filters: SearchFilters {
                    provider: Some(CaptureProvider::Codex),
                    file: Some("source_filter.rs".into()),
                    ..SearchFilters::default()
                },
                ..PacketOptions::default()
            },
        )
        .unwrap();
        assert_eq!(file_only.results.len(), 1);
        assert!(file_only.results[0]
            .why_matched
            .iter()
            .any(|reason| reason == "file_touched"));
        assert!(!file_only.results[0]
            .why_matched
            .iter()
            .any(|reason| reason == "recent_activity"));
        assert!(file_only.results[0].citations.iter().any(|citation| {
            citation.citation_type == ContextCitationType::File && citation.id == file.id
        }));

        let wrong_provider = search_packet(
            &store,
            "source-filter-needle",
            &PacketOptions {
                limit: 10,
                filters: SearchFilters {
                    provider: Some(CaptureProvider::Pi),
                    ..SearchFilters::default()
                },
                ..PacketOptions::default()
            },
        )
        .unwrap();
        assert!(wrong_provider.results.is_empty());
    }

    #[test]
    fn search_filters_custom_history_source_identity() {
        let (_temp, store) = test_store();
        let record = HistoryRecord::new(
            "Custom plugin import",
            "ordinary body",
            Vec::new(),
            "session",
            Some("/workspace/custom".into()),
        );
        store.insert_record(&record).unwrap();

        let source_id = Uuid::parse_str("018f45d0-0000-7000-8000-000000000451").unwrap();
        let source = CaptureSource {
            id: source_id,
            descriptor: CaptureSourceDescriptor {
                kind: CaptureSourceKind::ProviderImport,
                provider: CaptureProvider::Custom,
                machine_id: "machine-1".into(),
                process_id: None,
                cwd: Some("/workspace/custom".into()),
                raw_source_path: Some("/tmp/dorkos-plugin/ctx-history-plugin.json".into()),
                external_session_id: Some("ctx-history-jsonl-v1-session".into()),
            },
            started_at: fixed_time(),
            ended_at: None,
            sync: SyncMetadata {
                metadata: serde_json::json!({
                    "ctx_history_plugin": {
                        "plugin_name": "dorkos",
                        "plugin_source_id": "default",
                        "history_source": "dorkos/default"
                    },
                    "ctx_history_jsonl_v1": {
                        "provider_key": "dorkos",
                        "source_id": "default",
                        "source_format": "dorkos-history-v1"
                    }
                }),
                ..sync_metadata()
            },
        };
        store.upsert_capture_source(&source).unwrap();

        let session = Session {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000452").unwrap(),
            history_record_id: Some(record.id),
            parent_session_id: None,
            root_session_id: None,
            capture_source_id: Some(source_id),
            provider: CaptureProvider::Custom,
            external_session_id: Some("ctx-history-jsonl-v1-session".into()),
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("primary".into()),
            is_primary: true,
            status: SessionStatus::Imported,
            transcript_blob_id: None,
            started_at: fixed_time(),
            ended_at: None,
            timestamps: timestamps(),
            sync: sync_metadata(),
        };
        store.upsert_session(&session).unwrap();

        let event = Event {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000453").unwrap(),
            seq: 451,
            history_record_id: Some(record.id),
            session_id: Some(session.id),
            run_id: None,
            event_type: EventType::Message,
            role: Some(EventRole::Assistant),
            occurred_at: fixed_time(),
            capture_source_id: Some(source_id),
            payload: serde_json::json!({
                "body": {
                    "text": "dorkos-source-filter-needle"
                }
            }),
            payload_blob_id: None,
            dedupe_key: Some("custom-history-source-filter-event".into()),
            redaction_state: RedactionState::SafePreview,
            sync: sync_metadata(),
        };
        store.upsert_event(&event).unwrap();
        store.upsert_record(&record).unwrap();

        let packet = search_packet(
            &store,
            "dorkos-source-filter-needle",
            &PacketOptions {
                limit: 10,
                filters: SearchFilters {
                    provider: Some(CaptureProvider::Custom),
                    history_source: Some("dorkos/default".into()),
                    ..SearchFilters::default()
                },
                ..PacketOptions::default()
            },
        )
        .unwrap();

        assert_eq!(packet.results.len(), 1);
        let result = &packet.results[0];
        assert_eq!(result.provider, Some(CaptureProvider::Custom));
        assert_eq!(result.history_source.as_deref(), Some("dorkos/default"));
        assert_eq!(result.history_source_plugin.as_deref(), Some("dorkos"));
        assert_eq!(result.provider_key.as_deref(), Some("dorkos"));
        assert_eq!(result.source_id.as_deref(), Some("default"));
        assert_eq!(result.source_format.as_deref(), Some("dorkos-history-v1"));

        let provider_source_packet = search_packet(
            &store,
            "dorkos-source-filter-needle",
            &PacketOptions {
                limit: 10,
                filters: SearchFilters {
                    provider: Some(CaptureProvider::Custom),
                    provider_key: Some("dorkos".into()),
                    source_id: Some("default".into()),
                    source_format: Some("dorkos-history-v1".into()),
                    ..SearchFilters::default()
                },
                ..PacketOptions::default()
            },
        )
        .unwrap();
        assert_eq!(provider_source_packet.results.len(), 1);

        let wrong_source = search_packet(
            &store,
            "dorkos-source-filter-needle",
            &PacketOptions {
                limit: 10,
                filters: SearchFilters {
                    provider: Some(CaptureProvider::Custom),
                    history_source: Some("openclaw/default".into()),
                    ..SearchFilters::default()
                },
                ..PacketOptions::default()
            },
        )
        .unwrap();
        assert!(wrong_source.results.is_empty());
    }

    #[test]
    fn fast_event_search_exposes_custom_history_source_identity() {
        let (_temp, store) = test_store();
        let record = HistoryRecord::new(
            "Large custom plugin import",
            "ordinary body",
            Vec::new(),
            "agent_history",
            Some("/workspace/custom".into()),
        );
        store.insert_record(&record).unwrap();

        let source_id = Uuid::parse_str("018f45d0-0000-7000-8000-000000000481").unwrap();
        store
            .upsert_capture_source(&CaptureSource {
                id: source_id,
                descriptor: CaptureSourceDescriptor {
                    kind: CaptureSourceKind::ProviderImport,
                    provider: CaptureProvider::Custom,
                    machine_id: "machine-1".into(),
                    process_id: None,
                    cwd: Some("/workspace/custom".into()),
                    raw_source_path: Some("/tmp/large-dorkos/ctx-history-plugin.json".into()),
                    external_session_id: Some("ctx-history-jsonl-v1-large".into()),
                },
                started_at: fixed_time(),
                ended_at: None,
                sync: SyncMetadata {
                    metadata: serde_json::json!({
                        "source_metadata": {
                            "ctx_history_plugin": {
                                "plugin_name": "dorkos",
                                "plugin_source_id": "default",
                                "history_source": "dorkos/default"
                            },
                            "ctx_history_jsonl_v1": {
                                "provider_key": "dorkos",
                                "source_id": "default",
                                "source_format": "dorkos-history-v1"
                            }
                        }
                    }),
                    ..sync_metadata()
                },
            })
            .unwrap();

        let session = Session {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000482").unwrap(),
            history_record_id: Some(record.id),
            parent_session_id: None,
            root_session_id: None,
            capture_source_id: Some(source_id),
            provider: CaptureProvider::Custom,
            external_session_id: Some("ctx-history-jsonl-v1-large".into()),
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("primary".into()),
            is_primary: true,
            status: SessionStatus::Imported,
            transcript_blob_id: None,
            started_at: fixed_time(),
            ended_at: None,
            timestamps: timestamps(),
            sync: sync_metadata(),
        };
        store.upsert_session(&session).unwrap();

        let target_event_id = Uuid::parse_str("018f45d0-0000-7000-8000-000000000483").unwrap();
        for index in 0..=(LARGE_EVENT_CORPUS_THRESHOLD as u64) {
            let event_id = if index == LARGE_EVENT_CORPUS_THRESHOLD as u64 {
                target_event_id
            } else {
                Uuid::parse_str(&format!("018f45d0-0000-7000-8000-0000002{index:05x}")).unwrap()
            };
            let text = if event_id == target_event_id {
                "large-custom-source-identity-needle"
            } else {
                "ordinary large custom event"
            };
            store
                .upsert_event(&Event {
                    id: event_id,
                    seq: 40_000 + index,
                    history_record_id: Some(record.id),
                    session_id: Some(session.id),
                    run_id: None,
                    event_type: EventType::Message,
                    role: Some(EventRole::Assistant),
                    occurred_at: fixed_time() + chrono::Duration::milliseconds(index as i64),
                    capture_source_id: Some(source_id),
                    payload: serde_json::json!({
                        "body": { "text": text }
                    }),
                    payload_blob_id: None,
                    dedupe_key: Some(format!("large-custom-source-identity-{index}")),
                    redaction_state: RedactionState::SafePreview,
                    sync: sync_metadata(),
                })
                .unwrap();
        }
        store.refresh_search_index().unwrap();

        let packet = search_packet(
            &store,
            "large-custom-source-identity-needle",
            &PacketOptions {
                limit: 5,
                filters: SearchFilters {
                    provider: Some(CaptureProvider::Custom),
                    ..SearchFilters::default()
                },
                ..PacketOptions::default()
            },
        )
        .unwrap();

        assert_eq!(packet.results.len(), 1);
        let result = &packet.results[0];
        assert_eq!(result.event_id, Some(target_event_id));
        assert_eq!(result.history_source.as_deref(), Some("dorkos/default"));
        assert_eq!(result.history_source_plugin.as_deref(), Some("dorkos"));
        assert_eq!(result.provider_key.as_deref(), Some("dorkos"));
        assert_eq!(result.source_id.as_deref(), Some("default"));
        assert_eq!(result.source_format.as_deref(), Some("dorkos-history-v1"));
    }

    #[test]
    fn filtered_search_pages_past_fts_decoys() {
        let (_temp, store) = test_store();
        let query = "overflow-filter-needle";
        let old_time = fixed_time() - chrono::Duration::days(14);
        let mut records = Vec::new();

        for index in 0..501_u16 {
            let mut decoy = HistoryRecord::new(
                "Overflow filter shared title",
                format!("{query} identical body for paging regression"),
                Vec::new(),
                "task",
                None,
            );
            decoy.id = Uuid::parse_str(&format!("018f45d0-0000-7000-8000-{index:012x}")).unwrap();
            decoy.created_at = old_time;
            decoy.updated_at = old_time;
            records.push(decoy);
        }

        let mut target = HistoryRecord::new(
            "Overflow filter shared title",
            format!("{query} identical body for paging regression"),
            Vec::new(),
            "task",
            Some("/workspace/ctx-filter-target".into()),
        );
        target.id = Uuid::parse_str("018f45d0-0000-7000-8000-ffffffffffff").unwrap();
        target.created_at = old_time;
        target.updated_at = fixed_time();
        records.push(target.clone());
        store.upsert_records(&records).unwrap();

        let session = Session {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-fffffffffffe").unwrap(),
            history_record_id: Some(target.id),
            parent_session_id: None,
            root_session_id: None,
            capture_source_id: None,
            provider: CaptureProvider::Codex,
            external_session_id: Some("overflow-filter-session".into()),
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("primary".into()),
            is_primary: true,
            status: SessionStatus::Imported,
            transcript_blob_id: None,
            started_at: fixed_time(),
            ended_at: None,
            timestamps: timestamps(),
            sync: sync_metadata(),
        };
        store.upsert_session(&session).unwrap();

        let file = FileTouched {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-fffffffffffd").unwrap(),
            history_record_id: Some(target.id),
            run_id: None,
            event_id: None,
            vcs_workspace_id: None,
            path: "crates/search/src/overflow_filter.rs".into(),
            change_kind: Some(FileChangeKind::Modified),
            old_path: None,
            line_count_delta: Some(3),
            confidence: Confidence::Explicit,
            timestamps: timestamps(),
            source_id: None,
            sync: sync_metadata(),
        };
        store.upsert_file_touched(&file).unwrap();

        let first_raw_page = store.search_records(query, 500).unwrap();
        assert_eq!(first_raw_page.len(), 500);
        assert!(
            !first_raw_page.iter().any(|record| record.id == target.id),
            "regression setup must place the filtered hit behind the first 500 raw matches"
        );

        let cases = vec![
            (
                "provider",
                SearchFilters {
                    provider: Some(CaptureProvider::Codex),
                    ..SearchFilters::default()
                },
            ),
            (
                "repo",
                SearchFilters {
                    repo: Some("ctx-filter-target".into()),
                    ..SearchFilters::default()
                },
            ),
            (
                "file",
                SearchFilters {
                    file: Some("overflow_filter.rs".into()),
                    ..SearchFilters::default()
                },
            ),
            (
                "since",
                SearchFilters {
                    since: Some(fixed_time() - chrono::Duration::hours(1)),
                    ..SearchFilters::default()
                },
            ),
            (
                "combined",
                SearchFilters {
                    provider: Some(CaptureProvider::Codex),
                    repo: Some("ctx-filter-target".into()),
                    since: Some(fixed_time() - chrono::Duration::hours(1)),
                    file: Some("overflow_filter.rs".into()),
                    ..SearchFilters::default()
                },
            ),
        ];

        for (name, filters) in cases {
            let packet = search_packet(
                &store,
                query,
                &PacketOptions {
                    limit: 1,
                    filters,
                    ..PacketOptions::default()
                },
            )
            .unwrap();

            assert_eq!(
                packet
                    .results
                    .iter()
                    .map(|result| result.record_id)
                    .collect::<Vec<_>>(),
                vec![target.id],
                "{name} filter failed to page past decoys"
            );
        }
    }

    #[test]
    fn file_filter_matches_event_linked_file_touches_on_fast_path() {
        let (_temp, store) = test_store();
        let record = HistoryRecord::new(
            "Event linked file touch",
            "record body without the event needle",
            Vec::new(),
            "task",
            Some("/workspace/ctx".into()),
        );
        store.insert_record(&record).unwrap();

        let session = Session {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-00000000f101").unwrap(),
            history_record_id: Some(record.id),
            parent_session_id: None,
            root_session_id: None,
            capture_source_id: None,
            provider: CaptureProvider::Codex,
            external_session_id: Some("event-linked-file-touch-session".into()),
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("primary".into()),
            is_primary: true,
            status: SessionStatus::Imported,
            transcript_blob_id: None,
            started_at: fixed_time(),
            ended_at: None,
            timestamps: timestamps(),
            sync: sync_metadata(),
        };
        store.upsert_session(&session).unwrap();

        let event = Event {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-00000000f102").unwrap(),
            seq: 7,
            history_record_id: None,
            session_id: Some(session.id),
            run_id: None,
            event_type: EventType::ToolCall,
            role: Some(EventRole::Assistant),
            occurred_at: fixed_time(),
            capture_source_id: None,
            payload: serde_json::json!({"text": "event-file-scope-needle apply patch"}),
            payload_blob_id: None,
            dedupe_key: None,
            redaction_state: RedactionState::SafePreview,
            sync: sync_metadata(),
        };
        store.upsert_event(&event).unwrap();
        for index in 0..(LARGE_EVENT_CORPUS_THRESHOLD - 1) {
            let decoy = Event {
                id: Uuid::parse_str(&format!("018f45d0-0000-7000-8000-00000001{index:04x}"))
                    .unwrap(),
                seq: 1000 + index as u64,
                history_record_id: None,
                session_id: Some(session.id),
                run_id: None,
                event_type: EventType::Message,
                role: Some(EventRole::Assistant),
                occurred_at: fixed_time() + chrono::Duration::milliseconds(index),
                capture_source_id: None,
                payload: serde_json::json!({"text": format!("decoy event {index}")}),
                payload_blob_id: None,
                dedupe_key: None,
                redaction_state: RedactionState::SafePreview,
                sync: sync_metadata(),
            };
            store.upsert_event(&decoy).unwrap();
        }

        store
            .upsert_file_touched(&FileTouched {
                id: Uuid::parse_str("018f45d0-0000-7000-8000-00000000f103").unwrap(),
                history_record_id: None,
                run_id: None,
                event_id: Some(event.id),
                vcs_workspace_id: None,
                path: "crates/ctx-cli/src/main.rs".into(),
                change_kind: Some(FileChangeKind::Modified),
                old_path: None,
                line_count_delta: None,
                confidence: Confidence::Explicit,
                timestamps: timestamps(),
                source_id: None,
                sync: sync_metadata(),
            })
            .unwrap();

        let packet = search_packet(
            &store,
            "event-file-scope-needle",
            &PacketOptions {
                limit: 5,
                filters: SearchFilters {
                    file: Some("src/main.rs".into()),
                    ..SearchFilters::default()
                },
                ..PacketOptions::default()
            },
        )
        .unwrap();

        assert_eq!(packet.results.len(), 1);
        assert_eq!(packet.results[0].event_id, Some(event.id));
        assert_eq!(packet.results[0].result_scope, SearchResultScope::Session);

        let wrong_file = search_packet(
            &store,
            "event-file-scope-needle",
            &PacketOptions {
                limit: 5,
                filters: SearchFilters {
                    file: Some("src/lib.rs".into()),
                    ..SearchFilters::default()
                },
                ..PacketOptions::default()
            },
        )
        .unwrap();
        assert!(wrong_file.results.is_empty());
    }

    #[test]
    fn file_filter_treats_like_wildcards_as_literal_path_characters() {
        let (_temp, store) = test_store();
        let record = HistoryRecord::new(
            "Literal file wildcard test",
            "literal-file-wildcard-needle",
            Vec::new(),
            "task",
            Some("/workspace/ctx".into()),
        );
        store.insert_record(&record).unwrap();
        store
            .upsert_file_touched(&FileTouched {
                id: Uuid::parse_str("018f45d0-0000-7000-8000-00000000f203").unwrap(),
                history_record_id: Some(record.id),
                run_id: None,
                event_id: None,
                vcs_workspace_id: None,
                path: "src/fooXbar.rs".into(),
                change_kind: Some(FileChangeKind::Modified),
                old_path: None,
                line_count_delta: None,
                confidence: Confidence::Explicit,
                timestamps: timestamps(),
                source_id: None,
                sync: sync_metadata(),
            })
            .unwrap();

        let packet = search_packet(
            &store,
            "literal-file-wildcard-needle",
            &PacketOptions {
                limit: 5,
                filters: SearchFilters {
                    file: Some("src/foo_bar.rs".into()),
                    ..SearchFilters::default()
                },
                ..PacketOptions::default()
            },
        )
        .unwrap();

        assert!(packet.results.is_empty());
    }

    #[test]
    fn filtered_search_stops_at_scan_budget_when_no_candidates_match() {
        let (_temp, store) = test_store();
        let query = "scan-budget-needle";
        let mut records = Vec::new();
        for index in 0..=(FILTERED_SEARCH_PAGE_SIZE * FILTERED_SEARCH_MAX_PAGES) {
            let mut record = HistoryRecord::new(
                "Scan budget decoy",
                format!("{query} decoy record {index:05}"),
                Vec::new(),
                "task",
                Some("/workspace/no-match".into()),
            );
            record.id = Uuid::parse_str(&format!("018f45d0-0000-7000-8000-{index:012x}")).unwrap();
            record.created_at = fixed_time() - chrono::Duration::seconds(index as i64);
            record.updated_at = record.created_at;
            records.push(record);
        }
        store.upsert_records(&records).unwrap();

        let packet = search_packet(
            &store,
            query,
            &PacketOptions {
                limit: 1,
                filters: SearchFilters {
                    repo: Some("workspace-that-does-not-exist".into()),
                    ..SearchFilters::default()
                },
                ..PacketOptions::default()
            },
        )
        .unwrap();

        assert!(packet.results.is_empty());
        assert!(packet.truncation.truncated);
        assert_eq!(packet.truncation.reason.as_deref(), Some("scan_budget"));
    }

    #[test]
    fn empty_query_filtered_search_returns_empty_without_scanning() {
        let (_temp, store) = test_store();
        let mut records = Vec::new();
        for index in 0..=(FILTERED_SEARCH_PAGE_SIZE * FILTERED_SEARCH_MAX_PAGES) {
            let mut record = HistoryRecord::new(
                "Empty query scan budget decoy",
                format!("empty query decoy record {index:05}"),
                Vec::new(),
                "task",
                Some("/workspace/no-match".into()),
            );
            record.id = Uuid::parse_str(&format!("018f45d0-0000-7000-8001-{index:012x}")).unwrap();
            record.created_at = fixed_time() - chrono::Duration::seconds(index as i64);
            record.updated_at = record.created_at;
            records.push(record);
        }
        store.upsert_records(&records).unwrap();

        let packet = search_packet(
            &store,
            "",
            &PacketOptions {
                limit: 1,
                filters: SearchFilters {
                    repo: Some("workspace-that-does-not-exist".into()),
                    ..SearchFilters::default()
                },
                ..PacketOptions::default()
            },
        )
        .unwrap();

        assert!(packet.results.is_empty());
        assert!(!packet.truncation.truncated);
        assert_eq!(packet.truncation.reason.as_deref(), None);
    }

    #[test]
    fn no_token_query_returns_empty_without_recent_activity() {
        let (_temp, store) = test_store();
        let record = HistoryRecord::new(
            "No-token query decoy",
            "This record should not be returned for punctuation-only search.",
            Vec::new(),
            "task",
            Some("/workspace/punctuation".into()),
        );
        store.upsert_record(&record).unwrap();

        for query in ["!!!", "---", "___"] {
            let packet =
                search_packet(&store, query, &PacketOptions::default()).expect("search packet");

            assert!(packet.results.is_empty(), "{query}");
            assert!(!packet.truncation.truncated, "{query}");
        }
    }

    #[test]
    fn search_result_limit_is_capped() {
        let (_temp, store) = test_store();
        let query = "limit-cap-needle";
        let mut records = Vec::new();
        for index in 0..250_usize {
            let mut record = HistoryRecord::new(
                "Limit cap candidate",
                format!("{query} candidate {index:03}"),
                Vec::new(),
                "task",
                Some("/workspace/limit-cap".into()),
            );
            record.id = Uuid::parse_str(&format!("018f45d0-0000-7000-8002-{index:012x}")).unwrap();
            record.created_at = fixed_time() - chrono::Duration::seconds(index as i64);
            record.updated_at = fixed_time() - chrono::Duration::seconds(index as i64);
            records.push(record);
        }
        store.upsert_records(&records).unwrap();

        let packet = search_packet(
            &store,
            query,
            &PacketOptions {
                limit: usize::MAX,
                ..PacketOptions::default()
            },
        )
        .unwrap();

        assert_eq!(packet.results.len(), MAX_RESULT_LIMIT);
        assert!(packet.truncation.truncated);
        assert_eq!(packet.truncation.reason.as_deref(), Some("limit"));
    }

    #[test]
    fn filtered_search_scores_full_fetched_page_before_limiting() {
        let (_temp, store) = test_store();
        let query = "samepagerankneedle";
        let workspace = Some("/workspace/same-page-rank".to_owned());
        let mut records = Vec::new();

        for (index, id) in [
            "018f45d0-0000-7000-8000-000000000101",
            "018f45d0-0000-7000-8000-000000000102",
            "018f45d0-0000-7000-8000-000000000103",
        ]
        .into_iter()
        .enumerate()
        {
            let mut record = HistoryRecord::new(
                "Same page filtered candidate",
                format!("{query} identical body for same page ranking"),
                Vec::new(),
                "task",
                workspace.clone(),
            );
            record.id = Uuid::parse_str(id).unwrap();
            record.created_at = fixed_time();
            record.updated_at = fixed_time() + chrono::Duration::seconds(index as i64);
            records.push(record);
        }

        let expected_best_id = records[2].id;
        store.upsert_records(&records).unwrap();

        let late_file_match = FileTouched {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000104").unwrap(),
            history_record_id: Some(expected_best_id),
            run_id: None,
            event_id: None,
            vcs_workspace_id: None,
            path: "crates/search/src/samepagerankneedle.rs".into(),
            change_kind: Some(FileChangeKind::Modified),
            old_path: None,
            line_count_delta: Some(1),
            confidence: Confidence::Explicit,
            timestamps: timestamps(),
            source_id: None,
            sync: sync_metadata(),
        };
        store.upsert_file_touched(&late_file_match).unwrap();

        let raw_page = store.search_records(query, 3).unwrap();
        assert_eq!(
            raw_page.iter().map(|record| record.id).collect::<Vec<_>>(),
            records.iter().map(|record| record.id).collect::<Vec<_>>(),
            "regression setup must put the best filtered hit after the first limit+1 raw matches"
        );

        let packet = search_packet(
            &store,
            query,
            &PacketOptions {
                limit: 1,
                filters: SearchFilters {
                    repo: Some("same-page-rank".into()),
                    ..SearchFilters::default()
                },
                ..PacketOptions::default()
            },
        )
        .unwrap();

        assert_eq!(
            packet
                .results
                .iter()
                .map(|result| result.record_id)
                .collect::<Vec<_>>(),
            vec![expected_best_id]
        );
        assert!(packet.results[0]
            .why_matched
            .iter()
            .any(|reason| reason == "file_touched"));
    }

    fn new_link_id(target_id: Uuid) -> Uuid {
        let mut bytes = *target_id.as_bytes();
        bytes[15] = bytes[15].wrapping_add(80);
        Uuid::from_bytes(bytes)
    }

    fn deterministic_tie_record(id: &str) -> HistoryRecord {
        let mut record = HistoryRecord::new(
            "Stable tie title",
            "stabletie exact equal body for deterministic ranking",
            vec!["stabletie".into()],
            "task",
            None,
        );
        record.id = Uuid::parse_str(id).unwrap();
        record.created_at = fixed_time();
        record.updated_at = fixed_time();
        record
    }

    fn packet_without_generated_at<T: Serialize>(packet: &T) -> serde_json::Value {
        let mut value = serde_json::to_value(packet).unwrap();
        value.as_object_mut().unwrap().remove("generated_at");
        value
    }

    #[test]
    fn search_packet_terms_merges_broad_queries_without_requiring_all_terms() {
        let (_temp, store) = test_store();
        for (id, title, body) in [
            (
                "018f45d0-0000-7000-8000-000000020001",
                "Signed metadata release",
                "signed metadata verification and trusted release manifests",
            ),
            (
                "018f45d0-0000-7000-8000-000000020002",
                "Buildkite worker setup",
                "buildkite pipeline worker provisioning and release queue setup",
            ),
        ] {
            let mut record = HistoryRecord::new(title, body, Vec::new(), "task", None);
            record.id = Uuid::parse_str(id).unwrap();
            record.created_at = fixed_time();
            record.updated_at = fixed_time();
            store.insert_record(&record).unwrap();
        }
        let options = PacketOptions {
            limit: 10,
            snippet_chars: 160,
            ..PacketOptions::default()
        };

        let exact = search_packet(&store, "signed metadata buildkite", &options).unwrap();
        assert_eq!(exact.results.len(), 0);

        let broad = search_packet_terms(
            &store,
            "signed metadata",
            &[String::from("buildkite")],
            &options,
        )
        .unwrap();
        let titles = broad
            .results
            .iter()
            .map(|result| result.title.as_str())
            .collect::<Vec<_>>();
        assert!(titles.contains(&"Signed metadata release"));
        assert!(titles.contains(&"Buildkite worker setup"));
        assert_eq!(broad.query, "signed metadata OR buildkite");
    }

    fn maybe_write_synthetic_search_smoke_artifact() {
        let Ok(out_dir) = std::env::var("CTX_ARTIFACT_DIR") else {
            return;
        };

        let (_temp, store) = test_store();
        let mut records = Vec::new();
        for index in 0..48 {
            let mut record = HistoryRecord::new(
                format!("Synthetic search smoke {index:03}"),
                format!(
                    "syntheticneedle generated body {index:03} {}",
                    "detail ".repeat(12)
                ),
                vec!["synthetic".into(), "smoke".into()],
                "task",
                Some("/workspace/ctx".into()),
            );
            record.id =
                Uuid::parse_str(&format!("018f45d0-0000-7000-8000-00000002{index:04x}")).unwrap();
            record.created_at = fixed_time() + chrono::Duration::seconds(index);
            record.updated_at = record.created_at;
            records.push(record);
        }

        let import_started = std::time::Instant::now();
        store.upsert_records(&records).unwrap();
        let import_elapsed = import_started.elapsed();

        let options = PacketOptions {
            limit: 12,
            snippet_chars: 180,
            filters: SearchFilters::default(),
            result_mode: SearchResultMode::Sessions,
        };
        let search_started = std::time::Instant::now();
        let search = search_packet(&store, "syntheticneedle", &options).unwrap();
        let search_elapsed = search_started.elapsed();

        let import_secs = import_elapsed.as_secs_f64();
        let artifact = serde_json::json!({
            "schema_version": 1,
            "profile": "smoke",
            "corpus": {
                "records": records.len(),
                "events": records.len()
            },
            "import": {
                "duration_ms": import_elapsed.as_millis(),
                "events_per_sec": if import_secs > 0.0 {
                    records.len() as f64 / import_secs
                } else {
                    records.len() as f64
                }
            },
            "storage": {
                "db_bytes": std::fs::metadata(store.path()).map(|metadata| metadata.len()).unwrap_or(0)
            },
            "search": {
                "duration_ms": search_elapsed.as_millis(),
                "result_count": search.results.len(),
                "citation_count": search.results.iter().map(|result| result.citations.len()).sum::<usize>(),
                "truncation": search.truncation
            }
        });

        let out_dir = std::path::Path::new(&out_dir);
        std::fs::create_dir_all(out_dir).unwrap();
        std::fs::write(
            out_dir.join("synthetic-search-smoke.json"),
            serde_json::to_vec_pretty(&artifact).unwrap(),
        )
        .unwrap();
    }

    #[test]
    #[ignore = "manual perf benchmark; private release gates run scripts/public-ctx/perf-smoke.sh from ctx-private"]
    fn synthetic_search_perf_records_thresholded_evidence() {
        let out_dir = std::env::var_os("CTX_ARTIFACT_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .ancestors()
                    .nth(2)
                    .unwrap()
                    .join("target/ctx-artifacts/synthetic_search_perf")
            });
        std::fs::create_dir_all(&out_dir).unwrap();
        let artifact_path = out_dir.join("synthetic-search-perf.json");

        let event_count = perf_event_count();
        let events_per_record = perf_events_per_record();
        let search_repeats = perf_repeats("CTX_SEARCH_PERF_SEARCH_REPEATS", 9);
        let filtered_search_repeats = perf_repeats("CTX_SEARCH_PERF_FILTERED_SEARCH_REPEATS", 5);
        let thresholds = perf_thresholds(event_count);

        let generation_started = std::time::Instant::now();
        let archive = synthetic_perf_archive(event_count, events_per_record);
        let generation_ms = elapsed_ms(generation_started.elapsed());
        let corpus = PerfCorpus {
            records: archive.records.len(),
            capture_sources: archive.capture_sources.len(),
            sessions: archive.sessions.len(),
            runs: archive.runs.len(),
            events: archive.events.len(),
            summaries: archive.summaries.len(),
            files_touched: archive.files_touched.len(),
        };

        let (_temp, mut store) = test_store();
        let import_started = std::time::Instant::now();
        store.import_archive(&archive, false).unwrap();
        let import_ms = elapsed_ms(import_started.elapsed());
        let import_secs = (import_ms / 1000.0).max(0.001);
        let import_events_per_sec = corpus.events as f64 / import_secs;

        let search_options = PacketOptions {
            limit: 24,
            snippet_chars: 320,
            filters: SearchFilters::default(),
            result_mode: SearchResultMode::Sessions,
        };
        let filtered_search_options = PacketOptions {
            limit: 24,
            snippet_chars: 320,
            filters: SearchFilters {
                provider: Some(CaptureProvider::Codex),
                repo: Some("ctx".into()),
                event_type: Some(EventType::ToolCall),
                file: Some("perf_profile.rs".into()),
                ..SearchFilters::default()
            },
            result_mode: SearchResultMode::Sessions,
        };

        let search_warmup = search_packet(&store, "perfneedle", &search_options).unwrap();
        assert_perf_results("search warmup", search_warmup.results.len());
        let filtered_search_warmup =
            search_packet(&store, "perfneedle", &filtered_search_options).unwrap();
        assert_perf_results(
            "filtered search warmup",
            filtered_search_warmup.results.len(),
        );

        let mut search_samples = Vec::new();
        let mut last_search_results = 0;
        let mut last_search_citations = 0;
        for _ in 0..search_repeats {
            let started = std::time::Instant::now();
            let packet = search_packet(&store, "perfneedle", &search_options).unwrap();
            let elapsed = elapsed_ms(started.elapsed());
            assert_perf_results("search sample", packet.results.len());
            last_search_results = packet.results.len();
            last_search_citations = packet
                .results
                .iter()
                .map(|result| result.citations.len())
                .sum();
            search_samples.push(elapsed);
        }

        let mut filtered_search_samples = Vec::new();
        let mut last_filtered_search_results = 0;
        let mut last_filtered_search_citations = 0;
        for _ in 0..filtered_search_repeats {
            let started = std::time::Instant::now();
            let packet = search_packet(&store, "perfneedle", &filtered_search_options).unwrap();
            let elapsed = elapsed_ms(started.elapsed());
            assert_perf_results("filtered search sample", packet.results.len());
            last_filtered_search_results = packet.results.len();
            last_filtered_search_citations = packet
                .results
                .iter()
                .map(|result| result.citations.len())
                .sum();
            filtered_search_samples.push(elapsed);
        }

        let db_path = store.path().to_path_buf();
        drop(store);
        let db_bytes = sqlite_footprint_bytes(&db_path);
        let main_db_bytes = std::fs::metadata(&db_path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);

        let import_stats = timing_stats(&[import_ms]);
        let search_stats = timing_stats(&search_samples);
        let filtered_search_stats = timing_stats(&filtered_search_samples);
        let max_db_bytes = thresholds.max_db_bytes_per_event * corpus.events as u64;
        let checks = vec![
            serde_json::json!({
                "name": "corpus_events_at_least_10000",
                "passed": corpus.events >= 10_000,
                "actual": corpus.events,
                "threshold": 10_000
            }),
            serde_json::json!({
                "name": "import_events_per_sec",
                "passed": import_events_per_sec >= thresholds.import_min_events_per_sec,
                "actual": rounded(import_events_per_sec),
                "threshold": thresholds.import_min_events_per_sec
            }),
            serde_json::json!({
                "name": "search_p95_ms",
                "passed": search_stats.p95_ms <= thresholds.search_p95_ms,
                "actual": search_stats.p95_ms,
                "threshold": thresholds.search_p95_ms
            }),
            serde_json::json!({
                "name": "filtered_search_p95_ms",
                "passed": filtered_search_stats.p95_ms <= thresholds.filtered_search_p95_ms,
                "actual": filtered_search_stats.p95_ms,
                "threshold": thresholds.filtered_search_p95_ms
            }),
            serde_json::json!({
                "name": "db_footprint_bytes",
                "passed": db_bytes <= max_db_bytes,
                "actual": db_bytes,
                "threshold": max_db_bytes
            }),
        ];
        let passed = checks
            .iter()
            .all(|check| check["passed"].as_bool().unwrap_or(false));

        let artifact = serde_json::json!({
            "schema_version": 1,
            "profile": "synthetic-search-perf",
            "mode": if event_count >= 100_000 { "slow" } else { "standard" },
            "status": if passed { "passed" } else { "failed" },
            "corpus": {
                "records": corpus.records,
                "capture_sources": corpus.capture_sources,
                "sessions": corpus.sessions,
                "runs": corpus.runs,
                "events": corpus.events,
                "summaries": corpus.summaries,
                "files_touched": corpus.files_touched,
                "events_per_record": events_per_record,
                "query": "perfneedle"
            },
            "thresholds": {
                "import_min_events_per_sec": thresholds.import_min_events_per_sec,
                "search_p95_ms": thresholds.search_p95_ms,
                "filtered_search_p95_ms": thresholds.filtered_search_p95_ms,
                "max_db_bytes_per_event": thresholds.max_db_bytes_per_event,
                "env_overrides": [
                    "CTX_SEARCH_PERF_IMPORT_MIN_EVENTS_PER_SEC",
                    "CTX_SEARCH_PERF_SEARCH_P95_MS",
                    "CTX_SEARCH_PERF_FILTERED_SEARCH_P95_MS",
                    "CTX_SEARCH_PERF_MAX_DB_BYTES_PER_EVENT"
                ]
            },
            "profiles": {
                "generation": {
                    "duration_ms": generation_ms
                },
                "import": {
                    "timings": import_stats.to_json(),
                    "events_per_sec": rounded(import_events_per_sec)
                },
                "search": {
                    "timings": search_stats.to_json(),
                    "result_count": last_search_results,
                    "citation_count": last_search_citations,
                    "repeats": search_repeats
                },
                "filtered_search": {
                    "timings": filtered_search_stats.to_json(),
                    "result_count": last_filtered_search_results,
                    "citation_count": last_filtered_search_citations,
                    "repeats": filtered_search_repeats
                }
            },
            "storage": {
                "main_db_bytes": main_db_bytes,
                "db_footprint_bytes": db_bytes,
                "db_bytes_per_event": rounded(db_bytes as f64 / corpus.events as f64)
            },
            "checks": checks
        });

        std::fs::write(
            &artifact_path,
            serde_json::to_vec_pretty(&artifact).unwrap(),
        )
        .unwrap();
        println!(
            "synthetic search perf artifact: {}",
            artifact_path.display()
        );

        assert!(
            passed,
            "synthetic search perf thresholds failed; see {}",
            artifact_path.display()
        );
    }

    struct PerfCorpus {
        records: usize,
        capture_sources: usize,
        sessions: usize,
        runs: usize,
        events: usize,
        summaries: usize,
        files_touched: usize,
    }

    #[derive(Clone, Copy)]
    struct PerfThresholds {
        import_min_events_per_sec: f64,
        search_p95_ms: f64,
        filtered_search_p95_ms: f64,
        max_db_bytes_per_event: u64,
    }

    struct PerfTimingStats {
        samples_ms: Vec<f64>,
        p50_ms: f64,
        p95_ms: f64,
        min_ms: f64,
        max_ms: f64,
    }

    impl PerfTimingStats {
        fn to_json(&self) -> serde_json::Value {
            serde_json::json!({
                "sample_count": self.samples_ms.len(),
                "samples_ms": self.samples_ms,
                "p50_ms": self.p50_ms,
                "p95_ms": self.p95_ms,
                "min_ms": self.min_ms,
                "max_ms": self.max_ms
            })
        }
    }

    fn synthetic_perf_archive(
        event_count: usize,
        events_per_record: usize,
    ) -> SessionHistoryArchive {
        let mut archive = SessionHistoryArchive::default();
        let record_count = event_count.div_ceil(events_per_record);
        let workspace_id = perf_uuid(0x5000, 0);
        archive.vcs_workspaces.push(VcsWorkspace {
            id: workspace_id,
            kind: VcsKind::Git,
            root_path: "/workspace/ctx".into(),
            repo_fingerprint: "git:ctx-search-perf".into(),
            primary_remote_url_normalized: Some("https://github.com/ctxrs/ctx".into()),
            host: VcsHost::Github,
            owner: Some("ctxrs".into()),
            name: Some("ctx".into()),
            monorepo_subpath: None,
            timestamps: timestamps(),
            source_id: None,
            sync: sync_metadata(),
        });

        for record_index in 0..record_count {
            let record_id = perf_uuid(0x1000, record_index as u64);
            let source_id = perf_uuid(0x1100, record_index as u64);
            let session_id = perf_uuid(0x2000, record_index as u64);
            let run_id = perf_uuid(0x3000, record_index as u64);
            let summary_id = perf_uuid(0x4000, record_index as u64);
            let file_id = perf_uuid(0x4100, record_index as u64);
            let time = fixed_time() + chrono::Duration::seconds(record_index as i64);

            let mut record = HistoryRecord::new(
                format!("Synthetic perf profile {record_index:05}"),
                format!(
                    "perfneedle import search retrieval profile record {record_index:05}; \
                     routing storage ranking citations threshold evidence {}",
                    "detail ".repeat(8)
                ),
                vec![
                    "perf".into(),
                    "synthetic".into(),
                    format!("bucket-{:02}", record_index % 32),
                ],
                "task",
                Some("/workspace/ctx".into()),
            );
            record.id = record_id;
            record.created_at = time;
            record.updated_at = time;
            archive.records.push(record);

            archive.capture_sources.push(CaptureSource {
                id: source_id,
                descriptor: CaptureSourceDescriptor {
                    kind: CaptureSourceKind::ProviderImport,
                    provider: CaptureProvider::Codex,
                    machine_id: "synthetic-perf-host".into(),
                    process_id: None,
                    cwd: Some("/workspace/ctx".into()),
                    raw_source_path: Some(format!(
                        "/workspace/ctx/.ctx/synthetic/perf-session-{record_index:05}.jsonl"
                    )),
                    external_session_id: Some(format!("perf-session-{record_index:05}")),
                },
                started_at: time,
                ended_at: Some(time + chrono::Duration::seconds(events_per_record as i64)),
                sync: SyncMetadata {
                    metadata: serde_json::json!({
                        "source_format": "synthetic_perf_jsonl",
                        "cursor": {
                            "after": {
                                "stream": "provider:codex:synthetic_perf_jsonl",
                                "cursor": format!("line:{}", record_index * events_per_record),
                                "observed_at": time.to_rfc3339()
                            }
                        }
                    }),
                    ..sync_metadata()
                },
            });

            archive.sessions.push(Session {
                id: session_id,
                history_record_id: Some(record_id),
                parent_session_id: None,
                root_session_id: None,
                capture_source_id: Some(source_id),
                provider: CaptureProvider::Codex,
                external_session_id: Some(format!("perf-session-{record_index:05}")),
                external_agent_id: Some(format!("agent-{record_index:05}")),
                agent_type: AgentType::Primary,
                role_hint: Some("implementation-worker".into()),
                is_primary: true,
                status: SessionStatus::Imported,
                transcript_blob_id: None,
                started_at: time,
                ended_at: Some(time + chrono::Duration::seconds(events_per_record as i64)),
                timestamps: EntityTimestamps {
                    created_at: time,
                    updated_at: time,
                },
                sync: sync_metadata(),
            });

            archive.runs.push(Run {
                id: run_id,
                history_record_id: Some(record_id),
                session_id: Some(session_id),
                run_type: RunType::Command,
                status: RunStatus::Succeeded,
                started_at: time,
                ended_at: Some(time + chrono::Duration::seconds(1)),
                exit_code: Some(0),
                cwd: Some("/workspace/ctx".into()),
                command_preview: Some(format!(
                    "ctx search perfneedle --refresh off --limit 5 # synthetic record {record_index:05}"
                )),
                input_blob_id: None,
                output_blob_id: None,
                timestamps: EntityTimestamps {
                    created_at: time,
                    updated_at: time,
                },
                source_id: Some(source_id),
                sync: sync_metadata(),
            });

            archive.summaries.push(Summary {
                id: summary_id,
                history_record_id: Some(record_id),
                session_id: Some(session_id),
                kind: SummaryKind::ImportedProviderSummary,
                model_or_source: Some("synthetic-perf".into()),
                text: format!(
                    "perfneedle summary for import search retrieval record {record_index:05}; \
                     captures commands, files, and citations"
                ),
                citations: Vec::new(),
                timestamps: EntityTimestamps {
                    created_at: time,
                    updated_at: time,
                },
                source_id: Some(source_id),
                sync: sync_metadata(),
            });

            archive.files_touched.push(FileTouched {
                id: file_id,
                history_record_id: Some(record_id),
                run_id: Some(run_id),
                event_id: None,
                vcs_workspace_id: Some(workspace_id),
                path: format!(
                    "crates/perf/profile_{:02}/perf_profile.rs",
                    record_index % 24
                ),
                change_kind: Some(FileChangeKind::Modified),
                old_path: None,
                line_count_delta: Some((record_index % 17) as i64 - 3),
                confidence: Confidence::Explicit,
                timestamps: EntityTimestamps {
                    created_at: time,
                    updated_at: time,
                },
                source_id: Some(source_id),
                sync: sync_metadata(),
            });

            let event_start = record_index * events_per_record;
            let event_end = event_count.min(event_start + events_per_record);
            for event_index in event_start..event_end {
                let local_index = event_index - event_start;
                let event_time = time + chrono::Duration::milliseconds(local_index as i64);
                let event_type = match local_index % 5 {
                    0 => EventType::ToolCall,
                    1 => EventType::ToolOutput,
                    2 => EventType::Message,
                    3 => EventType::CommandOutput,
                    _ => EventType::Notice,
                };
                let role = match event_type {
                    EventType::Message => Some(EventRole::User),
                    EventType::ToolOutput | EventType::CommandOutput => Some(EventRole::Tool),
                    EventType::ToolCall => Some(EventRole::Assistant),
                    _ => Some(EventRole::System),
                };
                let event_id = perf_uuid(0x6000, event_index as u64);
                archive.events.push(Event {
                    id: event_id,
                    seq: (event_index + 1) as u64,
                    history_record_id: Some(record_id),
                    session_id: Some(session_id),
                    run_id: Some(run_id),
                    event_type,
                    role,
                    occurred_at: event_time,
                    capture_source_id: Some(source_id),
                    payload: serde_json::json!({
                        "cursor": format!("line:{}", local_index + 1),
                        "body": {
                            "text": format!(
                                "perfneedle import search retrieval profile record {record_index:05} event {local_index:02} indexed event {event_index:06}"
                            )
                        }
                    }),
                    payload_blob_id: None,
                    dedupe_key: (local_index == 0).then(|| {
                        format!("provider:codex:s{record_index:05}:{local_index}:h{event_index:06}")
                    }),
                    redaction_state: RedactionState::SafePreview,
                    sync: sync_metadata(),
                });
            }
        }

        archive
    }

    fn perf_uuid(namespace: u16, index: u64) -> Uuid {
        Uuid::parse_str(&format!("018f45d0-{namespace:04x}-7000-8000-{index:012x}")).unwrap()
    }

    fn perf_event_count() -> usize {
        let requested = env_usize("CTX_SEARCH_PERF_EVENTS").unwrap_or_else(|| {
            if env_flag("CTX_SEARCH_PERF_SLOW") {
                100_000
            } else {
                10_000
            }
        });
        requested.max(10_000)
    }

    fn perf_events_per_record() -> usize {
        env_usize("CTX_SEARCH_PERF_EVENTS_PER_RECORD")
            .unwrap_or(50)
            .clamp(1, 50)
    }

    fn perf_repeats(name: &str, default: usize) -> usize {
        env_usize(name).unwrap_or(default).clamp(1, 50)
    }

    fn perf_thresholds(event_count: usize) -> PerfThresholds {
        let slow = event_count >= 100_000;
        PerfThresholds {
            import_min_events_per_sec: env_f64("CTX_SEARCH_PERF_IMPORT_MIN_EVENTS_PER_SEC")
                .unwrap_or(if slow { 25.0 } else { 40.0 }),
            search_p95_ms: env_f64("CTX_SEARCH_PERF_SEARCH_P95_MS").unwrap_or(if slow {
                2_500.0
            } else {
                1_500.0
            }),
            filtered_search_p95_ms: env_f64("CTX_SEARCH_PERF_FILTERED_SEARCH_P95_MS")
                .unwrap_or(if slow { 8_000.0 } else { 5_000.0 }),
            max_db_bytes_per_event: env_u64("CTX_SEARCH_PERF_MAX_DB_BYTES_PER_EVENT")
                .unwrap_or(if slow { 10_240 } else { 12_288 }),
        }
    }

    fn env_flag(name: &str) -> bool {
        std::env::var(name).is_ok_and(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on" | "slow"
            )
        })
    }

    fn env_usize(name: &str) -> Option<usize> {
        std::env::var(name).ok()?.parse().ok()
    }

    fn env_u64(name: &str) -> Option<u64> {
        std::env::var(name).ok()?.parse().ok()
    }

    fn env_f64(name: &str) -> Option<f64> {
        std::env::var(name).ok()?.parse().ok()
    }

    fn assert_perf_results(label: &str, result_count: usize) {
        assert!(result_count > 0, "{label} returned no results");
    }

    fn elapsed_ms(duration: std::time::Duration) -> f64 {
        rounded(duration.as_secs_f64() * 1000.0)
    }

    fn timing_stats(samples: &[f64]) -> PerfTimingStats {
        assert!(!samples.is_empty(), "perf timing samples must not be empty");
        let mut sorted = samples.to_vec();
        sorted.sort_by(|left, right| left.total_cmp(right));
        PerfTimingStats {
            samples_ms: samples.iter().copied().map(rounded).collect(),
            p50_ms: percentile_sorted(&sorted, 50.0),
            p95_ms: percentile_sorted(&sorted, 95.0),
            min_ms: rounded(*sorted.first().unwrap()),
            max_ms: rounded(*sorted.last().unwrap()),
        }
    }

    fn percentile_sorted(sorted: &[f64], percentile: f64) -> f64 {
        let rank = ((percentile / 100.0) * (sorted.len().saturating_sub(1) as f64)).ceil();
        rounded(sorted[rank as usize])
    }

    fn rounded(value: f64) -> f64 {
        (value * 1000.0).round() / 1000.0
    }

    fn sqlite_footprint_bytes(path: &Path) -> u64 {
        let main = std::fs::metadata(path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        main + sqlite_sidecar_bytes(path, "-wal") + sqlite_sidecar_bytes(path, "-shm")
    }

    fn sqlite_sidecar_bytes(path: &Path, suffix: &str) -> u64 {
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            return 0;
        };
        let sidecar = path.with_file_name(format!("{file_name}{suffix}"));
        std::fs::metadata(sidecar)
            .map(|metadata| metadata.len())
            .unwrap_or(0)
    }

    #[test]
    fn search_packet_is_deterministic_for_large_history_and_equal_ties_use_record_id() {
        let (_temp, store) = test_store();
        for id in [
            "018f45d0-0000-7000-8000-000000010004",
            "018f45d0-0000-7000-8000-000000010001",
            "018f45d0-0000-7000-8000-000000010003",
            "018f45d0-0000-7000-8000-000000010002",
        ] {
            store.insert_record(&deterministic_tie_record(id)).unwrap();
        }

        let expected_order = vec![
            Uuid::parse_str("018f45d0-0000-7000-8000-000000010001").unwrap(),
            Uuid::parse_str("018f45d0-0000-7000-8000-000000010002").unwrap(),
            Uuid::parse_str("018f45d0-0000-7000-8000-000000010003").unwrap(),
            Uuid::parse_str("018f45d0-0000-7000-8000-000000010004").unwrap(),
        ];
        let options = PacketOptions {
            limit: 10,
            snippet_chars: 160,
            ..PacketOptions::default()
        };

        let first_search = search_packet(&store, "stabletie", &options).unwrap();
        let second_search = search_packet(&store, "stabletie", &options).unwrap();
        assert_eq!(
            first_search
                .results
                .iter()
                .map(|result| result.record_id)
                .collect::<Vec<_>>(),
            expected_order
        );
        assert_eq!(
            packet_without_generated_at(&first_search),
            packet_without_generated_at(&second_search)
        );
    }
}
