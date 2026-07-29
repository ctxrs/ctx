use super::*;

pub(super) struct ParsedProjection<'a> {
    pub(super) rows: Vec<ClineEventRow>,
    pub(super) outputs: Vec<OutputCandidate<'a>>,
    pub(super) occurred_at_millis: Option<i64>,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn parse_api_projection<'a>(
    envelope: &RawEnvelope<'a>,
    component: ClineEventComponent,
    identity: &ClineTaskIdentity,
    native_key: &ClineNativeItemKey,
    native_index: u64,
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
        component,
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
                content,
                OutputObservationKind::Tool,
                envelope,
                &mut projection.outputs,
            )?;
            return Ok(projection);
        }
        push_explicit_outputs(
            envelope.direct_result_body(),
            OutputCandidateContext {
                kind: OutputObservationKind::Tool,
                base_sub_index: 0,
                call_id: envelope.call_id.clone(),
                outcome: envelope.outcome(),
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
                block,
                context,
                sub_index,
                positive_conversation,
                envelope,
                &mut projection,
            )?;
        }
        return Ok(projection);
    }
    if content_text.starts_with('{') {
        parse_api_block(
            content,
            context,
            0,
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

pub(super) fn parse_api_block<'a>(
    raw_block: &'a RawValue,
    context: ClineEventContext<'_>,
    sub_index: u32,
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
            block.block_result_body(),
            OutputCandidateContext {
                kind: OutputObservationKind::Tool,
                base_sub_index: sub_index.saturating_mul(1_024),
                call_id: block.call_id.clone().or_else(|| outer.call_id.clone()),
                outcome,
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
    envelope: &RawEnvelope<'a>,
    identity: &ClineTaskIdentity,
    native_key: &ClineNativeItemKey,
    native_index: u64,
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
                content,
                OutputObservationKind::Command,
                envelope,
                &mut projection.outputs,
            )?;
        } else {
            push_explicit_outputs(
                envelope.direct_result_body(),
                OutputCandidateContext {
                    kind: OutputObservationKind::Command,
                    base_sub_index: 0,
                    call_id: envelope.call_id.clone(),
                    outcome: envelope.outcome(),
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
