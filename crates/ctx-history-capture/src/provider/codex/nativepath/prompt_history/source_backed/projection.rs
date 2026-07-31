use super::*;

pub(super) fn core_record(
    source: &CodexPromptHistorySourceBackedSourceV0,
    native_record: &RetainedPromptRecord,
) -> CodexPromptHistorySourceBackedResultV0<CoreRecord> {
    let session_id = stable_session_id(&source.source, &native_record.line.session_id)?;
    let native_item_key = NativeItemKey::certified_position(
        EVENT_POSITION_KIND,
        TypedKey::U64(native_record.physical_ordinal),
        PositionStability::AppendStable,
    )?;
    let event_id = derive_event_id(EventIdentityInput {
        source: &source.source,
        session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let body = prompt_lexical_body(&native_record.line.text);
    let occurred_at_unix_ms = chrono::DateTime::from_timestamp(native_record.line.ts, 0)
        .map(|value| value.timestamp_millis());
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        session_id,
        source.source.clone(),
        native_record.physical_ordinal,
        "message",
        AgentType::Primary.as_str(),
        true,
        PARSER_REVISION,
        body,
    )?;
    record.provider_session_id = Some(native_record.line.session_id.clone());
    record.native_event_id = Some(TypedKey::U64(native_record.physical_ordinal));
    record.occurred_at_unix_ms = occurred_at_unix_ms;
    record.role = Some("user".to_owned());
    record.validate_contract()?;
    Ok(record)
}

pub(super) fn prompt_lexical_body(text: &str) -> String {
    if text.is_empty() {
        "message".to_owned()
    } else {
        text.to_owned()
    }
}

pub(super) fn stable_session_id(
    source: &SourceKey,
    native_session_id: &str,
) -> CodexPromptHistorySourceBackedResultV0<StableEntityId> {
    let native_session_key =
        NativeSessionKey::native_id(SESSION_KEY_NAMESPACE, TypedKey::utf8(native_session_id)?)?;
    Ok(derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: LOGICAL_SESSION_KIND,
        native_session_key: &native_session_key,
    })?)
}

pub(super) fn retained_record_bytes(record: &CoreRecord) -> usize {
    record
        .content
        .normalized_body
        .as_ref()
        .map_or(0, String::len)
        .saturating_add(record.provider_session_id.as_ref().map_or(0, String::len))
        .saturating_add(512)
}
