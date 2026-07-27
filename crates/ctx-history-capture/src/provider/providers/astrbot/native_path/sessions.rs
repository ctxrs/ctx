use super::*;

pub(super) fn conversation_session_fact(row: &ConversationRow) -> SessionFact {
    let started_at = timestamp(row.created_at, DateTime::<Utc>::UNIX_EPOCH);
    SessionFact {
        provider_session_id: provider_session_id(row),
        external_agent_id: row.platform_id.clone(),
        role_hint: "llm-context",
        started_at,
        ended_at: row
            .updated_at
            .map(|value| timestamp(Some(value), started_at)),
        metadata: json!({
            "source_format": ASTRBOT_SQLITE_SOURCE_FORMAT,
            "conversation_id": row.conversation_id,
            "inner_conversation_id": row.inner_conversation_id,
            "platform_id": capped_optional(row.platform_id.as_deref()),
            "user_id": capped_optional(row.user_id.as_deref()),
            "title": capped_optional(row.title.as_deref()),
            "persona_id": capped_optional(row.persona_id.as_deref()),
            "token_usage": row.token_usage.as_deref().map(provider_json_text),
            "fidelity_gap": "The AstrBot importer reads local LLM context plus available platform history from data_v4.db; platform-native chats may still be partial when upstream stores non-LLM replies on the IM platform",
        }),
        preserve_existing: false,
    }
}

pub(super) fn platform_session_fact(
    row: &PlatformMessageRow,
    link: Option<&PlatformMessageLink>,
) -> SessionFact {
    let provider_session_id = link
        .map(|link| link.provider_session_id.clone())
        .unwrap_or_else(|| {
            format!(
                "platform/{}/{}",
                row.platform_id.as_deref().unwrap_or("unknown"),
                row.user_id.as_deref().unwrap_or("unknown")
            )
        });
    let started_at = link
        .and_then(|link| link.parent_created_at)
        .map(|value| timestamp(Some(value), DateTime::<Utc>::UNIX_EPOCH))
        .unwrap_or_else(|| timestamp(row.created_at, DateTime::<Utc>::UNIX_EPOCH));
    SessionFact {
        provider_session_id,
        external_agent_id: row.platform_id.clone(),
        role_hint: if link.is_some() {
            "llm-context"
        } else {
            "platform-history"
        },
        started_at,
        ended_at: None,
        metadata: json!({
            "source_format": ASTRBOT_SQLITE_SOURCE_FORMAT,
            "linked_checkpoint_id": row.llm_checkpoint_id,
            "platform_id": capped_optional(row.platform_id.as_deref()),
            "user_id": capped_optional(row.user_id.as_deref()),
            "fidelity_gap": (!link.is_some()).then_some(
                "platform history row was not linked to a conversations checkpoint"
            ),
        }),
        preserve_existing: link.is_some(),
    }
}

pub(super) fn conversation_items(raw: &str) -> (Vec<Value>, bool) {
    match provider_json_text(raw) {
        Value::Array(items) => (items, true),
        value => (vec![value], false),
    }
}

pub(super) fn finish_conversation_row(frontier: &mut AstrBotFrontier, active: &ActiveConversation) {
    frontier.conversation_after_rowid = Some(active.physical_rowid);
    frontier.conversation_prefix_sha256 =
        chain_hash(frontier.conversation_prefix_sha256, active.row_sha256);
    frontier.last_conversation_order = Some(active.order);
    frontier.conversation_in_row = None;
}

pub(super) fn rejected_conversation(candidate: RowCandidate, detail: &str) -> ActiveConversation {
    let row_sha256 = candidate_hash(b"astrbot-conversation-oversize-v1\0", candidate);
    ActiveConversation {
        physical_rowid: candidate.physical_rowid,
        order: candidate.legacy_order,
        row_sha256,
        row: ConversationRow {
            row_id: candidate.legacy_order.logical_id,
            inner_conversation_id: None,
            conversation_id: format!("oversize-row-{}", candidate.physical_rowid),
            platform_id: None,
            user_id: None,
            content: Value::Null.to_string(),
            title: None,
            persona_id: None,
            token_usage: None,
            created_at: None,
            updated_at: None,
        },
        items: Vec::new(),
        content_is_array: true,
        next_item_index: 0,
        rejection: Some(detail.to_owned()),
    }
}
