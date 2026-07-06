#[allow(unused_imports)]
use super::*;

#[derive(Debug, Clone)]
pub(crate) struct ForgeCodeConversationRow {
    pub(crate) rowid: i64,
    pub(crate) conversation_id: String,
    pub(crate) title: Option<String>,
    pub(crate) workspace_id: i64,
    pub(crate) context: Option<String>,
    pub(crate) created_at: String,
    pub(crate) updated_at: Option<String>,
    pub(crate) metrics: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ForgeCodeMessageParts<'a> {
    pub(crate) variant: &'static str,
    pub(crate) body: &'a Value,
    pub(crate) usage: Option<&'a Value>,
}
