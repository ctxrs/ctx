use super::lifecycle::{classify_cursor, stop_after_changed_group, unique_session_id};
use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn import_auggie_source(
    store: &Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    configured_source_root: &Path,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    parsed: &ParsedAuggieSource,
    known_route: Option<&KnownAuggieRoute>,
    session_index: &BTreeMap<String, Option<Uuid>>,
    summary: &mut ProviderImportSummary,
) -> Result<SourceCompletion> {
    let path = &parsed.stamp.canonical_path;
    let locator_identity = provider_path_identity(path)?;
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Auggie,
        AUGGIE_SESSION_JSON_SOURCE_FORMAT,
        &locator_identity,
    );
    let stored = store.get_sync_cursor(None, &context.machine_id, &stream)?;
    let plan = classify_cursor(stored.as_ref(), parsed)?;
    if let CursorPlan::AlreadyCommitted(cursor) = plan {
        let route = known_route.ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Auggie NativePath cursor has no matching canonical source route".to_owned(),
            )
        })?;
        summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
        summary.skipped_events = summary.skipped_events.saturating_add(parsed.events.len());
        summary.skipped = summary
            .skipped
            .saturating_add(parsed.events.len().saturating_add(1));
        summary.accepted_content_records = summary
            .accepted_content_records
            .saturating_add(parsed.events.len());
        summary.failed = summary
            .failed
            .saturating_add(usize::try_from(cursor.rejected_records).unwrap_or(usize::MAX));
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(SourceCompletion {
            changed_groups: 0,
            terminal: true,
            session_id: route.session_id,
        });
    }
    let CursorPlan::Publish {
        mut expected_cursor,
        generation,
        mut next_event,
        rejected_records,
    } = plan
    else {
        unreachable!("already-committed cursor returned above");
    };
    let mut changed_groups = 0_usize;
    let mut session_id = known_route.map(|route| route.session_id);
    loop {
        let page_end = next_event
            .saturating_add(AUGGIE_CORE_EVENTS_PER_PAGE)
            .min(parsed.events.len());
        let terminal = page_end == parsed.events.len();
        let prefix_sha256 = event_prefix_digest(&parsed.events[..page_end])?;
        let provider_cursor = AuggieNativeCursor {
            version: AUGGIE_NATIVE_CURSOR_VERSION,
            parser_revision: AUGGIE_PARSER_REVISION.to_owned(),
            policy_revision: AUGGIE_POLICY_REVISION.to_owned(),
            source_path: path.clone(),
            source_revision: parsed.source_revision.clone(),
            generation,
            next_event: u64::try_from(page_end).map_err(|_| {
                CaptureError::InvalidPayload("Auggie event frontier exceeds u64".to_owned())
            })?,
            prefix_sha256,
            terminal,
            event_count: u64::try_from(parsed.events.len()).map_err(|_| {
                CaptureError::InvalidPayload("Auggie event count exceeds u64".to_owned())
            })?,
            provider_session_id: parsed.session.provider_session_id.clone(),
            rejected_records,
        };
        let next_cursor = provider_sync_cursor(
            &context.machine_id,
            stream.clone(),
            encode_cursor(&provider_cursor)?,
            context.imported_at,
        );
        let transition =
            NativePathCursorTransition::new(expected_cursor.clone(), next_cursor.clone());
        let page = &parsed.events[next_event..page_end];
        let publication_id =
            source_publication_id(parsed, page, generation, next_event, &transition);
        let retained_bytes = retained_core_page_bytes(parsed, page)?;
        let accounting = NativePathGroupAccounting::new(1, 1, retained_bytes)?;
        if !parsed.stamp.revalidate()? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let admission = store.admit_event_search_bulk_group(bulk_guard)?;
        let mut group = store.begin_native_path_publication_group(admission, accounting)?;
        match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
            NativePathCursorSetClassification::AllNextSameGroup { .. } => {
                group.commit()?;
                session_id = session_id.or_else(|| known_route.map(|route| route.session_id));
            }
            NativePathCursorSetClassification::AllExpected => {
                let resolved = publish_source_and_session(
                    committed_store,
                    &mut group,
                    configured_source_root,
                    context,
                    options,
                    parsed,
                    &locator_identity,
                    &stream,
                    session_index,
                    summary,
                    next_event == 0,
                )?;
                session_id = Some(resolved.1);
                publish_events(
                    committed_store,
                    &mut group,
                    context,
                    options,
                    parsed,
                    generation,
                    resolved.0,
                    resolved.1,
                    page,
                    summary,
                )?;
                if !parsed.stamp.revalidate()? {
                    return Err(CaptureError::SourceChangedDuringCapture);
                }
                group.prepare_journal_checkpoint()?;
                group.publish_cursor_set()?;
                group.commit()?;
                changed_groups = changed_groups.saturating_add(1);
                summary.set_work_result(ProviderImportWorkResult::Changed);
            }
        }
        expected_cursor = store
            .get_sync_cursor(None, &context.machine_id, &stream)?
            .map(|cursor| cursor.cursor);
        next_event = page_end;
        if terminal || stop_after_changed_group(options, changed_groups) {
            return Ok(SourceCompletion {
                changed_groups,
                terminal,
                session_id: session_id.ok_or(CaptureError::SystemInvariant(
                    "Auggie publication lost its session identity",
                ))?,
            });
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn publish_source_and_session(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    configured_source_root: &Path,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    parsed: &ParsedAuggieSource,
    locator_identity: &str,
    stream: &str,
    session_index: &BTreeMap<String, Option<Uuid>>,
    summary: &mut ProviderImportSummary,
    count_session: bool,
) -> Result<(Uuid, Uuid)> {
    let source_root = configured_source_root.display().to_string();
    let raw_source_path = parsed.session.raw_source_path.clone();
    let proposed_source_identity = provider_source_identity(
        CaptureProvider::Auggie,
        AUGGIE_SESSION_JSON_SOURCE_FORMAT,
        Some(&source_root),
        Some(&raw_source_path),
        None,
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Auggie NativePath source has no canonical identity",
    ))?;
    let resolution =
        group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::Auggie,
            source_format: AUGGIE_SESSION_JSON_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity: locator_identity.to_owned(),
            cursor_stream: stream.to_owned(),
            proposed_source_identity,
            raw_source_path: Some(raw_source_path.clone()),
            source_revision: parsed.source_revision.clone(),
            observed_at_ms: context.imported_at.timestamp_millis(),
        })?;
    let source_id = committed_store
        .capture_source_by_canonical_identity_session(
            CaptureProvider::Auggie,
            AUGGIE_SESSION_JSON_SOURCE_FORMAT,
            &context.machine_id,
            &resolution.canonical_source_identity,
            &parsed.session.provider_session_id,
        )?
        .map(|source| source.id)
        .unwrap_or_else(|| {
            provider_scoped_source_uuid(
                CaptureProvider::Auggie,
                &parsed.session.provider_session_id,
                AUGGIE_SESSION_JSON_SOURCE_FORMAT,
                Some(&raw_source_path),
            )
        });
    let source = capture_source(
        configured_source_root,
        context,
        parsed,
        source_id,
        &resolution.canonical_source_identity,
    );
    group.upsert_capture_source(&source)?;
    group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;

    let session_id = provider_import_session_uuid(
        committed_store,
        CaptureProvider::Auggie,
        &parsed.session.provider_session_id,
        source_id,
        Some(&resolution.canonical_source_identity),
    )?;
    let parent_session_id = parsed
        .session
        .parent_provider_session_id
        .as_ref()
        .and_then(|provider_id| unique_session_id(session_index, provider_id));
    let root_session_id = parsed
        .session
        .root_provider_session_id
        .as_ref()
        .and_then(|provider_id| unique_session_id(session_index, provider_id))
        .or(parent_session_id);
    let session = canonical_session(
        context,
        options,
        parsed,
        source_id,
        session_id,
        parent_session_id,
        root_session_id,
    );
    let existed = committed_store.get_session(session_id).is_ok();
    group.upsert_session(&session)?;
    if count_session {
        if existed {
            summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
        } else {
            summary.imported_sessions = summary.imported_sessions.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        }
    }
    Ok((source_id, session_id))
}

fn capture_source(
    configured_source_root: &Path,
    context: &ProviderAdapterContext,
    parsed: &ParsedAuggieSource,
    source_id: Uuid,
    canonical_source_identity: &str,
) -> CaptureSource {
    let source_root = configured_source_root.display().to_string();
    CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Auggie,
            machine_id: context.machine_id.clone(),
            process_id: None,
            cwd: parsed.session.cwd.clone(),
            raw_source_path: Some(parsed.session.raw_source_path.clone()),
            source_format: Some(AUGGIE_SESSION_JSON_SOURCE_FORMAT.to_owned()),
            source_root: Some(source_root.clone()),
            source_identity: Some(canonical_source_identity.to_owned()),
            external_session_id: Some(parsed.session.provider_session_id.clone()),
        },
        started_at: parsed.session.started_at,
        ended_at: parsed.session.ended_at,
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": parsed.session.provider_session_id,
                "source_format": AUGGIE_SESSION_JSON_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "source_identity": canonical_source_identity,
                "source_root": source_root,
                "source_revision": parsed.source_revision,
                "source_identity_key": provider_scoped_source_identity_key(
                    CaptureProvider::Auggie,
                    &parsed.session.provider_session_id,
                    AUGGIE_SESSION_JSON_SOURCE_FORMAT,
                    Some(&parsed.session.raw_source_path),
                ),
                "source_metadata": parsed.session.source_metadata,
                "nativepath_publication": AUGGIE_PARSER_REVISION,
            }),
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn canonical_session(
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    parsed: &ParsedAuggieSource,
    source_id: Uuid,
    session_id: Uuid,
    parent_session_id: Option<Uuid>,
    root_session_id: Option<Uuid>,
) -> Session {
    Session {
        id: session_id,
        history_record_id: options.history_record_id,
        parent_session_id,
        root_session_id,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Auggie,
        external_session_id: Some(parsed.session.provider_session_id.clone()),
        external_agent_id: parsed.session.external_agent_id.clone(),
        agent_type: AgentType::Primary,
        role_hint: Some("primary".to_owned()),
        is_primary: true,
        status: if parsed.session.ended_at.is_some() {
            SessionStatus::Completed
        } else {
            SessionStatus::Imported
        },
        transcript_blob_id: None,
        started_at: parsed.session.started_at,
        ended_at: parsed.session.ended_at,
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": parsed.session.provider_session_id,
                "parent_provider_session_id": parsed.session.parent_provider_session_id,
                "root_provider_session_id": parsed.session.root_provider_session_id,
                "source_format": AUGGIE_SESSION_JSON_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "metadata": parsed.session.session_metadata,
                "nativepath_publication": AUGGIE_PARSER_REVISION,
            }),
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn publish_events(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    parsed: &ParsedAuggieSource,
    generation: u64,
    source_id: Uuid,
    session_id: Uuid,
    events: &[ParsedAuggieEvent],
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    for event in events {
        let retained = &event.event;
        let provider_event_index = generation
            .checked_mul(AUGGIE_GENERATION_EVENT_STRIDE)
            .and_then(|base| base.checked_add(retained.provider_event_index))
            .ok_or_else(|| {
                CaptureError::InvalidPayload(
                    "Auggie generation-scoped event index exceeds u64".to_owned(),
                )
            })?;
        let event_hash = retained.provider_event_hash.as_str();
        let identity = provider_event_import_identity_with_exact_legacy_source(
            committed_store,
            CaptureProvider::Auggie,
            &parsed.session.provider_session_id,
            source_id,
            provider_event_index,
            provider_event_index,
            event_hash,
            None,
            Some(u64::try_from(event.chat_index).unwrap_or(u64::MAX)),
            session_id
                == crate::provider::importer::provider_session_uuid(
                    CaptureProvider::Auggie,
                    &parsed.session.provider_session_id,
                ),
        )?;
        let dedupe_key =
            Store::provider_event_dedupe_key_with_payload_hash(&identity.dedupe_key, event_hash)
                .unwrap_or(identity.dedupe_key);
        let mut provider_metadata = retained.metadata.clone();
        let verified_locators = provider_metadata
            .as_object_mut()
            .and_then(|metadata| metadata.remove(VERIFIED_CONTENT_LOCATORS_METADATA_KEY));
        let mut sync_metadata = json!({
            "provider_session_id": parsed.session.provider_session_id,
            "provider_event_index": provider_event_index,
            "native_provider_event_index": retained.provider_event_index,
            "source_generation": generation,
            "provider_event_hash": event_hash,
            "provider_event_hash_authority": "provider_supplied",
            "cursor": retained.cursor,
            "source_format": AUGGIE_SESSION_JSON_SOURCE_FORMAT,
            "source_trust": "provider_native",
            "imported_at": context.imported_at,
            "source_record_ordinal": event.chat_index,
            "source_record_subrecord_index": event.sub_index,
            "metadata": provider_metadata,
        });
        if let (Some(metadata), Some(locators)) = (sync_metadata.as_object_mut(), verified_locators)
        {
            metadata.insert(VERIFIED_CONTENT_LOCATORS_METADATA_KEY.to_owned(), locators);
        }
        let normalized = Event {
            id: identity.id,
            seq: identity.seq,
            history_record_id: options.history_record_id,
            session_id: Some(session_id),
            run_id: None,
            event_type: retained.event_type,
            role: Some(retained.role),
            occurred_at: retained.occurred_at,
            capture_source_id: Some(source_id),
            payload: json!({
                "provider": CaptureProvider::Auggie.as_str(),
                "provider_session_id": parsed.session.provider_session_id,
                "provider_event_index": provider_event_index,
                "native_provider_event_index": retained.provider_event_index,
                "source_generation": generation,
                "provider_event_hash": event_hash,
                "cursor": retained.cursor,
                "artifacts": [],
                "body": crate::provider::importer::compact_provider_result_payload(
                    retained.event_type,
                    &retained.payload,
                ),
            }),
            payload_blob_id: None,
            dedupe_key: Some(dedupe_key),
            sync: provider_sync_metadata(Fidelity::Imported, sync_metadata),
        };
        if group
            .reconcile_provider_event(&normalized, ProviderEventHashAuthority::ProviderSupplied)?
        {
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
                CaptureProvider::Auggie.as_str(),
                machine_id,
                stream
            ),
            "provider-sync-cursor",
        ),
        team_id: None,
        device_id: machine_id.to_owned(),
        stream,
        cursor,
        last_synced_at: Some(observed_at),
        timestamps: timestamps(observed_at),
    }
}

fn source_publication_id(
    parsed: &ParsedAuggieSource,
    events: &[ParsedAuggieEvent],
    generation: u64,
    start: usize,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-auggie-nativepath-publication-v1\0");
    digest.update(parsed.stamp.canonical_path.as_os_str().as_encoded_bytes());
    digest.update(parsed.source_revision.as_bytes());
    digest.update(generation.to_be_bytes());
    digest.update((start as u64).to_be_bytes());
    for event in events {
        digest.update(event.event.provider_event_hash.as_bytes());
    }
    digest.update(transition.next().stream.as_bytes());
    digest.update(transition.next().cursor.as_bytes());
    format!("auggie-nativepath-v1:{:x}", digest.finalize())
}

pub(super) fn retirement_publication_id(retirement: &ProviderSourceRouteRetirement) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-auggie-nativepath-route-retirement-v1\0");
    digest.update(retirement.provider.as_str().as_bytes());
    digest.update(retirement.source_format.as_bytes());
    digest.update(retirement.machine_id.as_bytes());
    digest.update(retirement.locator_identity.as_bytes());
    digest.update(retirement.cursor_stream.as_bytes());
    digest.update(retirement.expected_canonical_source_identity.as_bytes());
    digest.update(retirement.expected_source_revision.as_bytes());
    digest.update(format!("{:?}", retirement.reason).as_bytes());
    format!("auggie-nativepath-retirement-v1:{:x}", digest.finalize())
}

pub(super) fn relationship_publication_id(
    relationship: &RelationshipFact,
    parent_session_id: Option<Uuid>,
    root_session_id: Option<Uuid>,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-auggie-nativepath-relationship-v1\0");
    digest.update(relationship.path.as_os_str().as_encoded_bytes());
    digest.update(relationship.provider_session_id.as_bytes());
    digest.update(
        parent_session_id
            .map(|id| id.to_string())
            .unwrap_or_default(),
    );
    digest.update(root_session_id.map(|id| id.to_string()).unwrap_or_default());
    digest.update(transition.next().cursor.as_bytes());
    format!("auggie-nativepath-relationship-v1:{:x}", digest.finalize())
}

fn retained_core_page_bytes(
    parsed: &ParsedAuggieSource,
    events: &[ParsedAuggieEvent],
) -> Result<usize> {
    let mut retained = serde_json::to_vec(&parsed.session.session_metadata)?
        .len()
        .saturating_add(serde_json::to_vec(&parsed.session.source_metadata)?.len())
        .saturating_add(PAGE_ACCOUNTING_OVERHEAD_BYTES);
    for event in events {
        retained = retained.saturating_add(released_auggie_event_encoding(&event.event)?.len());
    }
    if retained > ctx_history_store::NATIVE_PATH_MAX_RETAINED_PAGE_BYTES {
        return Err(CaptureError::InvalidPayload(
            "Auggie Core page exceeds the NativePath retained-byte bound".to_owned(),
        ));
    }
    Ok(retained)
}
