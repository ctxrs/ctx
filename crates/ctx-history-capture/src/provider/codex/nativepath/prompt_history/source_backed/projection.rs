use super::*;

pub(super) fn lexical_document(
    source: &CodexPromptHistorySourceBackedSourceV0,
    record: &RetainedPromptRecord,
) -> CodexPromptHistorySourceBackedResultV0<LexicalDocument> {
    let session_id = stable_session_id(&source.source, &record.line.session_id)?;
    let native_item_key = NativeItemKey::certified_position(
        EVENT_POSITION_KIND,
        TypedKey::U64(record.physical_ordinal),
        PositionStability::AppendStable,
    )?;
    let event_id = derive_event_id(EventIdentityInput {
        source: &source.source,
        session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let locator = SourceRecordLocator::new(
        source.source.clone(),
        NativeRecordCoordinate::Jsonl {
            byte_offset: record.byte_offset,
            byte_length: record.byte_length,
            physical_ordinal: record.physical_ordinal,
            native_session_key: Some(TypedKey::utf8(&record.line.session_id)?),
            native_event_key: Some(TypedKey::U64(record.physical_ordinal)),
        },
        LocatorRevisionPolicy::StableRecordEvidence,
        None,
        record.record_digest,
    )?;
    let body = prompt_lexical_body(&record.line.text);
    let occurred_at_unix_ms =
        chrono::DateTime::from_timestamp(record.line.ts, 0).map(|value| value.timestamp_millis());
    Ok(LexicalDocument {
        event_id,
        session_id,
        parent_session_id: None,
        root_session_id: session_id,
        source: source.source.clone(),
        locator,
        provider_session_id: bounded_metadata(&record.line.session_id),
        branch: None,
        source_path: source.path().to_str().and_then(bounded_metadata),
        agent_type: AgentType::Primary.as_str().to_owned(),
        is_primary: true,
        event_sequence: record.physical_ordinal,
        occurred_at_unix_ms,
        event_type: "message".to_owned(),
        role: Some("user".to_owned()),
        body,
        workspace: None,
        cwd: None,
        touched_files: Vec::new(),
    })
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

pub(super) fn retained_document_bytes(document: &LexicalDocument) -> usize {
    document
        .body
        .len()
        .saturating_add(document.provider_session_id.as_ref().map_or(0, String::len))
        .saturating_add(document.source_path.as_ref().map_or(0, String::len))
        .saturating_add(512)
}

fn bounded_metadata(value: &str) -> Option<String> {
    (!value.is_empty() && value.len() <= DOCUMENT_METADATA_MAX_BYTES).then(|| value.to_owned())
}
