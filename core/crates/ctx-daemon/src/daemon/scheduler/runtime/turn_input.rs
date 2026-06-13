use std::collections::HashMap;

use anyhow::Result;
use serde_json::json;

use ctx_core::models::{Message, Session};
use ctx_providers::adapters::TurnInput;

use super::helpers::{
    load_system_prompt_append_for_relationship, normalize_session_model_id,
    provider_supports_system_prompt_append,
};

pub(super) async fn prepare_turn_input(
    store: &ctx_store::Store,
    session: &Session,
    message: &Message,
    full_model_id: &str,
    provider_env: &mut HashMap<String, String>,
) -> Result<TurnInput> {
    let system_prompt_append =
        load_system_prompt_append_for_relationship(store, session.relationship.as_deref()).await?;
    let mut context_blocks = Vec::new();
    if let Some(append) = system_prompt_append.as_deref() {
        if !provider_supports_system_prompt_append(&session.provider_id) {
            context_blocks.push(json!({"type": "text", "text": append}));
        }
        provider_env.insert("CTX_SYSTEM_PROMPT_APPEND".to_string(), append.to_string());
    }

    Ok(TurnInput {
        content: message.content.clone(),
        attachments: message.attachments.clone(),
        context_blocks,
        model_id: normalize_session_model_id(full_model_id),
    })
}
