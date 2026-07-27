use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_core_page(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    snapshot: &crate::provider::sqlite::ProviderSqliteSourceSnapshot,
    canonical_path: &Path,
    raw_source_path: &str,
    source_root: &str,
    context: &ProviderAdapterContext,
    import_options: &ProviderImportOptions,
    stream: &str,
    expected_cursor: Option<String>,
    retirement: Option<ShelleyRouteAuthority>,
    page: ShelleyCorePage,
) -> Result<ProviderImportSummary> {
    let provider_cursor = serde_json::to_string(&page.next_cursor)?;
    let next = provider_sync_cursor(
        &context.machine_id,
        stream.to_owned(),
        provider_cursor,
        context.imported_at,
    );
    let transition = NativePathCursorTransition::new(expected_cursor, next);
    let publication_id = page_publication_id(&page, &transition);
    let accounting = NativePathGroupAccounting::new(1, 1, page.retained_bytes)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }

    if let Some(retirement) = retirement {
        group.retire_provider_source_route(&ProviderSourceRouteRetirement {
            provider: CaptureProvider::Shelley,
            source_format: SHELLEY_SQLITE_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity: retirement.locator_identity,
            cursor_stream: stream.to_owned(),
            expected_canonical_source_identity: retirement.canonical_source_identity,
            expected_source_revision: retirement.source_revision,
            retired_at_ms: context.imported_at.timestamp_millis(),
            reason: ProviderSourceRouteRetirementReason::Replaced,
        })?;
    }
    let proposed_source_identity = page.next_cursor.canonical_source_identity.clone();
    let resolution =
        group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::Shelley,
            source_format: SHELLEY_SQLITE_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity: page.next_cursor.locator_identity.clone(),
            cursor_stream: stream.to_owned(),
            proposed_source_identity,
            raw_source_path: Some(canonical_path.display().to_string()),
            source_revision: page.next_cursor.source_revision.clone(),
            observed_at_ms: context.imported_at.timestamp_millis(),
        })?;

    let mut summary = ProviderImportSummary::default();
    match &page.rows {
        ShelleyCorePageRows::Conversations(rows) => {
            publish_conversations(
                committed_store,
                &mut group,
                rows,
                &resolution.canonical_source_identity,
                page.released_source_identity.as_deref(),
                &resolution.route_binding(),
                canonical_path,
                raw_source_path,
                source_root,
                context,
                import_options,
                &page.next_cursor,
                &mut summary,
            )?;
        }
        ShelleyCorePageRows::Messages(rows) => {
            publish_messages(
                committed_store,
                &mut group,
                rows,
                &resolution.canonical_source_identity,
                page.released_source_identity.as_deref(),
                canonical_path,
                raw_source_path,
                context,
                import_options,
                &mut summary,
            )?;
        }
        ShelleyCorePageRows::Observation => {}
    }
    record_page_failures(&page.rows, &mut summary);
    if !snapshot.revalidate(canonical_path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

#[allow(clippy::too_many_arguments)]
fn publish_conversations(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    rows: &[ShelleyUnit<ShelleyConversationRow>],
    canonical_source_identity: &str,
    released_source_identity: Option<&str>,
    route_binding: &ctx_history_store::ProviderSourceRouteBinding,
    canonical_path: &Path,
    raw_source_path: &str,
    source_root: &str,
    context: &ProviderAdapterContext,
    import_options: &ProviderImportOptions,
    cursor: &ShelleyNativeCursor,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let mut page_sessions = std::collections::BTreeMap::<Uuid, Session>::new();
    for row in rows {
        let ShelleyUnit::Accepted { value, .. } = row else {
            continue;
        };
        let capture_source_id = source_id(
            committed_store,
            &context.machine_id,
            value.conversation_id.as_str(),
            canonical_source_identity,
            released_source_identity,
            canonical_path,
            raw_source_path,
        )?;
        let stable_session_identity = canonical_source_identity;
        let session_id = provider_import_session_uuid(
            committed_store,
            CaptureProvider::Shelley,
            &value.conversation_id,
            capture_source_id,
            Some(stable_session_identity),
        )?;
        let parent_id = value
            .parent_conversation_id
            .as_deref()
            .map(|parent| {
                provider_import_session_uuid(
                    committed_store,
                    CaptureProvider::Shelley,
                    parent,
                    source_id(
                        committed_store,
                        &context.machine_id,
                        parent,
                        canonical_source_identity,
                        released_source_identity,
                        canonical_path,
                        raw_source_path,
                    )?,
                    Some(stable_session_identity),
                )
            })
            .transpose()?;
        let source = capture_source(
            value,
            capture_source_id,
            canonical_source_identity,
            canonical_path,
            raw_source_path,
            source_root,
            context,
            cursor,
        );
        group.upsert_capture_source(&source)?;
        group.bind_capture_source_provider_route(capture_source_id, route_binding)?;
        let session = session(
            value,
            session_id,
            parent_id,
            capture_source_id,
            context,
            import_options,
        );
        let existed = committed_store.get_session(session.id).is_ok()
            || page_sessions.contains_key(&session.id);
        group.upsert_session(&session)?;
        page_sessions.insert(session.id, session.clone());
        if existed {
            summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
        } else {
            summary.imported_sessions = summary.imported_sessions.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        }
        if let (Some(parent_id), Some(parent_external)) =
            (parent_id, value.parent_conversation_id.as_deref())
        {
            let parent = if let Some(parent) = page_sessions.get(&parent_id) {
                parent.clone()
            } else {
                match committed_store.get_session(parent_id) {
                    Ok(parent) => parent,
                    Err(ctx_history_store::StoreError::NotFound(_)) => {
                        let placeholder = relationship_placeholder(
                            parent_id,
                            parent_external,
                            context,
                            import_options,
                        );
                        group.upsert_session(&placeholder)?;
                        page_sessions.insert(parent_id, placeholder.clone());
                        placeholder
                    }
                    Err(error) => return Err(error.into()),
                }
            };
            let edge = relationship_edge(
                value,
                &session,
                parent_id,
                capture_source_id,
                stable_session_identity,
                context,
            );
            let existed = committed_store.session_edge_exists(edge.id)?;
            group.upsert_projection_neutral_session_edge(&actor(&parent), &edge)?;
            if existed {
                summary.skipped_edges = summary.skipped_edges.saturating_add(1);
            } else {
                summary.imported_edges = summary.imported_edges.saturating_add(1);
                summary.imported = summary.imported.saturating_add(1);
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn publish_messages(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    rows: &[ShelleyUnit<ShelleyMessage>],
    canonical_source_identity: &str,
    released_source_identity: Option<&str>,
    canonical_path: &Path,
    raw_source_path: &str,
    context: &ProviderAdapterContext,
    import_options: &ProviderImportOptions,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let stable_session_identity = canonical_source_identity;
    for row in rows {
        let ShelleyUnit::Accepted { rowid, value, .. } = row else {
            continue;
        };
        let Some(mut provider_event) = shelley_core_event(
            &value.message,
            &value.conversation,
            context,
            value.parent_bearing,
        )?
        else {
            continue;
        };
        let source_id = source_id(
            committed_store,
            &context.machine_id,
            &value.conversation.conversation_id,
            canonical_source_identity,
            released_source_identity,
            canonical_path,
            raw_source_path,
        )?;
        let session_id = provider_import_session_uuid(
            committed_store,
            CaptureProvider::Shelley,
            &value.conversation.conversation_id,
            source_id,
            Some(stable_session_identity),
        )?;
        provider_event.provider_event_index =
            retained_or_planned_event_index(committed_store, source_id, value)?;
        let event_hash = compute_payload_hash(&provider_event.payload)?;
        let identity = provider_event_import_identity_with_exact_legacy_source(
            committed_store,
            CaptureProvider::Shelley,
            &value.conversation.conversation_id,
            source_id,
            provider_event.provider_event_index,
            provider_event.provider_event_index,
            &event_hash,
            None,
            None,
            session_id
                == provider_session_uuid(
                    CaptureProvider::Shelley,
                    &value.conversation.conversation_id,
                ),
        )?;
        let line_number = usize::try_from(*rowid).unwrap_or(usize::MAX);
        let (event, run) = shelley_canonical_event(
            &value.conversation.conversation_id,
            source_id,
            session_id,
            line_number,
            &provider_event,
            &event_hash,
            &identity,
            context,
            import_options,
        )?;
        if let Some(run) = run.as_ref() {
            group.upsert_run(run)?;
        }
        if group.reconcile_provider_event_migrating_exact_legacy_provider_hash(
            &event,
            &provider_event.legacy_provider_event_hash,
        )? {
            summary.imported_events = summary.imported_events.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        } else {
            summary.skipped_events = summary.skipped_events.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
        }
        summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn shelley_canonical_event(
    provider_session_id: &str,
    source_id: Uuid,
    session_id: Uuid,
    line_number: usize,
    event: &ShelleyCoreEvent,
    event_hash: &str,
    identity: &crate::provider::importer::ProviderEventImportIdentity,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
) -> Result<(Event, Option<Run>)> {
    let mut provider_metadata = event.metadata.clone();
    let verified_content_locators = provider_metadata
        .as_object_mut()
        .and_then(|metadata| metadata.remove(VERIFIED_CONTENT_LOCATORS_METADATA_KEY))
        .map(|value| {
            VerifiedContentLocatorsV1::from_metadata_value(&value)
                .map(|locators| locators.to_metadata_value())
                .ok_or_else(|| {
                    CaptureError::InvalidPayload(
                        "Shelley verified content locator annotation is malformed".to_owned(),
                    )
                })
        })
        .transpose()?;
    let run = provider_command_run(
        CaptureProvider::Shelley,
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
        "provider_event_hash_authority": ProviderEventHashAuthority::NormalizedPayloadFallback.as_str(),
        "cursor": event.cursor,
        "source_format": SHELLEY_SQLITE_SOURCE_FORMAT,
        "source_trust": "provider_native",
        "fixture_line": line_number,
        "imported_at": context.imported_at,
        "event_idempotency_key": format!(
            "provider-event:{}:{}:{}",
            CaptureProvider::Shelley.as_str(),
            provider_session_id,
            event.provider_event_index,
        ),
        "source_record_ordinal": Value::Null,
        "source_record_subrecord_index": Value::Null,
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
                "provider": CaptureProvider::Shelley.as_str(),
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

fn record_page_failures(rows: &ShelleyCorePageRows, summary: &mut ProviderImportSummary) {
    match rows {
        ShelleyCorePageRows::Conversations(rows) => {
            record_failures_for_units(rows, summary);
        }
        ShelleyCorePageRows::Messages(rows) => {
            record_failures_for_units(rows, summary);
        }
        ShelleyCorePageRows::Observation => {}
    }
}

fn record_failures_for_units<T>(rows: &[ShelleyUnit<T>], summary: &mut ProviderImportSummary) {
    for row in rows {
        if let ShelleyUnit::Rejected { rowid, reason, .. } = row {
            summary.record_failure(ProviderImportFailure {
                line: usize::try_from(*rowid).unwrap_or(usize::MAX),
                error: reason.clone(),
            });
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn capture_source(
    conversation: &ShelleyConversationRow,
    source_id: Uuid,
    canonical_source_identity: &str,
    canonical_path: &Path,
    raw_source_path: &str,
    source_root: &str,
    context: &ProviderAdapterContext,
    cursor: &ShelleyNativeCursor,
) -> CaptureSource {
    let started_at = shelley_timestamp(conversation.created_at.as_deref(), context.imported_at);
    let ended_at = conversation
        .updated_at
        .as_deref()
        .map(|value| shelley_timestamp(Some(value), context.imported_at));
    CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Shelley,
            machine_id: context.machine_id.clone(),
            process_id: None,
            cwd: conversation.cwd.clone(),
            raw_source_path: Some(raw_source_path.to_owned()),
            source_format: Some(SHELLEY_SQLITE_SOURCE_FORMAT.to_owned()),
            source_root: Some(source_root.to_owned()),
            source_identity: Some(canonical_source_identity.to_owned()),
            external_session_id: Some(conversation.conversation_id.clone()),
        },
        started_at,
        ended_at,
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": conversation.conversation_id,
                "source_format": SHELLEY_SQLITE_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "source_identity": canonical_source_identity,
                "source_root": source_root,
                "source_revision": cursor.source_revision,
                "source_identity_key": canonical_source_key(
                    canonical_source_identity,
                    &conversation.conversation_id,
                ),
                "nativepath": {
                    "database_path": canonical_path,
                    "locator_identity": cursor.locator_identity,
                    "route_epoch": cursor.route_epoch,
                    "schema_fingerprint": cursor.schema_fingerprint,
                    "sqlite_user_version": cursor.sqlite_user_version,
                },
            }),
        ),
    }
}

fn session(
    conversation: &ShelleyConversationRow,
    id: Uuid,
    parent_id: Option<Uuid>,
    source_id: Uuid,
    context: &ProviderAdapterContext,
    import_options: &ProviderImportOptions,
) -> Session {
    let started_at = shelley_timestamp(conversation.created_at.as_deref(), context.imported_at);
    let ended_at = conversation
        .updated_at
        .as_deref()
        .map(|value| shelley_timestamp(Some(value), context.imported_at));
    let is_subagent = conversation.parent_conversation_id.is_some() || !conversation.user_initiated;
    Session {
        id,
        history_record_id: import_options.history_record_id,
        parent_session_id: parent_id,
        root_session_id: parent_id,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Shelley,
        external_session_id: Some(conversation.conversation_id.clone()),
        external_agent_id: None,
        agent_type: if is_subagent {
            AgentType::Subagent
        } else {
            AgentType::Primary
        },
        role_hint: Some(if is_subagent { "subagent" } else { "primary" }.to_owned()),
        is_primary: !is_subagent,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at,
        ended_at,
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": conversation.conversation_id,
                "parent_provider_session_id": conversation.parent_conversation_id,
                "root_provider_session_id": conversation.parent_conversation_id,
                "source_format": SHELLEY_SQLITE_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "metadata": {
                    "source_format": SHELLEY_SQLITE_SOURCE_FORMAT,
                    "conversation_id": conversation.conversation_id,
                    "slug": conversation.slug,
                    "title": conversation.slug,
                    "user_initiated": conversation.user_initiated,
                    "archived": conversation.archived,
                    "parent_conversation_id": conversation.parent_conversation_id,
                    "model": conversation.model,
                    "conversation_options": conversation
                        .conversation_options
                        .as_deref()
                        .map(crate::provider::normalization::provider_json_text),
                    "current_generation": conversation.current_generation,
                    "agent_working": conversation.agent_working,
                    "tags": conversation
                        .tags
                        .as_deref()
                        .map(crate::provider::normalization::provider_json_text),
                    "is_draft": conversation.is_draft,
                    "draft": conversation.draft,
                    "queued_messages": conversation
                        .queued_messages
                        .as_deref()
                        .map(crate::provider::normalization::provider_json_text),
                },
            }),
        ),
    }
}

fn relationship_placeholder(
    id: Uuid,
    external_session_id: &str,
    context: &ProviderAdapterContext,
    import_options: &ProviderImportOptions,
) -> Session {
    Session {
        id,
        history_record_id: import_options.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: None,
        provider: CaptureProvider::Shelley,
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
                "relationship_placeholder": true,
                "provider_session_id": external_session_id,
                "source_format": SHELLEY_SQLITE_SOURCE_FORMAT,
                "imported_at": context.imported_at,
            }),
        ),
    }
}

fn relationship_edge(
    conversation: &ShelleyConversationRow,
    session: &Session,
    parent_id: Uuid,
    source_id: Uuid,
    stable_session_identity: &str,
    context: &ProviderAdapterContext,
) -> SessionEdge {
    let id = if session.id
        == provider_session_uuid(CaptureProvider::Shelley, &conversation.conversation_id)
    {
        provider_edge_uuid(
            CaptureProvider::Shelley,
            &conversation.conversation_id,
            "parent_child",
        )
    } else {
        provider_source_edge_uuid(
            stable_session_identity,
            &conversation.conversation_id,
            "parent_child",
        )
    };
    SessionEdge {
        id,
        from_session_id: parent_id,
        to_session_id: session.id,
        edge_type: SessionEdgeType::ParentChild,
        confidence: ctx_history_core::Confidence::Explicit,
        source_id: Some(source_id),
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": conversation.conversation_id,
                "parent_provider_session_id": conversation.parent_conversation_id,
                "source_format": SHELLEY_SQLITE_SOURCE_FORMAT,
                "imported_at": context.imported_at,
            }),
        ),
    }
}

fn actor(session: &Session) -> CanonicalActor {
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

fn source_id(
    committed_store: &Store,
    machine_id: &str,
    provider_session_id: &str,
    canonical_source_identity: &str,
    released_source_identity: Option<&str>,
    canonical_path: &Path,
    raw_source_path: &str,
) -> Result<Uuid> {
    if let Some(existing) = committed_store.capture_source_by_canonical_identity_session(
        CaptureProvider::Shelley,
        SHELLEY_SQLITE_SOURCE_FORMAT,
        machine_id,
        canonical_source_identity,
        provider_session_id,
    )? {
        return Ok(existing.id);
    }

    if let Some(released_source_identity) = released_source_identity {
        if let Some(existing) = committed_store.capture_source_by_canonical_identity_session(
            CaptureProvider::Shelley,
            SHELLEY_SQLITE_SOURCE_FORMAT,
            machine_id,
            released_source_identity,
            provider_session_id,
        )? {
            return Ok(existing.id);
        }
    }

    let canonical_display = canonical_path.display().to_string();
    for legacy_path in [raw_source_path, canonical_display.as_str()] {
        let candidate = provider_scoped_source_uuid(
            CaptureProvider::Shelley,
            provider_session_id,
            SHELLEY_SQLITE_SOURCE_FORMAT,
            Some(legacy_path),
        );
        let existing = match committed_store.get_capture_source(candidate) {
            Ok(existing) => existing,
            Err(ctx_history_store::StoreError::NotFound(_)) => continue,
            Err(error) => return Err(error.into()),
        };
        if existing.descriptor.provider == CaptureProvider::Shelley
            && existing.descriptor.machine_id == machine_id
            && existing.descriptor.source_format.as_deref() == Some(SHELLEY_SQLITE_SOURCE_FORMAT)
            && existing.descriptor.external_session_id.as_deref() == Some(provider_session_id)
            && existing.descriptor.raw_source_path.as_deref() == Some(legacy_path)
        {
            return Ok(existing.id);
        }
    }

    Ok(stable_capture_uuid(
        &canonical_source_key(canonical_source_identity, provider_session_id),
        "source",
    ))
}
