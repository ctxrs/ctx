use super::*;

pub(super) fn canonical_session(
    store: &Store,
    source_id: Uuid,
    thread: &DeepAgentsThread,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    canonical_source_identity: &str,
) -> Result<Session> {
    Ok(Session {
        id: provider_import_session_uuid(
            store,
            CaptureProvider::DeepAgents,
            &thread.thread_id,
            source_id,
            Some(canonical_source_identity),
        )?,
        history_record_id: options.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::DeepAgents,
        external_session_id: Some(thread.thread_id.clone()),
        external_agent_id: thread.agent_name.clone(),
        agent_type: AgentType::Primary,
        role_hint: thread
            .agent_name
            .clone()
            .or_else(|| Some("agent".to_owned())),
        is_primary: true,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: thread.created_at,
        ended_at: Some(thread.updated_at),
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": thread.thread_id,
                "source_format": DEEPAGENTS_SQLITE_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "session_idempotency_key":
                    format!("provider-session:deepagents:{}", thread.thread_id),
                "metadata": {
                    "source_format": DEEPAGENTS_SQLITE_SOURCE_FORMAT,
                    "agent_name": thread.agent_name,
                    "git_branch": thread.git_branch,
                    "latest_checkpoint_id": thread.latest_checkpoint_id,
                    "storage": "LangGraph AsyncSqliteSaver checkpoints/writes",
                    "nativepath_publication": DEEPAGENTS_NATIVE_PARSER_REVISION,
                },
            }),
        ),
    })
}

pub(super) fn committed_source_and_session(
    store: &Store,
    key: &DeepAgentsWriteKey,
    authority: &DeepAgentsSourceAuthority,
    context: &ProviderAdapterContext,
) -> Result<Option<(CaptureSource, Session)>> {
    let raw_source_path = authority.canonical_database_path.display().to_string();
    let source_id = store
        .capture_source_by_canonical_identity_session(
            CaptureProvider::DeepAgents,
            DEEPAGENTS_SQLITE_SOURCE_FORMAT,
            &context.machine_id,
            &authority.canonical_source_identity,
            &key.thread_id,
        )?
        .map(|source| source.id)
        .unwrap_or_else(|| {
            provider_scoped_source_uuid(
                CaptureProvider::DeepAgents,
                &key.thread_id,
                DEEPAGENTS_SQLITE_SOURCE_FORMAT,
                Some(&raw_source_path),
            )
        });
    let source = match store.get_capture_source(source_id) {
        Ok(source) => source,
        Err(StoreError::NotFound(_))
        | Err(StoreError::Sql(rusqlite::Error::QueryReturnedNoRows)) => {
            return Ok(None);
        }
        Err(error) => return Err(CaptureError::Store(error)),
    };
    let session = store.session_by_capture_source_and_external_session(
        source_id,
        CaptureProvider::DeepAgents,
        &key.thread_id,
    )?;
    Ok(session.map(|session| (source, session)))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_core_messages(
    store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    source: &CaptureSource,
    session: &Session,
    key: &DeepAgentsWriteKey,
    page: &DeepAgentsWritePage,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    summary: &mut ProviderImportSummary,
    retained: &mut NativePathRetainedSourceEntities,
) -> Result<()> {
    let occurred_at = page.occurred_at.ok_or(CaptureError::SystemInvariant(
        "accepted Deep Agents write has no occurrence time",
    ))?;
    let record_digest =
        deepagents_write_record_digest(key, page.value_type.as_deref(), &page.value);
    for parsed in &page.messages {
        if !core_eligible(&parsed.message) {
            continue;
        }
        let identity = parsed
            .message
            .message_id
            .as_deref()
            .map(|message_id| deepagents_message_identity(&key.thread_id, message_id));
        let cursor = format!(
            "thread:{}:checkpoint:{}:task:{}:write:{}:message:{}",
            key.thread_id, key.checkpoint_id, key.task_id, key.idx, parsed.offset
        );
        let released_identity =
            released_v025_event_identity(store, source, session, parsed, &cursor)?;
        let migrate_released_hash = released_identity.is_some();
        let mut native = deepagents_native_event(
            key,
            parsed,
            occurred_at,
            &cursor,
            identity.as_ref().map(|identity| identity.provider_index),
            Some(record_digest.clone()),
        );
        // LangGraph message IDs provide stable identity, not content authority. Reconcile with
        // the normalized payload hash so same-ID edits update in place; released v0.25 cursor
        // hashes take the exact Store migration path below.
        native.provider_event_hash = None;
        let (event_hash, authority) = native.provider_event_hash.as_ref().map_or_else(
            || {
                compute_payload_hash(&native.payload)
                    .map(|hash| (hash, ProviderEventHashAuthority::NormalizedPayloadFallback))
            },
            |hash| Ok((hash.clone(), ProviderEventHashAuthority::ProviderSupplied)),
        )?;
        let provider_identity_index = identity
            .as_ref()
            .map_or(parsed.provider_event_index, |identity| {
                identity.provider_index
            });
        let import_identity = released_identity.map_or_else(
            || {
                provider_event_import_identity_with_exact_legacy_source(
                    store,
                    CaptureProvider::DeepAgents,
                    &key.thread_id,
                    source.id,
                    provider_identity_index,
                    parsed.provider_event_index,
                    &event_hash,
                    None,
                    Some(provider_identity_index),
                    session.id
                        == provider_session_uuid(CaptureProvider::DeepAgents, &key.thread_id),
                )
            },
            Ok,
        )?;
        if let Some(metadata) = native.metadata.as_object_mut() {
            metadata.insert(
                "source_record_ordinal".to_owned(),
                json!(page.rowid.unwrap_or_default()),
            );
            metadata.insert(
                "source_record_subrecord_index".to_owned(),
                json!(parsed.offset),
            );
        }
        let line = page
            .rowid
            .and_then(|rowid| usize::try_from(rowid).ok())
            .unwrap_or(usize::MAX);
        let event = deepagents_core_event(
            context,
            options,
            &key.thread_id,
            source.id,
            session.id,
            line,
            &native,
            &event_hash,
            authority,
            &import_identity,
        )?;
        let inserted = if migrate_released_hash {
            group.reconcile_provider_event_migrating_exact_legacy_provider_hash(&event, &cursor)?
        } else {
            group.reconcile_provider_event(&event, authority)?
        };
        if inserted {
            summary.imported_events = summary.imported_events.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        } else {
            summary.skipped_events = summary.skipped_events.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
        }
        retained.event_ids.push(event.id);
        summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
    }
    Ok(())
}

pub(super) fn released_v025_event_identity(
    store: &Store,
    source: &CaptureSource,
    session: &Session,
    parsed: &DeepAgentsParsedMessage,
    cursor: &str,
) -> Result<Option<ProviderEventImportIdentity>> {
    let legacy =
        provider_source_event_import_identity(source.id, parsed.provider_event_index, cursor);
    let event = match store.get_event(legacy.id) {
        Ok(event) => event,
        Err(StoreError::NotFound(_))
        | Err(StoreError::Sql(rusqlite::Error::QueryReturnedNoRows)) => return Ok(None),
        Err(error) => return Err(CaptureError::Store(error)),
    };
    let exact_source_record = event.capture_source_id == Some(source.id)
        && event.session_id == Some(session.id)
        && event
            .sync
            .metadata
            .get("provider_event_index")
            .and_then(Value::as_u64)
            == Some(parsed.provider_event_index)
        && event.sync.metadata.get("cursor").and_then(Value::as_str) == Some(cursor);
    if !exact_source_record {
        return Ok(None);
    }
    Ok(event
        .dedupe_key
        .map(|dedupe_key| ProviderEventImportIdentity {
            id: event.id,
            seq: event.seq,
            dedupe_key,
            run_source_id: event.capture_source_id,
        }))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn deepagents_core_event(
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    provider_session_id: &str,
    source_id: Uuid,
    session_id: Uuid,
    line_number: usize,
    native: &DeepAgentsNativeEvent,
    event_hash: &str,
    authority: ProviderEventHashAuthority,
    identity: &ProviderEventImportIdentity,
) -> Result<Event> {
    let dedupe_key =
        Store::provider_event_dedupe_key_with_payload_hash(&identity.dedupe_key, event_hash)
            .unwrap_or_else(|| identity.dedupe_key.clone());
    let mut provider_metadata = native.metadata.clone();
    let source_record_coordinates =
        take_deepagents_source_record_coordinates(&mut provider_metadata)?;
    let verified_content_locators = provider_metadata
        .as_object_mut()
        .and_then(|metadata| metadata.remove(VERIFIED_CONTENT_LOCATORS_METADATA_KEY))
        .map(|value| {
            VerifiedContentLocatorsV1::from_metadata_value(&value).ok_or_else(|| {
                CaptureError::InvalidPayload(
                    "verified content locator annotation is malformed".to_owned(),
                )
            })
        })
        .transpose()?;
    let mut sync_metadata = json!({
        "provider_session_id": provider_session_id,
        "provider_event_index": native.provider_event_index,
        "provider_event_hash": event_hash,
        "provider_event_hash_authority": authority.as_str(),
        "cursor": native.cursor,
        "source_format": DEEPAGENTS_SQLITE_SOURCE_FORMAT,
        "source_trust": ProviderSourceTrust::ProviderNative,
        "fixture_line": line_number,
        "imported_at": context.imported_at,
        "event_idempotency_key": format!(
            "provider-event:{}:{}:{}",
            CaptureProvider::DeepAgents.as_str(),
            provider_session_id,
            native.provider_event_index,
        ),
        "source_record_ordinal": source_record_coordinates
            .as_ref()
            .map(|coordinates| coordinates.0),
        "source_record_subrecord_index": source_record_coordinates
            .as_ref()
            .map(|coordinates| coordinates.1),
        "metadata": provider_metadata,
    });
    if let (Some(metadata), Some(locators)) = (
        sync_metadata.as_object_mut(),
        verified_content_locators.as_ref(),
    ) {
        metadata.insert(
            VERIFIED_CONTENT_LOCATORS_METADATA_KEY.to_owned(),
            locators.to_metadata_value(),
        );
    }
    Ok(Event {
        id: identity.id,
        seq: identity.seq,
        history_record_id: options.history_record_id,
        session_id: Some(session_id),
        run_id: None,
        event_type: native.event_type,
        role: native.role,
        occurred_at: native.occurred_at,
        capture_source_id: Some(source_id),
        payload: json!({
            "provider": CaptureProvider::DeepAgents.as_str(),
            "provider_session_id": provider_session_id,
            "provider_event_index": native.provider_event_index,
            "provider_event_hash": event_hash,
            "cursor": native.cursor,
            "artifacts": [],
            "body": compact_provider_result_payload(native.event_type, &native.payload),
        }),
        payload_blob_id: None,
        dedupe_key: Some(dedupe_key),
        sync: provider_sync_metadata(Fidelity::Imported, sync_metadata),
    })
}

pub(super) fn take_deepagents_source_record_coordinates(
    metadata: &mut Value,
) -> Result<Option<(u64, u32)>> {
    let Some(object) = metadata.as_object_mut() else {
        return Ok(None);
    };
    let ordinal = object.remove("source_record_ordinal");
    let subrecord = object.remove("source_record_subrecord_index");
    if ordinal.is_none() && subrecord.is_none() {
        return Ok(None);
    }
    let ordinal = ordinal.and_then(|value| value.as_u64()).ok_or_else(|| {
        CaptureError::InvalidPayload("source record ordinal annotation is malformed".to_owned())
    })?;
    let subrecord = subrecord
        .and_then(|value| value.as_u64())
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "source record subrecord annotation is malformed".to_owned(),
            )
        })?;
    Ok(Some((ordinal, subrecord)))
}

pub(super) fn core_eligible(message: &DeepAgentsMessage) -> bool {
    if message.role != EventRole::Tool {
        return true;
    }
    matches!(
        deepagents_output_outcome(message).outcome,
        OutputOutcome::Failure | OutputOutcome::Timeout
    )
}

pub(super) fn attach_native_message_content_locator(
    event: &mut DeepAgentsNativeEvent,
    key: &DeepAgentsWriteKey,
    message_offset: usize,
    text: &str,
    record_digest: Option<crate::complete_content::CompleteContentBodyDigest>,
) {
    let Some(locator) = deepagents_content_locator(
        &event.payload,
        key,
        message_offset,
        text,
        record_digest,
        event
            .provider_event_hash
            .clone()
            .unwrap_or_else(|| event.cursor.clone()),
    ) else {
        return;
    };
    let _ = attach_verified_content_locator(&mut event.metadata, locator);
}

pub(super) fn deepagents_content_locator(
    payload: &Value,
    key: &DeepAgentsWriteKey,
    message_offset: usize,
    text: &str,
    record_digest: Option<crate::complete_content::CompleteContentBodyDigest>,
    native_record_id: String,
) -> Option<VerifiedContentLocatorV1> {
    if payload
        .pointer("/text_retention/truncated")
        .and_then(Value::as_bool)
        != Some(true)
    {
        return None;
    }
    let record_digest = record_digest?;
    let address = DeepAgentsContentAddress::from_write(key, message_offset)?;
    let locator_value = address.encode()?;
    let content_ref = ContentRef::from_bytes(text.as_bytes())?;
    let profile = verified_content_profile(
        CaptureProvider::DeepAgents,
        DEEPAGENTS_SQLITE_SOURCE_FORMAT,
        CompleteContentSourceFamily::Sqlite,
        VerifiedContentRole::MessageBody,
    )?;
    VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        profile,
        content_ref,
        CompleteContentSourceFamily::Sqlite,
        DEEPAGENTS_CONTENT_LOCATOR_KIND,
        &locator_value,
        native_record_id,
        record_digest,
    )
}

pub(super) fn deepagents_output_outcome(message: &DeepAgentsMessage) -> OutputOutcomeMetadata {
    let status = message
        .status
        .as_deref()
        .map(str::trim)
        .map(str::to_ascii_lowercase);
    let timeout = message.timed_out
        || status
            .as_deref()
            .is_some_and(|status| matches!(status, "timeout" | "timed_out" | "timedout"));
    let failure = message.is_error == Some(true)
        || message.success == Some(false)
        || message.exit_code.is_some_and(|code| code != 0)
        || status.as_deref().is_some_and(|status| {
            matches!(
                status,
                "failed" | "failure" | "error" | "errored" | "cancelled" | "canceled"
            )
        });
    let success = message.success == Some(true)
        || message.exit_code == Some(0)
        || status.as_deref().is_some_and(|status| {
            matches!(
                status,
                "success" | "succeeded" | "complete" | "completed" | "ok" | "passed"
            )
        });
    OutputOutcomeMetadata {
        outcome: if timeout {
            OutputOutcome::Timeout
        } else if failure {
            OutputOutcome::Failure
        } else if success {
            OutputOutcome::Success
        } else {
            OutputOutcome::Unknown
        },
        exit_code: message.exit_code,
        duration_ms: message.duration_ms,
    }
}

pub(super) fn reconcile_locator(
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    authority: &DeepAgentsSourceAuthority,
    context: &ProviderAdapterContext,
) -> Result<ctx_history_store::ProviderSourceLocatorResolution> {
    group
        .reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::DeepAgents,
            source_format: DEEPAGENTS_SQLITE_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity: authority.route_identity.clone(),
            cursor_stream: authority.cursor_stream.clone(),
            proposed_source_identity: authority.proposed_source_identity.clone(),
            raw_source_path: Some(authority.canonical_database_path.display().to_string()),
            source_revision: authority.source_revision.clone(),
            observed_at_ms: context.imported_at.timestamp_millis(),
        })
        .map_err(CaptureError::from)
}

pub(super) fn generation_key(
    authority: &DeepAgentsSourceAuthority,
    context: &ProviderAdapterContext,
    canonical_source_identity: &str,
    generation: u64,
) -> NativePathSourceGenerationKey {
    NativePathSourceGenerationKey {
        provider: CaptureProvider::DeepAgents,
        source_format: DEEPAGENTS_SQLITE_SOURCE_FORMAT.to_owned(),
        machine_id: context.machine_id.clone(),
        canonical_source_identity: canonical_source_identity.to_owned(),
        locator_identity: authority.route_identity.clone(),
        cursor_stream: authority.cursor_stream.clone(),
        source_revision: authority.source_revision.clone(),
        generation_id: format!("deepagents-native-generation-{generation}"),
    }
}
