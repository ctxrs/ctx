use chrono::{DateTime, Utc};
use ctx_history_core::{EventRole, EventType};

use super::source::AuggieFileStamp;

pub(super) const AUGGIE_PARSER_REVISION: &str = "auggie-nativepath-json-v1";
pub(super) const AUGGIE_MAX_DISCOVERED_FILES: usize = 4_096;

pub(super) struct ParsedAuggieSource {
    pub(super) stamp: AuggieFileStamp,
    pub(super) content_digest: [u8; 32],
    pub(super) session: ParsedAuggieSession,
    pub(super) events: Vec<ParsedAuggieEvent>,
}

pub(super) struct ParsedAuggieSession {
    pub(super) provider_session_id: String,
    pub(super) parent_provider_session_id: Option<String>,
    pub(super) root_provider_session_id: Option<String>,
    pub(super) cwd: Option<String>,
    pub(super) raw_source_path: String,
}

pub(super) struct ParsedAuggieEvent {
    pub(super) provider_event_index: u64,
    pub(super) provider_event_hash: String,
    pub(super) event_type: EventType,
    pub(super) role: EventRole,
    pub(super) occurred_at: DateTime<Utc>,
    pub(super) text: String,
    pub(super) chat_index: usize,
    pub(super) message_kind: &'static str,
    pub(super) native_event_id: Option<String>,
    pub(super) json_pointer: String,
}

pub(super) fn saturating_i64(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}
