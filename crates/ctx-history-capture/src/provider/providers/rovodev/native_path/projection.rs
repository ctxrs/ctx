use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn rovodev_canonical_event(
    provider_session_id: &str,
    source_id: Uuid,
    session_id: Uuid,
    line_number: usize,
    event: &RovoDevCoreEvent,
    event_hash: &str,
    identity: &crate::provider::importer::ProviderEventImportIdentity,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
) -> Result<(Event, Option<Run>)> {
    let mut provider_metadata = event.metadata.clone();
    let source_record_ordinal = provider_metadata
        .as_object_mut()
        .and_then(|metadata| metadata.remove("source_record_ordinal"))
        .and_then(|value| value.as_u64())
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "RovoDev source record ordinal annotation is malformed".to_owned(),
            )
        })?;
    let source_record_subrecord_index = provider_metadata
        .as_object_mut()
        .and_then(|metadata| metadata.remove("source_record_subrecord_index"))
        .and_then(|value| value.as_u64())
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "RovoDev source record subrecord annotation is malformed".to_owned(),
            )
        })?;
    let verified_content_locators = provider_metadata
        .as_object_mut()
        .and_then(|metadata| metadata.remove(VERIFIED_CONTENT_LOCATORS_METADATA_KEY))
        .map(|value| {
            VerifiedContentLocatorsV1::from_metadata_value(&value)
                .map(|locators| locators.to_metadata_value())
                .ok_or_else(|| {
                    CaptureError::InvalidPayload(
                        "RovoDev verified content locator annotation is malformed".to_owned(),
                    )
                })
        })
        .transpose()?;
    let run = provider_command_run(
        CaptureProvider::RovoDev,
        provider_session_id,
        session_id,
        source_id,
        identity.run_source_id,
        options.history_record_id,
        event.event_type,
        event.occurred_at,
        Fidelity::Imported,
        event.provider_event_index,
        &event.payload,
        event_hash,
    )?;
    let dedupe_key =
        Store::provider_event_dedupe_key_with_payload_hash(&identity.dedupe_key, event_hash)
            .unwrap_or_else(|| identity.dedupe_key.clone());
    let mut sync_metadata = json!({
        "provider_session_id": provider_session_id,
        "provider_event_index": event.provider_event_index,
        "provider_event_hash": event_hash,
        "provider_event_hash_authority": ProviderEventHashAuthority::ProviderSupplied.as_str(),
        "cursor": event.cursor,
        "source_format": ROVODEV_SOURCE_FORMAT,
        "source_trust": "provider_native",
        "fixture_line": line_number,
        "imported_at": context.imported_at,
        "event_idempotency_key": format!(
            "provider-event:{}:{}:{}",
            CaptureProvider::RovoDev.as_str(),
            provider_session_id,
            event.provider_event_index,
        ),
        "source_record_ordinal": source_record_ordinal,
        "source_record_subrecord_index": source_record_subrecord_index,
        "metadata": provider_metadata,
    });
    if let (Some(metadata), Some(locators)) =
        (sync_metadata.as_object_mut(), verified_content_locators)
    {
        metadata.insert(VERIFIED_CONTENT_LOCATORS_METADATA_KEY.to_owned(), locators);
    }
    Ok((
        Event {
            id: identity.id,
            seq: identity.seq,
            history_record_id: options.history_record_id,
            session_id: Some(session_id),
            run_id: run.as_ref().map(|run| run.id),
            event_type: event.event_type,
            role: event.role,
            occurred_at: event.occurred_at,
            capture_source_id: Some(source_id),
            payload: json!({
                "provider": CaptureProvider::RovoDev.as_str(),
                "provider_session_id": provider_session_id,
                "provider_event_index": event.provider_event_index,
                "provider_event_hash": event_hash,
                "cursor": event.cursor,
                "artifacts": [],
                "body": compact_provider_result_payload(event.event_type, &event.payload),
            }),
            payload_blob_id: None,
            dedupe_key: Some(dedupe_key),
            sync: provider_sync_metadata(Fidelity::Imported, sync_metadata),
        },
        run,
    ))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn rovodev_canonical_file_touch(
    touch: &RovoDevFileTouch,
    provider_session_id: &str,
    history_record_id: Option<Uuid>,
    source_id: Uuid,
    session_id: Uuid,
    event_id: Option<Uuid>,
    touch_id: Uuid,
) -> FileTouched {
    FileTouched {
        id: touch_id,
        history_record_id,
        run_id: None,
        event_id,
        vcs_workspace_id: None,
        path: touch.path.clone(),
        change_kind: touch.change_kind,
        old_path: touch.old_path.clone(),
        line_count_delta: touch.line_count_delta,
        confidence: touch.confidence,
        timestamps: timestamps(touch.occurred_at),
        source_id: Some(source_id),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider": CaptureProvider::RovoDev.as_str(),
                "provider_session_id": provider_session_id,
                "provider_touch_index": touch.provider_touch_index,
                "provider_event_index": touch.provider_event_index,
                "raw_source_path": touch.raw_source_path,
                "source_id": source_id,
                "source_format": ROVODEV_SOURCE_FORMAT,
                "source_root": provider_source_root(
                    touch.source_root.as_deref(),
                    touch.raw_source_path.as_deref(),
                ),
                "metadata": touch.metadata,
                "session_id": session_id,
            }),
        ),
    }
}

pub(super) fn attach_rovodev_complete_content_locator(
    event: &mut RovoDevCoreEvent,
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
            "RovoDev complete-content native record identity is invalid".to_owned(),
        ));
    }
    let locator = rovodev_structured_locator(
        source_record_ordinal,
        source_record_subrecord_index,
        native_record_id,
    )?;
    let record_sha256 =
        CompleteContentBodyDigest::parse(format!("{:x}", Sha256::digest(record_bytes))).ok_or(
            CaptureError::SystemInvariant("RovoDev SHA-256 formatting produced an invalid digest"),
        )?;
    let content_ref = ContentRef::from_bytes(complete_text.as_bytes()).ok_or(
        CaptureError::SystemInvariant("RovoDev content length exceeds ContentRef bounds"),
    )?;
    let profile = verified_content_profile(
        CaptureProvider::RovoDev,
        ROVODEV_SOURCE_FORMAT,
        CompleteContentSourceFamily::Structured,
        VerifiedContentRole::MessageBody,
    )
    .ok_or(CaptureError::SystemInvariant(
        "RovoDev message route must have a verified-content profile",
    ))?;
    let locator = VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        profile,
        content_ref,
        CompleteContentSourceFamily::Structured,
        STRUCTURED_COMPLETE_CONTENT_LOCATOR_KIND,
        &locator,
        native_record_id,
        record_sha256,
    )
    .ok_or(CaptureError::SystemInvariant(
        "RovoDev complete-content locator exceeds its bounded schema",
    ))?;
    attach_verified_content_locator(&mut event.metadata, locator).ok_or(
        CaptureError::SystemInvariant("RovoDev verified-content locator collection is malformed"),
    )?;
    Ok(())
}

pub(super) fn rovodev_structured_locator(
    ordinal: u64,
    subrecord: u32,
    native_record_id: &str,
) -> Result<Vec<u8>> {
    const MAGIC: &[u8; 4] = b"SC\0\x01";
    let provider = CaptureProvider::RovoDev.as_str().as_bytes();
    let provider_len = u8::try_from(provider.len())
        .map_err(|_| CaptureError::SystemInvariant("provider identity exceeds locator bounds"))?;
    let native_record_id = native_record_id.as_bytes();
    let native_len = u16::try_from(native_record_id.len()).map_err(|_| {
        CaptureError::InvalidPayload(
            "RovoDev complete-content native record identity is too long".to_owned(),
        )
    })?;
    let mut locator =
        Vec::with_capacity(MAGIC.len() + 1 + provider.len() + 8 + 4 + 2 + native_record_id.len());
    locator.extend_from_slice(MAGIC);
    locator.push(provider_len);
    locator.extend_from_slice(provider);
    locator.extend_from_slice(&ordinal.to_be_bytes());
    locator.extend_from_slice(&subrecord.to_be_bytes());
    locator.extend_from_slice(&native_len.to_be_bytes());
    locator.extend_from_slice(native_record_id);
    Ok(locator)
}

pub(super) fn canonical_session(
    committed_store: &Store,
    source_id: Uuid,
    canonical_source_identity: &str,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    document: &PreparedDocument,
) -> Result<Session> {
    let id = provider_import_session_uuid(
        committed_store,
        CaptureProvider::RovoDev,
        &document.provider_session_id,
        source_id,
        Some(canonical_source_identity),
    )?;
    let parent_session_id = document
        .parent_provider_session_id
        .as_deref()
        .map(|parent| {
            provider_import_session_uuid(
                committed_store,
                CaptureProvider::RovoDev,
                parent,
                source_id,
                Some(canonical_source_identity),
            )
        })
        .transpose()?;
    let is_primary = parent_session_id.is_none();
    Ok(Session {
        id,
        history_record_id: options.history_record_id,
        parent_session_id,
        root_session_id: parent_session_id,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::RovoDev,
        external_session_id: Some(document.provider_session_id.clone()),
        external_agent_id: provider_string_field(
            &document.metadata,
            &["agent_id", "agentId", "agent_name", "agentName"],
        ),
        agent_type: if is_primary {
            AgentType::Primary
        } else {
            AgentType::Subagent
        },
        role_hint: Some(if is_primary { "primary" } else { "subagent" }.to_owned()),
        is_primary,
        status: if document.ended_at.is_some() {
            SessionStatus::Completed
        } else {
            SessionStatus::Imported
        },
        transcript_blob_id: None,
        started_at: document.started_at,
        ended_at: document.ended_at,
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": document.provider_session_id,
                "parent_provider_session_id": document.parent_provider_session_id,
                "source_format": ROVODEV_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "metadata": {
                    "title": provider_string_field(&document.metadata, &["title", "name"]),
                    "workspace_path": provider_string_field(
                        &document.metadata,
                        &["workspace_path", "workspacePath"]
                    ),
                    "message_count": document.messages.len(),
                    "metadata": document.metadata_preview,
                    "context": document.context_metadata,
                    "nativepath_parser": ROVODEV_NATIVE_PARSER_REVISION,
                },
            }),
        ),
    })
}

pub(super) fn relationship_placeholder(
    id: Uuid,
    source_id: Uuid,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    external_session_id: &str,
) -> Session {
    Session {
        id,
        history_record_id: options.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::RovoDev,
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
                "source_format": ROVODEV_SOURCE_FORMAT,
                "relationship_placeholder": true,
            }),
        ),
    }
}

pub(super) fn relationship_edge(
    source_id: Uuid,
    canonical_source_identity: &str,
    context: &ProviderAdapterContext,
    session: &Session,
    parent_id: Uuid,
) -> SessionEdge {
    let provider_session_id = session.external_session_id.as_deref().unwrap_or_default();
    SessionEdge {
        id: provider_source_edge_uuid(
            canonical_source_identity,
            provider_session_id,
            "parent_child",
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
                "provider_session_id": provider_session_id,
                "source_format": ROVODEV_SOURCE_FORMAT,
                "imported_at": context.imported_at,
            }),
        ),
    }
}

pub(super) fn canonical_actor(session: &Session) -> CanonicalActor {
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
