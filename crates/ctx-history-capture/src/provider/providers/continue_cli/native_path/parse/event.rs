use super::*;

#[derive(Default)]
pub(super) struct ParsedMessageContent {
    pub(super) text: Vec<String>,
    pub(super) calls: Vec<RawContinueMessageCall>,
    pub(super) admitted: bool,
}

pub(super) enum ParsedContentBlock {
    Text(String),
    Call(RawContinueMessageCall),
}

pub(super) fn parse_message_content(
    value: JsonSpan<'_>,
    stats: &mut ContinueOutputExclusionStats,
) -> Result<ParsedMessageContent, String> {
    match value.kind() {
        JsonKind::String => Ok(ParsedMessageContent {
            text: retained_unbounded_string(value, stats)?
                .into_iter()
                .collect(),
            calls: Vec::new(),
            admitted: true,
        }),
        JsonKind::Array => {
            let mut content = ParsedMessageContent::default();
            for block in value.as_array().map_err(scan_error)? {
                let block = block.map_err(scan_error)?;
                match block.kind() {
                    JsonKind::String => {
                        if let Some(text) = retained_unbounded_string(block, stats)? {
                            content.text.push(text);
                            content.admitted = true;
                        }
                    }
                    JsonKind::Object => {
                        if let Some(block) = parse_proven_content_block(block, stats)? {
                            content.admitted = true;
                            match block {
                                ParsedContentBlock::Text(text) => content.text.push(text),
                                ParsedContentBlock::Call(call) => content.calls.push(call),
                            }
                        }
                    }
                    JsonKind::Null => {}
                    JsonKind::Bool | JsonKind::Number | JsonKind::Array => {
                        record_unproven(stats, block);
                    }
                }
            }
            Ok(content)
        }
        JsonKind::Object => {
            let mut content = ParsedMessageContent::default();
            if let Some(block) = parse_proven_content_block(value, stats)? {
                content.admitted = true;
                match block {
                    ParsedContentBlock::Text(text) => content.text.push(text),
                    ParsedContentBlock::Call(call) => content.calls.push(call),
                }
            }
            Ok(content)
        }
        JsonKind::Null => Ok(ParsedMessageContent::default()),
        JsonKind::Bool | JsonKind::Number => {
            record_unproven(stats, value);
            Ok(ParsedMessageContent::default())
        }
    }
}

pub(super) fn parse_proven_content_block(
    block: JsonSpan<'_>,
    stats: &mut ContinueOutputExclusionStats,
) -> Result<Option<ParsedContentBlock>, String> {
    let mut admission = TagAdmission::Missing;
    for field in block.as_object().map_err(scan_error)? {
        let (key, value) = field.map_err(scan_error)?;
        if key.is("type") || key.is("kind") {
            admission = admission.observe(value);
        }
    }
    match admission {
        TagAdmission::Text => {
            parse_text_block(block, stats).map(|text| text.map(ParsedContentBlock::Text))
        }
        TagAdmission::Call => {
            parse_message_call_block(block, stats).map(|call| call.map(ParsedContentBlock::Call))
        }
        TagAdmission::Result => {
            record_result(stats, block);
            Ok(None)
        }
        TagAdmission::Missing | TagAdmission::Context | TagAdmission::Unknown => {
            record_unproven(stats, block);
            Ok(None)
        }
    }
}

pub(super) fn parse_text_block(
    block: JsonSpan<'_>,
    stats: &mut ContinueOutputExclusionStats,
) -> Result<Option<String>, String> {
    let mut text = None;
    let mut structurally_safe = true;
    for field in block.as_object().map_err(scan_error)? {
        let (key, value) = field.map_err(scan_error)?;
        if key.is("type") || key.is("kind") {
        } else if key.is("text") || key.is("content") {
            if value.kind() == JsonKind::String && text.is_none() {
                text = Some(value);
            } else {
                structurally_safe = false;
            }
        } else {
            structurally_safe = false;
        }
    }
    if !structurally_safe {
        record_unproven(stats, block);
        return Ok(None);
    }
    match text {
        Some(text) => retained_unbounded_string(text, stats),
        None => Ok(None),
    }
}

pub(super) fn parse_message_call_block(
    block: JsonSpan<'_>,
    stats: &mut ContinueOutputExclusionStats,
) -> Result<Option<RawContinueMessageCall>, String> {
    let mut structurally_safe = true;
    let mut contains_result = false;
    let mut saw_id = false;
    let mut saw_name = false;
    let mut saw_function = false;
    for field in block.as_object().map_err(scan_error)? {
        let (key, value) = field.map_err(scan_error)?;
        if key.is("type")
            || key.is("kind")
            || key.is("id")
            || key.is("name")
            || key.is("function")
            || key.is("arguments")
            || key.is("input")
            || key.is("parameters")
        {
            if key.is("id") {
                structurally_safe &= !saw_id;
                saw_id = true;
            } else if key.is("name") {
                structurally_safe &= !saw_name;
                saw_name = true;
            } else if key.is("function") {
                structurally_safe &= !saw_function;
                saw_function = true;
            }
            if (key.is("id") || key.is("name"))
                && !matches!(value.kind(), JsonKind::String | JsonKind::Null)
                || key.is("function") && !matches!(value.kind(), JsonKind::Object | JsonKind::Null)
            {
                structurally_safe = false;
            }
        } else if key.is_result_like() {
            contains_result = true;
        } else {
            structurally_safe = false;
        }
    }
    if contains_result {
        record_result(stats, block);
        return Ok(None);
    }
    if !structurally_safe {
        record_unproven(stats, block);
        return Ok(None);
    }

    let mut id = None;
    let kind = Some("tool_call".to_owned());
    let mut direct_name = None;
    let mut function_name = None;
    let mut file_touches = Vec::new();
    for field in block.as_object().map_err(scan_error)? {
        let (key, value) = field.map_err(scan_error)?;
        if key.is("id") {
            id = retained_bounded_string(value, MAX_CALL_ID_BYTES, stats)?;
        } else if key.is("type") || key.is("kind") {
        } else if key.is("name") {
            direct_name = retained_bounded_string(value, MAX_TOOL_NAME_BYTES, stats)?;
        } else if key.is("function") {
            function_name = parse_tool_function(value, stats)?;
            file_touches.extend(extract_continue_file_touches(value)?);
        } else if key.is("arguments") || key.is("input") || key.is("parameters") {
            record_call_body(stats, value);
            file_touches.extend(extract_continue_file_touches(value)?);
        }
    }
    Ok(Some(RawContinueMessageCall {
        id,
        kind,
        name: function_name.or(direct_name),
        file_touches,
    }))
}

pub(super) fn parse_context_items(
    value: JsonSpan<'_>,
    stats: &mut ContinueOutputExclusionStats,
) -> Result<Vec<RawContinueContextItem>, String> {
    if value.kind() != JsonKind::Array {
        if value.kind() != JsonKind::Null {
            record_unproven(stats, value);
        }
        return Ok(Vec::new());
    }
    let mut contexts = Vec::new();
    for context in value.as_array().map_err(scan_error)? {
        let context = context.map_err(scan_error)?;
        if context.kind() != JsonKind::Object {
            if context.kind() != JsonKind::Null {
                record_unproven(stats, context);
            }
            continue;
        }
        if let Some(context) = parse_context_item(context, stats)? {
            contexts.push(context);
        }
    }
    Ok(contexts)
}

pub(super) fn parse_context_item(
    context: JsonSpan<'_>,
    stats: &mut ContinueOutputExclusionStats,
) -> Result<Option<RawContinueContextItem>, String> {
    let mut admission = TagAdmission::Missing;
    let mut name = None;
    let mut content = None;
    let mut description = None;
    let mut structurally_safe = true;
    for field in context.as_object().map_err(scan_error)? {
        let (key, value) = field.map_err(scan_error)?;
        if key.is("type") || key.is("kind") {
            admission = admission.observe(value);
        } else if key.is("name") {
            if value.kind() == JsonKind::String && name.is_none() {
                name = Some(value);
            } else {
                structurally_safe = false;
            }
        } else if key.is("content") {
            if value.kind() == JsonKind::String && content.is_none() {
                content = Some(value);
            } else {
                structurally_safe = false;
            }
        } else if key.is("description") {
            if description.is_some() {
                structurally_safe = false;
            }
            description = Some(value);
            structurally_safe &= matches!(value.kind(), JsonKind::String | JsonKind::Null);
        } else if key.is("uri") {
            structurally_safe &= matches!(value.kind(), JsonKind::String | JsonKind::Null);
        } else {
            structurally_safe = false;
        }
    }
    if admission == TagAdmission::Result {
        record_result(stats, context);
        return Ok(None);
    }
    if name.is_some_and(is_result_tag) || description.is_some_and(is_result_tag) {
        record_result(stats, context);
        return Ok(None);
    }
    if !structurally_safe || !matches!(admission, TagAdmission::Text | TagAdmission::Context) {
        record_unproven(stats, context);
        return Ok(None);
    }
    let name = match name {
        Some(value) => retained_bounded_string(value, MAX_SESSION_METADATA_STRING_BYTES, stats)?,
        None => None,
    };
    let content = match content {
        Some(value) => retained_unbounded_string(value, stats)?,
        None => None,
    };
    Ok((name.is_some() || content.is_some()).then_some(RawContinueContextItem { name, content }))
}

pub(super) fn parse_tool_call_states(
    value: JsonSpan<'_>,
    stats: &mut ContinueOutputExclusionStats,
) -> Result<Vec<RawContinueToolCallState>, String> {
    if value.kind() != JsonKind::Array {
        if value.kind() != JsonKind::Null {
            record_unproven(stats, value);
        }
        return Ok(Vec::new());
    }
    let mut states = Vec::new();
    for state in value.as_array().map_err(scan_error)? {
        let state = state.map_err(scan_error)?;
        if state.kind() != JsonKind::Object {
            if state.kind() != JsonKind::Null {
                record_unproven(stats, state);
            }
            continue;
        }
        if let Some(state) = parse_tool_call_state(state, stats)? {
            states.push(state);
        }
    }
    Ok(states)
}

pub(super) fn parse_tool_call_state(
    state: JsonSpan<'_>,
    stats: &mut ContinueOutputExclusionStats,
) -> Result<Option<RawContinueToolCallState>, String> {
    let mut structurally_safe = true;
    let mut has_request_identity = false;
    let mut saw_tool_call_id = false;
    let mut saw_tool_call = false;
    let mut saw_status = false;
    let mut saw_exit_code = false;
    let mut saw_duration_ms = false;
    let mut saw_timed_out = false;
    for field in state.as_object().map_err(scan_error)? {
        let (key, value) = field.map_err(scan_error)?;
        if key.is_result_like() {
            record_result(stats, value);
            continue;
        }
        let known = key.is("toolCallId")
            || key.is("toolCall")
            || key.is("status")
            || key.is("exitCode")
            || key.is("durationMs")
            || key.is("timedOut");
        let duplicate = if key.is("toolCallId") {
            let duplicate = saw_tool_call_id;
            saw_tool_call_id = true;
            duplicate
        } else if key.is("toolCall") {
            let duplicate = saw_tool_call;
            saw_tool_call = true;
            duplicate
        } else if key.is("status") {
            let duplicate = saw_status;
            saw_status = true;
            duplicate
        } else if key.is("exitCode") {
            let duplicate = saw_exit_code;
            saw_exit_code = true;
            duplicate
        } else if key.is("durationMs") {
            let duplicate = saw_duration_ms;
            saw_duration_ms = true;
            duplicate
        } else if key.is("timedOut") {
            let duplicate = saw_timed_out;
            saw_timed_out = true;
            duplicate
        } else {
            false
        };
        has_request_identity |= key.is("toolCallId") || key.is("toolCall");
        let wrong_kind = (key.is("toolCallId") || key.is("status"))
            && !matches!(value.kind(), JsonKind::String | JsonKind::Null)
            || key.is("toolCall") && !matches!(value.kind(), JsonKind::Object | JsonKind::Null)
            || (key.is("exitCode") || key.is("durationMs"))
                && !matches!(value.kind(), JsonKind::Number | JsonKind::Null)
            || key.is("timedOut") && !matches!(value.kind(), JsonKind::Bool | JsonKind::Null);
        if !known || duplicate || wrong_kind {
            structurally_safe = false;
        }
    }
    if !structurally_safe {
        record_unproven(stats, state);
        return Ok(None);
    }

    let mut tool_call_id = None;
    let mut tool_call = None;
    let mut status = None;
    let mut exit_code = None;
    let mut duration_ms = None;
    let mut timed_out = None;
    for field in state.as_object().map_err(scan_error)? {
        let (key, value) = field.map_err(scan_error)?;
        if key.is("toolCallId") {
            tool_call_id = retained_bounded_string(value, MAX_CALL_ID_BYTES, stats)?;
        } else if key.is("toolCall") {
            tool_call = parse_tool_call(value, stats)?;
        } else if key.is("status") {
            status = retained_bounded_string(value, MAX_TOOL_STATUS_BYTES, stats)?;
        } else if key.is("exitCode") {
            exit_code = decode_i64(value);
        } else if key.is("durationMs") {
            duration_ms = decode_i64(value);
        } else if key.is("timedOut") {
            timed_out = decode_bool(value);
        }
    }
    Ok(
        (has_request_identity && (tool_call_id.is_some() || tool_call.is_some())).then_some(
            RawContinueToolCallState {
                tool_call_id,
                tool_call,
                status,
                exit_code,
                duration_ms,
                timed_out,
            },
        ),
    )
}

pub(super) fn parse_tool_call(
    value: JsonSpan<'_>,
    stats: &mut ContinueOutputExclusionStats,
) -> Result<Option<RawContinueToolCall>, String> {
    if value.kind() != JsonKind::Object {
        if value.kind() != JsonKind::Null {
            record_unproven(stats, value);
        }
        return Ok(None);
    }
    let mut structurally_safe = true;
    let mut admission = TagAdmission::Missing;
    let mut has_request_identity = false;
    let mut saw_id = false;
    let mut saw_name = false;
    let mut saw_function = false;
    for field in value.as_object().map_err(scan_error)? {
        let (key, field_value) = field.map_err(scan_error)?;
        if key.is_result_like() {
            record_result(stats, field_value);
            continue;
        }
        if key.is("type") || key.is("kind") {
            admission = admission.observe(field_value);
        }
        let known = key.is("id")
            || key.is("type")
            || key.is("kind")
            || key.is("name")
            || key.is("function")
            || key.is("arguments")
            || key.is("input")
            || key.is("parameters");
        let duplicate = if key.is("id") {
            let duplicate = saw_id;
            saw_id = true;
            duplicate
        } else if key.is("name") {
            let duplicate = saw_name;
            saw_name = true;
            duplicate
        } else if key.is("function") {
            let duplicate = saw_function;
            saw_function = true;
            duplicate
        } else {
            false
        };
        has_request_identity |= key.is("id") || key.is("name") || key.is("function");
        let wrong_kind = (key.is("id") || key.is("type") || key.is("kind") || key.is("name"))
            && !matches!(field_value.kind(), JsonKind::String | JsonKind::Null)
            || key.is("function")
                && !matches!(field_value.kind(), JsonKind::Object | JsonKind::Null);
        if !known || duplicate || wrong_kind {
            structurally_safe = false;
        }
    }
    if admission == TagAdmission::Result {
        record_result(stats, value);
        return Ok(None);
    }
    if !structurally_safe
        || !has_request_identity
        || !matches!(admission, TagAdmission::Missing | TagAdmission::Call)
    {
        record_unproven(stats, value);
        return Ok(None);
    }

    let mut id = None;
    let mut kind = None;
    let mut name = None;
    let mut function_name = None;
    let mut file_touches = Vec::new();
    for field in value.as_object().map_err(scan_error)? {
        let (key, value) = field.map_err(scan_error)?;
        if key.is("id") {
            id = retained_bounded_string(value, MAX_CALL_ID_BYTES, stats)?;
        } else if key.is("type") || key.is("kind") {
            kind = Some("tool_call".to_owned());
        } else if key.is("name") {
            name = retained_bounded_string(value, MAX_TOOL_NAME_BYTES, stats)?;
        } else if key.is("function") {
            function_name = parse_tool_function(value, stats)?;
            file_touches.extend(extract_continue_file_touches(value)?);
        } else if key.is("arguments") || key.is("input") || key.is("parameters") {
            record_call_body(stats, value);
            file_touches.extend(extract_continue_file_touches(value)?);
        }
    }
    let retained_request_identity = id.is_some() || name.is_some() || function_name.is_some();
    Ok(retained_request_identity.then_some(RawContinueToolCall {
        id,
        kind,
        name,
        function_name,
        file_touches,
    }))
}

pub(super) fn extract_continue_file_touches(
    value: JsonSpan<'_>,
) -> Result<Vec<ContinueFileTouch>, String> {
    let value = serde_json::from_slice::<Value>(value.raw())
        .map_err(|error| format!("invalid Continue tool request body: {error}"))?;
    let mut touches = Vec::new();
    let outcome = visit_provider_file_touch_drafts_with_limit(
        &value,
        true,
        CONTINUE_NATIVE_MAX_FILE_TOUCHES_PER_EVENT,
        |(_, touch)| {
            touches.push(ContinueFileTouch {
                path: touch.path,
                old_path: touch.old_path,
                change_kind: touch.change_kind,
                confidence: touch.confidence,
                metadata: touch.metadata,
            });
            Ok::<(), String>(())
        },
    )?;
    if outcome.limit_exceeded() {
        return Err(format!(
            "Continue tool request exceeds the {CONTINUE_NATIVE_MAX_FILE_TOUCHES_PER_EVENT} \
             unique file-touch transaction bound"
        ));
    }
    Ok(touches)
}

pub(super) fn parse_tool_function(
    value: JsonSpan<'_>,
    stats: &mut ContinueOutputExclusionStats,
) -> Result<Option<String>, String> {
    if value.kind() != JsonKind::Object {
        if value.kind() != JsonKind::Null {
            record_unproven(stats, value);
        }
        return Ok(None);
    }
    let mut structurally_safe = true;
    for field in value.as_object().map_err(scan_error)? {
        let (key, field_value) = field.map_err(scan_error)?;
        if !(key.is("name")
            || key.is("arguments")
            || key.is("input")
            || key.is("parameters")
            || key.is_result_like())
        {
            structurally_safe = false;
        }
        if key.is("name") && !matches!(field_value.kind(), JsonKind::String | JsonKind::Null) {
            structurally_safe = false;
        }
    }
    if !structurally_safe {
        record_unproven(stats, value);
        return Ok(None);
    }

    let mut name = None;
    for field in value.as_object().map_err(scan_error)? {
        let (key, value) = field.map_err(scan_error)?;
        if key.is("name") {
            name = retained_bounded_string(value, MAX_TOOL_NAME_BYTES, stats)?;
        } else if key.is("arguments") || key.is("input") || key.is("parameters") {
            record_call_body(stats, value);
        } else if key.is_result_like() {
            record_result(stats, value);
        }
    }
    Ok(name)
}

pub(super) fn parse_timestamp(
    value: JsonSpan<'_>,
    stats: &mut ContinueOutputExclusionStats,
) -> Result<Option<RawTimestamp>, String> {
    match value.kind() {
        JsonKind::String => Ok(retained_bounded_string(value, 128, stats)?.map(RawTimestamp::Text)),
        JsonKind::Number => Ok(decode_f64(value).map(RawTimestamp::Number)),
        JsonKind::Null | JsonKind::Bool | JsonKind::Array | JsonKind::Object => Ok(None),
    }
}

pub(super) fn retained_bounded_string(
    value: JsonSpan<'_>,
    maximum_bytes: usize,
    stats: &mut ContinueOutputExclusionStats,
) -> Result<Option<String>, String> {
    let value = decode_string(value, maximum_bytes).map_err(|error| error.to_string())?;
    if let Some(value) = value.as_ref() {
        record_retained_string(stats, value);
    }
    Ok(value)
}

pub(super) fn retained_unbounded_string(
    value: JsonSpan<'_>,
    stats: &mut ContinueOutputExclusionStats,
) -> Result<Option<String>, String> {
    let value = decode_unbounded_string(value).map_err(|error| error.to_string())?;
    if let Some(value) = value.as_ref() {
        record_retained_string(stats, value);
    }
    Ok(value)
}

pub(super) fn record_retained_string(stats: &mut ContinueOutputExclusionStats, value: &str) {
    stats.retained_decode_string_allocations =
        stats.retained_decode_string_allocations.saturating_add(1);
    stats.retained_decode_string_bytes = stats
        .retained_decode_string_bytes
        .saturating_add(u64::try_from(value.len()).unwrap_or(u64::MAX));
}

pub(super) fn record_result(stats: &mut ContinueOutputExclusionStats, value: JsonSpan<'_>) {
    stats.native_results_observed = stats.native_results_observed.saturating_add(1);
    stats.result_payload_bytes_skipped = stats
        .result_payload_bytes_skipped
        .saturating_add(u64::try_from(value.encoded_len()).unwrap_or(u64::MAX));
}

pub(super) fn record_unproven(stats: &mut ContinueOutputExclusionStats, value: JsonSpan<'_>) {
    stats.unproven_payloads_skipped = stats.unproven_payloads_skipped.saturating_add(1);
    stats.result_payload_bytes_skipped = stats
        .result_payload_bytes_skipped
        .saturating_add(u64::try_from(value.encoded_len()).unwrap_or(u64::MAX));
}

pub(super) fn record_call_body(stats: &mut ContinueOutputExclusionStats, value: JsonSpan<'_>) {
    stats.call_body_bytes_skipped = stats
        .call_body_bytes_skipped
        .saturating_add(u64::try_from(value.encoded_len()).unwrap_or(u64::MAX));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum KnownRole {
    User,
    Assistant,
    System,
    Developer,
}

impl KnownRole {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::System => "system",
            Self::Developer => "developer",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RoleAdmission {
    Missing,
    Retained(KnownRole),
    Result,
    Unknown,
    Conflict,
}

impl RoleAdmission {
    pub(super) fn observe(self, role: JsonSpan<'_>) -> Self {
        let observed = if role.string_normalized_is("user") {
            Self::Retained(KnownRole::User)
        } else if role.string_normalized_is("assistant") {
            Self::Retained(KnownRole::Assistant)
        } else if role.string_normalized_is("system") {
            Self::Retained(KnownRole::System)
        } else if role.string_normalized_is("developer") {
            Self::Retained(KnownRole::Developer)
        } else if is_result_role(role) {
            Self::Result
        } else {
            Self::Unknown
        };
        match (self, observed) {
            (Self::Result, _) | (_, Self::Result) => Self::Result,
            (Self::Missing, observed) => observed,
            (Self::Retained(left), Self::Retained(right)) if left == right => Self::Retained(left),
            (Self::Unknown, Self::Unknown) => Self::Unknown,
            (Self::Conflict, _) | (_, Self::Conflict) => Self::Conflict,
            _ => Self::Conflict,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TagAdmission {
    Missing,
    Text,
    Call,
    Context,
    Result,
    Unknown,
}

impl TagAdmission {
    pub(super) fn observe(self, tag: JsonSpan<'_>) -> Self {
        let observed = if is_result_tag(tag) {
            Self::Result
        } else if is_safe_call_tag(tag) {
            Self::Call
        } else if is_safe_text_tag(tag) {
            Self::Text
        } else if is_safe_context_tag(tag) {
            Self::Context
        } else {
            Self::Unknown
        };
        match (self, observed) {
            (Self::Result, _) | (_, Self::Result) => Self::Result,
            (Self::Missing, observed) => observed,
            (left, right) if left == right => left,
            _ => Self::Unknown,
        }
    }
}

pub(super) fn is_safe_text_tag(tag: JsonSpan<'_>) -> bool {
    ["text", "inputtext", "markdown", "message"]
        .iter()
        .any(|candidate| tag.string_normalized_is(candidate))
}

pub(super) fn is_safe_context_tag(tag: JsonSpan<'_>) -> bool {
    ["text", "context", "file", "code", "snippet"]
        .iter()
        .any(|candidate| tag.string_normalized_is(candidate))
}

pub(super) fn is_safe_call_tag(tag: JsonSpan<'_>) -> bool {
    [
        "tooluse",
        "toolcall",
        "function",
        "functioncall",
        "command",
        "shellcommand",
    ]
    .iter()
    .any(|candidate| tag.string_normalized_is(candidate))
}

pub(super) fn is_result_tag(tag: JsonSpan<'_>) -> bool {
    [
        "toolresult",
        "tooloutput",
        "commandresult",
        "commandoutput",
        "shelloutput",
        "terminaloutput",
        "bashresult",
        "bashexecutionresult",
        "functioncalloutput",
        "customtoolcalloutput",
        "future",
        "futureoutput",
        "result",
        "output",
    ]
    .iter()
    .any(|candidate| tag.string_normalized_is(candidate))
}

pub(super) fn is_result_role(role: JsonSpan<'_>) -> bool {
    [
        "tool",
        "toolresult",
        "tooloutput",
        "command",
        "commandresult",
        "commandoutput",
        "bashexecution",
        "shelloutput",
        "functionresult",
    ]
    .iter()
    .any(|candidate| role.string_normalized_is(candidate))
}

pub(super) fn scan_error(error: impl std::fmt::Display) -> String {
    format!("invalid Continue JSON structure: {error}")
}
