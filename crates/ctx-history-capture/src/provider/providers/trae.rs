mod event;
mod json_stream;
pub(crate) mod nativepath;
mod workspace;

use crate::MAX_PROVIDER_JSONL_LINE_BYTES;

pub(crate) use json_stream::{trae_payload_admission, TraePayloadAdmission};

pub(crate) const TRAE_STATE_VSCDB_SOURCE_FORMAT: &str = "trae_state_vscdb";
pub(crate) const TRAE_CN_INPUT_HISTORY_KEY: &str = "icube-ai-agent-storage-input-history";
pub(crate) const TRAE_CHAT_KEYS: &[&str] = &[
    "memento/icube-ai-agent-storage",
    TRAE_CN_INPUT_HISTORY_KEY,
    "chat.ChatSessionStore.index",
    "ChatStore",
    "memento/icube-ai-chat-storage-7467774676505887760",
    "memento/icube-ai-ng-chat-storage-7467774676505887760",
];

pub(crate) const TRAE_SQLITE_VALUE_OVERHEAD_BYTES: u64 = 16 * 64;
pub(crate) const TRAE_CHAT_ROWS_QUERY: &str =
    "select [key], count(*), typeof(value), coalesce(octet_length(value), 0), \
            case when count(*) = 1 \
                       and typeof(value) = 'text' \
                       and octet_length(value) + octet_length([key]) + ?7 <= ?8 \
                 then cast(value as text) end \
     from ItemTable \
     where [key] in (?1, ?2, ?3, ?4, ?5, ?6) \
     group by [key]";

pub(crate) fn trae_sqlite_value_fits_parser_bound(chat_key: &str, retained_bytes: u64) -> bool {
    retained_bytes
        .saturating_add(TRAE_SQLITE_VALUE_OVERHEAD_BYTES)
        .saturating_add(u64::try_from(chat_key.len()).unwrap_or(u64::MAX))
        <= u64::try_from(MAX_PROVIDER_JSONL_LINE_BYTES).unwrap_or(u64::MAX)
}
