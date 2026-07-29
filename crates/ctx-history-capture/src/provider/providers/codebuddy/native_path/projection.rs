use super::*;

pub(super) fn validate_page_bounds(units: usize, bytes: usize) -> Result<()> {
    if units == 0 || units > CODEBUDDY_NATIVE_PAGE_MAX_UNITS {
        return Err(CaptureError::InvalidPayload(format!(
            "CodeBuddy NativePath page has {units} logical units"
        )));
    }
    if bytes > CODEBUDDY_NATIVE_PAGE_MAX_BYTES {
        return Err(CaptureError::InvalidPayload(format!(
            "CodeBuddy NativePath page has {bytes} conservatively encoded bytes"
        )));
    }
    Ok(())
}

pub(super) fn record_scan_rejection(
    state: &mut CodeBuddyScanState,
    line: usize,
    error: String,
) -> Result<()> {
    state.rejected_records =
        state
            .rejected_records
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "CodeBuddy rejection count overflowed",
            ))?;
    if state.failures.len() < CODEBUDDY_MAX_SCAN_REJECTIONS {
        state.failures.push(CodeBuddyScanRejection {
            line,
            error: bounded_failure(error),
        });
    }
    Ok(())
}

pub(super) fn bounded_failure(mut error: String) -> String {
    if error.is_empty() {
        return "CodeBuddy record was deterministically rejected".to_owned();
    }
    if error.len() <= CODEBUDDY_MAX_FAILURE_BYTES {
        return error;
    }
    let mut boundary = CODEBUDDY_MAX_FAILURE_BYTES;
    while !error.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    error.truncate(boundary);
    error
}

pub(super) fn update_cli_session(
    session: &mut CodeBuddySessionState,
    value: &Value,
    imported_at: DateTime<Utc>,
) {
    if let Some(session_id) = value
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.len() <= CODEBUDDY_MAX_NATIVE_ID_BYTES)
    {
        session.native_session_id = session_id.to_owned();
    }
    if session.cwd.is_none() {
        session.cwd = value
            .get("cwd")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty() && value.len() <= 8 * 1024)
            .map(str::to_owned);
    }
    let Some(occurred_at) = cli_message_time(value, imported_at) else {
        return;
    };
    let prior_start = session
        .started_at
        .as_deref()
        .and_then(|value| value.parse::<DateTime<Utc>>().ok());
    let prior_end = session
        .ended_at
        .as_deref()
        .and_then(|value| value.parse::<DateTime<Utc>>().ok());
    session.started_at = Some(
        prior_start
            .map(|prior| prior.min(occurred_at))
            .unwrap_or(occurred_at)
            .to_rfc3339(),
    );
    session.ended_at = Some(
        prior_end
            .map(|prior| prior.max(occurred_at))
            .unwrap_or(occurred_at)
            .to_rfc3339(),
    );
}

#[allow(clippy::too_many_arguments)]
pub(super) fn cli_core_row(
    context: &ProviderAdapterContext,
    session: &mut CodeBuddySessionState,
    session_title: &mut Option<String>,
    ordinal: u64,
    physical_line: usize,
    byte_start: u64,
    byte_end_exclusive: u64,
    record_bytes: &[u8],
    value: Value,
) -> Result<CodeBuddyRecordClassification> {
    let text = cli_message_text(&value);
    let role = value.get("role").and_then(Value::as_str).map(str::to_owned);
    let ref_type = value.get("type").and_then(Value::as_str).map(str::to_owned);
    let event_type = codebuddy_event_type(role.as_deref(), ref_type.as_deref(), &value);
    let explicit_native_message_id = codebuddy_cli_explicit_native_message_id(&value);
    let native_message_id = explicit_native_message_id
        .clone()
        .unwrap_or_else(|| format!("line-{physical_line}"));
    let occurred_at = cli_message_time(&value, context.imported_at).unwrap_or(context.imported_at);
    if !codebuddy_is_message_record(role.as_deref(), ref_type.as_deref()) {
        return Ok(CodeBuddyRecordClassification::SkippedMetadata);
    }
    if session_title.is_none()
        && session.generated_title_anchor.is_none()
        && provider_role(role.as_deref()) == EventRole::User
    {
        *session_title = codebuddy_title_from_text(&text);
        if session_title.is_some() {
            session.generated_title_anchor = Some(CodeBuddyGeneratedTitleAnchor::Cli {
                native_ordinal: ordinal,
                byte_start,
                byte_end_exclusive,
                payload_sha256: sha256_hex(record_bytes),
            });
        }
    }
    let provider_session_id = session.provider_session_id();
    let started_at = session.started_at()?.unwrap_or(occurred_at);
    let (session, event) = codebuddy_normalized_rows(
        &CodeBuddySessionInput {
            provider_session_id: &provider_session_id,
            started_at,
            ended_at: session.ended_at()?,
            cwd: session.cwd.as_deref(),
        },
        CodeBuddyEventInput {
            provider_event_index: stable_provider_event_index(
                explicit_native_message_id.as_deref(),
                ordinal,
            ),
            native_message_id,
            event_type,
            role,
            occurred_at,
            text,
        },
    );
    Ok(CodeBuddyRecordClassification::AcceptedMessage(
        CodeBuddyCoreRow { session, event },
    ))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn extension_core_row(
    context: &ProviderAdapterContext,
    metadata: &CodeBuddyExtensionMetadata,
    session: &mut CodeBuddySessionState,
    session_title: &mut Option<String>,
    ordinal: u64,
    message_index: usize,
    message_ref: &Value,
    message_path: &Path,
    _record_bytes: &[u8],
    raw_message: Value,
) -> Result<CodeBuddyRecordClassification> {
    let decoded_message = codebuddy_decoded_message(&raw_message);
    let text = codebuddy_message_text(&decoded_message, &raw_message);
    let role = message_ref
        .get("role")
        .and_then(Value::as_str)
        .or_else(|| raw_message.get("role").and_then(Value::as_str))
        .map(str::to_owned);
    let ref_type = message_ref
        .get("type")
        .and_then(Value::as_str)
        .or_else(|| raw_message.get("type").and_then(Value::as_str))
        .map(str::to_owned);
    let event_type = codebuddy_event_type(role.as_deref(), ref_type.as_deref(), &decoded_message);
    let message_id = message_ref
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or(CaptureError::SystemInvariant(
            "CodeBuddy extension message lost its manifest identity",
        ))?
        .to_owned();
    let occurred_at = codebuddy_message_time(
        &raw_message,
        &decoded_message,
        message_path,
        context.imported_at,
    );
    update_session_times(session, occurred_at);
    if !codebuddy_is_message_record(role.as_deref(), ref_type.as_deref()) {
        return Ok(CodeBuddyRecordClassification::SkippedMetadata);
    }
    if session_title.is_none()
        && session.generated_title_anchor.is_none()
        && provider_role(role.as_deref()) == EventRole::User
    {
        *session_title = codebuddy_title_from_text(&text);
        if session_title.is_some() {
            session.generated_title_anchor = Some(CodeBuddyGeneratedTitleAnchor::Extension {
                message_index: message_index as u64,
            });
        }
    }
    let provider_session_id = session.provider_session_id();
    let cwd = codebuddy_extension_metadata_text(
        metadata,
        &["projectPath", "project_path", "cwd", "workspace"],
    );
    let (session, event) = codebuddy_normalized_rows(
        &CodeBuddySessionInput {
            provider_session_id: &provider_session_id,
            started_at: session.started_at()?.unwrap_or(occurred_at),
            ended_at: session.ended_at()?,
            cwd: cwd.as_deref(),
        },
        CodeBuddyEventInput {
            provider_event_index: stable_provider_event_index(Some(&message_id), ordinal),
            native_message_id: message_id,
            event_type,
            role,
            occurred_at,
            text,
        },
    );
    Ok(CodeBuddyRecordClassification::AcceptedMessage(
        CodeBuddyCoreRow { session, event },
    ))
}

pub(super) fn codebuddy_is_message_record(role: Option<&str>, ref_type: Option<&str>) -> bool {
    match ref_type.map(str::trim).filter(|value| !value.is_empty()) {
        Some(kind) => kind.eq_ignore_ascii_case("message"),
        None => matches!(
            provider_role(role),
            EventRole::User | EventRole::Assistant | EventRole::System
        ),
    }
}

pub(super) fn stable_provider_event_index(
    native_message_id: Option<&str>,
    native_ordinal: u64,
) -> u64 {
    let Some(native_message_id) = native_message_id else {
        return native_ordinal;
    };
    let mut digest = Sha256::new();
    digest.update(b"ctx-codebuddy-native-message-index-v1\0");
    digest.update((native_message_id.len() as u64).to_be_bytes());
    digest.update(native_message_id.as_bytes());
    u64::from_be_bytes(
        digest.finalize()[..8]
            .try_into()
            .expect("SHA-256 is 32 bytes"),
    )
}

pub(super) fn codebuddy_cli_explicit_native_message_id(value: &Value) -> Option<String> {
    value
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| {
            !id.is_empty()
                && id.len() <= CODEBUDDY_MAX_NATIVE_ID_BYTES
                && !id.chars().any(char::is_control)
        })
        .map(str::to_owned)
}

pub(super) fn update_session_times(
    session: &mut CodeBuddySessionState,
    occurred_at: DateTime<Utc>,
) {
    let started = session
        .started_at
        .as_deref()
        .and_then(|value| value.parse::<DateTime<Utc>>().ok())
        .map(|value| value.min(occurred_at))
        .unwrap_or(occurred_at);
    let ended = session
        .ended_at
        .as_deref()
        .and_then(|value| value.parse::<DateTime<Utc>>().ok())
        .map(|value| value.max(occurred_at))
        .unwrap_or(occurred_at);
    session.started_at = Some(started.to_rfc3339());
    session.ended_at = Some(ended.to_rfc3339());
}

pub(super) fn codebuddy_event_type(
    role: Option<&str>,
    ref_type: Option<&str>,
    value: &Value,
) -> EventType {
    let role = role.unwrap_or_default().to_ascii_lowercase();
    let kind = ref_type.unwrap_or_default().to_ascii_lowercase();
    if matches!(role.as_str(), "tool" | "function")
        || kind.contains("tool_result")
        || kind.contains("tool-result")
        || kind.contains("tool_output")
        || kind.contains("tool-output")
        || kind == "result"
        || kind == "output"
        || value.get("toolUseResult").is_some()
        || value.get("tool_result").is_some()
    {
        EventType::ToolOutput
    } else if kind.contains("tool_call")
        || kind.contains("tool-call")
        || value.get("toolUse").is_some()
        || value.get("tool_call").is_some()
    {
        EventType::ToolCall
    } else {
        EventType::Message
    }
}

pub(super) fn cli_project_hash(path: &Path) -> String {
    path.parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty() && *name != "projects")
        .map(str::to_owned)
        .unwrap_or_else(|| "unknown-project".to_owned())
}

pub(super) fn cli_message_text(value: &Value) -> String {
    let text = value
        .get("content")
        .and_then(provider_value_text)
        .or_else(|| {
            value
                .pointer("/message/content")
                .and_then(provider_value_text)
        })
        .unwrap_or_default();
    codebuddy_clean_content(&text)
}

pub(super) fn cli_message_time(value: &Value, fallback: DateTime<Utc>) -> Option<DateTime<Utc>> {
    value
        .get("timestamp")
        .and_then(Value::as_i64)
        .and_then(DateTime::<Utc>::from_timestamp_millis)
        .or_else(|| {
            value
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(crate::common::time::parse_rfc3339_utc)
        })
        .or_else(|| {
            value
                .get("__timestamp")
                .and_then(Value::as_str)
                .and_then(crate::common::time::parse_rfc3339_utc)
        })
        .or(Some(fallback))
}
