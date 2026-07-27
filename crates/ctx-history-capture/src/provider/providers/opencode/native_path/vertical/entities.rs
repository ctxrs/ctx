use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_session(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    context: &OpenCodePublicationContext<'_>,
    route_binding: &ctx_history_store::ProviderSourceRouteBinding,
    relocated: bool,
    page_session_ids: &BTreeSet<&str>,
    native: &OpenCodeNativeSession,
    summary: &mut ProviderImportSummary,
    retained: &mut NativePathRetainedSourceEntities,
) -> Result<()> {
    let source_id =
        source_id_for_session(committed_store, context, &native.native_identity, relocated)?;
    let source = capture_source(context, native, source_id)?;
    group.upsert_capture_source(&source)?;
    group.bind_capture_source_provider_route(source_id, route_binding)?;
    retained.capture_source_ids.push(source_id);
    let session_id = provider_import_session_uuid(
        committed_store,
        context.dialect.provider,
        &native.native_identity,
        source_id,
        Some(&context.canonical_source_identity),
    )?;
    let parent_session_id = native
        .parent_identity
        .as_deref()
        .map(|parent| {
            provider_import_session_uuid(
                committed_store,
                context.dialect.provider,
                parent,
                source_id,
                Some(&context.canonical_source_identity),
            )
        })
        .transpose()?;
    let root_session_id = (native.root_identity != native.native_identity)
        .then(|| {
            provider_import_session_uuid(
                committed_store,
                context.dialect.provider,
                &native.root_identity,
                source_id,
                Some(&context.canonical_source_identity),
            )
        })
        .transpose()?
        .or(parent_session_id);
    for (related_id, external_id) in [
        parent_session_id.zip(native.parent_identity.as_deref()),
        root_session_id.zip(Some(native.root_identity.as_str())),
    ]
    .into_iter()
    .flatten()
    {
        if related_id != session_id {
            retained.session_ids.push(related_id);
        }
        if related_id != session_id
            && committed_store.get_session(related_id).is_err()
            && !page_session_ids.contains(external_id)
        {
            group.upsert_session(&relationship_placeholder(
                context,
                source_id,
                related_id,
                external_id,
            ))?;
        }
    }
    let session = canonical_session(
        context,
        native,
        source_id,
        session_id,
        parent_session_id,
        root_session_id,
    )?;
    let existed = committed_store.get_session(session_id).is_ok();
    group.upsert_session(&session)?;
    retained.session_ids.push(session_id);
    if existed {
        summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    } else {
        summary.imported_sessions = summary.imported_sessions.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    }
    if let Some(parent_id) = parent_session_id {
        let edge = relationship_edge(context, source_id, &session, parent_id);
        retained.session_edge_ids.push(edge.id);
        let existed = committed_store.session_edge_exists(edge.id)?;
        group.upsert_projection_neutral_session_edge(&actor(&session), &edge)?;
        if existed {
            summary.skipped_edges = summary.skipped_edges.saturating_add(1);
        } else {
            summary.imported_edges = summary.imported_edges.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        }
    }
    Ok(())
}

pub(super) fn source_id_for_session(
    committed_store: &Store,
    context: &OpenCodePublicationContext<'_>,
    provider_session_id: &str,
    relocated: bool,
) -> Result<Uuid> {
    let candidate_source_ids = committed_store
        .list_capture_sources()?
        .into_iter()
        .filter(|source| {
            source.descriptor.provider == context.dialect.provider
                && source.descriptor.machine_id == context.adapter.machine_id
                && source.descriptor.source_format.as_deref() == Some(context.dialect.source_format)
                && source.descriptor.source_identity.as_deref()
                    == Some(context.canonical_source_identity.as_str())
                && source.descriptor.external_session_id.as_deref() == Some(provider_session_id)
        })
        .map(|source| source.id)
        .collect::<BTreeSet<_>>();
    if !candidate_source_ids.is_empty() {
        let source_id = committed_store
            .list_sessions()?
            .into_iter()
            .filter(|session| {
                session.provider == context.dialect.provider
                    && session.external_session_id.as_deref() == Some(provider_session_id)
            })
            .filter_map(|session| session.capture_source_id)
            .filter(|source_id| candidate_source_ids.contains(source_id))
            .min()
            .or_else(|| candidate_source_ids.first().copied())
            .ok_or(CaptureError::SystemInvariant(
                "OpenCode NativePath lost a nonempty capture-source candidate",
            ))?;
        return Ok(source_id);
    }
    if relocated || context.replacement {
        return Ok(stable_capture_uuid(
            &serde_json::to_string(&(
                "opencode-nativepath-canonical-source-v1",
                context.dialect.provider.as_str(),
                context.dialect.source_format,
                &context.canonical_source_identity,
                provider_session_id,
            ))?,
            "source",
        ));
    }
    Ok(provider_scoped_source_uuid(
        context.dialect.provider,
        provider_session_id,
        context.dialect.source_format,
        Some(&context.raw_source_path),
    ))
}

pub(super) fn capture_source(
    context: &OpenCodePublicationContext<'_>,
    native: &OpenCodeNativeSession,
    source_id: Uuid,
) -> Result<CaptureSource> {
    Ok(CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: context.dialect.provider,
            machine_id: context.adapter.machine_id.clone(),
            process_id: None,
            cwd: native.directory.clone(),
            raw_source_path: Some(context.raw_source_path.clone()),
            source_format: Some(context.dialect.source_format.to_owned()),
            source_root: Some(context.source_root.clone()),
            source_identity: Some(context.canonical_source_identity.clone()),
            external_session_id: Some(native.native_identity.clone()),
        },
        started_at: timestamp(
            native.time_created,
            context.dialect.session_time_created_field,
        )?,
        ended_at: None,
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": native.native_identity,
                "source_format": context.dialect.source_format,
                "source_trust": "provider_native",
                "imported_at": context.adapter.imported_at,
                "source_identity": context.canonical_source_identity,
                "source_root": context.source_root,
                "source_revision": context.source_revision,
                "source_identity_key": provider_scoped_source_identity_key(
                    context.dialect.provider,
                    &native.native_identity,
                    context.dialect.source_format,
                    Some(&context.raw_source_path),
                ),
            }),
        ),
    })
}

pub(super) fn canonical_session(
    context: &OpenCodePublicationContext<'_>,
    native: &OpenCodeNativeSession,
    source_id: Uuid,
    session_id: Uuid,
    parent_session_id: Option<Uuid>,
    root_session_id: Option<Uuid>,
) -> Result<Session> {
    let is_subagent = parent_session_id.is_some();
    Ok(Session {
        id: session_id,
        history_record_id: context.options.history_record_id,
        parent_session_id,
        root_session_id,
        capture_source_id: Some(source_id),
        provider: context.dialect.provider,
        external_session_id: Some(native.native_identity.clone()),
        external_agent_id: native.agent_identity.clone(),
        agent_type: if is_subagent {
            AgentType::Subagent
        } else {
            AgentType::Primary
        },
        role_hint: native
            .agent_identity
            .clone()
            .or_else(|| Some(if is_subagent { "subagent" } else { "primary" }.to_owned())),
        is_primary: !is_subagent,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: timestamp(
            native.time_created,
            context.dialect.session_time_created_field,
        )?,
        ended_at: None,
        timestamps: timestamps(context.adapter.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": native.native_identity,
                "parent_provider_session_id": native.parent_identity,
                "root_provider_session_id": native.root_identity,
                "source_format": context.dialect.source_format,
                "source_trust": "provider_native",
                "imported_at": context.adapter.imported_at,
                "metadata": {
                    "title": native.title,
                    "directory": native.directory,
                    "model": native.model_identity,
                    "agent": native.agent_identity,
                    "time_updated": native.time_updated,
                },
            }),
        ),
    })
}

pub(super) fn relationship_placeholder(
    context: &OpenCodePublicationContext<'_>,
    source_id: Uuid,
    id: Uuid,
    external_session_id: &str,
) -> Session {
    Session {
        id,
        history_record_id: context.options.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: context.dialect.provider,
        external_session_id: Some(external_session_id.to_owned()),
        external_agent_id: None,
        agent_type: AgentType::Unknown,
        role_hint: Some("relationship_placeholder".to_owned()),
        is_primary: false,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: context.adapter.imported_at,
        ended_at: None,
        timestamps: timestamps(context.adapter.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Partial,
            json!({
                "provider_session_id": external_session_id,
                "source_format": context.dialect.source_format,
                "source_identity": context.canonical_source_identity,
                "relationship_placeholder": true,
            }),
        ),
    }
}

pub(super) fn relationship_edge(
    context: &OpenCodePublicationContext<'_>,
    source_id: Uuid,
    session: &Session,
    parent_id: Uuid,
) -> SessionEdge {
    SessionEdge {
        id: provider_source_edge_uuid(
            &context.canonical_source_identity,
            session.external_session_id.as_deref().unwrap_or_default(),
            "parent_child",
        ),
        from_session_id: session.id,
        to_session_id: parent_id,
        edge_type: SessionEdgeType::ParentChild,
        confidence: Confidence::Explicit,
        source_id: Some(source_id),
        timestamps: timestamps(context.adapter.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": session.external_session_id,
                "source_format": context.dialect.source_format,
                "imported_at": context.adapter.imported_at,
            }),
        ),
    }
}

pub(super) fn actor(session: &Session) -> ctx_history_store::CanonicalActor {
    ctx_history_store::CanonicalActor {
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
    reader: &OpenCodeNativePathReader,
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    context: &OpenCodePublicationContext<'_>,
    route_binding: &ctx_history_store::ProviderSourceRouteBinding,
    relocated: bool,
    native: &OpenCodeNativeEvent,
    summary: &mut ProviderImportSummary,
    retained: &mut NativePathRetainedSourceEntities,
) -> Result<()> {
    let source_id = source_id_for_session(
        committed_store,
        context,
        &native.session_identity,
        relocated,
    )?;
    let session_id = provider_import_session_uuid(
        committed_store,
        context.dialect.provider,
        &native.session_identity,
        source_id,
        Some(&context.canonical_source_identity),
    )?;
    let session = committed_store.get_session(session_id).map_err(|_| {
        CaptureError::InvalidPayload(format!(
            "{} event references missing committed session {}",
            context.dialect.display_name, native.session_identity
        ))
    })?;
    let source = event_capture_source(context, &session, source_id)?;
    group.upsert_capture_source(&source)?;
    group.bind_capture_source_provider_route(source_id, route_binding)?;
    retained.capture_source_ids.push(source_id);
    retained.session_ids.push(session_id);

    let event_type = event_type(native.kind);
    let occurred_at = timestamp(
        native.time_created,
        context.dialect.event_time_created_field,
    )?;
    let policy_text = provider_policy_event_text(event_type, &native.searchable_text, &native.body);
    let result_evidence = native
        .body
        .get("result_evidence")
        .cloned()
        .unwrap_or_else(|| {
            provider_result_identifier_evidence(event_type, &native.searchable_text, &native.body)
        });
    let result_outcome = native
        .body
        .get("result_outcome")
        .cloned()
        .unwrap_or_else(|| provider_result_outcome_evidence(event_type, &native.body));
    let mut payload = if event_type == EventType::Message {
        super::super::super::opencode_normalized_message_payload(
            &native.message_identity,
            &native.searchable_text,
            &native.body,
        )
    } else {
        json!({
            "entry_type": event_kind_label(native.kind),
            "message_id": native.message_identity,
            "text": policy_text.text,
            "text_retention": policy_text.retention.as_json(),
            "result_evidence": result_evidence,
            "result_outcome": result_outcome,
            "body": provider_capped_json(
                &provider_policy_body(event_type, &native.body),
                PROVIDER_MAX_PREVIEW_CHARS,
            ),
        })
    };
    if let Some(object) = payload.as_object_mut() {
        for key in ["exit_code", "timed_out", "duration_ms"] {
            if let Some(value) = native.body.get(key) {
                object.insert(key.to_owned(), value.clone());
            }
        }
    }
    payload = crate::provider::importer::compact_provider_result_payload(event_type, &payload);
    if matches!(event_type, EventType::ToolOutput | EventType::CommandOutput) {
        if let Some(object) = payload.as_object_mut() {
            object.remove("body");
            object.remove("output_preview");
        }
    }
    let normalized_payload_hash = compute_payload_hash(&payload)?;
    let legacy_native_record_id = legacy_provider_event_hash(context, native);
    let legacy_provider_event_hash = native.content_digest.as_str();
    let verified_content_locators = if event_type == EventType::Message
        && payload
            .pointer("/text_retention/truncated")
            .and_then(Value::as_bool)
            == Some(true)
    {
        let (locator, values, complete_text, reconstructed_hash) =
            reader.complete_message_record(native, context.dialect)?;
        if reconstructed_hash != normalized_payload_hash {
            return Err(CaptureError::SystemInvariant(
                "OpenCode complete-message reconstruction hash diverged from publication",
            ));
        }
        let mut metadata = json!({});
        crate::complete_content::sqlite::attach_sqlite_complete_content_locator(
            context.dialect.provider,
            context.dialect.source_format,
            &legacy_native_record_id,
            &payload,
            &mut metadata,
            &locator,
            &values[1..],
            || complete_text,
        )?;
        metadata
            .get(crate::complete_content::VERIFIED_CONTENT_LOCATORS_METADATA_KEY)
            .cloned()
    } else {
        None
    };
    let identity = publication_identity(
        committed_store,
        context,
        source_id,
        session_id,
        native,
        &normalized_payload_hash,
        &legacy_native_record_id,
        legacy_provider_event_hash,
    )?;
    let mut sync_metadata = json!({
        "provider_session_id": native.session_identity,
        "provider_event_index": identity.provider_event_index,
        "provider_event_hash": normalized_payload_hash,
        "provider_event_hash_authority": "normalized_payload_fallback",
        "cursor": event_cursor(native),
        "source_format": context.dialect.source_format,
        "source_trust": "provider_native",
        "imported_at": context.adapter.imported_at,
        "source_record_ordinal": native.source_record_ordinal,
        "source_record_subrecord_index": 0,
        "native_record_id": legacy_native_record_id,
        "metadata": {
            "stable_provider_event_index": native.provider_event_index,
            "legacy_provider_event_index": native.legacy_provider_event_index,
            "message_id": native.message_identity,
            "time_created": native.time_created,
            "time_updated": native.time_updated,
            "native_locator_kind": native.locator.kind,
            "native_locator_version": native.locator.version,
        },
    });
    if let (Some(object), Some(locators)) =
        (sync_metadata.as_object_mut(), verified_content_locators)
    {
        object.insert(
            crate::complete_content::VERIFIED_CONTENT_LOCATORS_METADATA_KEY.to_owned(),
            locators,
        );
    }
    let event = Event {
        id: identity.identity.id,
        seq: identity.identity.seq,
        history_record_id: context.options.history_record_id,
        session_id: Some(session_id),
        run_id: None,
        event_type,
        role: Some(event_role(&native.role)),
        occurred_at,
        capture_source_id: Some(source_id),
        payload,
        payload_blob_id: None,
        dedupe_key: Some(identity.identity.dedupe_key.clone()),
        sync: provider_sync_metadata(Fidelity::Imported, sync_metadata),
    };
    retained.event_ids.push(event.id);
    let changed = if identity.migrate_exact_released_hash {
        group.reconcile_provider_event_migrating_exact_legacy_provider_hash(
            &event,
            legacy_provider_event_hash,
        )?
    } else {
        group.reconcile_provider_event(
            &event,
            ProviderEventHashAuthority::NormalizedPayloadFallback,
        )?
    };
    if changed {
        summary.imported_events = summary.imported_events.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    } else {
        summary.skipped_events = summary.skipped_events.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    }
    summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);

    for (touch_index, touch) in native.file_touches.iter().enumerate() {
        let local_touch_index = u64::try_from(touch_index).map_err(|_| {
            CaptureError::InvalidPayload(
                "OpenCode NativePath file-touch index exceeds u64".to_owned(),
            )
        })?;
        // Query pages encode signed SQLite rowids into sortable u64 space.
        // Undo that transform before constructing the bounded legacy ordinal.
        let legacy_source_record_ordinal = native.source_record_ordinal ^ (1_u64 << 63);
        let legacy_touch_index = legacy_source_record_ordinal
            .checked_mul(u64::from(u16::MAX) + 1)
            .and_then(|base| base.checked_add(local_touch_index));
        let id = opencode_file_touch_import_id(
            committed_store,
            context,
            native,
            source_id,
            local_touch_index,
            legacy_touch_index,
            session_id
                == crate::provider::importer::provider_session_uuid(
                    context.dialect.provider,
                    &native.session_identity,
                ),
        )?;
        retained.file_touch_ids.push(id);
        group.upsert_file_touched(&FileTouched {
            id,
            history_record_id: context.options.history_record_id,
            run_id: None,
            event_id: Some(event.id),
            vcs_workspace_id: None,
            path: touch.path.clone(),
            change_kind: Some(FileChangeKind::Modified),
            old_path: None,
            line_count_delta: None,
            confidence: Confidence::Explicit,
            timestamps: timestamps(occurred_at),
            source_id: Some(source_id),
            sync: provider_sync_metadata(
                Fidelity::Imported,
                json!({
                    "provider": context.dialect.provider.as_str(),
                    "provider_session_id": native.session_identity,
                    "provider_touch_index": legacy_touch_index,
                    "provider_event_index": identity.provider_event_index,
                    "stable_provider_event_index": native.provider_event_index,
                    "legacy_source_record_ordinal": legacy_source_record_ordinal,
                    "source_event_touch_index": local_touch_index,
                    "source_format": context.dialect.source_format,
                    "session_id": session_id,
                }),
            ),
        })?;
    }
    Ok(())
}

pub(super) fn opencode_file_touch_import_id(
    store: &Store,
    context: &OpenCodePublicationContext<'_>,
    native: &OpenCodeNativeEvent,
    source_id: Uuid,
    local_touch_index: u64,
    legacy_touch_index: Option<u64>,
    allow_legacy_provider_identity: bool,
) -> Result<Uuid> {
    let identity_key = serde_json::to_string(&(
        "provider-source-event-file-touch-v2",
        source_id,
        native.provider_event_index,
        local_touch_index,
    ))?;
    let source_scoped_id = stable_capture_uuid(&identity_key, "file-touch");
    if store.file_touched_exists(source_scoped_id)? {
        return Ok(source_scoped_id);
    }

    if let Some(legacy_touch_index) = legacy_touch_index {
        let legacy_source_id = stable_capture_uuid(
            &format!("provider-source:{source_id}:file-touch:{legacy_touch_index}"),
            "file-touch",
        );
        if store.file_touched_exists(legacy_source_id)? {
            return Ok(legacy_source_id);
        }
        if allow_legacy_provider_identity {
            let legacy_provider_id = stable_capture_uuid(
                &format!(
                    "provider:{}:{}:file-touch:{legacy_touch_index}",
                    context.dialect.provider.as_str(),
                    native.session_identity
                ),
                "file-touch",
            );
            if store.file_touched_exists(legacy_provider_id)? {
                return Ok(legacy_provider_id);
            }
        }
    }

    let compatible_id = provider_file_touch_import_id(
        store,
        context.dialect.provider,
        &native.session_identity,
        source_id,
        Some(native.provider_event_index),
        local_touch_index,
        allow_legacy_provider_identity,
    )?;
    if store.file_touched_exists(compatible_id)? {
        return Ok(compatible_id);
    }
    Ok(source_scoped_id)
}

struct OpenCodePublicationIdentity {
    identity: ProviderEventImportIdentity,
    provider_event_index: u64,
    migrate_exact_released_hash: bool,
}

#[allow(clippy::too_many_arguments)]
fn publication_identity(
    store: &Store,
    context: &OpenCodePublicationContext<'_>,
    source_id: Uuid,
    session_id: Uuid,
    native: &OpenCodeNativeEvent,
    normalized_payload_hash: &str,
    exact_released_native_record_id: &str,
    exact_released_provider_event_hash: &str,
) -> Result<OpenCodePublicationIdentity> {
    let allow_legacy_provider_identity = session_id
        == crate::provider::importer::provider_session_uuid(
            context.dialect.provider,
            &native.session_identity,
        );
    let stable = provider_event_import_identity_with_exact_legacy_source(
        store,
        context.dialect.provider,
        &native.session_identity,
        source_id,
        native.provider_event_index,
        native.source_record_ordinal,
        normalized_payload_hash,
        None,
        None,
        false,
    )?;
    if stored_event(store, stable.id)?.is_some() {
        return Ok(OpenCodePublicationIdentity {
            identity: stable,
            provider_event_index: native.provider_event_index,
            migrate_exact_released_hash: false,
        });
    }

    let mut released = provider_event_import_identity_with_exact_legacy_source(
        store,
        context.dialect.provider,
        &native.session_identity,
        source_id,
        native.legacy_provider_event_index,
        native.legacy_provider_event_index,
        exact_released_native_record_id,
        None,
        Some(native.legacy_provider_event_index),
        allow_legacy_provider_identity,
    )?;
    if let Some(existing) = stored_event(store, released.id)? {
        let metadata = &existing.sync.metadata;
        let same_native_record = metadata.get("provider_session_id").and_then(Value::as_str)
            == Some(native.session_identity.as_str())
            && metadata.get("native_record_id").and_then(Value::as_str)
                == Some(exact_released_native_record_id);
        let authority = metadata
            .get("provider_event_hash_authority")
            .and_then(Value::as_str);
        let stored_hash = metadata.get("provider_event_hash").and_then(Value::as_str);
        let exact_released = authority == Some("provider_supplied")
            && stored_hash == Some(exact_released_provider_event_hash);
        let already_migrated = authority == Some("normalized_payload_fallback")
            && stored_hash == Some(normalized_payload_hash);
        if same_native_record && (exact_released || already_migrated) {
            let existing_dedupe =
                existing
                    .dedupe_key
                    .as_deref()
                    .ok_or(CaptureError::SystemInvariant(
                        "released OpenCode event has no provider dedupe key",
                    ))?;
            released.dedupe_key = Store::provider_event_dedupe_key_with_payload_hash(
                existing_dedupe,
                normalized_payload_hash,
            )
            .ok_or(CaptureError::SystemInvariant(
                "released OpenCode event has an invalid provider dedupe key",
            ))?;
            released.id = existing.id;
            released.seq = existing.seq;
            released.run_source_id = existing.capture_source_id;
            return Ok(OpenCodePublicationIdentity {
                identity: released,
                provider_event_index: native.legacy_provider_event_index,
                migrate_exact_released_hash: exact_released,
            });
        }
    }

    Ok(OpenCodePublicationIdentity {
        identity: stable,
        provider_event_index: native.provider_event_index,
        migrate_exact_released_hash: false,
    })
}

pub(super) fn stored_event(store: &Store, id: Uuid) -> Result<Option<Event>> {
    match store.get_event(id) {
        Ok(event) => Ok(Some(event)),
        Err(StoreError::NotFound(_))
        | Err(StoreError::Sql(rusqlite::Error::QueryReturnedNoRows)) => Ok(None),
        Err(error) => Err(CaptureError::Store(error)),
    }
}

pub(super) fn event_capture_source(
    context: &OpenCodePublicationContext<'_>,
    session: &Session,
    source_id: Uuid,
) -> Result<CaptureSource> {
    Ok(CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: context.dialect.provider,
            machine_id: context.adapter.machine_id.clone(),
            process_id: None,
            cwd: None,
            raw_source_path: Some(context.raw_source_path.clone()),
            source_format: Some(context.dialect.source_format.to_owned()),
            source_root: Some(context.source_root.clone()),
            source_identity: Some(context.canonical_source_identity.clone()),
            external_session_id: session.external_session_id.clone(),
        },
        started_at: session.started_at,
        ended_at: session.ended_at,
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": session.external_session_id,
                "source_format": context.dialect.source_format,
                "source_trust": "provider_native",
                "imported_at": context.adapter.imported_at,
                "source_identity": context.canonical_source_identity,
                "source_root": context.source_root,
                "source_revision": context.source_revision,
            }),
        ),
    })
}

pub(super) fn event_type(kind: OpenCodeNativeEventKind) -> EventType {
    match kind {
        OpenCodeNativeEventKind::Message => EventType::Message,
        OpenCodeNativeEventKind::Summary => EventType::Summary,
        OpenCodeNativeEventKind::Notice => EventType::Notice,
        OpenCodeNativeEventKind::ToolCall => EventType::ToolCall,
        OpenCodeNativeEventKind::ToolOutput => EventType::ToolOutput,
        OpenCodeNativeEventKind::CommandOutput => EventType::CommandOutput,
    }
}

pub(super) fn event_kind_label(kind: OpenCodeNativeEventKind) -> &'static str {
    match kind {
        OpenCodeNativeEventKind::Message => "message",
        OpenCodeNativeEventKind::Summary => "summary",
        OpenCodeNativeEventKind::Notice => "notice",
        OpenCodeNativeEventKind::ToolCall => "tool_call",
        OpenCodeNativeEventKind::ToolOutput => "tool_output",
        OpenCodeNativeEventKind::CommandOutput => "command_output",
    }
}

pub(super) fn event_role(role: &str) -> EventRole {
    provider_role(Some(role))
}

pub(super) fn legacy_provider_event_hash(
    context: &OpenCodePublicationContext<'_>,
    event: &OpenCodeNativeEvent,
) -> String {
    if context.current_state.schema_family == super::super::OpenCodeNativeSchemaFamily::MessagePart
    {
        format!("{}:{}", event.message_identity, event.native_identity)
    } else {
        event.native_identity.clone()
    }
}

pub(super) fn event_cursor(event: &OpenCodeNativeEvent) -> String {
    match &event.native_order {
        super::super::OpenCodeNativeOrder::ExplicitSequence { sequence, .. } => {
            format!("session_message:{}:seq:{sequence}", event.session_identity)
        }
        super::super::OpenCodeNativeOrder::SynthesizedSequence { .. } => {
            format!(
                "session_message:{}:{}",
                event.session_identity, event.native_identity
            )
        }
        super::super::OpenCodeNativeOrder::MessagePart { part_id, .. } => {
            format!("message:{}:part:{part_id}", event.message_identity)
        }
    }
}

pub(super) fn timestamp(value: i64, field: &str) -> Result<DateTime<Utc>> {
    DateTime::<Utc>::from_timestamp_millis(value).ok_or_else(|| {
        CaptureError::InvalidPayload(format!("{field} is outside the supported timestamp range"))
    })
}
