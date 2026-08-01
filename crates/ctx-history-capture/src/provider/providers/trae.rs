mod event;
mod json_stream;
pub(crate) mod nativepath;
mod workspace;

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
