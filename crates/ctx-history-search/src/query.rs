use chrono::Utc;
use ctx_history_core::EventType;
use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

use crate::snippets::non_blank;

pub const DEFAULT_RESULT_LIMIT: usize = 10;
pub const MAX_RESULT_LIMIT: usize = 200;
pub const DEFAULT_SNIPPET_CHARS: usize = 320;
pub(crate) const LARGE_EVENT_CORPUS_THRESHOLD: i64 = 1_024;
pub(crate) const FILTERED_SEARCH_PAGE_SIZE: usize = 500;
pub(crate) const FILTERED_SEARCH_MAX_PAGES: usize = 20;

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

pub(crate) fn normalized_options(options: &PacketOptions) -> PacketOptions {
    PacketOptions {
        limit: options.limit.clamp(1, MAX_RESULT_LIMIT),
        snippet_chars: options.snippet_chars.clamp(32, 2_000),
        filters: options.filters.clone(),
        result_mode: options.result_mode,
    }
}

pub(crate) fn composed_search_terms(query: &str, terms: &[String]) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::<String>::new();
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

pub(crate) fn query_terms(query: &str) -> Vec<String> {
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
