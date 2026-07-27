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

pub(super) fn record_cursor_failure(
    cursor: &mut CodeBuddyNativeCursor,
    line: usize,
    error: String,
) -> Result<()> {
    cursor.rejected_records =
        cursor
            .rejected_records
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "CodeBuddy rejection count overflowed",
            ))?;
    if cursor.failures.len() < CODEBUDDY_MAX_CHECKPOINT_FAILURES {
        cursor.failures.push(CodeBuddyCursorFailure {
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
    session: &mut CodeBuddySessionCheckpoint,
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
    source: &CodeBuddySource,
    context: &ProviderAdapterContext,
    session: &mut CodeBuddySessionCheckpoint,
    session_title: &mut Option<String>,
    ordinal: u64,
    physical_line: usize,
    byte_start: u64,
    byte_end_exclusive: u64,
    record_bytes: &[u8],
    value: Value,
) -> Result<(CodeBuddyRecordClassification, Option<CodeBuddyOutputDraft>)> {
    let text = cli_message_text(&value);
    let role = value.get("role").and_then(Value::as_str).map(str::to_owned);
    let ref_type = value.get("type").and_then(Value::as_str).map(str::to_owned);
    let event_type = codebuddy_event_type(role.as_deref(), ref_type.as_deref(), &value);
    let explicit_native_message_id = codebuddy_cli_explicit_native_message_id(&value);
    let native_message_id = explicit_native_message_id
        .clone()
        .unwrap_or_else(|| format!("line-{physical_line}"));
    let occurred_at = cli_message_time(&value, context.imported_at).unwrap_or(context.imported_at);
    let output = output_draft(
        event_type,
        ordinal,
        native_message_id.clone(),
        occurred_at,
        &text,
        &value,
    );
    if !codebuddy_is_message_record(role.as_deref(), ref_type.as_deref()) {
        return Ok((CodeBuddyRecordClassification::SkippedMetadata, output));
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
    let source_path = source.canonical_path.display().to_string();
    let session_index = json!({
        "source": "codebuddy_cli_jsonl",
        "path": source_path,
        "rows": session.row_count,
    });
    let started_at = session.started_at()?.unwrap_or(occurred_at);
    let (session, mut event) = codebuddy_normalized_rows(
        &CodeBuddySessionInput {
            provider_session_id: &provider_session_id,
            native_session_id: &session.native_session_id,
            project_hash: &session.project_hash,
            started_at,
            ended_at: session.ended_at()?,
            title: session_title.as_deref(),
            cwd: session.cwd.as_deref(),
            project_index: None,
            conversation: None,
            session_index: &session_index,
            file_names: &["projects/*/*.jsonl"],
            shape: CodeBuddyNativeShape::Cli,
        },
        CodeBuddyEventInput {
            provider_event_index: stable_provider_event_index(
                explicit_native_message_id.as_deref(),
                ordinal,
            ),
            legacy_provider_event_index: ordinal,
            native_message_id,
            event_hash: compute_payload_hash(&value)?,
            event_type: EventType::Message,
            role,
            ref_type,
            occurred_at,
            text,
            raw_message: value.clone(),
            decoded_message: value.clone(),
        },
    );
    attach_cli_complete_content_locator(
        &mut event,
        &value,
        physical_line,
        record_bytes,
        source,
        byte_start,
        byte_end_exclusive,
    )?;
    Ok((
        CodeBuddyRecordClassification::AcceptedMessage(CodeBuddyCoreRow { session, event }),
        output,
    ))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn extension_core_row(
    context: &ProviderAdapterContext,
    metadata: &CodeBuddyExtensionMetadata,
    session: &mut CodeBuddySessionCheckpoint,
    session_title: &mut Option<String>,
    ordinal: u64,
    message_index: usize,
    message_ref: &Value,
    message_path: &Path,
    record_bytes: &[u8],
    raw_message: Value,
) -> Result<(CodeBuddyRecordClassification, Option<CodeBuddyOutputDraft>)> {
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
    let output = output_draft(
        event_type,
        ordinal,
        message_id.clone(),
        occurred_at,
        &text,
        &raw_message,
    );
    if !codebuddy_is_message_record(role.as_deref(), ref_type.as_deref()) {
        return Ok((CodeBuddyRecordClassification::SkippedMetadata, output));
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
    let (session, mut event) = codebuddy_normalized_rows(
        &CodeBuddySessionInput {
            provider_session_id: &provider_session_id,
            native_session_id: &session.native_session_id,
            project_hash: &session.project_hash,
            started_at: session.started_at()?.unwrap_or(occurred_at),
            ended_at: session.ended_at()?,
            title: session_title.as_deref(),
            cwd: cwd.as_deref(),
            project_index: metadata.project_index.as_ref(),
            conversation: metadata.conversation.as_ref(),
            session_index: &metadata.session_index,
            file_names: &["index.json", "messages/*.json"],
            shape: CodeBuddyNativeShape::Extension,
        },
        CodeBuddyEventInput {
            provider_event_index: stable_provider_event_index(Some(&message_id), ordinal),
            legacy_provider_event_index: message_index as u64,
            native_message_id: message_id,
            event_hash: compute_payload_hash(&raw_message)?,
            event_type: EventType::Message,
            role,
            ref_type,
            occurred_at,
            text: text.clone(),
            raw_message,
            decoded_message,
        },
    );
    let native_id = event.legacy_provider_event_hash.clone();
    attach_extension_complete_content_locator(
        &mut event,
        ordinal,
        &native_id,
        record_bytes,
        &text,
    )?;
    Ok((
        CodeBuddyRecordClassification::AcceptedMessage(CodeBuddyCoreRow { session, event }),
        output,
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
    session: &mut CodeBuddySessionCheckpoint,
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

pub(super) fn attach_cli_complete_content_locator(
    event: &mut CodeBuddyEventDraft,
    value: &Value,
    physical_line: usize,
    record_bytes: &[u8],
    source: &CodeBuddySource,
    byte_start: u64,
    byte_end_exclusive: u64,
) -> Result<()> {
    if event.event_type != EventType::Message
        || !verified_content_address_supported(
            CaptureProvider::CodeBuddy,
            CODEBUDDY_SOURCE_FORMAT,
            CompleteContentSourceFamily::Jsonl,
            VerifiedContentRole::MessageBody,
            EXACT_JSONL_COMPLETE_CONTENT_LOCATOR_KIND,
        )
    {
        return Ok(());
    }
    let Some((text, native_record_id)) =
        codebuddy_cli_complete_content_record(value, physical_line)
    else {
        return Ok(());
    };
    if text.chars().count() <= PROVIDER_MAX_TEXT_CHARS
        || text.len() > COMPLETE_CONTENT_MAX_BODY_BYTES
    {
        return Ok(());
    }
    let Some(content_ref) = ContentRef::from_bytes(text.as_bytes()) else {
        return Ok(());
    };
    let Some(profile) = verified_content_profile(
        CaptureProvider::CodeBuddy,
        CODEBUDDY_SOURCE_FORMAT,
        CompleteContentSourceFamily::Jsonl,
        VerifiedContentRole::MessageBody,
    ) else {
        return Err(CaptureError::SystemInvariant(
            "supported exact CodeBuddy JSONL route must have a verified-content profile",
        ));
    };

    let mut locator = [0_u8; 80];
    locator[..8].copy_from_slice(&byte_start.to_be_bytes());
    locator[8..16].copy_from_slice(&byte_end_exclusive.to_be_bytes());
    locator[16..48].copy_from_slice(&codebuddy_complete_content_digest(
        CODEBUDDY_EXACT_SOURCE_REVISION_DIGEST_DOMAIN,
        &source.base_source_revision,
    ));
    locator[48..].copy_from_slice(&codebuddy_complete_content_digest(
        CODEBUDDY_EXACT_PATH_IDENTITY_DIGEST_DOMAIN,
        &source.locator_identity,
    ));
    let Some(locator) = VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        profile,
        content_ref,
        CompleteContentSourceFamily::Jsonl,
        EXACT_JSONL_COMPLETE_CONTENT_LOCATOR_KIND,
        &locator,
        native_record_id,
        CompleteContentBodyDigest::from_bytes(record_bytes),
    ) else {
        return Ok(());
    };
    attach_verified_content_locator(&mut event.metadata, locator).ok_or(
        CaptureError::SystemInvariant("verified-content locator collection is malformed"),
    )
}

pub(super) fn attach_extension_complete_content_locator(
    event: &mut CodeBuddyEventDraft,
    source_record_ordinal: u64,
    native_record_id: &str,
    record_bytes: &[u8],
    complete_text: &str,
) -> Result<()> {
    const STRUCTURED_LOCATOR_MAGIC: &[u8; 4] = b"SC\0\x01";
    if event.event_type != EventType::Message
        || complete_text.chars().count() <= PROVIDER_MAX_TEXT_CHARS
    {
        return Ok(());
    }
    if native_record_id.is_empty()
        || native_record_id.len() > CODEBUDDY_MAX_NATIVE_ID_BYTES
        || native_record_id.chars().any(char::is_control)
    {
        return Err(CaptureError::InvalidPayload(
            "structured complete-content native record identity is invalid".to_owned(),
        ));
    }

    let provider = CaptureProvider::CodeBuddy.as_str().as_bytes();
    let provider_len = u8::try_from(provider.len())
        .map_err(|_| CaptureError::SystemInvariant("provider identity exceeds locator bounds"))?;
    let native_id = native_record_id.as_bytes();
    let native_len = u16::try_from(native_id.len()).map_err(|_| {
        CaptureError::InvalidPayload(
            "structured complete-content native record identity is too long".to_owned(),
        )
    })?;
    let mut locator_value = Vec::with_capacity(
        STRUCTURED_LOCATOR_MAGIC.len() + 1 + provider.len() + 8 + 4 + 2 + native_id.len(),
    );
    locator_value.extend_from_slice(STRUCTURED_LOCATOR_MAGIC);
    locator_value.push(provider_len);
    locator_value.extend_from_slice(provider);
    locator_value.extend_from_slice(&source_record_ordinal.to_be_bytes());
    locator_value.extend_from_slice(&0_u32.to_be_bytes());
    locator_value.extend_from_slice(&native_len.to_be_bytes());
    locator_value.extend_from_slice(native_id);

    let content_ref = ContentRef::from_bytes(complete_text.as_bytes()).ok_or(
        CaptureError::SystemInvariant("structured content length exceeds ContentRef bounds"),
    )?;
    let profile = verified_content_profile(
        CaptureProvider::CodeBuddy,
        CODEBUDDY_SOURCE_FORMAT,
        CompleteContentSourceFamily::Structured,
        VerifiedContentRole::MessageBody,
    )
    .ok_or(CaptureError::SystemInvariant(
        "supported structured message route must have a verified-content profile",
    ))?;
    let locator = VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        profile,
        content_ref,
        CompleteContentSourceFamily::Structured,
        STRUCTURED_COMPLETE_CONTENT_LOCATOR_KIND,
        &locator_value,
        native_record_id,
        CompleteContentBodyDigest::from_bytes(record_bytes),
    )
    .ok_or(CaptureError::SystemInvariant(
        "structured complete-content locator exceeds its bounded schema",
    ))?;
    attach_verified_content_locator(&mut event.metadata, locator).ok_or(
        CaptureError::SystemInvariant("verified-content locator collection is malformed"),
    )?;
    Ok(())
}

pub(super) fn codebuddy_complete_content_digest(domain: &[u8], value: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value.as_bytes());
    digest.finalize().into()
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

pub(super) fn output_draft(
    event_type: EventType,
    ordinal: u64,
    native_record_id: String,
    occurred_at: DateTime<Utc>,
    text: &str,
    value: &Value,
) -> Option<CodeBuddyOutputDraft> {
    if !matches!(event_type, EventType::ToolOutput | EventType::CommandOutput) {
        return None;
    }
    let (outcome, exit_code, duration_ms) = output_outcome(value);
    let call_id = value
        .get("callId")
        .or_else(|| value.get("call_id"))
        .or_else(|| value.get("toolCallId"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned);
    Some(CodeBuddyOutputDraft {
        native_record_id,
        content: text.as_bytes().to_vec(),
        occurred_at_unix_ms: occurred_at.timestamp_millis(),
        outcome: OutputOutcomeMetadata {
            outcome,
            exit_code,
            duration_ms,
        },
        kind: OutputObservationKind::Tool,
        call_id: call_id.or_else(|| Some(format!("codebuddy-output-{ordinal}"))),
    })
}

pub(super) fn output_outcome(value: &Value) -> (OutputOutcome, Option<i32>, Option<u64>) {
    let exit_code = value
        .get("exitCode")
        .or_else(|| value.get("exit_code"))
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok());
    let duration_ms = value
        .get("durationMs")
        .or_else(|| value.get("duration_ms"))
        .and_then(Value::as_u64);
    let status = value
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let success = value
        .get("success")
        .or_else(|| value.get("ok"))
        .and_then(Value::as_bool);
    let outcome = if status.contains("timeout") {
        OutputOutcome::Timeout
    } else if success == Some(false)
        || exit_code.is_some_and(|value| value != 0)
        || matches!(
            status.as_str(),
            "failed" | "failure" | "error" | "errored" | "cancelled" | "canceled"
        )
    {
        OutputOutcome::Failure
    } else if success == Some(true)
        || exit_code == Some(0)
        || matches!(
            status.as_str(),
            "success" | "succeeded" | "complete" | "completed" | "ok" | "passed"
        )
    {
        OutputOutcome::Success
    } else {
        OutputOutcome::Unknown
    };
    (outcome, exit_code, duration_ms)
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
