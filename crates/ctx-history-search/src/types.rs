use crate::*;
use serde::Serialize;

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

pub(crate) fn is_default_result_scope(value: &SearchResultScope) -> bool {
    *value == SearchResultScope::Event
}

pub(crate) fn is_zero_usize(value: &usize) -> bool {
    *value == 0
}

pub(crate) fn is_zero_f32(value: &f32) -> bool {
    *value == 0.0
}

#[derive(Debug, Clone)]
pub(crate) struct Candidate {
    pub(crate) record: HistoryRecord,
    pub(crate) context: RecordContext,
    pub(crate) score: f32,
    pub(crate) why_matched: Vec<String>,
    pub(crate) citations: Vec<ContextCitation>,
    pub(crate) primary_hit: Option<HitMetadata>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct RecordContext {
    pub(crate) sessions: Vec<Session>,
    pub(crate) runs: Vec<Run>,
    pub(crate) events: Vec<Event>,
    pub(crate) artifacts: Vec<Artifact>,
    pub(crate) files_touched: Vec<FileTouched>,
    pub(crate) vcs_changes: Vec<VcsChange>,
    pub(crate) summaries: Vec<Summary>,
    pub(crate) sources: BTreeMap<Uuid, ctx_history_core::CaptureSource>,
}

#[derive(Debug, Clone)]
pub(crate) struct SearchSection {
    pub(crate) reason: &'static str,
    pub(crate) weight: f32,
    pub(crate) text: String,
    pub(crate) citation: ContextCitation,
    pub(crate) hit: HitMetadata,
}

#[derive(Debug, Clone)]
pub(crate) struct HitMetadata {
    pub(crate) time: chrono::DateTime<Utc>,
    pub(crate) provider: Option<ctx_history_core::CaptureProvider>,
    pub(crate) provider_session_id: Option<String>,
    pub(crate) history_source: Option<String>,
    pub(crate) history_source_plugin: Option<String>,
    pub(crate) provider_key: Option<String>,
    pub(crate) source_id: Option<String>,
    pub(crate) source_format: Option<String>,
    pub(crate) session_id: Option<Uuid>,
    pub(crate) parent_session_id: Option<Uuid>,
    pub(crate) root_session_id: Option<Uuid>,
    pub(crate) event_id: Option<Uuid>,
    pub(crate) event_seq: Option<u64>,
    pub(crate) cwd: Option<String>,
    pub(crate) raw_source_path: Option<String>,
    pub(crate) raw_source_exists: Option<bool>,
    pub(crate) cursor: Option<String>,
}

pub(crate) struct CandidateSearch {
    pub(crate) candidates: Vec<Candidate>,
    pub(crate) scan_budget_exhausted: bool,
}
