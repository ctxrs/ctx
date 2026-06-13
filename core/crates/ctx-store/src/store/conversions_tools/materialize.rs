pub(super) fn build_turn_tool_from_event(
    event: &SessionEvent,
    turn_id: TurnId,
) -> Option<SessionTurnTool> {
    if !matches!(
        event.event_type,
        SessionEventType::ToolCall
            | SessionEventType::ToolCallUpdate
            | SessionEventType::ToolResult
    ) {
        return None;
    }
    let tool_call_id = tool_call_id_from_payload(&event.payload_json)?;
    let update = extract_tool_update(&event.payload_json);

    let tool_kind = tool_kind_from_update(update);
    let provider_tool_name = tool_name_from_update(update);
    let title = tool_title_from_update(update);
    let raw_status = tool_status_from_update(update);
    let status = if let Some(raw_status) = raw_status {
        Some(normalize_tool_status(raw_status, event.event_type.clone()))
    } else if matches!(event.event_type, SessionEventType::ToolResult) {
        Some("completed".to_string())
    } else if matches!(event.event_type, SessionEventType::ToolCall) {
        Some("pending".to_string())
    } else {
        None
    };

    let input = update
        .pointer("/rawInput")
        .or_else(|| update.pointer("/toolCall/rawInput"))
        .or_else(|| update.pointer("/toolCall/input"))
        .or_else(|| update.pointer("/input"))
        .or_else(|| update.pointer("/args"));
    let input_preview = tool_input_preview_from_value(input);
    let input_preview = input_preview.or_else(|| update.get("input_preview").cloned());
    let input_meta = build_json_preview(input, input_preview);
    let subtitle = tool_subtitle_from_preview(
        tool_kind.as_deref(),
        provider_tool_name.as_deref(),
        input_meta.preview.as_ref(),
    );
    let input_truncated = update
        .get("input_truncated")
        .and_then(|v| v.as_bool())
        .or(input_meta.truncated);
    let input_original_bytes = update
        .get("input_original_bytes")
        .and_then(|v| v.as_i64())
        .or(input_meta.original_bytes);

    let output_meta = extract_tool_output_text(update)
        .map(|output| build_output_preview(&output))
        .filter(|preview| !preview.preview.trim().is_empty());
    let output_truncated = update
        .get("output_truncated")
        .and_then(|v| v.as_bool())
        .or(output_meta.as_ref().map(|preview| preview.truncated));
    let output_original_bytes = update
        .get("output_original_bytes")
        .and_then(|v| v.as_i64())
        .or(output_meta
            .as_ref()
            .map(|preview| preview.original_bytes as i64));

    Some(SessionTurnTool {
        session_id: event.session_id,
        tool_call_id,
        turn_id,
        tool_kind,
        provider_tool_name,
        title,
        subtitle,
        status,
        input_json: input_meta.preview,
        output_text: output_meta.as_ref().map(|preview| preview.preview.clone()),
        order_seq: read_event_order_seq(&event.payload_json)?,
        first_event_seq: Some(event.seq),
        input_truncated,
        input_original_bytes,
        output_truncated,
        output_original_bytes,
        created_at: event.created_at,
        updated_at: event.created_at,
    })
}

pub(super) fn tool_call_id_from_payload(payload: &Value) -> Option<String> {
    let direct = payload.get("tool_call_id").and_then(|v| v.as_str());
    if let Some(v) = direct {
        return Some(v.to_string());
    }
    let update = extract_tool_update(payload);
    let direct = update
        .get("toolCallId")
        .and_then(|v| v.as_str())
        .or_else(|| update.get("tool_call_id").and_then(|v| v.as_str()));
    if let Some(v) = direct {
        return Some(v.to_string());
    }
    let from_raw = update
        .pointer("/rawInput/call_id")
        .and_then(|v| v.as_str())
        .or_else(|| {
            update
                .pointer("/raw_input/call_id")
                .and_then(|v| v.as_str())
        });
    from_raw.map(|v| v.to_string())
}

pub(super) fn extract_tool_output_text(update: &Value) -> Option<String> {
    let direct = update
        .get("outputText")
        .and_then(|v| v.as_str())
        .or_else(|| update.get("output_text").and_then(|v| v.as_str()))
        .or_else(|| update.get("output_preview").and_then(|v| v.as_str()))
        .or_else(|| {
            update
                .pointer("/toolCall/outputText")
                .and_then(|v| v.as_str())
        })
        .or_else(|| {
            update
                .pointer("/toolCall/output_text")
                .and_then(|v| v.as_str())
        })
        .or_else(|| update.get("result").and_then(|v| v.as_str()))
        .or_else(|| {
            update
                .pointer("/rawOutput/aggregated_output")
                .and_then(|v| v.as_str())
        })
        .or_else(|| update.pointer("/rawOutput/output").and_then(|v| v.as_str()));
    if let Some(v) = direct {
        let trimmed = v.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    let blocks = update.get("content").and_then(|v| v.as_array())?;
    let mut out = String::new();
    for b in blocks {
        if let Some(t) = b
            .get("content")
            .and_then(|c| c.get("text"))
            .and_then(|v| v.as_str())
        {
            out.push_str(t);
        } else if let Some(t) = b.get("text").and_then(|v| v.as_str()) {
            out.push_str(t);
        }
    }
    if out.trim().is_empty() {
        None
    } else {
        Some(out.trim().to_string())
    }
}

pub(super) fn build_turn_tools_from_events(
    session_id: SessionId,
    turn_id: TurnId,
    events: &[SessionEvent],
) -> Vec<SessionTurnTool> {
    #[derive(Default)]
    struct ToolAgg {
        order_seq: Option<i64>,
        tool_kind: Option<String>,
        provider_tool_name: Option<String>,
        title: Option<String>,
        subtitle: Option<String>,
        status: Option<String>,
        input_json: Option<Value>,
        output_text: Option<String>,
        input_truncated: Option<bool>,
        input_original_bytes: Option<i64>,
        output_truncated: Option<bool>,
        output_original_bytes: Option<i64>,
        created_at: DateTime<Utc>,
        updated_at: DateTime<Utc>,
        first_event_seq: Option<i64>,
        initialized: bool,
    }

    let mut map: HashMap<String, ToolAgg> = HashMap::new();

    for ev in events {
        if !matches!(
            ev.event_type,
            SessionEventType::ToolCall
                | SessionEventType::ToolCallUpdate
                | SessionEventType::ToolResult
        ) {
            continue;
        }
        let tool_call_id = match tool_call_id_from_payload(&ev.payload_json) {
            Some(v) => v,
            None => continue,
        };
        let update = extract_tool_update(&ev.payload_json);
        let entry = map.entry(tool_call_id.clone()).or_default();
        if !entry.initialized {
            entry.created_at = ev.created_at;
            entry.updated_at = ev.created_at;
            entry.first_event_seq = Some(ev.seq);
            entry.order_seq = read_event_order_seq(&ev.payload_json);
            entry.initialized = true;
        } else {
            entry.updated_at = ev.created_at;
            if entry
                .first_event_seq
                .is_none_or(|existing| ev.seq < existing)
            {
                entry.first_event_seq = Some(ev.seq);
                entry.created_at = ev.created_at;
            }
            if let Some(order_seq) = read_event_order_seq(&ev.payload_json) {
                entry.order_seq = Some(match entry.order_seq {
                    Some(existing) => existing.min(order_seq),
                    None => order_seq,
                });
            }
        }

        if let Some(v) = tool_kind_from_update(update) {
            entry.tool_kind = Some(v);
        }
        if let Some(v) = tool_name_from_update(update) {
            entry.provider_tool_name = Some(v);
        }
        if let Some(v) = tool_title_from_update(update) {
            entry.title = Some(v);
        }
        if let Some(raw) = tool_status_from_update(update) {
            let normalized = normalize_tool_status(raw, ev.event_type.clone());
            entry.status = Some(merge_tool_status(entry.status.as_deref(), &normalized));
        } else if matches!(ev.event_type, SessionEventType::ToolResult) {
            entry.status = Some(merge_tool_status(entry.status.as_deref(), "completed"));
        } else if matches!(ev.event_type, SessionEventType::ToolCall) && entry.status.is_none() {
            entry.status = Some("pending".to_string());
        }

        let input = update
            .pointer("/rawInput")
            .or_else(|| update.pointer("/toolCall/rawInput"))
            .or_else(|| update.pointer("/toolCall/input"))
            .or_else(|| update.pointer("/input"))
            .or_else(|| update.pointer("/args"));
        let input_preview = tool_input_preview_from_value(input);
        let input_preview = input_preview.or_else(|| update.get("input_preview").cloned());
        let input_meta = build_json_preview(input, input_preview);
        if let Some(value) = input_meta.preview {
            entry.input_json = Some(value);
        }
        if let Some(value) = tool_subtitle_from_preview(
            entry.tool_kind.as_deref(),
            entry.provider_tool_name.as_deref(),
            entry.input_json.as_ref(),
        ) {
            entry.subtitle = Some(value);
        }
        if let Some(value) = update
            .get("input_truncated")
            .and_then(|v| v.as_bool())
            .or(input_meta.truncated)
        {
            entry.input_truncated = Some(value);
        }
        if let Some(value) = update
            .get("input_original_bytes")
            .and_then(|v| v.as_i64())
            .or(input_meta.original_bytes)
        {
            entry.input_original_bytes = Some(value);
        }

        if let Some(output) = extract_tool_output_text(update) {
            let preview = build_output_preview(&output);
            entry.output_text = Some(preview.preview.clone());
            entry.output_truncated = Some(
                update
                    .get("output_truncated")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(preview.truncated),
            );
            entry.output_original_bytes = Some(
                update
                    .get("output_original_bytes")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(preview.original_bytes as i64),
            );
        }
    }

    let mut out = Vec::with_capacity(map.len());
    for (tool_call_id, agg) in map {
        if !agg.initialized {
            continue;
        }
        out.push(SessionTurnTool {
            session_id,
            tool_call_id,
            turn_id,
            tool_kind: agg.tool_kind,
            provider_tool_name: agg.provider_tool_name,
            title: agg.title,
            subtitle: agg.subtitle,
            status: agg.status,
            input_json: agg.input_json,
            output_text: agg.output_text,
            order_seq: match agg.order_seq {
                Some(order_seq) => order_seq,
                None => continue,
            },
            first_event_seq: agg.first_event_seq,
            input_truncated: agg.input_truncated,
            input_original_bytes: agg.input_original_bytes,
            output_truncated: agg.output_truncated,
            output_original_bytes: agg.output_original_bytes,
            created_at: agg.created_at,
            updated_at: agg.updated_at,
        });
    }
    out.sort_by(compare_tool_order);
    out
}
