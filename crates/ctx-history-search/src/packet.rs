use chrono::Utc;
use ctx_history_core::{
    ContextCitation, ContextLinks, ContextPagination, ContextTruncation, Visibility,
};
use serde::Serialize;
use uuid::Uuid;

use crate::query::{PacketOptions, SearchFilters};

pub const SEARCH_PACKET_SCHEMA_VERSION: u32 = 1;

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

pub(crate) fn empty_search_packet(query: &str, options: &PacketOptions) -> SearchPacket {
    SearchPacket {
        schema_version: SEARCH_PACKET_SCHEMA_VERSION,
        query: query.to_owned(),
        filters: options.filters.clone(),
        generated_at: ctx_history_core::utc_now(),
        results: Vec::new(),
        pagination: pagination(Some(0), false),
        truncation: ContextTruncation::default(),
    }
}

pub(crate) fn pagination(cursor_base: Option<usize>, has_more: bool) -> ContextPagination {
    ContextPagination {
        cursor: if has_more {
            cursor_base.map(|value| format!("offset:{value}"))
        } else {
            None
        },
        has_more,
    }
}
