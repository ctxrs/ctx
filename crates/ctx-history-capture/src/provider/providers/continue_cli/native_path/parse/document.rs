use super::{event::*, *};

pub(super) fn parse_document(
    root: JsonSpan<'_>,
) -> Result<(RawContinueDocument, Option<JsonSpan<'_>>), String> {
    if root.kind() != JsonKind::Object {
        return Err("Continue session document is not a JSON object".to_owned());
    }
    let mut stats = ContinueOutputExclusionStats::default();
    let mut session_id = None;
    let mut saw_session_id = false;
    let mut session_id_conflict = false;
    let mut title = None;
    let mut created_at = None;
    let mut started_at = None;
    let mut workspace_directory = None;
    let mut mode = None;
    let mut chat_model_title = None;
    let mut usage = None;
    let mut history = None;
    for field in root.as_object().map_err(scan_error)? {
        let (key, value) = field.map_err(scan_error)?;
        if key.is("sessionId") {
            let observed =
                retained_bounded_string(value, MAX_CONTINUE_SESSION_ID_BYTES, &mut stats)?;
            if saw_session_id && session_id != observed {
                session_id_conflict = true;
            } else if !saw_session_id {
                session_id = observed;
            }
            saw_session_id = true;
        } else if key.is("title") {
            title = retained_bounded_string(value, MAX_SESSION_METADATA_STRING_BYTES, &mut stats)?;
        } else if key.is("createdAt") {
            created_at = parse_timestamp(value, &mut stats)?;
        } else if key.is("startedAt") {
            started_at = parse_timestamp(value, &mut stats)?;
        } else if key.is("workspaceDirectory") {
            workspace_directory =
                retained_bounded_string(value, MAX_SESSION_METADATA_STRING_BYTES, &mut stats)?;
        } else if key.is("mode") {
            mode = retained_bounded_string(value, 128, &mut stats)?;
        } else if key.is("chatModelTitle") {
            chat_model_title = retained_bounded_string(value, 512, &mut stats)?;
        } else if key.is("usage") {
            usage = parse_usage(value)?;
        } else if key.is("history") {
            if history.is_some() {
                return Err("Continue document has duplicate history fields".to_owned());
            }
            if value.kind() != JsonKind::Array {
                return Err("Continue history is not a JSON array".to_owned());
            }
            history = Some(value);
        } else if key.is_result_like() {
            record_result(&mut stats, value);
        }
    }
    Ok((
        RawContinueDocument {
            session_id,
            session_id_conflict,
            title,
            created_at,
            started_at,
            workspace_directory,
            mode,
            chat_model_title,
            usage,
            output_exclusion: stats,
        },
        history,
    ))
}

pub(super) fn parse_usage(value: JsonSpan<'_>) -> Result<Option<RawContinueUsage>, String> {
    if value.kind() != JsonKind::Object {
        return Ok(None);
    }
    let mut usage = RawContinueUsage {
        prompt_tokens: None,
        completion_tokens: None,
        total_tokens: None,
    };
    for field in value.as_object().map_err(scan_error)? {
        let (key, value) = field.map_err(scan_error)?;
        if key.is("promptTokens") {
            usage.prompt_tokens = super::super::decode::decode_u64(value);
        } else if key.is("completionTokens") {
            usage.completion_tokens = super::super::decode::decode_u64(value);
        } else if key.is("totalTokens") {
            usage.total_tokens = super::super::decode::decode_u64(value);
        }
    }
    Ok(Some(usage))
}

pub(super) fn parse_history_item(
    item: JsonSpan<'_>,
    stats: &mut ContinueOutputExclusionStats,
) -> Result<Option<RawContinueHistoryItem>, String> {
    let mut admission = TagAdmission::Missing;
    let mut saw_type = false;
    let mut saw_kind = false;
    let mut saw_message = false;
    let mut saw_editor_state = false;
    let mut saw_context_items = false;
    let mut saw_tool_call_states = false;
    let mut saw_conversation_summary = false;
    let mut saw_result_field = false;
    let mut duplicate_body_field = false;
    for field in item.as_object().map_err(scan_error)? {
        let (key, value) = field.map_err(scan_error)?;
        if key.is("type") {
            duplicate_body_field |= saw_type;
            saw_type = true;
            admission = admission.observe(value);
        } else if key.is("kind") {
            duplicate_body_field |= saw_kind;
            saw_kind = true;
            admission = admission.observe(value);
        } else if key.is("message") {
            duplicate_body_field |= saw_message;
            saw_message = true;
        } else if key.is("editorState") {
            duplicate_body_field |= saw_editor_state;
            saw_editor_state = true;
        } else if key.is("contextItems") {
            duplicate_body_field |= saw_context_items;
            saw_context_items = true;
        } else if key.is("toolCallStates") {
            duplicate_body_field |= saw_tool_call_states;
            saw_tool_call_states = true;
        } else if key.is("conversationSummary") {
            duplicate_body_field |= saw_conversation_summary;
            saw_conversation_summary = true;
        } else if key.is_result_like() {
            duplicate_body_field |= saw_result_field;
            saw_result_field = true;
        }
    }
    if admission == TagAdmission::Result {
        record_result(stats, item);
        return Ok(None);
    }
    if duplicate_body_field
        || !matches!(
            admission,
            TagAdmission::Missing | TagAdmission::Text | TagAdmission::Call | TagAdmission::Context
        )
    {
        record_unproven(stats, item);
        return Ok(None);
    }

    let mut id = None;
    let mut timestamp = None;
    let mut created_at = None;
    let mut message = None;
    let mut editor_text = None;
    let mut context_items = Vec::new();
    let mut tool_call_states = Vec::new();
    let mut conversation_summary = None;
    for field in item.as_object().map_err(scan_error)? {
        let (key, value) = field.map_err(scan_error)?;
        if key.is("type") || key.is("kind") {
        } else if key.is("id") {
            id = retained_bounded_string(value, MAX_NATIVE_ITEM_ID_BYTES, stats)?;
        } else if key.is("timestamp") {
            timestamp = parse_timestamp(value, stats)?;
        } else if key.is("createdAt") {
            created_at = parse_timestamp(value, stats)?;
        } else if key.is("message") {
            message = parse_message(value, stats)?;
        } else if key.is("editorState") {
            if value.kind() == JsonKind::String {
                editor_text = retained_unbounded_string(value, stats)?;
            } else if value.kind() != JsonKind::Null {
                record_unproven(stats, value);
            }
        } else if key.is("contextItems") {
            context_items = parse_context_items(value, stats)?;
        } else if key.is("toolCallStates") {
            tool_call_states = parse_tool_call_states(value, stats)?;
        } else if key.is("conversationSummary") {
            if value.kind() == JsonKind::String {
                conversation_summary = retained_unbounded_string(value, stats)?;
            } else if value.kind() != JsonKind::Null {
                record_unproven(stats, value);
            }
        } else if key.is_result_like() {
            record_result(stats, value);
        } else if value.kind() != JsonKind::Null {
            record_unproven(stats, value);
        }
    }
    let retained = message.is_some()
        || editor_text.is_some()
        || !context_items.is_empty()
        || !tool_call_states.is_empty()
        || conversation_summary.is_some();
    Ok(retained.then_some(RawContinueHistoryItem {
        id,
        timestamp,
        created_at,
        message,
        editor_text,
        context_items,
        tool_call_states,
        conversation_summary,
    }))
}

pub(super) fn parse_message(
    value: JsonSpan<'_>,
    stats: &mut ContinueOutputExclusionStats,
) -> Result<Option<RawContinueMessage>, String> {
    if value.kind() != JsonKind::Object {
        if value.kind() != JsonKind::Null {
            record_unproven(stats, value);
        }
        return Ok(None);
    }
    let mut role_admission = RoleAdmission::Missing;
    let mut tag_admission = TagAdmission::Missing;
    let mut saw_role = false;
    let mut saw_type = false;
    let mut saw_kind = false;
    let mut saw_content = false;
    let mut saw_timestamp = false;
    let mut duplicate_field = false;
    let mut saw_result_field = false;
    for field in value.as_object().map_err(scan_error)? {
        let (key, field_value) = field.map_err(scan_error)?;
        if key.is("role") {
            duplicate_field |= saw_role;
            saw_role = true;
            role_admission = role_admission.observe(field_value);
        } else if key.is("type") {
            duplicate_field |= saw_type;
            saw_type = true;
            tag_admission = tag_admission.observe(field_value);
        } else if key.is("kind") {
            duplicate_field |= saw_kind;
            saw_kind = true;
            tag_admission = tag_admission.observe(field_value);
        } else if key.is("content") {
            duplicate_field |= saw_content;
            saw_content = true;
        } else if key.is("timestamp") {
            duplicate_field |= saw_timestamp;
            saw_timestamp = true;
        } else if key.is_result_like() {
            duplicate_field |= saw_result_field;
            saw_result_field = true;
        }
    }
    let role = match role_admission {
        RoleAdmission::Retained(role) => role,
        RoleAdmission::Result => {
            record_result(stats, value);
            return Ok(None);
        }
        RoleAdmission::Missing | RoleAdmission::Unknown | RoleAdmission::Conflict => {
            record_unproven(stats, value);
            return Ok(None);
        }
    };
    if tag_admission == TagAdmission::Result {
        record_result(stats, value);
        return Ok(None);
    }
    if duplicate_field
        || !matches!(
            tag_admission,
            TagAdmission::Missing | TagAdmission::Text | TagAdmission::Call | TagAdmission::Context
        )
    {
        record_unproven(stats, value);
        return Ok(None);
    }

    let mut content = ParsedMessageContent::default();
    let mut timestamp = None;
    for field in value.as_object().map_err(scan_error)? {
        let (key, field_value) = field.map_err(scan_error)?;
        if key.is("role") || key.is("type") || key.is("kind") {
        } else if key.is("content") {
            content = parse_message_content(field_value, stats)?;
        } else if key.is("timestamp") {
            timestamp = parse_timestamp(field_value, stats)?;
        } else if key.is_result_like() {
            record_result(stats, field_value);
        } else if field_value.kind() != JsonKind::Null {
            record_unproven(stats, field_value);
        }
    }
    if (saw_content || saw_result_field) && !content.admitted {
        return Ok(None);
    }
    let role = role.as_str().to_owned();
    record_retained_string(stats, &role);
    Ok(Some(RawContinueMessage {
        role: Some(role),
        content: content.text,
        calls: content.calls,
        timestamp,
    }))
}
