use super::*;

pub(super) fn publish_cursor_group(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &CursorPublicationContext<'_>,
    pages: &[CursorPendingPage],
) -> Result<ProviderImportSummary> {
    let source_paths = pages
        .iter()
        .map(|pending| pending.observation.path.clone())
        .collect::<BTreeSet<_>>();
    for path in &source_paths {
        revalidate_cursor_source(pages, path)?;
    }

    let mut transitions = Vec::with_capacity(source_paths.len());
    for path in &source_paths {
        let pending = pages
            .iter()
            .rev()
            .find(|pending| &pending.observation.path == path)
            .expect("Cursor pending source exists");
        let stream = provider_source_cursor_stream_for_path(
            CaptureProvider::Cursor,
            CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
            &pending.observation.locator_identity,
        );
        let stored = store.get_sync_cursor(None, context.machine_id, &stream)?;
        let provider_cursor = encode_cursor_native_cursor(
            &pending.observation.proposed_source_identity,
            &pending.observation,
            &pending.page.next_checkpoint,
            pending.retained_event_count,
            pending.page.rejected_records,
            &pending.page.rejections,
        )?;
        transitions.push(NativePathCursorTransition::new(
            stored.as_ref().map(|cursor| cursor.cursor.clone()),
            provider_sync_cursor(
                context.machine_id,
                stream,
                provider_cursor,
                context.imported_at,
            ),
        ));
    }
    let publication_id = cursor_publication_id(pages, &transitions);
    let retained_bytes = pages.iter().fold(0_usize, |total, pending| {
        total.saturating_add(pending.page.serialized_bytes)
    });
    let accounting =
        NativePathGroupAccounting::new(pages.len(), source_paths.len(), retained_bytes)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    if matches!(
        group.classify_cursor_set(&publication_id, &transitions)?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }

    let mut summary = ProviderImportSummary::default();
    let mut resolved = BTreeMap::new();
    for path in &source_paths {
        let pending = pages
            .iter()
            .rev()
            .find(|pending| &pending.observation.path == path)
            .expect("Cursor pending source exists");
        let raw_source_path = path.display().to_string();
        let source_root = context.source_root.display().to_string();
        let stream = provider_source_cursor_stream_for_path(
            CaptureProvider::Cursor,
            CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
            &pending.observation.locator_identity,
        );
        let source_revision = cursor_source_revision(&pending.observation);
        let resolution =
            group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
                provider: CaptureProvider::Cursor,
                source_format: CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT.to_owned(),
                machine_id: context.machine_id.to_owned(),
                locator_identity: pending.observation.locator_identity.clone(),
                cursor_stream: stream,
                proposed_source_identity: pending.observation.proposed_source_identity.clone(),
                raw_source_path: Some(raw_source_path.clone()),
                source_revision: source_revision.clone(),
                observed_at_ms: context.imported_at.timestamp_millis(),
            })?;
        let native_session_id = &pending.observation.native_session_id;
        let source_id = cursor_existing_source_id(
            committed_store,
            context.machine_id,
            &resolution.canonical_source_identity,
            native_session_id,
        )?
        .unwrap_or_else(|| {
            provider_scoped_source_uuid(
                CaptureProvider::Cursor,
                native_session_id,
                CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
                Some(&raw_source_path),
            )
        });
        let session_fact = cursor_session_fact(pending);
        group.upsert_capture_source(&cursor_capture_source(
            context,
            session_fact.as_ref(),
            source_id,
            &raw_source_path,
            &source_root,
            &resolution.canonical_source_identity,
            &source_revision,
        ))?;
        group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;
        let session = session_fact
            .as_ref()
            .map(|fact| {
                cursor_session(
                    committed_store,
                    context,
                    fact,
                    source_id,
                    &resolution.canonical_source_identity,
                )
            })
            .transpose()?;
        if let Some(session) = &session {
            let existed = committed_store.get_session(session.id).is_ok();
            group.upsert_session(session)?;
            if existed {
                summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
                summary.skipped = summary.skipped.saturating_add(1);
            } else {
                summary.imported_sessions = summary.imported_sessions.saturating_add(1);
                summary.imported = summary.imported.saturating_add(1);
            }
        }
        resolved.insert(path.clone(), ResolvedCursorSource { source_id, session });
    }

    for pending in pages {
        let source =
            resolved
                .get(&pending.observation.path)
                .ok_or(CaptureError::SystemInvariant(
                    "Cursor publication lost its resolved source",
                ))?;
        for event in &pending.page.events {
            let session = source
                .session
                .as_ref()
                .ok_or(CaptureError::SystemInvariant(
                    "Cursor retained event has no canonical session",
                ))?;
            publish_cursor_event(
                &mut group,
                committed_store,
                context,
                source.source_id,
                session,
                &pending.observation,
                event,
                &mut summary,
            )?;
        }
    }

    for path in &source_paths {
        revalidate_cursor_source(pages, path)?;
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

fn cursor_existing_source_id(
    store: &Store,
    machine_id: &str,
    canonical_source_identity: &str,
    native_session_id: &str,
) -> Result<Option<Uuid>> {
    for source_format in [
        CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
        LEGACY_CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
    ] {
        if let Some(source) = store.capture_source_by_canonical_identity_session(
            CaptureProvider::Cursor,
            source_format,
            machine_id,
            canonical_source_identity,
            native_session_id,
        )? {
            return Ok(Some(source.id));
        }
    }
    Ok(None)
}

fn cursor_session_fact(pending: &CursorPendingPage) -> Option<CursorNativeSession> {
    let checkpoint = &pending.page.next_checkpoint.session;
    let has_session = !pending.page.events.is_empty()
        || checkpoint.started_at.is_some()
        || checkpoint.title.is_some();
    has_session.then(|| CursorNativeSession {
        native_session_id: pending.observation.native_session_id.clone(),
        project: pending.transcript.project().to_path_buf(),
        started_at: checkpoint.started_at,
        ended_at: checkpoint.ended_at,
        title: checkpoint.title.clone(),
    })
}

fn cursor_capture_source(
    context: &CursorPublicationContext<'_>,
    session: Option<&CursorNativeSession>,
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
            provider: CaptureProvider::Cursor,
            machine_id: context.machine_id.to_owned(),
            process_id: None,
            cwd: None,
            raw_source_path: Some(raw_source_path.to_owned()),
            source_format: Some(CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT.to_owned()),
            source_root: Some(source_root.to_owned()),
            source_identity: Some(source_identity.to_owned()),
            external_session_id: session.map(|session| session.native_session_id.clone()),
        },
        started_at: session
            .and_then(|session| session.started_at)
            .unwrap_or(context.imported_at),
        ended_at: session.and_then(|session| session.ended_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": session.map(|session| &session.native_session_id),
                "source_format": CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "source_identity": source_identity,
                "source_revision": source_revision,
                "source_identity_key": session.map(|session| {
                    provider_scoped_source_identity_key(
                        CaptureProvider::Cursor,
                        &session.native_session_id,
                        CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
                        Some(raw_source_path),
                    )
                }),
            }),
        ),
    }
}

fn cursor_session(
    committed_store: &Store,
    context: &CursorPublicationContext<'_>,
    fact: &CursorNativeSession,
    source_id: Uuid,
    source_identity: &str,
) -> Result<Session> {
    let id = provider_import_session_uuid(
        committed_store,
        CaptureProvider::Cursor,
        &fact.native_session_id,
        source_id,
        Some(source_identity),
    )?;
    Ok(Session {
        id,
        history_record_id: context.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Cursor,
        external_session_id: Some(fact.native_session_id.clone()),
        external_agent_id: None,
        agent_type: AgentType::Primary,
        role_hint: Some("primary".to_owned()),
        is_primary: true,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: fact.started_at.unwrap_or(context.imported_at),
        ended_at: fact.ended_at,
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": fact.native_session_id,
                "project": fact.project,
                "title": fact.title,
                "source_format": CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
            }),
        ),
    })
}

#[allow(clippy::too_many_arguments)]
fn publish_cursor_event(
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    committed_store: &Store,
    context: &CursorPublicationContext<'_>,
    source_id: Uuid,
    session: &Session,
    observation: &CursorSourceObservation,
    event: &CursorNativeEvent,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let provider_event_index = cursor_event_index(event)?;
    let identity = provider_event_import_identity_with_exact_legacy_source(
        committed_store,
        CaptureProvider::Cursor,
        session.external_session_id.as_deref().unwrap_or_default(),
        source_id,
        provider_event_index,
        provider_event_index,
        &event.provider_event_hash,
        None,
        event.legacy_provider_event_index(),
        session.id
            == crate::provider::importer::provider_session_uuid(
                CaptureProvider::Cursor,
                session.external_session_id.as_deref().unwrap_or_default(),
            ),
    )?;
    let dedupe_key = Store::provider_event_dedupe_key_with_payload_hash(
        &identity.dedupe_key,
        &event.provider_event_hash,
    )
    .unwrap_or(identity.dedupe_key);
    let occurred_at = event.occurred_at.unwrap_or(session.started_at);
    let mut payload = json!({
        "provider": CaptureProvider::Cursor.as_str(),
        "provider_session_id": session.external_session_id,
        "provider_event_index": provider_event_index,
        "provider_event_hash": event.provider_event_hash,
        "native_identity": event.identity.provider_identity(),
        "body": event.body,
        "artifacts": [],
    });
    if let crate::provider::providers::cursor::CursorEventBody::Text { text } = &event.body {
        let object = payload
            .as_object_mut()
            .ok_or(CaptureError::SystemInvariant(
                "Cursor normalized payload must be an object",
            ))?;
        object.insert("text".to_owned(), json!(text));
        object.insert(
            "text_retention".to_owned(),
            event
                .text_retention
                .clone()
                .ok_or(CaptureError::SystemInvariant(
                    "Cursor text event is missing standard retention metadata",
                ))?,
        );
    }
    let mut event_metadata = json!({
        "provider_session_id": session.external_session_id,
        "provider_event_index": provider_event_index,
        "provider_event_hash": event.provider_event_hash,
        "provider_event_hash_authority": "normalized_payload_fallback",
        "source_format": CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
        "source_trust": "provider_native",
        "cursor": event.identity.provider_identity(),
        "fixture_line": event.native_order.physical_ordinal.saturating_add(1),
        "source_record_ordinal": event.native_order.physical_ordinal,
        "source_record_subrecord_index": event.native_order.part_ordinal,
        "source_semantic_ordinal": event.native_order.semantic_ordinal,
        "native_identity": event.identity.provider_identity(),
    });
    attach_cursor_message_locator(&mut event_metadata, observation, event)?;
    let normalized = Event {
        id: identity.id,
        seq: identity.seq,
        history_record_id: context.history_record_id,
        session_id: Some(session.id),
        run_id: None,
        event_type: event.event_type,
        role: Some(event.role),
        occurred_at,
        capture_source_id: Some(source_id),
        payload,
        payload_blob_id: None,
        dedupe_key: Some(dedupe_key),
        sync: provider_sync_metadata(Fidelity::Imported, event_metadata),
    };
    if group.reconcile_provider_event(
        &normalized,
        ProviderEventHashAuthority::NormalizedPayloadFallback,
    )? {
        summary.imported_events = summary.imported_events.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    } else {
        summary.skipped_events = summary.skipped_events.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    }
    summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);

    if let crate::provider::providers::cursor::CursorEventBody::ToolCall { input_paths, .. } =
        &event.body
    {
        for (touch_ordinal, path) in input_paths.iter().enumerate() {
            let packed_touch = event
                .native_order
                .semantic_ordinal
                .checked_mul(u64::from(u16::MAX) + 1)
                .and_then(|base| base.checked_add(touch_ordinal as u64))
                .ok_or(CaptureError::SystemInvariant(
                    "Cursor file-touch identity overflowed",
                ))?;
            let id = provider_file_touch_import_id(
                committed_store,
                CaptureProvider::Cursor,
                session.external_session_id.as_deref().unwrap_or_default(),
                source_id,
                Some(provider_event_index),
                packed_touch,
                session.id
                    == crate::provider::importer::provider_session_uuid(
                        CaptureProvider::Cursor,
                        session.external_session_id.as_deref().unwrap_or_default(),
                    ),
            )?;
            group.upsert_file_touched(&FileTouched {
                id,
                history_record_id: context.history_record_id,
                run_id: None,
                event_id: Some(normalized.id),
                vcs_workspace_id: None,
                path: path.clone(),
                change_kind: Some(FileChangeKind::Unknown),
                old_path: None,
                line_count_delta: None,
                confidence: Confidence::Explicit,
                timestamps: timestamps(occurred_at),
                source_id: Some(source_id),
                sync: provider_sync_metadata(
                    Fidelity::Imported,
                    json!({
                        "provider": CaptureProvider::Cursor.as_str(),
                        "provider_session_id": session.external_session_id,
                        "provider_event_index": provider_event_index,
                        "source_format": CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
                    }),
                ),
            })?;
        }
    }
    Ok(())
}

fn attach_cursor_message_locator(
    metadata: &mut serde_json::Value,
    observation: &CursorSourceObservation,
    event: &CursorNativeEvent,
) -> Result<()> {
    let Some(content_ref) = event.complete_content_ref.clone() else {
        return Ok(());
    };
    if event.event_type != EventType::Message
        || !matches!(
            event.body,
            crate::provider::providers::cursor::CursorEventBody::Text { .. }
        )
    {
        return Err(CaptureError::SystemInvariant(
            "Cursor complete message reference was attached to a non-message event",
        ));
    }
    if !verified_content_address_supported(
        CaptureProvider::Cursor,
        CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
        CompleteContentSourceFamily::Jsonl,
        VerifiedContentRole::MessageBody,
        EXACT_JSONL_COMPLETE_CONTENT_LOCATOR_KIND,
    ) {
        return Err(CaptureError::SystemInvariant(
            "Cursor truncated message route must support exact JSONL recovery",
        ));
    }
    let profile = verified_content_profile(
        CaptureProvider::Cursor,
        CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
        CompleteContentSourceFamily::Jsonl,
        VerifiedContentRole::MessageBody,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Cursor exact JSONL message route must have a verified-content profile",
    ))?;
    let mut locator = [0_u8; 80];
    locator[..8].copy_from_slice(&event.record_byte_start.to_be_bytes());
    locator[8..16].copy_from_slice(&event.record_byte_end_exclusive.to_be_bytes());
    locator[16..48].copy_from_slice(&cursor_complete_content_digest(
        CURSOR_EXACT_SOURCE_REVISION_DIGEST_DOMAIN,
        &cursor_complete_content_source_revision(observation),
    ));
    locator[48..].copy_from_slice(&cursor_complete_content_digest(
        CURSOR_EXACT_PATH_IDENTITY_DIGEST_DOMAIN,
        &observation.locator_identity,
    ));
    let record_sha256 = CompleteContentBodyDigest::parse(hex_digest(event.record_sha256)).ok_or(
        CaptureError::SystemInvariant("Cursor record SHA-256 is malformed"),
    )?;
    let persisted = VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        profile,
        content_ref,
        CompleteContentSourceFamily::Jsonl,
        EXACT_JSONL_COMPLETE_CONTENT_LOCATOR_KIND,
        &locator,
        format!(
            "cursor-line-v1:{}:{}",
            event.native_order.physical_ordinal, event.native_order.part_ordinal
        ),
        record_sha256,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Cursor exact JSONL message locator exceeds its bounded schema",
    ))?;
    attach_verified_content_locator(metadata, persisted).ok_or(CaptureError::SystemInvariant(
        "Cursor verified-content locator collection is malformed",
    ))
}

fn cursor_complete_content_digest(domain: &[u8], value: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value.as_bytes());
    digest.finalize().into()
}

fn cursor_event_index(event: &CursorNativeEvent) -> Result<u64> {
    if event.native_order.part_ordinal == 0 {
        return Ok(event.native_order.semantic_ordinal);
    }
    event
        .native_order
        .semantic_ordinal
        .checked_mul(u64::from(u16::MAX) + 1)
        .and_then(|index| index.checked_add(u64::from(event.native_order.part_ordinal)))
        .map(|index| index | (1_u64 << 63))
        .ok_or(CaptureError::SystemInvariant(
            "Cursor provider event identity index overflowed",
        ))
}

pub(super) fn cursor_event_touch_count(event: &CursorNativeEvent) -> usize {
    match &event.body {
        crate::provider::providers::cursor::CursorEventBody::ToolCall { input_paths, .. } => {
            input_paths.len()
        }
        _ => 0,
    }
}

fn revalidate_cursor_source(pages: &[CursorPendingPage], path: &Path) -> Result<()> {
    let pending = pages
        .iter()
        .find(|pending| pending.observation.path == path)
        .expect("Cursor pending source exists");
    let frozen = freeze_cursor_source(&pending.transcript)?;
    if frozen.observation() != &pending.observation {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    frozen.revalidate()
}

pub(super) fn cursor_source_revision(observation: &CursorSourceObservation) -> String {
    format!(
        "cursor-nativepath-strong-v1:length={};sha256={};modified={}:{}.{:09};readonly={};device={};inode={}",
        observation.length,
        hex_digest(observation.content_sha256),
        if observation.modified.before_epoch { '-' } else { '+' },
        observation.modified.seconds,
        observation.modified.nanos,
        observation.readonly,
        observation
            .file_identity
            .map_or_else(|| "none".to_owned(), |identity| identity.device.to_string()),
        observation
            .file_identity
            .map_or_else(|| "none".to_owned(), |identity| identity.inode.to_string()),
    )
}

pub(super) fn encode_cursor_native_cursor(
    canonical_source_identity: &str,
    observation: &CursorSourceObservation,
    checkpoint: &CursorCheckpoint,
    retained_event_count: u64,
    rejected_records: u64,
    rejections: &[CursorRecordRejection],
) -> Result<String> {
    Ok(serde_json::to_string(&CursorNativeCursorWire {
        version: CURSOR_NATIVE_CURSOR_VERSION,
        kind: "cursor-nativepath".to_owned(),
        canonical_source_identity: canonical_source_identity.to_owned(),
        observation: observation.clone(),
        checkpoint: checkpoint.clone(),
        retained_event_count,
        rejected_records,
        rejections: rejections.to_vec(),
    })?)
}

pub(super) fn decode_cursor_native_cursor(
    encoded_store_cursor: &str,
) -> Result<Option<CursorNativeCursorWire>> {
    let encoded = ctx_history_store::decode_native_path_committed_cursor(encoded_store_cursor)
        .map(|cursor| cursor.provider_cursor().to_owned())
        .unwrap_or_else(|_| encoded_store_cursor.to_owned());
    let Ok(wire) = serde_json::from_str::<CursorNativeCursorWire>(&encoded) else {
        return Ok(None);
    };
    Ok((wire.version == CURSOR_NATIVE_CURSOR_VERSION
        && wire.kind == "cursor-nativepath"
        && wire.checkpoint.schema_version == CursorCheckpoint::SCHEMA_VERSION
        && wire.checkpoint.parser_revision == CursorCheckpoint::PARSER_REVISION)
        .then_some(wire))
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
                CaptureProvider::Cursor.as_str(),
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

fn cursor_publication_id(
    pages: &[CursorPendingPage],
    transitions: &[NativePathCursorTransition],
) -> String {
    let mut digest = Sha256::new();
    digest.update(CURSOR_PUBLICATION_DOMAIN);
    digest.update((pages.len() as u64).to_be_bytes());
    for pending in pages {
        digest.update(pending.observation.content_sha256);
        digest.update(pending.page.expected_checkpoint.prefix.sha256);
        digest.update(pending.page.next_checkpoint.prefix.sha256);
    }
    for transition in transitions {
        digest.update(transition.key().stream().as_bytes());
        if let Some(expected) = transition.expected_cursor() {
            digest.update((expected.len() as u64).to_be_bytes());
            digest.update(expected.as_bytes());
        }
        digest.update((transition.next().cursor.len() as u64).to_be_bytes());
        digest.update(transition.next().cursor.as_bytes());
    }
    format!("cursor-nativepath-v1:{:x}", digest.finalize())
}

pub(super) fn cursor_retirement_publication_id(
    retirement: &ProviderSourceRouteRetirement,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-cursor-nativepath-route-retirement-v1\0");
    digest.update(retirement.provider.as_str().as_bytes());
    digest.update(retirement.source_format.as_bytes());
    digest.update(retirement.machine_id.as_bytes());
    digest.update(retirement.locator_identity.as_bytes());
    digest.update(retirement.cursor_stream.as_bytes());
    digest.update(retirement.expected_canonical_source_identity.as_bytes());
    digest.update(retirement.expected_source_revision.as_bytes());
    digest.update(format!("{:?}", retirement.reason).as_bytes());
    format!("cursor-nativepath-retirement-v1:{:x}", digest.finalize())
}

fn hex_digest(digest: [u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(super) fn cursor_rejection_message(
    kind: crate::provider::providers::cursor::CursorRejectionKind,
    observed_bytes: u64,
) -> String {
    let reason = match kind {
        crate::provider::providers::cursor::CursorRejectionKind::MalformedJson => "malformed JSONL",
        crate::provider::providers::cursor::CursorRejectionKind::Oversized => "oversized JSONL",
        crate::provider::providers::cursor::CursorRejectionKind::UnsupportedShape => {
            "unsupported JSONL shape"
        }
    };
    format!("Cursor {reason} record ({observed_bytes} bytes)")
}
