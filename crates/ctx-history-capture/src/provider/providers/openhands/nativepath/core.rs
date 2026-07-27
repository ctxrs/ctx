use super::*;

pub(super) fn load_stored_core_cursor(
    store: &Store,
    source: &OpenHandsObservedFile,
    machine_id: &str,
) -> Result<StoredCoreCursor> {
    let Some(stored) = store.get_sync_cursor(None, machine_id, &source.cursor_stream)? else {
        return Ok(StoredCoreCursor::Fresh);
    };
    if let Ok(committed) = decode_native_path_committed_cursor(&stored.cursor) {
        let cursor: OpenHandsNativeCursor = serde_json::from_str(committed.provider_cursor())
            .map_err(|_| {
                CaptureError::InvalidPayload(
                    "OpenHands NativePath cursor payload is malformed".to_owned(),
                )
            })?;
        return Ok(StoredCoreCursor::Native { stored, cursor });
    }
    match CertifiedProviderCursor::decode_if_certified(&stored.cursor)? {
        Some(_) => Ok(StoredCoreCursor::Migrated { stored }),
        None => Err(CaptureError::InvalidPayload(
            "OpenHands cursor is neither NativePath nor a released migration cursor".to_owned(),
        )),
    }
}

pub(super) fn prepare_core_page(
    store: &Store,
    source: &OpenHandsObservedFile,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    reconcile_current_route: bool,
) -> Result<Option<PreparedCorePage>> {
    let cursor_revision = source.cursor_revision(options.inventory_observation_token.as_deref());
    let stored = load_stored_core_cursor(store, source, &context.machine_id)?;
    let expected_cursor = stored.expected_encoded();
    let (mut cursor, source_change) = match &stored {
        StoredCoreCursor::Fresh => (
            OpenHandsNativeCursor::for_source(source, cursor_revision.clone(), 0),
            OpenHandsSourceChange::Fresh,
        ),
        StoredCoreCursor::Migrated { .. } => {
            let mut cursor = OpenHandsNativeCursor::for_source(source, cursor_revision.clone(), 0);
            cursor.legacy_source_layout = legacy_source_layout_required(store, source)?;
            (cursor, OpenHandsSourceChange::Migrated)
        }
        StoredCoreCursor::Native { cursor, .. } => {
            if !cursor.route_supported_for(source) {
                return Err(CaptureError::InvalidPayload(
                    "OpenHands NativePath cursor route or revision is inconsistent".to_owned(),
                ));
            }
            if cursor.deleted {
                let generation =
                    cursor
                        .generation
                        .checked_add(1)
                        .ok_or(CaptureError::SystemInvariant(
                            "OpenHands NativePath generation exhausted",
                        ))?;
                let mut next =
                    OpenHandsNativeCursor::for_source(source, cursor_revision.clone(), generation);
                next.locator_identity = reactivated_locator_identity(source, generation);
                next.legacy_source_layout = cursor.legacy_source_layout;
                (next, OpenHandsSourceChange::Replacement)
            } else if cursor.source_revision == cursor_revision {
                if cursor.terminal && !reconcile_current_route {
                    return Ok(None);
                }
                (cursor.clone(), OpenHandsSourceChange::Unchanged)
            } else {
                let generation =
                    cursor
                        .generation
                        .checked_add(1)
                        .ok_or(CaptureError::SystemInvariant(
                            "OpenHands NativePath generation exhausted",
                        ))?;
                let source_change = classify_source_change(cursor, source);
                let mut next =
                    OpenHandsNativeCursor::for_source(source, cursor_revision.clone(), generation);
                next.legacy_source_layout = cursor.legacy_source_layout;
                (next, source_change)
            }
        }
    };

    let mut rejection = None;
    let mut event = None;
    let mut touches = Vec::new();
    if source_change == OpenHandsSourceChange::Migrated {
        cursor.terminal = false;
        return finish_prepared_page(
            cursor_revision,
            expected_cursor,
            cursor,
            event,
            touches,
            rejection,
            source_change,
        )
        .map(Some);
    }
    let raw_bytes = match source.raw_bytes.as_deref() {
        Some(bytes) => bytes,
        None => {
            rejection = Some(ProviderImportFailure {
                line: openhands_line_number(&source.canonical_path),
                error: format!(
                    "provider record exceeds the {MAX_PROVIDER_JSONL_LINE_BYTES} byte limit (observed {} bytes)",
                    source.observation.length
                ),
            });
            cursor.rejected_records = cursor.rejected_records.saturating_add(1);
            cursor.terminal = true;
            cursor.next_touch = 0;
            return finish_prepared_page(
                cursor_revision,
                expected_cursor,
                cursor,
                event,
                touches,
                rejection,
                source_change,
            )
            .map(Some);
        }
    };
    let decoded = match decode_openhands_event(&source.canonical_path, raw_bytes) {
        Ok(decoded) => decoded,
        Err(error) => {
            rejection = Some(ProviderImportFailure {
                line: openhands_line_number(&source.canonical_path),
                error: bounded_failure(error.to_string()),
            });
            cursor.rejected_records = cursor.rejected_records.saturating_add(1);
            cursor.terminal = true;
            cursor.next_touch = 0;
            return finish_prepared_page(
                cursor_revision,
                expected_cursor,
                cursor,
                event,
                touches,
                rejection,
                source_change,
            )
            .map(Some);
        }
    };

    if cursor.next_touch == 0 && !cursor.accepted_event {
        let retained = retained_core_event(source, &decoded, raw_bytes)?;
        if retained
            .as_ref()
            .map(serde_json::to_vec)
            .transpose()?
            .is_some_and(|bytes| bytes.len() > OPENHANDS_NATIVE_PAGE_MAX_BYTES)
        {
            rejection = Some(ProviderImportFailure {
                line: openhands_line_number(&source.canonical_path),
                error: "OpenHands normalized event exceeds the bounded NativePath Core page"
                    .to_owned(),
            });
            cursor.rejected_records = cursor.rejected_records.saturating_add(1);
            cursor.terminal = true;
        } else {
            event = retained;
            cursor.accepted_event = true;
        }
    }

    if !cursor.terminal {
        let touch_page = collect_touch_page(
            source,
            &decoded,
            usize::try_from(cursor.next_touch).map_err(|_| {
                CaptureError::SystemInvariant(
                    "OpenHands NativePath touch frontier exceeds platform limits",
                )
            })?,
            context,
        )?;
        touches = touch_page.touches;
        cursor.next_touch = cursor
            .next_touch
            .checked_add(u64::try_from(touches.len()).map_err(|_| {
                CaptureError::SystemInvariant("OpenHands touch page count exceeds u64")
            })?)
            .ok_or(CaptureError::SystemInvariant(
                "OpenHands touch frontier overflowed",
            ))?;
        cursor.accepted_file_touches = cursor.next_touch;
        cursor.terminal = !touch_page.has_more;
        if touch_page.limit_exceeded {
            rejection = Some(ProviderImportFailure {
                line: openhands_line_number(&source.canonical_path),
                error: PROVIDER_FILE_TOUCH_LIMIT_REJECTION.to_owned(),
            });
            cursor.rejected_records = cursor.rejected_records.saturating_add(1);
            cursor.terminal = true;
        }
    }

    finish_prepared_page(
        cursor_revision,
        expected_cursor,
        cursor,
        event,
        touches,
        rejection,
        source_change,
    )
    .map(Some)
}

fn finish_prepared_page(
    cursor_revision: String,
    expected_cursor: Option<String>,
    next_cursor: OpenHandsNativeCursor,
    event: Option<OpenHandsEventFact>,
    touches: Vec<(usize, OpenHandsTouchFact)>,
    rejection: Option<ProviderImportFailure>,
    source_change: OpenHandsSourceChange,
) -> Result<PreparedCorePage> {
    let conservative_serialized_bytes = 4 * 1024
        + event
            .as_ref()
            .map(serde_json::to_vec)
            .transpose()?
            .map_or(0, |bytes| bytes.len())
        + serde_json::to_vec(&touches)?.len()
        + rejection
            .as_ref()
            .map_or(0, |failure| failure.error.len().saturating_add(64));
    if conservative_serialized_bytes > OPENHANDS_NATIVE_PAGE_MAX_BYTES {
        return Err(CaptureError::InvalidPayload(
            "OpenHands NativePath Core page exceeds its retained-byte bound".to_owned(),
        ));
    }
    Ok(PreparedCorePage {
        cursor_revision,
        expected_cursor,
        next_cursor,
        event,
        touches,
        rejection,
        conservative_serialized_bytes,
        source_change,
    })
}

fn classify_source_change(
    previous: &OpenHandsNativeCursor,
    source: &OpenHandsObservedFile,
) -> OpenHandsSourceChange {
    let Some(previous_observation) = previous.observation.as_ref() else {
        return OpenHandsSourceChange::Rewrite;
    };
    if previous_observation.physical_identity() != source.observation.physical_identity() {
        return OpenHandsSourceChange::Replacement;
    }
    if source.observation.length < previous_observation.length {
        return OpenHandsSourceChange::Truncation;
    }
    if source.observation.length > previous_observation.length
        && previous
            .content_sha256
            .is_some_and(|hash| source.current_prefix_matches(previous_observation.length, hash))
    {
        return OpenHandsSourceChange::Append;
    }
    OpenHandsSourceChange::Rewrite
}

struct TouchPage {
    touches: Vec<(usize, OpenHandsTouchFact)>,
    has_more: bool,
    limit_exceeded: bool,
}

fn collect_touch_page(
    source: &OpenHandsObservedFile,
    decoded: &OpenHandsDecodedEvent,
    skip: usize,
    context: &ProviderAdapterContext,
) -> Result<TouchPage> {
    #[derive(Debug)]
    enum Stop {
        PageFull,
    }

    let provider_event_index = event_identity_index(source, decoded.event_id());
    let include_structured_touches = matches!(
        decoded.event_type(),
        EventType::ToolCall | EventType::FileTouched
    );
    let mut touches = Vec::new();
    let source_root = context.source_root_display();
    let line_number = openhands_line_number(&source.canonical_path);
    let outcome = visit_provider_file_touch_drafts_with_limit(
        decoded.value(),
        include_structured_touches,
        MAX_PROVIDER_FILE_TOUCHES_PER_EVENT,
        |(touch_ordinal, draft)| {
            let touch_ordinal = usize::try_from(touch_ordinal).unwrap_or(usize::MAX);
            if touch_ordinal < skip {
                return Ok(());
            }
            if touches.len() == OPENHANDS_NATIVE_PAGE_TOUCHES {
                return Err(Stop::PageFull);
            }
            let provider_touch_index = if provider_event_index > MAX_PACKED_PROVIDER_EVENT_INDEX {
                u64::try_from(touch_ordinal).unwrap_or(u64::MAX)
            } else {
                (provider_event_index << 16) | u64::try_from(touch_ordinal).unwrap_or(u64::MAX)
            };
            touches.push((
                line_number,
                OpenHandsTouchFact {
                    provider_session_id: source.session_id.clone(),
                    provider_event_hash: decoded.event_id().to_owned(),
                    provider_touch_index,
                    provider_event_index: Some(provider_event_index),
                    raw_source_path: source.canonical_path_text.clone(),
                    source_root: source_root.clone(),
                    path: draft.path,
                    change_kind: draft.change_kind,
                    old_path: draft.old_path,
                    line_count_delta: None,
                    confidence: draft.confidence,
                    occurred_at: decoded.timestamp(),
                    metadata: draft.metadata,
                },
            ));
            Ok(())
        },
    );
    match outcome {
        Ok(outcome) => Ok(TouchPage {
            touches,
            has_more: false,
            limit_exceeded: outcome.limit_exceeded(),
        }),
        Err(Stop::PageFull) => Ok(TouchPage {
            touches,
            has_more: true,
            limit_exceeded: false,
        }),
    }
}

fn retained_core_event(
    source: &OpenHandsObservedFile,
    decoded: &OpenHandsDecodedEvent,
    raw_bytes: &[u8],
) -> Result<Option<OpenHandsEventFact>> {
    let is_output = matches!(
        decoded.event_type(),
        EventType::ToolOutput | EventType::CommandOutput
    );
    let outcome = openhands_output_outcome(decoded);
    let retained_failure = is_output
        && matches!(
            outcome.outcome,
            OutputOutcome::Failure | OutputOutcome::Timeout
        )
        && (decoded.event_type() != EventType::CommandOutput
            || openhands_output_command_context(decoded).is_some());
    if is_output && !retained_failure {
        return Ok(None);
    }
    let mut event = openhands_event_fact(source, decoded);
    if retained_failure {
        apply_failure_diagnostic(
            &mut event,
            super::openhands_result_content(decoded).as_deref(),
            &outcome,
            openhands_output_call_id(decoded.value()).as_deref(),
            openhands_output_command_context(decoded).as_ref(),
        )?;
    } else {
        attach_openhands_complete_content_locator(
            &mut event,
            0,
            0,
            decoded.event_id(),
            raw_bytes,
            decoded.text(),
        )?;
    }
    Ok(Some(event))
}

fn openhands_event_fact(
    source: &OpenHandsObservedFile,
    decoded: &OpenHandsDecodedEvent,
) -> OpenHandsEventFact {
    let identity = event_identity_index(source, decoded.event_id());
    let legacy_source_event_candidate = openhands_legacy_filename_index_candidate(
        &source.canonical_path,
    )
    .map(|provider_event_index| {
        json!({
            "raw_source_path": source.conversation_dir.display().to_string(),
            "provider_event_index": provider_event_index,
        })
    });
    let event_type = decoded.event_type();
    let text = decoded.text();
    let body = decoded.value();
    let retained_text = provider_policy_event_text(event_type, text, body);
    let retained_body = provider_policy_body(event_type, body);
    OpenHandsEventFact {
        provider_event_index: identity,
        provider_event_hash: decoded.event_id().to_owned(),
        cursor: format!("{}:{}", source.canonical_path.display(), decoded.event_id()),
        event_type,
        role: decoded.role(),
        occurred_at: decoded.timestamp(),
        payload: json!({
            "text": retained_text.text,
            "text_retention": retained_text.retention.as_json(),
            "result_evidence": provider_result_identifier_evidence(event_type, text, body),
            "result_outcome": provider_result_outcome_evidence(event_type, body),
            "source_format": OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
            "body": provider_capped_json(&retained_body, PROVIDER_MAX_PREVIEW_CHARS),
        }),
        metadata: json!({
            "source": OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
            "source_format": OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
            "event_id": decoded.event_id(),
            "entry_type": decoded.entry_type(),
            "event_path": source.canonical_path_text,
            "conversation_id": source.session_id,
            "provider_event_identity_index": identity,
            "event_file_identity": format!("{identity:016x}"),
            "legacy_source_event_candidate_v1": legacy_source_event_candidate,
            "tool_name": decoded.value().get("tool_name").and_then(Value::as_str),
            "tool_call_id": decoded.value().get("tool_call_id").and_then(Value::as_str),
            "action_id": decoded.value().get("action_id").and_then(Value::as_str),
        }),
    }
}

fn attach_openhands_complete_content_locator(
    event: &mut OpenHandsEventFact,
    source_record_ordinal: u64,
    source_record_subrecord_index: u32,
    native_record_id: &str,
    record_bytes: &[u8],
    complete_text: &str,
) -> Result<()> {
    if event.event_type != EventType::Message
        || complete_text.chars().count() <= PROVIDER_MAX_TEXT_CHARS
    {
        return Ok(());
    }
    if native_record_id.is_empty()
        || native_record_id.len() > 1_024
        || native_record_id.chars().any(char::is_control)
    {
        return Err(CaptureError::InvalidPayload(
            "OpenHands complete-content native record identity is invalid".to_owned(),
        ));
    }
    let locator_value = openhands_structured_locator(
        source_record_ordinal,
        source_record_subrecord_index,
        native_record_id,
    )?;
    let content_ref = ContentRef::from_bytes(complete_text.as_bytes()).ok_or(
        CaptureError::SystemInvariant("OpenHands complete content exceeds ContentRef bounds"),
    )?;
    let profile = verified_content_profile(
        CaptureProvider::OpenHands,
        OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
        CompleteContentSourceFamily::Structured,
        VerifiedContentRole::MessageBody,
    )
    .ok_or(CaptureError::SystemInvariant(
        "OpenHands complete-content profile is not registered",
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
        "OpenHands complete-content locator exceeds its typed bounds",
    ))?;
    attach_verified_content_locator(&mut event.metadata, locator).ok_or(
        CaptureError::SystemInvariant("OpenHands complete-content locator metadata is malformed"),
    )?;
    Ok(())
}

fn openhands_structured_locator(
    source_record_ordinal: u64,
    source_record_subrecord_index: u32,
    native_record_id: &str,
) -> Result<Vec<u8>> {
    let provider = CaptureProvider::OpenHands.as_str().as_bytes();
    let provider_len = u8::try_from(provider.len())
        .map_err(|_| CaptureError::SystemInvariant("OpenHands provider identity is too long"))?;
    let native_id = native_record_id.as_bytes();
    let native_len = u16::try_from(native_id.len()).map_err(|_| {
        CaptureError::InvalidPayload(
            "OpenHands complete-content native record identity is too long".to_owned(),
        )
    })?;
    let mut value = Vec::with_capacity(4 + 1 + provider.len() + 8 + 4 + 2 + native_id.len());
    value.extend_from_slice(b"SC\0\x01");
    value.push(provider_len);
    value.extend_from_slice(provider);
    value.extend_from_slice(&source_record_ordinal.to_be_bytes());
    value.extend_from_slice(&source_record_subrecord_index.to_be_bytes());
    value.extend_from_slice(&native_len.to_be_bytes());
    value.extend_from_slice(native_id);
    Ok(value)
}

pub(super) fn event_identity_index(source: &OpenHandsObservedFile, event_id: &str) -> u64 {
    event_identity_index_for_path(&source.path_identity, event_id)
}

pub(super) fn event_identity_index_for_path(path_identity: &str, event_id: &str) -> u64 {
    let identity = serde_json::to_string(&("openhands-native-event-v1", path_identity, event_id))
        .expect("OpenHands event identity should serialize");
    crate::fnv1a64(identity.as_bytes())
}

pub(super) fn event_file_identity_index_for_path(path_identity: &str) -> u64 {
    crate::fnv1a64(path_identity.as_bytes())
}

fn reactivated_locator_identity(source: &OpenHandsObservedFile, generation: u64) -> String {
    serde_json::to_string(&(
        "openhands-native-route-incarnation-v1",
        source.path_identity.as_str(),
        generation,
    ))
    .expect("OpenHands reactivated locator identity should serialize")
}

fn legacy_source_layout_required(store: &Store, source: &OpenHandsObservedFile) -> Result<bool> {
    let source_id = provider_scoped_source_uuid(
        CaptureProvider::OpenHands,
        &source.session_id,
        OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
        Some(&source.canonical_path_text),
    );
    match store.get_capture_source(source_id) {
        Ok(_) => Ok(false),
        Err(StoreError::NotFound(_)) => Ok(true),
        Err(error) => Err(error.into()),
    }
}
