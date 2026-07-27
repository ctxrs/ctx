use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn relationship_placeholder(
    source: &CrushNativeSource,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    id: Uuid,
    external_session_id: &str,
    source_id: Uuid,
    canonical_source_identity: &str,
) -> Session {
    Session {
        id,
        history_record_id: options.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Crush,
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
                "source_format": CRUSH_SQLITE_SOURCE_FORMAT,
                "source_identity": canonical_source_identity,
                "source_revision": source.source_revision,
                "relationship_placeholder": true,
            }),
        ),
    }
}

pub(super) fn relationship_edge(
    source: &CrushNativeSource,
    context: &ProviderAdapterContext,
    session: &Session,
    parent_id: Uuid,
    source_id: Uuid,
    canonical_source_identity: &str,
) -> SessionEdge {
    SessionEdge {
        id: stable_capture_uuid(
            &format!(
                "provider-source-root:{canonical_source_identity}:session:{}:parent_child",
                session.external_session_id.as_deref().unwrap_or_default()
            ),
            "session-edge",
        ),
        from_session_id: session.id,
        to_session_id: parent_id,
        edge_type: SessionEdgeType::ParentChild,
        confidence: ctx_history_core::Confidence::Explicit,
        source_id: Some(source_id),
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": session.external_session_id,
                "source_format": CRUSH_SQLITE_SOURCE_FORMAT,
                "source_revision": source.source_revision,
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

pub(super) fn attach_crush_complete_content_locator(
    event: &mut CrushEventDraft,
    native_record_id: &str,
    rowid: i64,
    digest_values: &[NativeSqliteValue],
    complete_text: &str,
) -> Result<()> {
    if event.event_type != EventType::Message
        || event
            .payload
            .pointer("/text_retention/truncated")
            .and_then(Value::as_bool)
            != Some(true)
    {
        return Ok(());
    }
    let content_ref = ContentRef::from_bytes(complete_text.as_bytes()).ok_or_else(|| {
        CaptureError::InvalidPayload(
            "Crush complete message exceeds the bounded content-reference schema".to_owned(),
        )
    })?;
    let profile = verified_content_profile(
        CaptureProvider::Crush,
        CRUSH_SQLITE_SOURCE_FORMAT,
        CompleteContentSourceFamily::Sqlite,
        VerifiedContentRole::MessageBody,
    )
    .ok_or(CaptureError::SystemInvariant(
        "supported SQLite message route must have a verified-content profile",
    ))?;
    let locator = message_locator(rowid)?;
    let persisted = VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        profile,
        content_ref,
        CompleteContentSourceFamily::Sqlite,
        locator.kind(),
        locator.value(),
        native_record_id,
        message_record_digest(digest_values)?,
    )
    .ok_or_else(|| {
        CaptureError::InvalidPayload(
            "Crush message identity exceeds the bounded complete-content locator schema".to_owned(),
        )
    })?;
    attach_verified_content_locator(&mut event.metadata, persisted).ok_or(
        CaptureError::SystemInvariant("verified-content locator collection is malformed"),
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn crush_core_event(
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    provider_session_id: &str,
    source_id: Uuid,
    session_id: Uuid,
    line_number: usize,
    event: &CrushEventDraft,
    native_record_id: &str,
    event_hash: &str,
    identity: &ProviderEventImportIdentity,
    run_id: Option<Uuid>,
) -> Event {
    let dedupe_key =
        Store::provider_event_dedupe_key_with_payload_hash(&identity.dedupe_key, event_hash)
            .unwrap_or_else(|| identity.dedupe_key.clone());
    let mut event_metadata = event.metadata.clone();
    let verified_content_locators = event_metadata
        .as_object_mut()
        .and_then(|metadata| metadata.remove(VERIFIED_CONTENT_LOCATORS_METADATA_KEY));
    let mut sync_metadata = json!({
        "provider_session_id": provider_session_id,
        "provider_event_index": event.provider_event_index,
        "legacy_provider_event_index": event.legacy_provider_event_index,
        "provider_event_hash": event_hash,
        "provider_event_hash_authority":
            ProviderEventHashAuthority::NormalizedPayloadFallback.as_str(),
        "native_record_id": native_record_id,
        "cursor": event.cursor,
        "source_format": CRUSH_SQLITE_SOURCE_FORMAT,
        "source_trust": "provider_native",
        "fixture_line": line_number,
        "imported_at": context.imported_at,
        "event_idempotency_key": format!(
            "provider-event:{}:{}:{}",
            CaptureProvider::Crush.as_str(),
            provider_session_id,
            event.provider_event_index,
        ),
        "source_record_ordinal": Value::Null,
        "source_record_subrecord_index": Value::Null,
        "metadata": event_metadata,
    });
    if let (Some(metadata), Some(locators)) =
        (sync_metadata.as_object_mut(), verified_content_locators)
    {
        metadata.insert(VERIFIED_CONTENT_LOCATORS_METADATA_KEY.to_owned(), locators);
    }
    Event {
        id: identity.id,
        seq: identity.seq,
        history_record_id: options.history_record_id,
        session_id: Some(session_id),
        run_id,
        event_type: event.event_type,
        role: event.role,
        occurred_at: event.occurred_at,
        capture_source_id: Some(source_id),
        payload: json!({
            "provider": CaptureProvider::Crush.as_str(),
            "provider_session_id": provider_session_id,
            "provider_event_index": event.provider_event_index,
            "provider_event_hash": event_hash,
            "native_record_id": native_record_id,
            "cursor": event.cursor,
            "artifacts": [],
            "body": compact_provider_result_payload(event.event_type, &event.payload),
        }),
        payload_blob_id: None,
        dedupe_key: Some(dedupe_key),
        sync: provider_sync_metadata(Fidelity::Imported, sync_metadata),
    }
}

#[allow(clippy::too_many_arguments)]
fn crush_event_import_identity(
    store: &Store,
    provider_session_id: &str,
    source_id: Uuid,
    session_id: Uuid,
    native_record_id: &str,
    provider_event_index: u64,
    legacy_provider_event_index: u64,
) -> Result<ProviderEventImportIdentity> {
    let legacy_source_event_id = stable_capture_uuid(
        &format!("provider-source:{source_id}:event:{legacy_provider_event_index}"),
        "event",
    );
    match store.get_event(legacy_source_event_id) {
        Ok(existing) => {
            let legacy_dedupe = Store::provider_source_event_dedupe_key(
                source_id,
                legacy_provider_event_index,
                native_record_id,
            );
            let migrated_dedupe_prefix =
                format!("provider-source:{source_id}:{legacy_provider_event_index}:");
            let exact_released = existing.dedupe_key.as_deref() == Some(legacy_dedupe.as_str())
                && existing
                    .sync
                    .metadata
                    .get("provider_event_hash")
                    .and_then(Value::as_str)
                    == Some(native_record_id);
            let exact_migrated = existing
                .dedupe_key
                .as_deref()
                .is_some_and(|dedupe| dedupe.starts_with(&migrated_dedupe_prefix))
                && existing
                    .sync
                    .metadata
                    .get("native_record_id")
                    .and_then(Value::as_str)
                    == Some(native_record_id)
                && existing
                    .sync
                    .metadata
                    .get("legacy_provider_event_index")
                    .and_then(Value::as_u64)
                    == Some(legacy_provider_event_index)
                && existing
                    .sync
                    .metadata
                    .get("provider_event_hash_authority")
                    .and_then(Value::as_str)
                    == Some(ProviderEventHashAuthority::NormalizedPayloadFallback.as_str());
            if existing.capture_source_id == Some(source_id)
                && existing.session_id == Some(session_id)
                && existing
                    .sync
                    .metadata
                    .get("provider_session_id")
                    .and_then(Value::as_str)
                    == Some(provider_session_id)
                && (exact_released || exact_migrated)
            {
                return Ok(ProviderEventImportIdentity {
                    id: existing.id,
                    seq: existing.seq,
                    dedupe_key: existing.dedupe_key.ok_or(CaptureError::SystemInvariant(
                        "Crush released event has no dedupe key",
                    ))?,
                    run_source_id: existing.capture_source_id,
                });
            }
        }
        Err(ctx_history_store::StoreError::NotFound(_)) => {}
        Err(error) => return Err(CaptureError::Store(error)),
    }

    provider_event_import_identity_with_exact_legacy_source(
        store,
        CaptureProvider::Crush,
        provider_session_id,
        source_id,
        provider_event_index,
        legacy_provider_event_index,
        native_record_id,
        None,
        Some(legacy_provider_event_index),
        session_id
            == crate::provider::importer::provider_session_uuid(
                CaptureProvider::Crush,
                provider_session_id,
            ),
    )
}

#[allow(clippy::too_many_arguments)]
fn crush_command_run(
    provider_session_id: &str,
    history_record_id: Option<Uuid>,
    source_id: Uuid,
    session_id: Uuid,
    event: &CrushEventDraft,
    native_record_id: &str,
    event_hash: &str,
    run_source_id: Option<Uuid>,
) -> Option<Run> {
    if event.event_type != EventType::CommandOutput {
        return None;
    }
    let run_id = run_source_id.map_or_else(
        || {
            stable_capture_uuid(
                &format!(
                    "provider:{}:{provider_session_id}:run:{native_record_id}",
                    CaptureProvider::Crush.as_str(),
                ),
                "run",
            )
        },
        |run_source_id| {
            stable_capture_uuid(
                &format!("provider-source:{run_source_id}:run:{native_record_id}"),
                "run",
            )
        },
    );
    Some(Run {
        id: run_id,
        history_record_id,
        session_id: Some(session_id),
        run_type: RunType::Command,
        status: match event.payload.get("result_outcome").and_then(Value::as_str) {
            Some("failure") => RunStatus::Failed,
            Some("success") => RunStatus::Succeeded,
            _ => RunStatus::Partial,
        },
        started_at: event.occurred_at,
        ended_at: Some(event.occurred_at),
        exit_code: None,
        cwd: None,
        command_preview: None,
        input_blob_id: None,
        output_blob_id: None,
        timestamps: timestamps(event.occurred_at),
        source_id: Some(source_id),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": provider_session_id,
                "provider_event_index": event.provider_event_index,
                "provider_event_hash": event_hash,
                "native_record_id": native_record_id,
                "call_id": Value::Null,
                "source": "provider_command_output",
            }),
        ),
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_message(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    source: &CrushNativeSource,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    canonical_source_identity: &str,
    route_binding: &ctx_history_store::ProviderSourceRouteBinding,
    mut projection: CrushMessageProjection,
    touches: Vec<CrushFileTouchDraft>,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let existing_source = committed_store
        .capture_source_by_canonical_identity_session(
            CaptureProvider::Crush,
            CRUSH_SQLITE_SOURCE_FORMAT,
            &context.machine_id,
            canonical_source_identity,
            &projection.provider_session_id,
        )?
        .ok_or(CaptureError::SystemInvariant(
            "Crush prepared message has no persisted capture source",
        ))?;
    group.bind_capture_source_provider_route(existing_source.id, route_binding)?;
    let session = committed_store
        .session_by_capture_source_and_external_session(
            existing_source.id,
            CaptureProvider::Crush,
            &projection.provider_session_id,
        )?
        .ok_or(CaptureError::SystemInvariant(
            "Crush prepared message has no canonical parent session",
        ))?;

    if let Some(event) = projection.event.take() {
        let event_hash = event.provider_event_hash.clone();
        let identity = crush_event_import_identity(
            committed_store,
            &projection.provider_session_id,
            existing_source.id,
            session.id,
            &projection.native_record_id,
            event.provider_event_index,
            event.legacy_provider_event_index,
        )?;
        let run = crush_command_run(
            &projection.provider_session_id,
            options.history_record_id,
            existing_source.id,
            session.id,
            &event,
            &projection.native_record_id,
            &event_hash,
            identity.run_source_id,
        );
        if let Some(run) = &run {
            group.upsert_run(run)?;
        }
        let normalized = crush_core_event(
            context,
            options,
            &projection.provider_session_id,
            existing_source.id,
            session.id,
            projection.line_number,
            &event,
            &projection.native_record_id,
            &event_hash,
            &identity,
            run.as_ref().map(|run| run.id),
        );
        let normalized_event_id = normalized.id;
        if group.reconcile_provider_event_migrating_exact_legacy_provider_hash(
            &normalized,
            &projection.native_record_id,
        )? {
            summary.imported_events = summary.imported_events.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        } else {
            summary.skipped_events = summary.skipped_events.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
        }
        summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
        for touch in touches {
            publish_file_touch(
                committed_store,
                group,
                source,
                context,
                options,
                canonical_source_identity,
                route_binding,
                touch,
                Some(normalized_event_id),
                summary,
            )?;
        }
    } else {
        for touch in touches {
            publish_file_touch(
                committed_store,
                group,
                source,
                context,
                options,
                canonical_source_identity,
                route_binding,
                touch,
                None,
                summary,
            )?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_file_touch(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    _source: &CrushNativeSource,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    canonical_source_identity: &str,
    route_binding: &ctx_history_store::ProviderSourceRouteBinding,
    touch: CrushFileTouchDraft,
    explicit_event_id: Option<Uuid>,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let Some(existing_source) = committed_store.capture_source_by_canonical_identity_session(
        CaptureProvider::Crush,
        CRUSH_SQLITE_SOURCE_FORMAT,
        &context.machine_id,
        canonical_source_identity,
        &touch.provider_session_id,
    )?
    else {
        return Err(CaptureError::SystemInvariant(
            "Crush prepared file touch has no persisted capture source",
        ));
    };
    group.bind_capture_source_provider_route(existing_source.id, route_binding)?;
    let session = committed_store
        .session_by_capture_source_and_external_session(
            existing_source.id,
            CaptureProvider::Crush,
            &touch.provider_session_id,
        )?
        .ok_or(CaptureError::SystemInvariant(
            "Crush prepared file touch has no canonical parent session",
        ))?;
    let event_id = match explicit_event_id {
        Some(event_id) => Some(event_id),
        None => touch
            .provider_event_index
            .map(|index| {
                crate::provider::importer::provider_file_touch_event_id(
                    committed_store,
                    CaptureProvider::Crush,
                    &touch.provider_session_id,
                    existing_source.id,
                    index,
                    session.id
                        == crate::provider::importer::provider_session_uuid(
                            CaptureProvider::Crush,
                            &touch.provider_session_id,
                        ),
                )
            })
            .transpose()?
            .flatten(),
    };
    let id = provider_file_touch_import_id(
        committed_store,
        CaptureProvider::Crush,
        &touch.provider_session_id,
        existing_source.id,
        touch.provider_event_index,
        touch.provider_touch_index,
        session.id
            == crate::provider::importer::provider_session_uuid(
                CaptureProvider::Crush,
                &touch.provider_session_id,
            ),
    )?;
    group.upsert_file_touched(&FileTouched {
        id,
        history_record_id: options.history_record_id,
        run_id: None,
        event_id,
        vcs_workspace_id: None,
        path: touch.path,
        change_kind: touch.change_kind,
        old_path: touch.old_path,
        line_count_delta: touch.line_count_delta,
        confidence: touch.confidence,
        timestamps: timestamps(touch.occurred_at),
        source_id: Some(existing_source.id),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider": CaptureProvider::Crush.as_str(),
                "provider_session_id": touch.provider_session_id,
                "provider_touch_index": touch.provider_touch_index,
                "provider_event_index": touch.provider_event_index,
                "source_format": CRUSH_SQLITE_SOURCE_FORMAT,
                "metadata": touch.metadata,
            }),
        ),
    })?;
    summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
    Ok(())
}

pub(super) fn encode_core_cursor(cursor: &CrushNativeCursor) -> Result<String> {
    serde_json::to_string(cursor).map_err(CaptureError::from)
}

pub(super) fn provider_sync_cursor(
    machine_id: &str,
    stream: String,
    cursor: String,
    observed_at: DateTime<Utc>,
) -> SyncCursor {
    SyncCursor {
        id: stable_capture_uuid(
            &format!(
                "provider-cursor:{}:{}:{}",
                CaptureProvider::Crush.as_str(),
                machine_id,
                stream
            ),
            "cursor",
        ),
        team_id: None,
        device_id: machine_id.to_owned(),
        stream,
        cursor,
        last_synced_at: Some(observed_at),
        timestamps: timestamps(observed_at),
    }
}

pub(super) fn core_publication_id(
    source: &CrushNativeSource,
    page: &CrushNativePage,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(CRUSH_NATIVE_PUBLICATION_DOMAIN);
    hash_field(&mut digest, source.locator_identity.as_bytes());
    hash_field(&mut digest, source.source_revision.as_bytes());
    hash_field(
        &mut digest,
        &serde_json::to_vec(&page.expected.frontier).unwrap_or_default(),
    );
    hash_field(
        &mut digest,
        &serde_json::to_vec(&page.next.frontier).unwrap_or_default(),
    );
    hash_field(&mut digest, transition.next().cursor.as_bytes());
    format!("crush-nativepath-v1:{:x}", digest.finalize())
}
