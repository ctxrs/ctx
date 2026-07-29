use super::*;

pub(super) fn hydrate_cli(
    source: &CodeBuddySource,
    session: &CodeBuddySessionState,
    locator: &SourceRecordLocator,
) -> Result<CodeBuddyHydratedSourceRecord> {
    if locator.revision_policy() != LocatorRevisionPolicy::StableRecordEvidence
        || locator.certified_source_revision_digest().is_some()
    {
        return Err(invalid_source_backed(
            "CLI locator has the wrong revision policy",
        ));
    }
    let NativeRecordCoordinate::Jsonl {
        byte_offset,
        byte_length,
        physical_ordinal,
        native_session_key,
        native_event_key,
    } = locator.coordinate()
    else {
        return Err(invalid_source_backed(
            "CLI locator is not a JSONL byte range",
        ));
    };
    if *byte_length > CODEBUDDY_NATIVE_RECORD_MAX_BYTES as u64 {
        return Err(invalid_source_backed(
            "CLI locator exceeds the bounded record size",
        ));
    }
    let provider_bytes = match source
        .capability
        .as_ref()
        .and_then(|capability| capability.primary.as_ref())
    {
        Some(file) => file.read_exact_range(
            *byte_offset,
            usize::try_from(*byte_length)
                .map_err(|_| invalid_source_backed("CLI locator range is too large"))?,
            CODEBUDDY_NATIVE_RECORD_MAX_BYTES,
        )?,
        None => read_exact_range(&source.path, *byte_offset, *byte_length)?,
    };
    let payload = jsonl_payload(&provider_bytes);
    if Sha256::digest(payload).as_slice() != locator.record_digest() {
        return Err(invalid_source_backed(
            "CLI locator record digest no longer matches provider bytes",
        ));
    }
    let value: Value = serde_json::from_slice(payload)?;
    let physical_line = usize::try_from(*physical_ordinal)
        .ok()
        .and_then(|ordinal| ordinal.checked_add(1))
        .ok_or(CaptureError::SystemInvariant(
            "CodeBuddy physical line exceeds platform limits",
        ))?;
    let (text, native_message_id) = codebuddy_cli_complete_content_record(&value, physical_line)
        .ok_or_else(|| {
            invalid_source_backed("CLI locator no longer resolves to a CodeBuddy message")
        })?;
    let expected_session = session_key_utf8(native_session_key.as_ref())
        .ok_or_else(|| invalid_source_backed("CLI locator has an invalid native session key"))?;
    let observed_session = value
        .get("sessionId")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(|value| format!("{}/{}", session.project_hash, value))
        .unwrap_or_else(|| session.provider_session_id());
    if expected_session != observed_session
        || !tagged_event_key_matches(
            native_event_key.as_ref(),
            CODEBUDDY_CLI_LOCATOR_TAG,
            &native_message_id,
        )
    {
        return Err(invalid_source_backed(
            "CLI locator native identity no longer matches the provider record",
        ));
    }
    Ok(CodeBuddyHydratedSourceRecord {
        provider_bytes: text.as_bytes().to_vec(),
        decoded_display_text: text,
    })
}

pub(super) fn hydrate_extension(
    source: &CodeBuddySource,
    session: &CodeBuddySessionState,
    locator: &SourceRecordLocator,
) -> Result<CodeBuddyHydratedSourceRecord> {
    if locator.revision_policy() != LocatorRevisionPolicy::ExactSourceRevision
        || locator.certified_source_revision_digest() != Some(&source_revision_digest(source))
    {
        return Err(invalid_source_backed(
            "structured locator source revision is stale",
        ));
    }
    let (relative_path, ordinal, native_record_id) = structured_coordinate(locator.coordinate())?;
    let message_id = relative_path
        .strip_prefix("messages/")
        .and_then(|value| value.strip_suffix(".json"))
        .filter(|value| provider_safe_path_segment(value))
        .ok_or_else(|| invalid_source_backed("structured locator message path is invalid"))?;
    let expected_native_record_id = format!("{}:{message_id}", session.provider_session_id());
    if native_record_id != expected_native_record_id {
        return Err(invalid_source_backed(
            "structured locator native identity does not match its source",
        ));
    }
    let path = source.path.join(&relative_path);
    let admitted = source
        .capability
        .as_ref()
        .and_then(|capability| capability.extension.as_ref())
        .and_then(|extension| extension.messages.get(message_id));
    let frozen = match admitted {
        Some(file) => CodeBuddyFrozenFile::from_metadata(file.metadata())?,
        None => CodeBuddyFrozenFile::read(&path)?,
    };
    if frozen.length > CODEBUDDY_NATIVE_RECORD_MAX_BYTES as u64 {
        return Err(invalid_source_backed(
            "structured locator exceeds the bounded record size",
        ));
    }
    let provider_bytes = match admitted {
        Some(file) => file.read_all_bounded(CODEBUDDY_NATIVE_RECORD_MAX_BYTES)?,
        None => fs::read(&path)?,
    };
    let revalidated = match admitted {
        Some(file) => file.revalidate().is_ok(),
        None => frozen.revalidate(&path)?,
    };
    if !revalidated || Sha256::digest(&provider_bytes).as_slice() != locator.record_digest() {
        return Err(invalid_source_backed(
            "structured locator record digest no longer matches provider bytes",
        ));
    }
    let raw: Value = serde_json::from_slice(&provider_bytes)?;
    let decoded = codebuddy_decoded_message(&raw);
    let text = codebuddy_message_text(&decoded, &raw);
    if text.trim().is_empty() {
        return Err(invalid_source_backed(
            "structured locator no longer resolves to displayable message content",
        ));
    }
    let _ = ordinal;
    Ok(CodeBuddyHydratedSourceRecord {
        provider_bytes: text.as_bytes().to_vec(),
        decoded_display_text: text,
    })
}

pub(super) fn structured_coordinate(
    coordinate: &NativeRecordCoordinate,
) -> Result<(String, u64, String)> {
    let NativeRecordCoordinate::TreeRecord {
        relative_file_key,
        record_coordinate,
    } = coordinate
    else {
        return Err(invalid_source_backed(
            "structured locator is not a tree record",
        ));
    };
    let TypedKey::Utf8(relative_path) = relative_file_key else {
        return Err(invalid_source_backed(
            "structured locator relative path is not UTF-8",
        ));
    };
    let TypedKey::Composite(parts) = record_coordinate else {
        return Err(invalid_source_backed(
            "structured locator coordinate is not tagged",
        ));
    };
    match parts.as_slice() {
        [TypedKey::Utf8(tag), TypedKey::U64(ordinal), TypedKey::Utf8(native_id)]
            if tag == CODEBUDDY_EXTENSION_LOCATOR_TAG =>
        {
            Ok((relative_path.clone(), *ordinal, native_id.clone()))
        }
        _ => Err(invalid_source_backed(
            "structured locator coordinate has the wrong format tag",
        )),
    }
}

pub(super) fn tagged_event_key_matches(key: Option<&TypedKey>, tag: &str, native_id: &str) -> bool {
    matches!(
        key,
        Some(TypedKey::Composite(parts))
            if matches!(
                parts.as_slice(),
                [TypedKey::Utf8(actual_tag), TypedKey::Utf8(actual_id)]
                    if actual_tag == tag && actual_id == native_id
            )
    )
}

fn session_key_utf8(key: Option<&TypedKey>) -> Option<&str> {
    match key {
        Some(TypedKey::Utf8(value)) => Some(value),
        _ => None,
    }
}

fn read_exact_range(path: &Path, offset: u64, length: u64) -> Result<Vec<u8>> {
    let end = offset
        .checked_add(length)
        .ok_or_else(|| invalid_source_backed("source record range overflowed"))?;
    let mut file = File::open(path)?;
    if end > file.metadata()?.len() {
        return Err(invalid_source_backed(
            "source record range ends after the provider source",
        ));
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = vec![
        0_u8;
        usize::try_from(length).map_err(|_| {
            invalid_source_backed("source record range exceeds platform limits")
        })?
    ];
    file.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn jsonl_payload(bytes: &[u8]) -> &[u8] {
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    bytes.strip_suffix(b"\r").unwrap_or(bytes)
}
