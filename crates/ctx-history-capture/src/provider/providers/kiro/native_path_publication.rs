use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_core_page(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    source: &KiroSource,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    page: KiroCorePage,
) -> Result<ProviderImportSummary> {
    source.revalidate()?;
    let stored = store.get_sync_cursor(None, &context.machine_id, &source.cursor_stream)?;
    let cursor_plan = cursor_plan(stored.as_ref(), source, &page)?;
    let raw_source_path = source.canonical_path.display().to_string();
    let source_root = source.configured_source_root.display().to_string();
    let proposed_source_identity = provider_source_identity(
        CaptureProvider::KiroCli,
        KIRO_SQLITE_SOURCE_FORMAT,
        Some(&source_root),
        Some(&raw_source_path),
        None,
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Kiro NativePath source has no canonical identity",
    ))?;
    let canonical_source_identity = anticipated_canonical_source_identity(
        committed_store,
        source,
        context,
        &raw_source_path,
        &proposed_source_identity,
    )?;
    let existing_capture_source_ids =
        kiro_capture_source_ids(committed_store, context, &canonical_source_identity)?;
    let generation_scope_present = !existing_capture_source_ids.is_empty() || page.fact.is_some();
    let retirement = (page.terminal && generation_scope_present).then_some(KiroRetirementRequest {
        after: None,
        committed: false,
    });
    let next_cursor = KiroStoreCursor {
        version: KIRO_NATIVE_CURSOR_VERSION,
        provider: CaptureProvider::KiroCli.as_str().to_owned(),
        locator_identity: source.locator_identity.clone(),
        canonical_source_identity: canonical_source_identity.clone(),
        source_revision: source.source_revision.clone(),
        frontier: page.next_frontier.clone(),
        retirement,
        terminal: page.terminal && !generation_scope_present,
        generation: cursor_plan.generation,
        rejected_records: cursor_plan
            .rejected_records
            .saturating_add(u64::try_from(page.rejections.len()).unwrap_or(u64::MAX)),
    };
    let transition = NativePathCursorTransition::new(
        stored.as_ref().map(|cursor| cursor.cursor.clone()),
        SyncCursor {
            id: Uuid::new_v4(),
            team_id: None,
            device_id: context.machine_id.clone(),
            stream: source.cursor_stream.clone(),
            cursor: next_cursor.encode()?,
            last_synced_at: Some(context.imported_at),
            timestamps: timestamps(context.imported_at),
        },
    );
    let publication_id = page_publication_id(source, &page, &transition);
    let accounting = NativePathGroupAccounting::new(1, 1, page.retained_bytes)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        let mut summary = ProviderImportSummary::default();
        summary.skipped_events = page.events.len();
        summary.skipped = summary.skipped_events;
        summary.work_remaining = !next_cursor.terminal;
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }

    let resolution =
        group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::KiroCli,
            source_format: KIRO_SQLITE_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity: source.locator_identity.clone(),
            cursor_stream: source.cursor_stream.clone(),
            proposed_source_identity,
            raw_source_path: Some(raw_source_path.clone()),
            source_revision: source.source_revision.clone(),
            observed_at_ms: context.imported_at.timestamp_millis(),
        })?;
    if resolution.canonical_source_identity != canonical_source_identity {
        return Err(CaptureError::SourceChangedDuringCapture);
    }

    let mut summary = ProviderImportSummary::default();
    let mut retained = NativePathRetainedSourceEntities {
        capture_source_ids: existing_capture_source_ids,
        ..NativePathRetainedSourceEntities::default()
    };
    if let Some(fact) = &page.fact {
        let existing_source = committed_store.capture_source_by_canonical_identity_session(
            CaptureProvider::KiroCli,
            KIRO_SQLITE_SOURCE_FORMAT,
            &context.machine_id,
            &resolution.canonical_source_identity,
            &fact.provider_session_id,
        )?;
        let source_id = existing_source
            .as_ref()
            .map(|source| source.id)
            .unwrap_or_else(|| {
                provider_scoped_source_uuid(
                    CaptureProvider::KiroCli,
                    &fact.provider_session_id,
                    KIRO_SQLITE_SOURCE_FORMAT,
                    Some(&raw_source_path),
                )
            });
        let capture_source = kiro_capture_source(
            source,
            context,
            fact,
            source_id,
            &raw_source_path,
            &source_root,
            &resolution.canonical_source_identity,
        );
        group.upsert_capture_source(&capture_source)?;
        group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;
        retained.capture_source_ids.push(source_id);
        let session_id = provider_import_session_uuid(
            committed_store,
            CaptureProvider::KiroCli,
            &fact.provider_session_id,
            source_id,
            Some(&resolution.canonical_source_identity),
        )?;
        let session = kiro_session(context, options, fact, source_id, session_id);
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
        for prepared in &page.events {
            publish_event(
                committed_store,
                &mut group,
                context,
                options,
                source_id,
                session_id,
                fact,
                prepared,
                &mut summary,
                &mut retained,
            )?;
        }
    }
    deduplicate_retained_entities(&mut retained);
    if !retained.capture_source_ids.is_empty() {
        group.stage_source_generation_page(
            &kiro_generation_key(
                source,
                context,
                &canonical_source_identity,
                cursor_plan.generation,
            ),
            &retained,
        )?;
    }
    for rejection in &page.rejections {
        summary.record_failure(ProviderImportFailure {
            line: usize::try_from(rejection.line)
                .unwrap_or(usize::MAX)
                .saturating_add(1),
            error: rejection.reason.clone(),
        });
    }

    source.revalidate()?;
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    summary.work_remaining = !next_cursor.terminal;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

struct KiroCursorPlan {
    generation: u64,
    rejected_records: u64,
}

fn cursor_plan(
    stored: Option<&SyncCursor>,
    source: &KiroSource,
    page: &KiroCorePage,
) -> Result<KiroCursorPlan> {
    let Some(stored) = stored else {
        if page.expected_frontier != KiroFrontier::initial(source.tables) {
            return Err(CaptureError::InvalidPayload(
                "Kiro NativePath fresh cursor does not begin at source start".to_owned(),
            ));
        }
        return Ok(KiroCursorPlan {
            generation: 0,
            rejected_records: 0,
        });
    };
    if let Ok(committed) = decode_native_path_committed_cursor(&stored.cursor) {
        let prior = KiroStoreCursor::decode(committed.provider_cursor())?;
        if prior.locator_identity != source.locator_identity {
            return Err(CaptureError::InvalidPayload(
                "Kiro NativePath cursor route changed unexpectedly".to_owned(),
            ));
        }
        if prior.source_revision == source.source_revision {
            if prior.frontier != page.expected_frontier || prior.terminal {
                return Err(CaptureError::InvalidPayload(
                    "Kiro NativePath cursor/frontier chain is discontinuous".to_owned(),
                ));
            }
            return Ok(KiroCursorPlan {
                generation: prior.generation,
                rejected_records: prior.rejected_records,
            });
        }
        if page.expected_frontier != KiroFrontier::initial(source.tables) {
            return Err(CaptureError::InvalidPayload(
                "Kiro source revision changed after NativePath resume".to_owned(),
            ));
        }
        return Ok(KiroCursorPlan {
            generation: prior
                .generation
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "Kiro NativePath generation overflowed",
                ))?,
            rejected_records: 0,
        });
    }
    decode_released_kiro_cursor(&stored.cursor)?;
    if page.expected_frontier != KiroFrontier::initial(source.tables) {
        return Err(CaptureError::InvalidPayload(
            "Kiro released cursor migration did not begin at source start".to_owned(),
        ));
    }
    Ok(KiroCursorPlan {
        generation: 0,
        rejected_records: 0,
    })
}

fn anticipated_canonical_source_identity(
    store: &Store,
    source: &KiroSource,
    context: &ProviderAdapterContext,
    raw_source_path: &str,
    proposed_source_identity: &str,
) -> Result<String> {
    let candidates = store
        .list_capture_sources()?
        .into_iter()
        .filter(|candidate| {
            candidate.descriptor.provider == CaptureProvider::KiroCli
                && candidate.descriptor.machine_id == context.machine_id
                && candidate.descriptor.source_format.as_deref() == Some(KIRO_SQLITE_SOURCE_FORMAT)
                && candidate.sync.deleted_at.is_none()
        })
        .collect::<Vec<_>>();
    let exact_path = candidates
        .iter()
        .filter(|candidate| {
            candidate.descriptor.raw_source_path.as_deref() == Some(raw_source_path)
        })
        .filter_map(|candidate| candidate.descriptor.source_identity.clone())
        .collect::<BTreeSet<_>>();
    if let Some(identity) = unique_canonical_candidate(exact_path)? {
        return Ok(identity);
    }
    let exact_revision = candidates
        .iter()
        .filter(|candidate| {
            candidate
                .sync
                .metadata
                .get("source_revision")
                .and_then(Value::as_str)
                == Some(source.source_revision.as_str())
        })
        .filter_map(|candidate| candidate.descriptor.source_identity.clone())
        .collect::<BTreeSet<_>>();
    Ok(unique_canonical_candidate(exact_revision)?
        .unwrap_or_else(|| proposed_source_identity.to_owned()))
}

fn unique_canonical_candidate(candidates: BTreeSet<String>) -> Result<Option<String>> {
    if candidates.len() > 1 {
        return Err(CaptureError::InvalidPayload(
            "Kiro physical source resolves to multiple canonical source identities".to_owned(),
        ));
    }
    Ok(candidates.into_iter().next())
}

fn kiro_capture_source_ids(
    store: &Store,
    context: &ProviderAdapterContext,
    canonical_source_identity: &str,
) -> Result<Vec<Uuid>> {
    Ok(store
        .list_capture_sources()?
        .into_iter()
        .filter(|source| {
            source.descriptor.provider == CaptureProvider::KiroCli
                && source.descriptor.machine_id == context.machine_id
                && source.descriptor.source_format.as_deref() == Some(KIRO_SQLITE_SOURCE_FORMAT)
                && source.descriptor.source_identity.as_deref() == Some(canonical_source_identity)
                && source.sync.deleted_at.is_none()
        })
        .map(|source| source.id)
        .collect())
}

pub(super) fn kiro_generation_key(
    source: &KiroSource,
    context: &ProviderAdapterContext,
    canonical_source_identity: &str,
    generation: u64,
) -> NativePathSourceGenerationKey {
    let mut digest = Sha256::new();
    digest.update(b"ctx-kiro-nativepath-generation-v1\0");
    hash_field(&mut digest, source.locator_identity.as_bytes());
    hash_field(&mut digest, source.source_revision.as_bytes());
    digest.update(generation.to_be_bytes());
    NativePathSourceGenerationKey {
        provider: CaptureProvider::KiroCli,
        source_format: KIRO_SQLITE_SOURCE_FORMAT.to_owned(),
        machine_id: context.machine_id.clone(),
        canonical_source_identity: canonical_source_identity.to_owned(),
        locator_identity: source.locator_identity.clone(),
        cursor_stream: source.cursor_stream.clone(),
        source_revision: source.source_revision.clone(),
        generation_id: format!("kiro-generation-v1:{}", hex(&digest.finalize())),
    }
}

fn deduplicate_retained_entities(retained: &mut NativePathRetainedSourceEntities) {
    retained.capture_source_ids.sort_unstable();
    retained.capture_source_ids.dedup();
    retained.session_ids.sort_unstable();
    retained.session_ids.dedup();
    retained.session_edge_ids.sort_unstable();
    retained.session_edge_ids.dedup();
    retained.run_ids.sort_unstable();
    retained.run_ids.dedup();
    retained.event_ids.sort_unstable();
    retained.event_ids.dedup();
    retained.file_touch_ids.sort_unstable();
    retained.file_touch_ids.dedup();
}

pub(super) fn kiro_capture_source(
    source: &KiroSource,
    context: &ProviderAdapterContext,
    fact: &KiroSessionFact,
    source_id: Uuid,
    raw_source_path: &str,
    source_root: &str,
    canonical_source_identity: &str,
) -> CaptureSource {
    CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::KiroCli,
            machine_id: context.machine_id.clone(),
            process_id: None,
            cwd: (!fact.key.trim().is_empty()).then(|| fact.key.clone()),
            raw_source_path: Some(raw_source_path.to_owned()),
            source_format: Some(KIRO_SQLITE_SOURCE_FORMAT.to_owned()),
            source_root: Some(source_root.to_owned()),
            source_identity: Some(canonical_source_identity.to_owned()),
            external_session_id: Some(fact.provider_session_id.clone()),
        },
        started_at: fact.started_at,
        ended_at: Some(fact.ended_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": fact.provider_session_id,
                "source_format": KIRO_SQLITE_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "source_identity": canonical_source_identity,
                "source_root": source_root,
                "source_revision": source.source_revision,
                "source_identity_key": provider_scoped_source_identity_key(
                    CaptureProvider::KiroCli,
                    &fact.provider_session_id,
                    KIRO_SQLITE_SOURCE_FORMAT,
                    Some(raw_source_path),
                ),
                "sqlite_user_version": source.user_version,
                "schema_fingerprint": source.schema_fingerprint,
                "nativepath_publication": KIRO_NATIVE_PUBLICATION_REVISION,
            }),
        ),
    }
}

pub(super) fn kiro_session(
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    fact: &KiroSessionFact,
    source_id: Uuid,
    session_id: Uuid,
) -> Session {
    Session {
        id: session_id,
        history_record_id: options.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::KiroCli,
        external_session_id: Some(fact.provider_session_id.clone()),
        external_agent_id: None,
        agent_type: AgentType::Primary,
        role_hint: Some("primary".to_owned()),
        is_primary: true,
        status: SessionStatus::Completed,
        transcript_blob_id: None,
        started_at: fact.started_at,
        ended_at: Some(fact.ended_at),
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": fact.provider_session_id,
                "source_format": KIRO_SQLITE_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "metadata": {
                    "table": fact.table,
                    "rowid": fact.rowid,
                    "key": fact.key,
                    "history_len": fact.history_len,
                    "conversation": fact.conversation_preview,
                    "nativepath_publication": KIRO_NATIVE_PUBLICATION_REVISION,
                },
            }),
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn publish_event(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    source_id: Uuid,
    session_id: Uuid,
    fact: &KiroSessionFact,
    prepared: &KiroPreparedEvent,
    summary: &mut ProviderImportSummary,
    retained: &mut NativePathRetainedSourceEntities,
) -> Result<()> {
    let event_hash = compute_payload_hash(&prepared.event.payload)?;
    let legacy_provider_event_hash = prepared
        .event
        .metadata
        .get("legacy_provider_event_hash")
        .and_then(Value::as_str)
        .filter(|hash| !hash.is_empty())
        .ok_or(CaptureError::SystemInvariant(
            "Kiro NativePath event has no exact released positional hash",
        ))?;
    let identity = provider_event_import_identity_with_exact_legacy_source(
        committed_store,
        CaptureProvider::KiroCli,
        &fact.provider_session_id,
        source_id,
        prepared.event.provider_event_index,
        prepared.event.provider_event_index,
        &event_hash,
        None,
        Some(prepared.event.provider_event_index),
        session_id
            == crate::provider::importer::provider_session_uuid(
                CaptureProvider::KiroCli,
                &fact.provider_session_id,
            ),
    )?;
    let event = kiro_core_event(
        context,
        options,
        &fact.provider_session_id,
        source_id,
        session_id,
        usize::try_from(fact.rowid)
            .unwrap_or_default()
            .saturating_add(1),
        &prepared.event,
        &event_hash,
        ProviderEventHashAuthority::NormalizedPayloadFallback,
        &identity,
    )?;
    if group.reconcile_provider_event_migrating_exact_legacy_provider_hash(
        &event,
        legacy_provider_event_hash,
    )? {
        summary.imported_events = summary.imported_events.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    } else {
        summary.skipped_events = summary.skipped_events.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    }
    retained.event_ids.push(event.id);
    summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
    for touch in &prepared.touches {
        let touch_id = provider_file_touch_import_id(
            committed_store,
            CaptureProvider::KiroCli,
            &fact.provider_session_id,
            source_id,
            touch.provider_event_index,
            touch.provider_touch_index,
            session_id
                == crate::provider::importer::provider_session_uuid(
                    CaptureProvider::KiroCli,
                    &fact.provider_session_id,
                ),
        )?;
        let file = kiro_file_touched(
            touch,
            &fact.provider_session_id,
            options.history_record_id,
            source_id,
            session_id,
            Some(identity.id),
            touch_id,
        );
        let existed = committed_store.file_touched_exists(file.id)?;
        group.upsert_file_touched(&file)?;
        retained.file_touch_ids.push(file.id);
        if existed {
            summary.skipped = summary.skipped.saturating_add(1);
        } else {
            summary.imported = summary.imported.saturating_add(1);
        }
        summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn kiro_core_event(
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    provider_session_id: &str,
    source_id: Uuid,
    session_id: Uuid,
    line_number: usize,
    native: &KiroNativeEvent,
    event_hash: &str,
    authority: ProviderEventHashAuthority,
    identity: &ProviderEventImportIdentity,
) -> Result<Event> {
    let dedupe_key =
        Store::provider_event_dedupe_key_with_payload_hash(&identity.dedupe_key, event_hash)
            .unwrap_or_else(|| identity.dedupe_key.clone());
    let mut provider_metadata = native.metadata.clone();
    let source_record_coordinates = take_kiro_source_record_coordinates(&mut provider_metadata)?;
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
        "source_format": KIRO_SQLITE_SOURCE_FORMAT,
        "source_trust": ProviderSourceTrust::ProviderNative,
        "fixture_line": line_number,
        "imported_at": context.imported_at,
        "event_idempotency_key": format!(
            "provider-event:{}:{}:{}",
            CaptureProvider::KiroCli.as_str(),
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
            "provider": CaptureProvider::KiroCli.as_str(),
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

fn take_kiro_source_record_coordinates(metadata: &mut Value) -> Result<Option<(u64, u32)>> {
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

fn kiro_file_touched(
    touch: &KiroFileTouch,
    provider_session_id: &str,
    history_record_id: Option<Uuid>,
    source_id: Uuid,
    session_id: Uuid,
    event_id: Option<Uuid>,
    touch_id: Uuid,
) -> FileTouched {
    let source_root = provider_source_root(
        touch.source_root.as_deref(),
        touch.raw_source_path.as_deref(),
    );
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
                "provider": CaptureProvider::KiroCli.as_str(),
                "provider_session_id": provider_session_id,
                "provider_touch_index": touch.provider_touch_index,
                "provider_event_index": touch.provider_event_index,
                "raw_source_path": touch.raw_source_path,
                "source_id": source_id,
                "source_format": KIRO_SQLITE_SOURCE_FORMAT,
                "source_root": source_root,
                "metadata": touch.metadata,
                "session_id": session_id,
            }),
        ),
    }
}

fn page_publication_id(
    source: &KiroSource,
    page: &KiroCorePage,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(KIRO_PUBLICATION_DOMAIN);
    hash_field(&mut digest, source.locator_identity.as_bytes());
    hash_field(&mut digest, source.source_revision.as_bytes());
    hash_field(
        &mut digest,
        serde_json::to_string(&page.expected_frontier)
            .unwrap_or_default()
            .as_bytes(),
    );
    hash_field(
        &mut digest,
        serde_json::to_string(&page.next_frontier)
            .unwrap_or_default()
            .as_bytes(),
    );
    digest.update([u8::from(page.terminal)]);
    for prepared in &page.events {
        digest.update(prepared.event.provider_event_index.to_be_bytes());
        hash_field(
            &mut digest,
            prepared
                .event
                .provider_event_hash
                .as_deref()
                .unwrap_or_default()
                .as_bytes(),
        );
        hash_field(
            &mut digest,
            serde_json::to_string(&prepared.event.payload)
                .unwrap_or_default()
                .as_bytes(),
        );
    }
    for rejection in &page.rejections {
        digest.update(rejection.line.to_be_bytes());
        hash_field(&mut digest, rejection.reason.as_bytes());
    }
    hash_field(&mut digest, transition.next().cursor.as_bytes());
    format!("kiro-nativepath-v1:{}", hex(&digest.finalize()))
}
