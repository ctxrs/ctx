use super::*;

pub(super) struct ParsedProjection<'a> {
    pub(super) rows: Vec<ClineEventRow>,
    pub(super) outputs: Vec<OutputCandidate<'a>>,
    pub(super) occurred_at_millis: Option<i64>,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn parse_api_projection<'a>(
    raw_item: &'a RawValue,
    envelope: &RawEnvelope<'a>,
    source: &ClineFileSourceIdentity,
    identity: &ClineTaskIdentity,
    native_key: &ClineNativeItemKey,
    native_index: u64,
    item_byte_start: u64,
) -> Result<ParsedProjection<'a>, (ClineItemRejectionKind, String)> {
    let discriminators = envelope.normalized_discriminators().collect::<Vec<_>>();
    let top_result = discriminators
        .iter()
        .any(|value| is_result_discriminator(value));
    let positive_conversation = discriminators.iter().any(|value| {
        matches!(
            value.as_str(),
            "user" | "assistant" | "system" | "developer" | "message" | "text"
        )
    });
    let role = role_from_discriminators(&discriminators);
    let context = ClineEventContext {
        task: identity,
        component: match source.component {
            ClineComponent::FallbackHistory => ClineEventComponent::FallbackHistory,
            _ => ClineEventComponent::ApiHistory,
        },
        item: native_key,
        item_index: native_index,
        role,
        occurred_at_millis: envelope.occurred_at_millis,
    };
    let mut projection = ParsedProjection {
        rows: Vec::new(),
        outputs: Vec::new(),
        occurred_at_millis: envelope.occurred_at_millis,
    };
    if top_result {
        if envelope
            .content
            .is_some_and(|content| content.get().trim_start().starts_with('['))
        {
            let content = envelope.content.expect("checked Cline result content");
            push_explicit_result_blocks(
                raw_item,
                content,
                OutputObservationKind::Tool,
                envelope,
                item_byte_start,
                &mut projection.outputs,
            )?;
            return Ok(projection);
        }
        push_explicit_outputs(
            raw_item,
            envelope.direct_result_body(),
            OutputCandidateContext {
                kind: OutputObservationKind::Tool,
                base_sub_index: 0,
                call_id: envelope.call_id.clone(),
                outcome: envelope.outcome(),
                occurred_at_millis: envelope.occurred_at_millis,
                item_start: item_byte_start,
                fallback_start: item_byte_start,
            },
            &mut projection.outputs,
        )?;
        return Ok(projection);
    }
    let Some(content) = envelope.content else {
        return Ok(projection);
    };
    let content_text = content.get().trim_start();
    if content_text.starts_with('"') {
        if positive_conversation {
            if let Some(body) = decode_retained_text(content)? {
                if !body.trim().is_empty() {
                    projection.rows.push(ClineEventRow::message(
                        context,
                        0,
                        ClineEventKind::Message,
                        body,
                    ));
                }
            }
        }
        return Ok(projection);
    }
    if content_text.starts_with('[') {
        let blocks = deserialize_bounded_raw_array(content, "Cline API content array")?;
        for (index, block) in blocks.into_iter().enumerate() {
            let sub_index = u32::try_from(index).unwrap_or(u32::MAX);
            parse_api_block(
                raw_item,
                block,
                context,
                sub_index,
                item_byte_start,
                positive_conversation,
                envelope,
                &mut projection,
            )?;
        }
        return Ok(projection);
    }
    if content_text.starts_with('{') {
        parse_api_block(
            raw_item,
            content,
            context,
            0,
            item_byte_start,
            positive_conversation,
            envelope,
            &mut projection,
        )?;
        return Ok(projection);
    }
    Err((
        ClineItemRejectionKind::UnsupportedShape,
        "Cline API content is not text, an object, or an array".to_owned(),
    ))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn parse_api_block<'a>(
    raw_item: &'a RawValue,
    raw_block: &'a RawValue,
    context: ClineEventContext<'_>,
    sub_index: u32,
    item_byte_start: u64,
    retain_text: bool,
    outer: &RawEnvelope<'a>,
    projection: &mut ParsedProjection<'a>,
) -> Result<(), (ClineItemRejectionKind, String)> {
    let row_sub_index = sub_index.saturating_mul(1_024);
    if raw_block.get().trim_start().starts_with('"') {
        if !retain_text {
            return Ok(());
        }
        if let Some(body) = decode_retained_text(raw_block)? {
            if !body.trim().is_empty() {
                projection.rows.push(ClineEventRow::message(
                    context,
                    row_sub_index,
                    ClineEventKind::Message,
                    body,
                ));
            }
        }
        return Ok(());
    }
    if !raw_block.get().trim_start().starts_with('{') {
        return Ok(());
    }
    let block = serde_json::from_str::<RawEnvelope<'_>>(raw_block.get()).map_err(|error| {
        (
            ClineItemRejectionKind::MalformedRecord,
            format!("malformed Cline API content block: {error}"),
        )
    })?;
    if block.conflicting_discriminator || block.oversized_discriminator {
        return Err((
            ClineItemRejectionKind::ConflictingDiscriminator,
            "Cline API block has conflicting or oversized discriminator fields".to_owned(),
        ));
    }
    let discriminators = block.normalized_discriminators().collect::<Vec<_>>();
    let is_result = discriminators
        .iter()
        .any(|value| is_result_discriminator(value));
    let is_text = discriminators
        .iter()
        .any(|value| matches!(value.as_str(), "text" | "message"));
    let is_call = discriminators
        .iter()
        .any(|value| matches!(value.as_str(), "tooluse" | "functioncall" | "toolcall"));
    let block_start = (raw_block.get().as_ptr() as usize)
        .checked_sub(raw_item.get().as_ptr() as usize)
        .and_then(|offset| item_byte_start.checked_add(offset as u64))
        .unwrap_or(item_byte_start);
    if is_result {
        let block_outcome = block.outcome();
        let outcome = if block_outcome.outcome == OutputOutcome::Unknown
            && block_outcome.exit_code.is_none()
            && block_outcome.duration_ms.is_none()
        {
            outer.outcome()
        } else {
            block_outcome
        };
        push_explicit_outputs(
            raw_item,
            block.block_result_body(),
            OutputCandidateContext {
                kind: OutputObservationKind::Tool,
                base_sub_index: sub_index.saturating_mul(1_024),
                call_id: block.call_id.clone().or_else(|| outer.call_id.clone()),
                outcome,
                occurred_at_millis: block.occurred_at_millis.or(context.occurred_at_millis),
                item_start: item_byte_start,
                fallback_start: block_start,
            },
            &mut projection.outputs,
        )?;
    } else if is_call {
        let file_touches = extract_file_touches(raw_block)?;
        let mut row = ClineEventRow::tool_call(context, row_sub_index, block.call_id, block.name);
        row.attach_file_touches(file_touches);
        projection.rows.push(row);
    } else if is_text && retain_text {
        if let Some(body) = block
            .retained_body()
            .map(decode_retained_text)
            .transpose()?
        {
            if let Some(body) = body.filter(|body| !body.trim().is_empty()) {
                projection.rows.push(ClineEventRow::message(
                    context,
                    row_sub_index,
                    ClineEventKind::Message,
                    body,
                ));
            }
        }
    }
    Ok(())
}

pub(super) fn extract_file_touches(
    raw_call: &RawValue,
) -> Result<Vec<ClineFileTouch>, (ClineItemRejectionKind, String)> {
    let raw_value = serde_json::from_str::<Value>(raw_call.get())
        .map_err(|error| (ClineItemRejectionKind::MalformedRecord, error.to_string()))?;
    let mut file_touches = Vec::new();
    let outcome = visit_provider_file_touch_drafts_with_limit(
        &raw_value,
        true,
        MAX_PROVIDER_FILE_TOUCHES_PER_EVENT,
        |(_, touch)| -> std::result::Result<(), ()> {
            file_touches.push(ClineFileTouch {
                path: touch.path.into_boxed_str(),
                old_path: touch.old_path.map(String::into_boxed_str),
                change_kind: touch.change_kind,
                confidence: touch.confidence,
                metadata: touch.metadata,
            });
            Ok(())
        },
    )
    .unwrap_or_else(|()| unreachable!("the file-touch collector is infallible"));
    if outcome.limit_exceeded() {
        return Err((
            ClineItemRejectionKind::UnsupportedShape,
            PROVIDER_FILE_TOUCH_LIMIT_REJECTION.to_owned(),
        ));
    }
    Ok(file_touches)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn parse_ui_projection<'a>(
    raw_item: &'a RawValue,
    envelope: &RawEnvelope<'a>,
    _source: &ClineFileSourceIdentity,
    identity: &ClineTaskIdentity,
    native_key: &ClineNativeItemKey,
    native_index: u64,
    byte_start: u64,
) -> Result<ParsedProjection<'a>, (ClineItemRejectionKind, String)> {
    let discriminators = envelope.normalized_discriminators().collect::<Vec<_>>();
    let command = discriminators.iter().any(|value| {
        is_result_discriminator(value) || matches!(value.as_str(), "executecommand" | "command")
    });
    let user = discriminators
        .iter()
        .any(|value| matches!(value.as_str(), "ask" | "user"));
    let assistant = discriminators
        .iter()
        .any(|value| matches!(value.as_str(), "say" | "assistant" | "text"));
    let summary = discriminators
        .iter()
        .any(|value| matches!(value.as_str(), "completionresult" | "summary"));
    let notice = discriminators.iter().any(|value| value == "notice");
    let mut projection = ParsedProjection {
        rows: Vec::new(),
        outputs: Vec::new(),
        occurred_at_millis: envelope.occurred_at_millis,
    };
    if command {
        if let Some(content) = envelope
            .content
            .filter(|content| content.get().trim_start().starts_with('['))
        {
            push_explicit_result_blocks(
                raw_item,
                content,
                OutputObservationKind::Command,
                envelope,
                byte_start,
                &mut projection.outputs,
            )?;
        } else {
            push_explicit_outputs(
                raw_item,
                envelope.direct_result_body(),
                OutputCandidateContext {
                    kind: OutputObservationKind::Command,
                    base_sub_index: 0,
                    call_id: envelope.call_id.clone(),
                    outcome: envelope.outcome(),
                    occurred_at_millis: envelope.occurred_at_millis,
                    item_start: byte_start,
                    fallback_start: byte_start,
                },
                &mut projection.outputs,
            )?;
        }
        return Ok(projection);
    }
    let Some((kind, role)) = user
        .then_some((ClineEventKind::Message, ClineEventRole::User))
        .or_else(|| assistant.then_some((ClineEventKind::Message, ClineEventRole::Assistant)))
        .or_else(|| summary.then_some((ClineEventKind::Summary, ClineEventRole::Assistant)))
        .or_else(|| notice.then_some((ClineEventKind::Notice, ClineEventRole::Unknown)))
    else {
        return Ok(projection);
    };
    if let Some(body) = envelope
        .retained_body()
        .map(decode_retained_text)
        .transpose()?
        .flatten()
        .filter(|body| !body.trim().is_empty())
    {
        projection.rows.push(ClineEventRow::message(
            ClineEventContext {
                task: identity,
                component: ClineEventComponent::UiMessages,
                item: native_key,
                item_index: native_index,
                role,
                occurred_at_millis: envelope.occurred_at_millis,
            },
            0,
            kind,
            body,
        ));
    }
    Ok(projection)
}

pub(super) fn decode_retained_text(
    raw: &RawValue,
) -> Result<Option<String>, (ClineItemRejectionKind, String)> {
    if !raw.get().trim_start().starts_with('"') {
        return Ok(None);
    }
    if raw.get().len() > CLINE_NATIVE_MAX_RETAINED_ITEM_BYTES {
        return Err((
            ClineItemRejectionKind::OversizedRetainedItem,
            "Cline retained JSON string exceeds 64 KiB before unescaping".to_owned(),
        ));
    }
    serde_json::from_str::<String>(raw.get())
        .map(Some)
        .map_err(|error| {
            (
                ClineItemRejectionKind::MalformedRecord,
                format!("invalid retained Cline text: {error}"),
            )
        })
}

pub(super) fn decode_output_body(raw: Option<&RawValue>) -> Result<Vec<u8>, &'static str> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    if raw.get().len() > MAX_OUTPUT_BODY_RAW_BYTES {
        return Err("Cline output body exceeds the independent 4 MiB transient bound");
    }
    if raw.get().trim_start().starts_with('"') {
        return serde_json::from_str::<String>(raw.get())
            .map(String::into_bytes)
            .map_err(|_| "Cline output body is not a valid JSON string");
    }
    if raw.get().trim() == "null" {
        return Ok(Vec::new());
    }
    let value = serde_json::from_str::<serde_json::Value>(raw.get())
        .map_err(|_| "Cline output body is not valid explicit JSON")?;
    serde_json::to_vec(&value).map_err(|_| "Cline output body could not be encoded")
}

pub(super) fn decode_failure_preview(raw: Option<&RawValue>) -> Option<Box<str>> {
    let raw = raw?;
    if raw.get().trim_start().starts_with('"') {
        decode_json_string_preview(raw.get()).map(String::into_boxed_str)
    } else {
        Some(failure_preview_from_bytes(raw.get().as_bytes()))
    }
}

pub(super) fn decode_json_string_preview(raw: &str) -> Option<String> {
    let bytes = raw.trim_start().as_bytes();
    if bytes.first() != Some(&b'"') {
        return None;
    }
    let mut output = String::new();
    let mut index = 1_usize;
    let mut chars = 0_usize;
    while index < bytes.len() && chars < CLINE_NATIVE_MAX_FAILURE_PREVIEW_BYTES {
        match bytes[index] {
            b'"' => return Some(output),
            b'\\' => {
                index = index.checked_add(1)?;
                let escaped = *bytes.get(index)?;
                let decoded = match escaped {
                    b'"' => '"',
                    b'\\' => '\\',
                    b'/' => '/',
                    b'b' => '\u{0008}',
                    b'f' => '\u{000c}',
                    b'n' => '\n',
                    b'r' => '\r',
                    b't' => '\t',
                    b'u' => {
                        let first = decode_hex_quad(bytes.get(index + 1..index + 5)?)?;
                        index = index.checked_add(4)?;
                        let scalar = if (0xd800..=0xdbff).contains(&first) {
                            if bytes.get(index + 1..index + 3) != Some(b"\\u") {
                                return None;
                            }
                            let second = decode_hex_quad(bytes.get(index + 3..index + 7)?)?;
                            if !(0xdc00..=0xdfff).contains(&second) {
                                return None;
                            }
                            index = index.checked_add(6)?;
                            0x1_0000
                                + ((u32::from(first) - 0xd800) << 10)
                                + (u32::from(second) - 0xdc00)
                        } else {
                            u32::from(first)
                        };
                        char::from_u32(scalar)?
                    }
                    _ => return None,
                };
                output.push(decoded);
                chars = chars.saturating_add(1);
                index = index.checked_add(1)?;
            }
            _ => {
                let tail = std::str::from_utf8(bytes.get(index..)?).ok()?;
                let decoded = tail.chars().next()?;
                output.push(decoded);
                chars = chars.saturating_add(1);
                index = index.checked_add(decoded.len_utf8())?;
            }
        }
    }
    Some(output)
}

pub(super) fn decode_hex_quad(bytes: &[u8]) -> Option<u16> {
    if bytes.len() != 4 {
        return None;
    }
    bytes.iter().try_fold(0_u16, |value, byte| {
        let digit = match byte {
            b'0'..=b'9' => u16::from(*byte - b'0'),
            b'a'..=b'f' => u16::from(*byte - b'a' + 10),
            b'A'..=b'F' => u16::from(*byte - b'A' + 10),
            _ => return None,
        };
        value.checked_mul(16)?.checked_add(digit)
    })
}

pub(super) fn failure_preview_from_bytes(bytes: &[u8]) -> Box<str> {
    String::from_utf8_lossy(bytes)
        .chars()
        .take(CLINE_NATIVE_MAX_FAILURE_PREVIEW_BYTES)
        .collect::<String>()
        .into_boxed_str()
}

pub(super) fn build_output_observation(
    source: &ClineFileSourceIdentity,
    identity: &ClineTaskIdentity,
    native_key: &ClineNativeItemKey,
    native_index: u64,
    output: OutputCandidate<'_>,
    content: Vec<u8>,
) -> ProOutputObservation {
    let component = match source.component {
        ClineComponent::ApiHistory => "api",
        ClineComponent::UiMessages => "ui",
        ClineComponent::FallbackHistory => "fallback",
        ClineComponent::TaskMetadata => "metadata",
        ClineComponent::HistoryItem => "history_item",
        ClineComponent::TaskIndex => "task_index",
        ClineComponent::RootIndex => "root",
    };
    let mut identity_hash = Sha256::new();
    identity_hash.update(b"ctx-task-json-output-item-v1\0");
    identity_hash.update(source.provider.as_bytes());
    identity_hash.update([source.component as u8]);
    match native_key {
        ClineNativeItemKey::NativeId {
            native_id,
            occurrence,
        } => {
            identity_hash.update(b"id\0");
            identity_hash.update(native_id.as_bytes());
            identity_hash.update(occurrence.to_le_bytes());
        }
        ClineNativeItemKey::ComponentOrdinal(ordinal) => {
            identity_hash.update(b"ordinal\0");
            identity_hash.update(ordinal.to_le_bytes());
        }
    }
    identity_hash.update(output.sub_index.to_le_bytes());
    let identity_hash = identity_hash.finalize();
    let mut encoded_identity = String::with_capacity(identity_hash.len() * 2);
    for byte in identity_hash {
        use std::fmt::Write as _;
        let _ = write!(encoded_identity, "{byte:02x}");
    }
    let unit_key = format!(
        "{}/nativepath/{component}/{}",
        source.provider, encoded_identity
    );
    let mut locator = Vec::with_capacity(29);
    locator.push(source.component as u8);
    locator.extend_from_slice(&native_index.to_be_bytes());
    locator.extend_from_slice(&output.sub_index.to_be_bytes());
    locator.extend_from_slice(&output.byte_start.to_be_bytes());
    locator.extend_from_slice(&output.byte_end_exclusive.to_be_bytes());
    ProOutputObservation {
        kind: output.kind,
        coordinate: OutputNativeCoordinate {
            unit_key: unit_key.clone(),
            native_sequence: native_index,
            native_record_id: Some(unit_key),
            source_record_ordinal: Some(native_index),
            source_record_subrecord_index: Some(output.sub_index),
            byte_start: Some(output.byte_start),
            byte_end_exclusive: Some(output.byte_end_exclusive),
        },
        occurred_at_unix_ms: output.occurred_at_millis,
        associations: OutputAssociations {
            direct_session_id: identity.as_str().to_owned(),
            root_session_id: identity.as_str().to_owned(),
            parent_session_id: None,
            provider_session_id: Some(identity.as_str().to_owned()),
            agent_id: None,
            repository: None,
        },
        call_id: output.call_id,
        command: None,
        outcome: output.outcome,
        locator: OutputSourceLocator {
            version: 1,
            kind: "cline_native_component_range".to_owned(),
            payload: locator,
        },
        content,
    }
}
