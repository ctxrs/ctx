use super::*;

pub(super) fn generation_source_id(
    committed_store: &Store,
    machine_id: &str,
    raw_source_path: &str,
    canonical_source_identity: &str,
    provider_session_id: &str,
    generation: u64,
) -> Result<Uuid> {
    if generation == 0 {
        return Ok(committed_store
            .capture_source_by_canonical_identity_session(
                CaptureProvider::ForgeCode,
                FORGECODE_SQLITE_SOURCE_FORMAT,
                machine_id,
                canonical_source_identity,
                provider_session_id,
            )?
            .map(|source| source.id)
            .unwrap_or_else(|| {
                provider_scoped_source_uuid(
                    CaptureProvider::ForgeCode,
                    provider_session_id,
                    FORGECODE_SQLITE_SOURCE_FORMAT,
                    Some(raw_source_path),
                )
            }));
    }
    Ok(stable_capture_uuid(
        &format!(
            "forgecode-nativepath-generation:{canonical_source_identity}:{provider_session_id}:{generation}"
        ),
        "source",
    ))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_events(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    context: &ProviderAdapterContext,
    import_options: &ProviderImportOptions,
    source_id: Uuid,
    session: &Session,
    row: &ForgeCodeConversationRow,
    page: &ForgeCodePage,
    summary: &mut ProviderImportSummary,
) -> Result<BTreeMap<u64, Uuid>> {
    let legacy_session =
        session.id == provider_session_uuid(CaptureProvider::ForgeCode, &row.conversation_id);
    let mut event_ids = BTreeMap::new();
    for retained in &page.events {
        let event_hash = retained
            .event
            .provider_event_hash
            .clone()
            .unwrap_or(compute_payload_hash(&retained.event.payload)?);
        let identity = provider_event_import_identity_with_exact_legacy_source(
            committed_store,
            CaptureProvider::ForgeCode,
            &row.conversation_id,
            source_id,
            retained.provider_event_index,
            retained.provider_event_index,
            &event_hash,
            None,
            Some(retained.provider_event_index),
            legacy_session,
        )?;
        let event = forgecode_core_event(
            context,
            import_options,
            &row.conversation_id,
            source_id,
            session.id,
            crate::provider::normalization::provider_line_from_index(retained.provider_event_index),
            &retained.event,
            &event_hash,
            &identity,
        )?;
        event_ids.insert(retained.provider_event_index, event.id);
        if group.reconcile_provider_event(&event, ProviderEventHashAuthority::ProviderSupplied)? {
            summary.imported_events = summary.imported_events.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        } else {
            summary.skipped_events = summary.skipped_events.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
        }
        summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
    }
    Ok(event_ids)
}

pub(super) fn publish_touches(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    import_options: &ProviderImportOptions,
    source_id: Uuid,
    session: &Session,
    page: &ForgeCodePage,
    event_ids: &BTreeMap<u64, Uuid>,
) -> Result<()> {
    let provider_session_id = session.external_session_id.as_deref().unwrap_or_default();
    let legacy_session =
        session.id == provider_session_uuid(CaptureProvider::ForgeCode, provider_session_id);
    for touch in &page.touches {
        let event_id = touch
            .provider_event_index
            .and_then(|index| event_ids.get(&index).copied());
        let touch_id = provider_file_touch_import_id(
            committed_store,
            CaptureProvider::ForgeCode,
            provider_session_id,
            source_id,
            touch.provider_event_index,
            touch.provider_touch_index,
            legacy_session,
        )?;
        group.upsert_file_touched(&forgecode_file_touched(
            touch,
            provider_session_id,
            import_options.history_record_id,
            source_id,
            session.id,
            event_id,
            touch_id,
        ))?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn forgecode_core_event(
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    provider_session_id: &str,
    source_id: Uuid,
    session_id: Uuid,
    line_number: usize,
    native: &super::super::super::event::ForgeCodeNativeEvent,
    event_hash: &str,
    identity: &ProviderEventImportIdentity,
) -> Result<Event> {
    let dedupe_key =
        Store::provider_event_dedupe_key_with_payload_hash(&identity.dedupe_key, event_hash)
            .unwrap_or_else(|| identity.dedupe_key.clone());
    let mut provider_metadata = native.metadata.clone();
    let source_record_coordinates =
        take_forgecode_source_record_coordinates(&mut provider_metadata)?;
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
        "provider_event_hash_authority": ProviderEventHashAuthority::ProviderSupplied.as_str(),
        "cursor": native.cursor,
        "source_format": FORGECODE_SQLITE_SOURCE_FORMAT,
        "source_trust": ProviderSourceTrust::ProviderNative,
        "fixture_line": line_number,
        "imported_at": context.imported_at,
        "event_idempotency_key": format!(
            "provider-event:{}:{}:{}",
            CaptureProvider::ForgeCode.as_str(),
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
            "provider": CaptureProvider::ForgeCode.as_str(),
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

fn take_forgecode_source_record_coordinates(
    metadata: &mut serde_json::Value,
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

fn forgecode_file_touched(
    touch: &super::super::super::event::ForgeCodeFileTouch,
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
                "provider": CaptureProvider::ForgeCode.as_str(),
                "provider_session_id": provider_session_id,
                "provider_touch_index": touch.provider_touch_index,
                "provider_event_index": touch.provider_event_index,
                "raw_source_path": touch.raw_source_path,
                "source_id": source_id,
                "source_format": FORGECODE_SQLITE_SOURCE_FORMAT,
                "source_root": source_root,
                "metadata": touch.metadata,
                "session_id": session_id,
            }),
        ),
    }
}
