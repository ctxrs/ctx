#[allow(unused_imports)]
use super::*;

#[derive(Debug, Clone)]
pub(crate) struct AstrBotConversationRow {
    pub(crate) row_id: i64,
    pub(crate) inner_conversation_id: Option<String>,
    pub(crate) conversation_id: String,
    pub(crate) platform_id: Option<String>,
    pub(crate) user_id: Option<String>,
    pub(crate) content: String,
    pub(crate) title: Option<String>,
    pub(crate) persona_id: Option<String>,
    pub(crate) token_usage: Option<String>,
    pub(crate) created_at: Option<i64>,
    pub(crate) updated_at: Option<i64>,
}

#[derive(Debug, Clone)]
pub(crate) struct AstrBotPlatformMessageRow {
    pub(crate) id: i64,
    pub(crate) platform_id: Option<String>,
    pub(crate) user_id: Option<String>,
    pub(crate) sender_id: Option<String>,
    pub(crate) sender_name: Option<String>,
    pub(crate) content: Option<String>,
    pub(crate) llm_checkpoint_id: Option<String>,
    pub(crate) created_at: Option<i64>,
}
