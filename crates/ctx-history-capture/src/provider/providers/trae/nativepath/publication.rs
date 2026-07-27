use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_core_page(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    authority: &TraeSourceAuthority,
    page: &TraeScanPage,
    summary: &mut ProviderImportSummary,
) -> Result<TraeRouteState> {
    let resolution =
        group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::Trae,
            source_format: TRAE_STATE_VSCDB_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity: authority.locator_identity.clone(),
            cursor_stream: authority.cursor_stream.clone(),
            proposed_source_identity: authority.proposed_source_identity.clone(),
            raw_source_path: Some(authority.raw_source_path.clone()),
            source_revision: authority.source_revision.clone(),
            observed_at_ms: context.imported_at.timestamp_millis(),
        })?;
    let mut sessions = BTreeMap::new();
    for fact in page.sessions.values() {
        let existing_source = committed_store.capture_source_by_canonical_identity_session(
            CaptureProvider::Trae,
            TRAE_STATE_VSCDB_SOURCE_FORMAT,
            &context.machine_id,
            &resolution.canonical_source_identity,
            &fact.provider_session_id,
        )?;
        let source_id = existing_source
            .as_ref()
            .map(|source| source.id)
            .unwrap_or_else(|| {
                provider_scoped_source_uuid(
                    CaptureProvider::Trae,
                    &fact.provider_session_id,
                    TRAE_STATE_VSCDB_SOURCE_FORMAT,
                    Some(&authority.raw_source_path),
                )
            });
        group.upsert_capture_source(&CaptureSource {
            id: source_id,
            descriptor: CaptureSourceDescriptor {
                kind: CaptureSourceKind::ProviderImport,
                provider: CaptureProvider::Trae,
                machine_id: context.machine_id.clone(),
                process_id: None,
                cwd: authority.workspace_folder.clone(),
                raw_source_path: Some(authority.raw_source_path.clone()),
                source_format: Some(TRAE_STATE_VSCDB_SOURCE_FORMAT.to_owned()),
                source_root: Some(authority.source_root.display().to_string()),
                source_identity: Some(resolution.canonical_source_identity.clone()),
                external_session_id: Some(fact.provider_session_id.clone()),
            },
            started_at: fact.started_at,
            ended_at: fact.ended_at,
            sync: provider_sync_metadata(
                Fidelity::Imported,
                json!({
                    "provider_session_id": fact.provider_session_id,
                    "source_format": TRAE_STATE_VSCDB_SOURCE_FORMAT,
                    "source_trust": "provider_native",
                    "imported_at": context.imported_at,
                    "source_identity": resolution.canonical_source_identity,
                    "source_root": authority.source_root,
                    "source_revision": authority.source_revision,
                    "source_identity_key": provider_scoped_source_identity_key(
                        CaptureProvider::Trae,
                        &fact.provider_session_id,
                        TRAE_STATE_VSCDB_SOURCE_FORMAT,
                        Some(&authority.raw_source_path),
                    ),
                    "nativepath_publication": TRAE_NATIVE_PARSER_REVISION,
                    "inventory_observation_token": options.inventory_observation_token,
                }),
            ),
        })?;
        group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;
        let session_id = provider_import_session_uuid(
            committed_store,
            CaptureProvider::Trae,
            &fact.provider_session_id,
            source_id,
            Some(&resolution.canonical_source_identity),
        )?;
        let existed = committed_store.get_session(session_id).is_ok();
        let session = Session {
            id: session_id,
            history_record_id: options.history_record_id,
            parent_session_id: None,
            root_session_id: None,
            capture_source_id: Some(source_id),
            provider: CaptureProvider::Trae,
            external_session_id: Some(fact.provider_session_id.clone()),
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("primary".to_owned()),
            is_primary: true,
            status: SessionStatus::Imported,
            transcript_blob_id: None,
            started_at: fact.started_at,
            ended_at: fact.ended_at,
            timestamps: timestamps(context.imported_at),
            sync: provider_sync_metadata(
                Fidelity::Imported,
                json!({
                    "provider_session_id": fact.provider_session_id,
                    "source_format": TRAE_STATE_VSCDB_SOURCE_FORMAT,
                    "source_trust": "provider_native",
                    "imported_at": context.imported_at,
                    "session_idempotency_key": format!(
                        "provider-session:trae:{}",
                        fact.provider_session_id
                    ),
                    "metadata": {
                        "display_name": "Trae",
                        "title": fact.title,
                        "native_workspace_id": authority.workspace_id,
                        "native_session_id": fact.native_session_id,
                        "workspace_folder": authority.workspace_folder,
                        "chat_key": fact.chat_key,
                        "session": fact.metadata_preview,
                        "nativepath_publication": TRAE_NATIVE_PARSER_REVISION,
                    },
                }),
            ),
        };
        group.upsert_session(&session)?;
        if existed {
            summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
        } else {
            summary.imported_sessions = summary.imported_sessions.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        }
        sessions.insert(fact.provider_session_id.clone(), (session, source_id));
    }

    for record in &page.core {
        let (session, source_id) =
            sessions
                .get(&record.provider_session_id)
                .ok_or(CaptureError::SystemInvariant(
                    "Trae Core record has no page session",
                ))?;
        let provider_event_sequence_index = packed_native_index(
            record.key_index,
            record.raw_session_index,
            record.message_index,
        )?;
        let legacy_provider_event_sequence_index = packed_native_index(
            record.key_index,
            record.legacy_session_index,
            record.message_index,
        )?;
        let legacy_provider_event_hash = record.event.provider_event_hash.as_str();
        let event_hash = compute_payload_hash(&record.event.payload)?;
        let chat_key = TRAE_CHAT_KEYS
            .get(usize::from(record.key_index))
            .copied()
            .ok_or(CaptureError::SystemInvariant(
                "Trae Core record has an invalid chat key",
            ))?;
        let provider_event_index = native_message_event_index(chat_key, legacy_provider_event_hash);
        let legacy_provider_event_index = legacy_native_event_index(
            record.key_index,
            record.legacy_session_index,
            record.message_index,
            legacy_provider_event_hash,
        );
        let stable_identity = provider_event_import_identity_with_exact_legacy_source(
            committed_store,
            CaptureProvider::Trae,
            &record.provider_session_id,
            *source_id,
            provider_event_index,
            provider_event_sequence_index,
            &event_hash,
            None,
            None,
            false,
        )?;
        let legacy_identity = provider_event_import_identity_with_exact_legacy_source(
            committed_store,
            CaptureProvider::Trae,
            &record.provider_session_id,
            *source_id,
            legacy_provider_event_index,
            legacy_provider_event_sequence_index,
            legacy_provider_event_hash,
            None,
            Some(u64::from(record.message_index)),
            true,
        )?;
        let identity = trae_native_message_import_identity(
            committed_store,
            session.id,
            *source_id,
            chat_key,
            legacy_provider_event_hash,
            provider_event_index,
            stable_identity,
            legacy_identity,
        )?;
        let dedupe_key =
            Store::provider_event_dedupe_key_with_payload_hash(&identity.dedupe_key, &event_hash)
                .ok_or(CaptureError::SystemInvariant(
                "Trae event identity has a malformed dedupe key",
            ))?;
        let event = Event {
            id: identity.id,
            seq: identity.seq,
            history_record_id: options.history_record_id,
            session_id: Some(session.id),
            run_id: None,
            event_type: record.event.event_type,
            role: record.event.role,
            occurred_at: record.event.occurred_at,
            capture_source_id: Some(*source_id),
            payload: record.event.payload.clone(),
            payload_blob_id: None,
            dedupe_key: Some(dedupe_key),
            sync: provider_sync_metadata(
                record.event.fidelity,
                json!({
                    "provider_session_id": record.provider_session_id,
                    "provider_event_index": provider_event_index,
                    "provider_event_sequence_index": provider_event_sequence_index,
                    "legacy_provider_event_index": legacy_provider_event_index,
                    "provider_event_hash": event_hash,
                    "provider_event_hash_authority": "normalized_payload_fallback",
                    "source_format": TRAE_STATE_VSCDB_SOURCE_FORMAT,
                    "source_trust": "provider_native",
                    "source_record_ordinal": record.key_index,
                    "source_record_subrecord_index": record.message_index,
                    "native_session_index": record.raw_session_index,
                    "metadata": record.event.metadata,
                }),
            ),
        };
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
    }
    Ok(TraeRouteState {
        path: authority.path.clone(),
        locator_identity: authority.locator_identity.clone(),
        cursor_stream: authority.cursor_stream.clone(),
        canonical_source_identity: resolution.canonical_source_identity,
        source_revision: authority.source_revision.clone(),
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn trae_native_message_import_identity(
    store: &Store,
    session_id: uuid::Uuid,
    source_id: uuid::Uuid,
    chat_key: &str,
    native_record_id: &str,
    stable_provider_event_index: u64,
    stable: ProviderEventImportIdentity,
    legacy: ProviderEventImportIdentity,
) -> Result<ProviderEventImportIdentity> {
    match store.get_event(stable.id) {
        Ok(event)
            if trae_event_matches_native_identity(
                &event,
                session_id,
                source_id,
                chat_key,
                native_record_id,
                stable_provider_event_index,
            ) =>
        {
            return provider_event_identity_from_existing(event);
        }
        Ok(_) => {
            return Err(CaptureError::InvalidPayload(
                "Trae stable native-message identity collides with another event".into(),
            ));
        }
        Err(StoreError::NotFound(_)) => {}
        Err(error) => return Err(CaptureError::Store(error)),
    }
    match store.get_event(legacy.id) {
        Ok(event)
            if trae_event_matches_native_identity(
                &event,
                session_id,
                source_id,
                chat_key,
                native_record_id,
                stable_provider_event_index,
            ) =>
        {
            return provider_event_identity_from_existing(event);
        }
        Ok(_) | Err(StoreError::NotFound(_)) => {}
        Err(error) => return Err(CaptureError::Store(error)),
    }

    let mut matches = store
        .events_for_session(session_id)?
        .into_iter()
        .filter(|event| {
            trae_event_matches_native_identity(
                event,
                session_id,
                source_id,
                chat_key,
                native_record_id,
                stable_provider_event_index,
            )
        });
    let Some(existing) = matches.next() else {
        return Ok(stable);
    };
    if matches.next().is_some() {
        return Err(CaptureError::InvalidPayload(
            "multiple Trae events claim the same native message identity".into(),
        ));
    }
    provider_event_identity_from_existing(existing)
}

pub(super) fn trae_event_matches_native_identity(
    event: &Event,
    session_id: uuid::Uuid,
    source_id: uuid::Uuid,
    chat_key: &str,
    native_record_id: &str,
    stable_provider_event_index: u64,
) -> bool {
    if event.session_id != Some(session_id)
        || event.capture_source_id != Some(source_id)
        || !trae_event_native_record_id_matches(event, native_record_id)
        || event
            .sync
            .metadata
            .pointer("/metadata/chat_key")
            .and_then(Value::as_str)
            != Some(chat_key)
    {
        return false;
    }
    let authority = event
        .sync
        .metadata
        .get("provider_event_hash_authority")
        .and_then(Value::as_str);
    if authority == Some(ProviderEventHashAuthority::NormalizedPayloadFallback.as_str()) {
        return event
            .sync
            .metadata
            .get("provider_event_index")
            .and_then(Value::as_u64)
            == Some(stable_provider_event_index);
    }
    event
        .sync
        .metadata
        .get("provider_event_hash")
        .and_then(Value::as_str)
        == Some(native_record_id)
}

pub(super) fn trae_event_native_record_id_matches(event: &Event, native_record_id: &str) -> bool {
    if event
        .payload
        .get("event_id")
        .or_else(|| event.payload.pointer("/body/event_id"))
        .and_then(Value::as_str)
        == Some(native_record_id)
    {
        return true;
    }
    let Some(provider_session_id) = event
        .sync
        .metadata
        .get("provider_session_id")
        .and_then(Value::as_str)
    else {
        return false;
    };
    let Some(native_message_id) = event
        .sync
        .metadata
        .pointer("/metadata/native_message_id")
        .and_then(Value::as_str)
    else {
        return false;
    };
    native_record_id
        .strip_prefix(provider_session_id)
        .and_then(|suffix| suffix.strip_prefix(':'))
        == Some(native_message_id)
}

pub(super) fn provider_event_identity_from_existing(
    event: Event,
) -> Result<ProviderEventImportIdentity> {
    let dedupe_key = event.dedupe_key.ok_or(CaptureError::SystemInvariant(
        "existing Trae native message has no provider dedupe key",
    ))?;
    Ok(ProviderEventImportIdentity {
        id: event.id,
        seq: event.seq,
        dedupe_key,
        run_source_id: event.capture_source_id,
    })
}

pub(super) fn page_publication_id(
    authority: &TraeSourceAuthority,
    page: &TraeScanPage,
    generation: u64,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-trae-nativepath-page-v1\0");
    digest.update(authority.locator_identity.as_bytes());
    digest.update(authority.source_revision.as_bytes());
    digest.update(generation.to_le_bytes());
    digest.update(serde_json::to_vec(&page.expected).unwrap_or_default());
    digest.update(serde_json::to_vec(&page.next).unwrap_or_default());
    for record in &page.core {
        digest.update(record.provider_session_id.as_bytes());
        digest.update(record.key_index.to_le_bytes());
        digest.update(record.raw_session_index.to_le_bytes());
        digest.update(record.legacy_session_index.to_le_bytes());
        digest.update(record.message_index.to_le_bytes());
        digest.update(record.event.provider_event_hash.as_bytes());
        digest.update(serde_json::to_vec(&record.event.payload).unwrap_or_default());
    }
    for rejection in &page.rejections {
        digest.update(rejection.line.to_le_bytes());
        digest.update(rejection.error.as_bytes());
    }
    digest.update(transition.next().cursor.as_bytes());
    format!("trae-nativepath-page-v1:{:x}", digest.finalize())
}

pub(super) fn packed_native_index(key: u16, session: u32, message: u32) -> Result<u64> {
    if session > 0x00ff_ffff || message > 0x00ff_ffff {
        return Err(CaptureError::InvalidPayload(
            "Trae native message coordinate exceeds packed identity bounds".into(),
        ));
    }
    Ok((u64::from(key) << 48) | (u64::from(session) << 24) | u64::from(message))
}

pub(super) fn native_message_event_index(chat_key: &str, native_record_id: &str) -> u64 {
    let mut digest = Sha256::new();
    digest.update(b"ctx-trae-native-message-identity-v2\0");
    digest.update(
        u64::try_from(chat_key.len())
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    digest.update(chat_key.as_bytes());
    digest.update(
        u64::try_from(native_record_id.len())
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    digest.update(native_record_id.as_bytes());
    let digest = digest.finalize();
    u64::from_be_bytes(
        digest[..8]
            .try_into()
            .expect("SHA-256 prefix is eight bytes"),
    )
}

pub(super) fn legacy_native_event_index(
    key: u16,
    session: u32,
    message: u32,
    event_hash: &str,
) -> u64 {
    let mut digest = Sha256::new();
    digest.update(b"ctx-trae-native-event-index-v1\0");
    digest.update(key.to_le_bytes());
    digest.update(session.to_le_bytes());
    digest.update(message.to_le_bytes());
    digest.update(event_hash.as_bytes());
    let digest = digest.finalize();
    u64::from_be_bytes(
        digest[..8]
            .try_into()
            .expect("SHA-256 prefix is eight bytes"),
    )
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
                CaptureProvider::Trae.as_str(),
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
