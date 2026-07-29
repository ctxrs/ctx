use super::*;

struct KimiOutputClassification {
    kind: OutputObservationKind,
    outcome: OutputOutcome,
}

#[derive(Debug)]
pub(super) struct DecodedKimiLocator {
    pub(super) byte_offset: u64,
    pub(super) byte_length: u64,
    pub(super) physical_ordinal: u64,
    pub(super) native_event_id: String,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn lexical_document(
    compound: &KimiCompoundObservation,
    session_id: StableEntityId,
    ordinal: u64,
    byte_offset: u64,
    byte_length: u64,
    record_bytes: &[u8],
    value: &Value,
    fallback_timestamp: DateTime<Utc>,
    source_revision_digest: [u8; 32],
) -> KimiSourceBackedResult<Option<LexicalDocument>> {
    let record_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let Some((event_type, body)) =
        kimi_lexical_body(value, ordinal, compound.native.session.cwd.as_deref())?
    else {
        return Ok(None);
    };
    let role = kimi_event_role(record_type, value, event_type);
    let occurred_at =
        kimi_record_timestamp(value, fallback_timestamp).unwrap_or(fallback_timestamp);
    let line_number = usize::try_from(ordinal)
        .ok()
        .and_then(|value| value.checked_add(1))
        .ok_or(KimiSourceBackedError::CountOverflow)?;
    let native_event_id = kimi_legacy_provider_event_hash(record_type, value, line_number);
    let event_key = NativeItemKey::certified_position(
        KIMI_NATIVE_EVENT_POSITION_KIND,
        TypedKey::U64(ordinal),
        PositionStability::AppendStable,
    )?;
    let event_id = derive_event_id(EventIdentityInput {
        source: &compound.source,
        session_id,
        logical_item_kind: KIMI_LOGICAL_EVENT_KIND,
        native_item_key: &event_key,
        subrecord_selector: None,
    })?;
    let coordinate = TypedKey::composite(vec![
        TypedKey::U64(byte_offset),
        TypedKey::U64(byte_length),
        TypedKey::U64(ordinal),
        TypedKey::utf8(&compound.native.session.provider_session_id)?,
        TypedKey::utf8(native_event_id)?,
    ])?;
    let locator = SourceRecordLocator::new(
        compound.source.clone(),
        NativeRecordCoordinate::TreeRecord {
            relative_file_key: TypedKey::bytes(compound.relative_file_key.clone())?,
            record_coordinate: coordinate,
        },
        LocatorRevisionPolicy::ExactSourceRevision,
        Some(source_revision_digest),
        Sha256::digest(record_bytes).into(),
    )?;
    let touched_files = kimi_touched_paths(
        value,
        event_type,
        event_type_supports_structured_file_touches(event_type),
    )?;
    let parent_session_id = compound
        .native
        .session
        .parent_provider_session_id
        .as_deref()
        .map(lineage_session_identity)
        .transpose()?;
    let root_session_id = compound
        .native
        .session
        .root_provider_session_id
        .as_deref()
        .map(lineage_session_identity)
        .transpose()?
        .unwrap_or(session_id);
    let workspace = compound.native.session.cwd.clone();
    Ok(Some(LexicalDocument {
        event_id,
        session_id,
        parent_session_id,
        root_session_id,
        source: compound.source.clone(),
        locator,
        provider_session_id: Some(compound.native.session.provider_session_id.clone()),
        branch: None,
        source_path: Some(compound.native.canonical_path().display().to_string()),
        agent_type: if compound.native.session.is_primary {
            AgentType::Primary
        } else {
            AgentType::Subagent
        }
        .as_str()
        .to_owned(),
        is_primary: compound.native.session.is_primary,
        event_sequence: ordinal,
        occurred_at_unix_ms: Some(occurred_at.timestamp_millis()),
        event_type: event_type.as_str().to_owned(),
        role: Some(role.as_str().to_owned()),
        body,
        workspace,
        cwd: compound.native.session.cwd.clone(),
        touched_files,
    }))
}

pub(super) fn kimi_lexical_body(
    value: &Value,
    _ordinal: u64,
    _cwd: Option<&str>,
) -> KimiSourceBackedResult<Option<(EventType, String)>> {
    let record_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let mut event_type = kimi_event_type(record_type, value);
    let body = if event_type == EventType::ToolOutput {
        let output = kimi_output_classification(value);
        if !matches!(
            output.outcome,
            OutputOutcome::Failure | OutputOutcome::Timeout
        ) {
            return Ok(None);
        }
        if output.kind == OutputObservationKind::Command {
            event_type = EventType::CommandOutput;
        }
        kimi_output_content(value).unwrap_or_default()
    } else {
        kimi_event_text(record_type, value, event_type)
    };
    let body = provider_local_preview(&body, PROVIDER_MAX_TEXT_CHARS).0;
    if body.is_empty() {
        return Ok(None);
    }
    Ok(Some((event_type, body)))
}

fn kimi_touched_paths(
    value: &Value,
    event_type: EventType,
    include_structured_touches: bool,
) -> KimiSourceBackedResult<Vec<String>> {
    if !matches!(
        event_type,
        EventType::ToolCall
            | EventType::ToolOutput
            | EventType::CommandOutput
            | EventType::FileTouched
    ) {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    visit_provider_file_touch_drafts_with_limit(
        value,
        include_structured_touches,
        MAX_PROVIDER_FILE_TOUCHES_PER_EVENT,
        |(_, draft)| {
            paths.push(draft.path);
            Ok::<(), CaptureError>(())
        },
    )?;
    Ok(paths)
}

fn kimi_output_classification(value: &Value) -> KimiOutputClassification {
    let event = value.get("event").unwrap_or(value);
    let tool_name = event
        .get("toolName")
        .or_else(|| event.get("tool_name"))
        .or_else(|| event.get("name"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("tool");
    let kind = if tool_input::is_command_tool(&tool_name.to_ascii_lowercase()) {
        OutputObservationKind::Command
    } else {
        OutputObservationKind::Tool
    };
    let outcome = if kimi_value_timed_out(event) {
        OutputOutcome::Timeout
    } else if provider_output_event_is_failure(event) {
        OutputOutcome::Failure
    } else if provider_result_outcome_evidence(EventType::ToolOutput, event).as_str()
        == Some("success")
    {
        OutputOutcome::Success
    } else {
        OutputOutcome::Unknown
    };
    KimiOutputClassification { kind, outcome }
}

fn kimi_value_timed_out(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(kimi_value_timed_out),
        Value::Object(values) => {
            values.iter().any(|(key, value)| {
                matches!(key.as_str(), "timed_out" | "timedOut" | "timeout")
                    && value.as_bool().unwrap_or(false)
                    || matches!(key.as_str(), "status" | "state" | "outcome")
                        && value.as_str().is_some_and(|value| {
                            matches!(
                                value.trim().to_ascii_lowercase().as_str(),
                                "timeout" | "timed_out" | "timedout"
                            )
                        })
            }) || values.values().any(kimi_value_timed_out)
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

pub(super) fn decode_locator(
    leaf: &KimiSourceLeaf,
    locator: &SourceRecordLocator,
) -> KimiSourceBackedResult<DecodedKimiLocator> {
    locator.validate_contract()?;
    if locator.revision_policy() != LocatorRevisionPolicy::ExactSourceRevision
        || !leaf.source.exact_descriptor_eq(locator.source())
    {
        return Err(KimiSourceBackedError::InvalidLocator);
    }
    let NativeRecordCoordinate::TreeRecord {
        relative_file_key,
        record_coordinate,
    } = locator.coordinate()
    else {
        return Err(KimiSourceBackedError::InvalidLocator);
    };
    if relative_file_key != &TypedKey::Bytes(leaf.relative_file_key.clone()) {
        return Err(KimiSourceBackedError::InvalidLocator);
    }
    let TypedKey::Composite(parts) = record_coordinate else {
        return Err(KimiSourceBackedError::InvalidLocator);
    };
    let [TypedKey::U64(byte_offset), TypedKey::U64(byte_length), TypedKey::U64(physical_ordinal), TypedKey::Utf8(provider_session_id), TypedKey::Utf8(native_event_id)] =
        parts.as_slice()
    else {
        return Err(KimiSourceBackedError::InvalidLocator);
    };
    if provider_session_id != &leaf.provider_session_id || *byte_length == 0 {
        return Err(KimiSourceBackedError::InvalidLocator);
    }
    if *byte_length > MAX_KIMI_HYDRATED_RECORD_BYTES {
        return Err(KimiSourceBackedError::LocatorRangeTooLarge);
    }
    Ok(DecodedKimiLocator {
        byte_offset: *byte_offset,
        byte_length: *byte_length,
        physical_ordinal: *physical_ordinal,
        native_event_id: native_event_id.clone(),
    })
}

pub(super) fn read_exact_record(
    file: &mut File,
    locator: &SourceRecordLocator,
    coordinate: &DecodedKimiLocator,
) -> KimiSourceBackedResult<Vec<u8>> {
    let range_end = coordinate
        .byte_offset
        .checked_add(coordinate.byte_length)
        .ok_or(KimiSourceBackedError::LocatorRangeTooLarge)?;
    if file.metadata()?.len() < range_end {
        return Err(KimiSourceBackedError::LocatorRangeMissing);
    }
    file.seek(SeekFrom::Start(coordinate.byte_offset))?;
    let length = usize::try_from(coordinate.byte_length)
        .map_err(|_| KimiSourceBackedError::LocatorRangeTooLarge)?;
    let mut provider_bytes = vec![0; length];
    file.read_exact(&mut provider_bytes)?;
    if provider_bytes[..provider_bytes.len().saturating_sub(1)].contains(&b'\n')
        || (provider_bytes.last() != Some(&b'\n') && range_end != file.metadata()?.len())
    {
        return Err(KimiSourceBackedError::StaleRecordEvidence);
    }
    let record_bytes = json_record_bytes(&provider_bytes);
    if &Sha256::digest(record_bytes)[..] != locator.record_digest() {
        return Err(KimiSourceBackedError::StaleRecordEvidence);
    }
    let value = serde_json::from_slice::<Value>(record_bytes)
        .map_err(|_| KimiSourceBackedError::StaleRecordEvidence)?;
    let record_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let line_number = usize::try_from(coordinate.physical_ordinal)
        .ok()
        .and_then(|value| value.checked_add(1))
        .ok_or(KimiSourceBackedError::InvalidLocator)?;
    if kimi_legacy_provider_event_hash(record_type, &value, line_number)
        != coordinate.native_event_id
    {
        return Err(KimiSourceBackedError::StaleRecordEvidence);
    }
    Ok(provider_bytes)
}

pub(super) fn hydration_failure(kind: HydrationFailureKind, detail: &str) -> HydrationFailure {
    HydrationFailure {
        kind,
        detail: detail.to_owned(),
    }
}

pub(super) fn map_hydration_error(error: KimiSourceBackedError) -> HydrationFailure {
    let kind = match error {
        KimiSourceBackedError::UnknownSource => HydrationFailureKind::ConfirmedDeleted,
        KimiSourceBackedError::InvalidLocator
        | KimiSourceBackedError::LocatorRangeTooLarge
        | KimiSourceBackedError::Projection(_)
        | KimiSourceBackedError::Resolver(_) => HydrationFailureKind::InvalidLocator,
        KimiSourceBackedError::LocatorRangeMissing => HydrationFailureKind::MissingRecord,
        KimiSourceBackedError::StaleRecordEvidence => HydrationFailureKind::StaleRecordEvidence,
        KimiSourceBackedError::SourceChanged | KimiSourceBackedError::InventoryChanged => {
            HydrationFailureKind::StaleSourceEvidence
        }
        KimiSourceBackedError::InventoryUnavailable
        | KimiSourceBackedError::Capture(_)
        | KimiSourceBackedError::Io(_)
        | KimiSourceBackedError::Index(_)
        | KimiSourceBackedError::DuplicateLineage(_)
        | KimiSourceBackedError::CountOverflow => HydrationFailureKind::TemporarilyUnavailable,
    };
    hydration_failure(
        kind,
        "Kimi provider source could not satisfy exact hydration",
    )
}
