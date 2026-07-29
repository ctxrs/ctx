use super::*;

pub(super) fn unique_transcript(
    selected_root: &Path,
    native_session_id: &str,
) -> Result<CursorTranscriptPath> {
    let inventory = discover_cursor_transcripts(selected_root);
    if !inventory.completed {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: selected_root.to_path_buf(),
            reason: "Cursor source-backed hydration inventory could not be completed",
        });
    }
    let mut matches = inventory
        .transcripts
        .into_iter()
        .filter(|transcript| transcript.native_session_id() == native_session_id);
    let transcript = matches.next().ok_or_else(|| {
        CaptureError::InvalidPayload(format!(
            "Cursor source-backed locator session {native_session_id:?} is absent from the selected projects root"
        ))
    })?;
    if matches.next().is_some() {
        return Err(CaptureError::InvalidPayload(format!(
            "Cursor source-backed locator session {native_session_id:?} is ambiguous in the selected projects root"
        )));
    }
    Ok(transcript)
}

pub(super) fn validate_locator(
    record: &CursorSourceBackedRecord,
) -> Result<(String, u64, u64, u64, u32)> {
    let source = record.locator.source();
    if source.provider() != CaptureProvider::Cursor.as_str()
        || source.source_format() != CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT
        || source.schema_variant() != CURSOR_SOURCE_SCHEMA_VARIANT
        || source.provider_identity_version() != 1
    {
        return Err(CaptureError::InvalidPayload(
            "locator is not a Cursor source-backed JSONL record".to_owned(),
        ));
    }
    let SourceAnchor::ProviderNative { namespace, key } = source.anchor() else {
        return Err(CaptureError::InvalidPayload(
            "Cursor source-backed locator has no native session anchor".to_owned(),
        ));
    };
    let TypedKey::Utf8(native_session_id) = key else {
        return Err(CaptureError::InvalidPayload(
            "Cursor source-backed locator has a malformed native session anchor".to_owned(),
        ));
    };
    if namespace != CURSOR_SOURCE_ANCHOR_NAMESPACE {
        return Err(CaptureError::InvalidPayload(
            "Cursor source-backed locator uses an unsupported source namespace".to_owned(),
        ));
    }
    let NativeRecordCoordinate::Jsonl {
        byte_offset,
        byte_length,
        physical_ordinal,
        native_session_key,
        native_event_key,
    } = record.locator.coordinate()
    else {
        return Err(CaptureError::InvalidPayload(
            "Cursor source-backed locator is not a JSONL byte range".to_owned(),
        ));
    };
    let expected_event_key = TypedKey::composite(vec![
        TypedKey::U64(record.native_order.semantic_ordinal),
        TypedKey::U64(u64::from(record.native_order.part_ordinal)),
    ])
    .map_err(|error| contract_error("native locator event key", error))?;
    if native_session_key.as_ref() != Some(&TypedKey::Utf8(native_session_id.clone()))
        || native_event_key.as_ref() != Some(&expected_event_key)
        || *physical_ordinal != record.native_order.physical_ordinal
    {
        return Err(CaptureError::InvalidPayload(
            "Cursor source-backed locator coordinates do not match the projected event".to_owned(),
        ));
    }
    Ok((
        native_session_id.clone(),
        *byte_offset,
        *byte_length,
        *physical_ordinal,
        record.native_order.part_ordinal,
    ))
}
