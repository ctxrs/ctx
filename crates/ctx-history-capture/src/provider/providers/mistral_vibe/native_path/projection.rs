use super::*;

pub(super) struct ProjectedRecord {
    pub(super) event: Option<EventFact>,
    pub(super) detached_touches: Vec<TouchFact>,
    pub(super) occurred_at: DateTime<Utc>,
    pub(super) rejection: Option<ProviderImportFailure>,
    pub(super) serialized_bytes: usize,
}

pub(super) fn record_checkpoint_rejection(
    checkpoint: &mut Checkpoint,
    page_rejections: &mut Vec<ProviderImportFailure>,
    failure: ProviderImportFailure,
) {
    let failure = bounded_rejection(failure);
    checkpoint.rejected_records = checkpoint.rejected_records.saturating_add(1);
    if checkpoint.rejection_details.len() < MAX_RETAINED_PROVIDER_FAILURES {
        checkpoint.rejection_details.push(CheckpointFailure {
            line: failure.line,
            error: failure.error.clone(),
        });
    }
    page_rejections.push(failure);
}

pub(super) fn bounded_rejection(mut failure: ProviderImportFailure) -> ProviderImportFailure {
    if failure.error.is_empty() {
        failure.error = "Mistral Vibe record was deterministically rejected".to_owned();
    }
    if failure.error.len() <= MAX_REJECTION_DETAIL_BYTES {
        return failure;
    }
    let mut boundary = MAX_REJECTION_DETAIL_BYTES;
    while !failure.error.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    failure.error.truncate(boundary);
    failure
}

pub(super) fn checkpoint_rejection_bytes(checkpoint: &Checkpoint) -> usize {
    checkpoint
        .rejection_details
        .iter()
        .map(|failure| failure.error.len().saturating_add(128))
        .sum()
}

pub(super) fn project_core_record(
    opened: &OpenedSource,
    bytes: &[u8],
    ordinal: u64,
    byte_start: u64,
    byte_end_exclusive: u64,
) -> Result<ProjectedRecord> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(ProjectedRecord {
            event: None,
            detached_touches: Vec::new(),
            occurred_at: opened.target_session.started_at,
            rejection: None,
            serialized_bytes: 16,
        });
    }
    let line_number = usize::try_from(ordinal)
        .ok()
        .and_then(|value| value.checked_add(1))
        .ok_or(CaptureError::SystemInvariant(
            "Mistral Vibe line number exceeds platform limits",
        ))?;
    let value = match serde_json::from_slice::<Value>(bytes) {
        Ok(value) => value,
        Err(error) => {
            let reason = format!(
                "malformed JSONL in {}: {error}",
                opened.source.messages_path.display()
            );
            return Ok(ProjectedRecord {
                event: None,
                detached_touches: Vec::new(),
                occurred_at: opened.target_session.started_at,
                rejection: Some(ProviderImportFailure {
                    line: line_number,
                    error: reason.clone(),
                }),
                serialized_bytes: reason.len().saturating_add(128),
            });
        }
    };
    let role_name = match valid_mistral_vibe_record_role(&value) {
        Ok(role) => role,
        Err(reason) => {
            let reason = format!(
                "invalid Mistral Vibe record in {}: {reason}",
                opened.source.messages_path.display()
            );
            return Ok(ProjectedRecord {
                event: None,
                detached_touches: Vec::new(),
                occurred_at: opened.target_session.started_at,
                rejection: Some(ProviderImportFailure {
                    line: line_number,
                    error: reason.clone(),
                }),
                serialized_bytes: bytes.len().saturating_add(reason.len()).saturating_add(128),
            });
        }
    };
    let event_type = mistral_vibe_event_type(role_name, &value);
    let occurred_at = native_jsonl_timestamp(&value).unwrap_or(opened.target_session.started_at);
    let touches = collect_touches(&value)?;
    let touch_limit_exceeded = touches.limit_exceeded;
    let touches = touches.touches;
    let output = (event_type == EventType::ToolOutput).then(|| {
        output_metadata(
            &value,
            line_number,
            role_name,
            opened.target_session.cwd.as_deref(),
        )
    });
    let retain_event = output.as_ref().is_none_or(|output| {
        matches!(
            output.outcome.outcome,
            OutputOutcome::Failure | OutputOutcome::Timeout
        )
    });
    let (event_touches, detached_touches) = if retain_event {
        (touches, Vec::new())
    } else {
        (Vec::new(), touches)
    };
    let event = retain_event
        .then(|| {
            build_event_fact(
                opened,
                bytes,
                &value,
                ordinal,
                line_number,
                byte_start,
                byte_end_exclusive,
                role_name,
                event_type,
                occurred_at,
                event_touches,
                output.as_ref(),
            )
        })
        .transpose()?;
    let rejection = touch_limit_exceeded.then(|| ProviderImportFailure {
        line: line_number,
        error: "Mistral Vibe record exceeds the NativePath file-touch page limit".to_owned(),
    });
    let serialized_bytes = bytes
        .len()
        .saturating_add(EVENT_BASE_BYTES)
        .saturating_add(event.as_ref().map_or(0, |event| {
            event.touches.iter().map(|touch| touch.path.len()).sum()
        }))
        .saturating_add(
            detached_touches
                .iter()
                .map(|touch| touch.path.len())
                .sum::<usize>(),
        )
        .saturating_add(rejection.as_ref().map_or(0, |failure| failure.error.len()));
    Ok(ProjectedRecord {
        event,
        detached_touches,
        occurred_at,
        rejection,
        serialized_bytes,
    })
}

pub(super) fn valid_mistral_vibe_record_role(
    value: &Value,
) -> std::result::Result<&str, &'static str> {
    if !value.is_object() {
        return Err("expected a JSON object");
    }
    let role = value
        .get("role")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|role| !role.is_empty())
        .ok_or("missing a non-empty string role")?;
    let carries_message_content = ["content", "reasoning_content", "images"]
        .iter()
        .any(|field| {
            value
                .get(*field)
                .and_then(crate::provider::normalization::provider_value_text)
                .is_some()
        });
    let carries_tool_call = value
        .get("tool_calls")
        .and_then(Value::as_array)
        .is_some_and(|calls| !calls.is_empty());
    if !carries_message_content && !carries_tool_call {
        return Err("does not contain message content, a tool call, or a tool result");
    }
    Ok(role)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn build_event_fact(
    opened: &OpenedSource,
    record_bytes: &[u8],
    value: &Value,
    ordinal: u64,
    line_number: usize,
    byte_start: u64,
    byte_end_exclusive: u64,
    role_name: &str,
    mut event_type: EventType,
    occurred_at: DateTime<Utc>,
    touches: Vec<TouchFact>,
    output: Option<&OutputMetadata>,
) -> Result<EventFact> {
    let provider_event_hash = mistral_vibe_event_id(value, line_number, role_name);
    let mut text = mistral_vibe_event_text(role_name, value, event_type);
    let mut body = value.clone();
    let mut metadata = json!({
        "source": MISTRAL_VIBE_SOURCE_FORMAT,
        "source_format": MISTRAL_VIBE_SOURCE_FORMAT,
        "line": line_number,
        "role": role_name,
        "message_id": value.get("message_id").and_then(Value::as_str),
        "reasoning_message_id": value.get("reasoning_message_id").and_then(Value::as_str),
        "tool_call_id": value.get("tool_call_id").and_then(Value::as_str),
        "name": value.get("name").and_then(Value::as_str),
        "tool_calls": value
            .get("tool_calls")
            .map(|calls| provider_capped_json_value(calls, PROVIDER_MAX_PREVIEW_CHARS)),
        "images": value
            .get("images")
            .map(|images| provider_capped_json_value(images, PROVIDER_MAX_PREVIEW_CHARS)),
        "agent_profile": opened.target_session.external_agent_id,
    });
    if let Some(output) = output {
        if output.kind == OutputObservationKind::Command {
            event_type = EventType::CommandOutput;
        }
        let content = mistral_vibe_result_content(value).unwrap_or_default();
        let (preview, _) = provider_local_preview(&content, PROVIDER_MAX_PREVIEW_CHARS);
        text = format!(
            "Mistral Vibe failed {} output",
            value.get("name").and_then(Value::as_str).unwrap_or("tool")
        );
        body = json!({
            "result_outcome": if output.outcome.outcome == OutputOutcome::Timeout { "timeout" } else { "failure" },
            "output_bytes": content.len(),
            "output_preview": preview,
            "call_id": output.call_id,
            "exit_code": output.outcome.exit_code,
            "duration_ms": output.outcome.duration_ms,
            "timed_out": output.outcome.outcome == OutputOutcome::Timeout,
            "tool": output.command.as_ref().map(|command| command.tool_name.as_str()),
            "command": output.command.as_ref().map(|command| command.command.as_str()),
            "cwd": output.command.as_ref().and_then(|command| command.working_directory.as_deref()),
        });
    } else if event_type == EventType::Message {
        let full_text = mistral_vibe_event_text(role_name, value, event_type);
        if full_text.chars().count() > PROVIDER_MAX_TEXT_CHARS
            && full_text.len() <= COMPLETE_CONTENT_MAX_BODY_BYTES
        {
            let Some(profile) = verified_content_profile(
                CaptureProvider::MistralVibe,
                MISTRAL_VIBE_SOURCE_FORMAT,
                CompleteContentSourceFamily::Jsonl,
                VerifiedContentRole::MessageBody,
            ) else {
                return Err(CaptureError::SystemInvariant(
                    "Mistral Vibe message route has no complete-content profile",
                ));
            };
            attach_exact_locator(
                &mut metadata,
                VerifiedContentRole::MessageBody,
                profile,
                &full_text,
                &provider_event_hash,
                record_bytes,
                byte_start,
                byte_end_exclusive,
                &opened.observation.exact_content_revision,
                &provider_path_identity(&opened.observation.canonical_messages_path)?,
            )?;
        }
    }
    Ok(EventFact {
        ordinal,
        line_number,
        byte_start,
        byte_end_exclusive,
        event_type,
        role: provider_role(Some(role_name)),
        occurred_at,
        provider_event_hash,
        text,
        body,
        metadata,
        touches,
    })
}

pub(super) struct CollectedTouches {
    pub(super) touches: Vec<TouchFact>,
    pub(super) limit_exceeded: bool,
}

pub(super) fn collect_touches(value: &Value) -> Result<CollectedTouches> {
    let mut seen = BTreeSet::new();
    let mut touches = Vec::new();
    let result = visit_all_file_touch_drafts(value, |draft| {
        let key = (
            draft.path.clone(),
            draft.old_path.clone(),
            draft.change_kind.map(|kind| format!("{kind:?}")),
        );
        if !seen.insert(key) {
            return Ok(());
        }
        if touches.len() >= MAX_TOUCHES_PER_RECORD {
            return Err(());
        }
        touches.push(TouchFact {
            path: draft.path,
            old_path: draft.old_path,
            change_kind: draft.change_kind,
            confidence: draft.confidence,
        });
        Ok(())
    });
    Ok(CollectedTouches {
        touches,
        limit_exceeded: result.is_err(),
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn capture_source(
    context: &ProviderAdapterContext,
    session: &SessionFact,
    source_id: Uuid,
    raw_source_path: &str,
    source_root: &str,
    source_identity: &str,
    source_revision: &str,
) -> CaptureSource {
    CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::MistralVibe,
            machine_id: context.machine_id.clone(),
            process_id: None,
            cwd: session.cwd.clone(),
            raw_source_path: Some(raw_source_path.to_owned()),
            source_format: Some(MISTRAL_VIBE_SOURCE_FORMAT.to_owned()),
            source_root: Some(source_root.to_owned()),
            source_identity: Some(source_identity.to_owned()),
            external_session_id: Some(session.provider_session_id.clone()),
        },
        started_at: session.started_at,
        ended_at: session.ended_at,
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": session.provider_session_id,
                "source_format": MISTRAL_VIBE_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "source_identity": source_identity,
                "source_root": source_root,
                "source_revision": source_revision,
                "source_identity_key": provider_scoped_source_identity_key(
                    CaptureProvider::MistralVibe,
                    &session.provider_session_id,
                    MISTRAL_VIBE_SOURCE_FORMAT,
                    Some(raw_source_path),
                ),
                "session_metadata": session.metadata,
                "nativepath_publication": CURSOR_VERSION,
            }),
        ),
    }
}

pub(super) fn canonical_session(
    committed_store: &Store,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    fact: &SessionFact,
    source_id: Uuid,
    source_identity: &str,
) -> Result<Session> {
    let id = provider_import_session_uuid(
        committed_store,
        CaptureProvider::MistralVibe,
        &fact.provider_session_id,
        source_id,
        Some(source_identity),
    )?;
    let parent_session_id = fact
        .parent_provider_session_id
        .as_deref()
        .map(|parent| {
            provider_import_session_uuid(
                committed_store,
                CaptureProvider::MistralVibe,
                parent,
                source_id,
                Some(source_identity),
            )
        })
        .transpose()?;
    Ok(Session {
        id,
        history_record_id: options.history_record_id,
        parent_session_id,
        root_session_id: parent_session_id,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::MistralVibe,
        external_session_id: Some(fact.provider_session_id.clone()),
        external_agent_id: fact.external_agent_id.clone(),
        agent_type: if fact.is_primary() {
            AgentType::Primary
        } else {
            AgentType::Subagent
        },
        role_hint: Some(if fact.is_primary() {
            "primary".to_owned()
        } else {
            "subagent".to_owned()
        }),
        is_primary: fact.is_primary(),
        status: if fact.ended_at.is_some() {
            SessionStatus::Completed
        } else {
            SessionStatus::Imported
        },
        transcript_blob_id: None,
        started_at: fact.started_at,
        ended_at: fact.ended_at,
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": fact.provider_session_id,
                "parent_provider_session_id": fact.parent_provider_session_id,
                "root_provider_session_id": fact.parent_provider_session_id,
                "source_format": MISTRAL_VIBE_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "metadata": fact.metadata,
                "nativepath_publication": CURSOR_VERSION,
            }),
        ),
    })
}

pub(super) fn relationship_placeholder(
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    source_id: Uuid,
    id: Uuid,
    external_session_id: &str,
    source_identity: &str,
) -> Session {
    Session {
        id,
        history_record_id: options.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::MistralVibe,
        external_session_id: Some(external_session_id.to_owned()),
        external_agent_id: None,
        agent_type: AgentType::Unknown,
        role_hint: Some("relationship_placeholder".to_owned()),
        is_primary: false,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: context.imported_at,
        ended_at: None,
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Partial,
            json!({
                "provider_session_id": external_session_id,
                "source_format": MISTRAL_VIBE_SOURCE_FORMAT,
                "source_identity": source_identity,
                "relationship_placeholder": true,
            }),
        ),
    }
}

pub(super) fn relationship_edge(
    context: &ProviderAdapterContext,
    source_id: Uuid,
    session: &Session,
    parent_id: Uuid,
    source_identity: &str,
) -> SessionEdge {
    SessionEdge {
        id: stable_capture_uuid(
            &format!(
                "provider-source-root:{source_identity}:session:{}:parent_child",
                session.external_session_id.as_deref().unwrap_or_default()
            ),
            "session-edge",
        ),
        from_session_id: session.id,
        to_session_id: parent_id,
        edge_type: SessionEdgeType::ParentChild,
        confidence: Confidence::Explicit,
        source_id: Some(source_id),
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": session.external_session_id,
                "source_format": MISTRAL_VIBE_SOURCE_FORMAT,
                "imported_at": context.imported_at,
            }),
        ),
    }
}

pub(super) fn actor(session: &Session) -> CanonicalActor {
    CanonicalActor {
        direct_session_id: session.id,
        root_session_id: session.root_session_id.unwrap_or(session.id),
        parent_session_id: session.parent_session_id,
        external_session_id: session.external_session_id.clone(),
        external_agent_id: session.external_agent_id.clone(),
        agent_type: session.agent_type.as_str().to_owned(),
        role_hint: session.role_hint.clone(),
        is_primary: session.is_primary,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_event(
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    committed_store: &Store,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    source_id: Uuid,
    session: &Session,
    fact: &EventFact,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let provider_session_id = session.external_session_id.as_deref().unwrap_or_default();
    let identity = provider_event_import_identity_with_exact_legacy_source(
        committed_store,
        CaptureProvider::MistralVibe,
        provider_session_id,
        source_id,
        fact.ordinal,
        fact.ordinal,
        &fact.provider_event_hash,
        None,
        Some(fact.ordinal),
        session.id
            == crate::provider::importer::provider_session_uuid(
                CaptureProvider::MistralVibe,
                provider_session_id,
            ),
    )?;
    let dedupe_key = Store::provider_event_dedupe_key_with_payload_hash(
        &identity.dedupe_key,
        &fact.provider_event_hash,
    )
    .unwrap_or(identity.dedupe_key);
    let retained_text = provider_policy_event_text(fact.event_type, &fact.text, &fact.body);
    let body = provider_policy_body(fact.event_type, &fact.body);
    let provider_payload = if matches!(
        fact.event_type,
        EventType::ToolOutput | EventType::CommandOutput
    ) {
        let mut payload = fact.body.clone();
        payload["result_evidence"] =
            provider_result_identifier_evidence(fact.event_type, &fact.text, &fact.body);
        payload["source_format"] = Value::String(MISTRAL_VIBE_SOURCE_FORMAT.to_owned());
        payload
    } else {
        json!({
            "text": retained_text.text,
            "text_retention": retained_text.retention.as_json(),
            "result_evidence": provider_result_identifier_evidence(
                fact.event_type,
                &fact.text,
                &fact.body,
            ),
            "result_outcome": provider_result_outcome_evidence(fact.event_type, &fact.body),
            "source_format": MISTRAL_VIBE_SOURCE_FORMAT,
            "body": provider_capped_json(&body, PROVIDER_MAX_PREVIEW_CHARS),
        })
    };
    let cursor = format!(
        "{}:line:{}",
        context
            .source_path
            .as_deref()
            .map(|path| path.display().to_string())
            .unwrap_or_default(),
        fact.line_number
    );
    let mut provider_metadata = fact.metadata.clone();
    let verified_locators = provider_metadata
        .as_object_mut()
        .and_then(|metadata| metadata.remove(VERIFIED_CONTENT_LOCATORS_METADATA_KEY));
    let mut sync_metadata = json!({
        "provider_session_id": provider_session_id,
        "provider_event_index": fact.ordinal,
        "provider_event_hash": fact.provider_event_hash,
        "provider_event_hash_authority": "provider_supplied",
        "cursor": cursor.clone(),
        "source_format": MISTRAL_VIBE_SOURCE_FORMAT,
        "source_trust": "provider_native",
        "fixture_line": fact.line_number,
        "imported_at": context.imported_at,
        "source_record_ordinal": fact.ordinal,
        "source_record_subrecord_index": 0,
        "byte_start": fact.byte_start,
        "byte_end_exclusive": fact.byte_end_exclusive,
        "metadata": provider_metadata,
    });
    if let (Some(metadata), Some(locators)) = (sync_metadata.as_object_mut(), verified_locators) {
        metadata.insert(VERIFIED_CONTENT_LOCATORS_METADATA_KEY.to_owned(), locators);
    }
    let normalized = Event {
        id: identity.id,
        seq: identity.seq,
        history_record_id: options.history_record_id,
        session_id: Some(session.id),
        run_id: None,
        event_type: fact.event_type,
        role: Some(fact.role),
        occurred_at: fact.occurred_at,
        capture_source_id: Some(source_id),
        payload: json!({
            "provider": CaptureProvider::MistralVibe.as_str(),
            "provider_session_id": provider_session_id,
            "provider_event_index": fact.ordinal,
            "provider_event_hash": fact.provider_event_hash,
            "cursor": cursor,
            "artifacts": [],
            "body": compact_provider_result_payload(fact.event_type, &provider_payload),
        }),
        payload_blob_id: None,
        dedupe_key: Some(dedupe_key),
        sync: provider_sync_metadata(Fidelity::Imported, sync_metadata),
    };
    if group.reconcile_provider_event(&normalized, ProviderEventHashAuthority::ProviderSupplied)? {
        summary.imported_events = summary.imported_events.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    } else {
        summary.skipped_events = summary.skipped_events.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    }
    summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
    publish_file_touches(
        group,
        committed_store,
        context,
        options,
        source_id,
        session,
        fact.ordinal,
        fact.occurred_at,
        Some(normalized.id),
        &fact.touches,
        summary,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_file_touches(
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    committed_store: &Store,
    _context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    source_id: Uuid,
    session: &Session,
    ordinal: u64,
    occurred_at: DateTime<Utc>,
    event_id: Option<Uuid>,
    touches: &[TouchFact],
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let provider_session_id = session.external_session_id.as_deref().unwrap_or_default();
    for (index, touch) in touches.iter().enumerate() {
        let touch_index = ordinal
            .checked_mul(u64::from(u16::MAX) + 1)
            .and_then(|base| base.checked_add(index as u64))
            .ok_or(CaptureError::SystemInvariant(
                "Mistral Vibe file-touch identity overflowed",
            ))?;
        let id = provider_file_touch_import_id(
            committed_store,
            CaptureProvider::MistralVibe,
            provider_session_id,
            source_id,
            Some(ordinal),
            touch_index,
            session.id
                == crate::provider::importer::provider_session_uuid(
                    CaptureProvider::MistralVibe,
                    provider_session_id,
                ),
        )?;
        group.upsert_file_touched(&FileTouched {
            id,
            history_record_id: options.history_record_id,
            run_id: None,
            event_id,
            vcs_workspace_id: None,
            path: touch.path.clone(),
            change_kind: touch.change_kind,
            old_path: touch.old_path.clone(),
            line_count_delta: None,
            confidence: touch.confidence,
            timestamps: timestamps(occurred_at),
            source_id: Some(source_id),
            sync: provider_sync_metadata(
                Fidelity::Imported,
                json!({
                    "provider": CaptureProvider::MistralVibe.as_str(),
                    "provider_session_id": provider_session_id,
                    "provider_touch_index": touch_index,
                    "provider_event_index": ordinal,
                    "source_format": MISTRAL_VIBE_SOURCE_FORMAT,
                    "session_id": session.id,
                }),
            ),
        })?;
        summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
    }
    Ok(())
}
