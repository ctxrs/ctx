#[allow(unused_imports)]
use super::*;

pub(crate) fn import_provider_file_touched_line(
    store: &mut Store,
    file: &ProviderFileTouchedEnvelope,
    options: &NormalizedProviderImportOptions,
) -> Result<()> {
    let session_id = provider_session_uuid(file.provider, &file.provider_session_id);
    let source_id = provider_scoped_source_uuid(
        file.provider,
        &file.provider_session_id,
        &file.source_format,
        file.raw_source_path.as_deref(),
    );
    let event_id = match file.provider_event_index {
        Some(index) => provider_file_touch_event_id(
            store,
            file.provider,
            &file.provider_session_id,
            source_id,
            index,
        )?,
        None => None,
    };
    let touch_id = provider_file_touch_import_id(
        store,
        file.provider,
        &file.provider_session_id,
        source_id,
        file.provider_touch_index,
    )?;
    let touched = FileTouched {
        id: touch_id,
        history_record_id: options.history_record_id,
        run_id: None,
        event_id,
        vcs_workspace_id: None,
        path: file.path.clone(),
        change_kind: file.change_kind,
        old_path: file.old_path.clone(),
        line_count_delta: file.line_count_delta,
        confidence: file.confidence,
        timestamps: timestamps(file.occurred_at),
        source_id: Some(source_id),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider": file.provider.as_str(),
                "provider_session_id": file.provider_session_id,
                "provider_touch_index": file.provider_touch_index,
                "provider_event_index": file.provider_event_index,
                "raw_source_path": file.raw_source_path,
                "source_id": source_id,
                "source_format": file.source_format,
                "metadata": file.metadata,
                "session_id": session_id,
            }),
        ),
    };
    store.upsert_file_touched(&touched)?;
    Ok(())
}

#[derive(Clone)]
pub(crate) struct PendingProviderEdge {
    pub(crate) provider_session_id: String,
    pub(crate) parent_provider_session_id: Option<String>,
    pub(crate) session_id: Uuid,
    pub(crate) parent_session_id: Uuid,
    pub(crate) root_session_id: Option<Uuid>,
    pub(crate) source_id: Uuid,
    pub(crate) source_format: String,
    pub(crate) imported_at: DateTime<Utc>,
    pub(crate) fidelity: Fidelity,
    pub(crate) line_number: usize,
}

pub(crate) fn resolve_pending_root_session_id(
    store: &Store,
    edge: &PendingProviderEdge,
    caches: &mut ProviderImportCaches,
) -> Result<Option<Uuid>> {
    match edge.root_session_id {
        Some(root_id)
            if root_id == edge.session_id
                || provider_session_exists_cached(store, root_id, &mut caches.session_exists)? =>
        {
            Ok(Some(root_id))
        }
        Some(_) | None => Ok(Some(edge.parent_session_id)),
    }
}

pub(crate) fn update_session_parent_if_needed(
    store: &mut Store,
    edge: &PendingProviderEdge,
    caches: &mut ProviderImportCaches,
) -> Result<()> {
    let root_session_id = resolve_pending_root_session_id(store, edge, caches)?;
    update_session_parent(store, edge, root_session_id)
}

pub(crate) fn update_session_parent(
    store: &mut Store,
    edge: &PendingProviderEdge,
    root_session_id: Option<Uuid>,
) -> Result<()> {
    let mut session = store.get_session(edge.session_id)?;
    if session.parent_session_id == Some(edge.parent_session_id)
        && session.root_session_id == root_session_id
    {
        return Ok(());
    }
    session.parent_session_id = Some(edge.parent_session_id);
    session.root_session_id = root_session_id;
    session.timestamps.updated_at = edge.imported_at;
    store.upsert_session(&session)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn provider_event_import_identity(
    store: &Store,
    provider: CaptureProvider,
    provider_session_id: &str,
    source_id: Uuid,
    provider_event_index: u64,
    provider_event_sequence_index: u64,
    event_hash: &str,
    legacy_provider_event_index: Option<u64>,
) -> Result<ProviderEventImportIdentity> {
    let source_identity = provider_source_event_import_identity_with_seq(
        source_id,
        provider_event_index,
        provider_event_sequence_index,
        event_hash,
    );
    let source_identity = avoid_provider_source_event_seq_collision(
        store,
        source_identity,
        source_id,
        provider_event_index,
        provider_event_sequence_index,
    )?;
    if provider_event_exists(store, &source_identity.dedupe_key)?
        || provider_event_id_exists(store, source_identity.id)?
    {
        return Ok(source_identity);
    }

    if let Some(legacy_index) = legacy_provider_event_index {
        let legacy_source_identity =
            provider_source_event_import_identity(source_id, legacy_index, event_hash);
        if provider_event_exists(store, &legacy_source_identity.dedupe_key)?
            || provider_event_id_exists(store, legacy_source_identity.id)?
        {
            return Ok(legacy_source_identity);
        }

        let legacy_provider_identity = provider_legacy_event_import_identity(
            provider,
            provider_session_id,
            legacy_index,
            event_hash,
        );
        if provider_event_exists(store, &legacy_provider_identity.dedupe_key)?
            || provider_event_id_exists(store, legacy_provider_identity.id)?
        {
            return Ok(legacy_provider_identity);
        }
    }

    let legacy_identity = provider_legacy_event_import_identity(
        provider,
        provider_session_id,
        provider_event_index,
        event_hash,
    );
    if provider_event_exists(store, &legacy_identity.dedupe_key)?
        || provider_event_id_exists(store, legacy_identity.id)?
    {
        Ok(legacy_identity)
    } else {
        Ok(source_identity)
    }
}

pub(crate) fn provider_legacy_event_import_identity(
    provider: CaptureProvider,
    provider_session_id: &str,
    provider_event_index: u64,
    event_hash: &str,
) -> ProviderEventImportIdentity {
    ProviderEventImportIdentity {
        id: provider_event_uuid(provider, provider_session_id, provider_event_index),
        seq: provider_event_seq(provider, provider_session_id, provider_event_index),
        dedupe_key: Store::provider_event_dedupe_key(
            provider,
            provider_session_id,
            provider_event_index,
            event_hash,
        ),
        run_source_id: None,
    }
}

pub(crate) fn provider_session_exists(store: &Store, session_id: Uuid) -> Result<bool> {
    match store.get_session(session_id) {
        Ok(_) => Ok(true),
        Err(StoreError::NotFound(_)) => Ok(false),
        Err(err) => Err(CaptureError::Store(err)),
    }
}

pub(crate) fn provider_session_exists_cached(
    store: &Store,
    session_id: Uuid,
    cache: &mut BTreeMap<Uuid, bool>,
) -> Result<bool> {
    if let Some(exists) = cache.get(&session_id) {
        return Ok(*exists);
    }
    let exists = provider_session_exists(store, session_id)?;
    cache.insert(session_id, exists);
    Ok(exists)
}

pub(crate) struct ProviderCommandRunInput<'a> {
    pub(crate) provider: CaptureProvider,
    pub(crate) provider_session_id: &'a str,
    pub(crate) session_id: Uuid,
    pub(crate) source_id: Uuid,
    pub(crate) run_source_id: Option<Uuid>,
    pub(crate) history_record_id: Option<Uuid>,
    pub(crate) event: &'a ProviderEventEnvelope,
    pub(crate) payload: &'a Value,
    pub(crate) event_hash: &'a str,
}

pub(crate) fn provider_command_run_from_event(
    input: ProviderCommandRunInput<'_>,
) -> Result<Option<Run>> {
    let ProviderCommandRunInput {
        provider,
        provider_session_id,
        session_id,
        source_id,
        run_source_id,
        history_record_id,
        event,
        payload,
        event_hash,
    } = input;
    if event.event_type != EventType::CommandOutput {
        return Ok(None);
    }
    let command_preview = payload
        .get("command")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned);
    let call_id = payload.get("call_id").and_then(Value::as_str);
    let key = call_id.unwrap_or(event_hash);
    let duration_ms = provider_command_duration_ms(payload)?;
    let ended_at = Some(event.occurred_at);
    let started_at = match duration_ms {
        Some(duration) => {
            let duration_value = duration;
            let duration = chrono::Duration::try_milliseconds(duration_value).ok_or_else(|| {
                CaptureError::InvalidPayload(format!(
                    "duration_ms is not representable as milliseconds: {duration_value}"
                ))
            })?;
            event
                .occurred_at
                .checked_sub_signed(duration)
                .ok_or_else(|| {
                    CaptureError::InvalidPayload(format!(
                        "duration_ms moves command start before representable time: {}",
                        duration_value
                    ))
                })?
        }
        None => event.occurred_at,
    };
    Ok(Some(Run {
        id: run_source_id
            .map(|source_id| provider_source_run_uuid(source_id, key))
            .unwrap_or_else(|| provider_run_uuid(provider, provider_session_id, key)),
        history_record_id,
        session_id: Some(session_id),
        run_type: RunType::Command,
        status: provider_command_run_status(payload),
        started_at,
        ended_at,
        exit_code: payload
            .get("exit_code")
            .and_then(Value::as_i64)
            .and_then(|value| i32::try_from(value).ok()),
        cwd: None,
        command_preview,
        input_blob_id: None,
        output_blob_id: None,
        timestamps: timestamps(event.occurred_at),
        source_id: Some(source_id),
        sync: provider_sync_metadata(
            event.fidelity,
            json!({
                "provider_session_id": provider_session_id,
                "provider_event_index": event.provider_event_index,
                "provider_event_hash": event_hash,
                "call_id": call_id,
                "source": "provider_command_output",
            }),
        ),
    }))
}

pub(crate) fn provider_session_uuid(provider: CaptureProvider, provider_session_id: &str) -> Uuid {
    stable_capture_uuid(
        &format!("provider:{}:{provider_session_id}", provider.as_str()),
        "session",
    )
}

pub(crate) fn provider_run_uuid(
    provider: CaptureProvider,
    provider_session_id: &str,
    run_key: &str,
) -> Uuid {
    stable_capture_uuid(
        &format!(
            "provider:{}:{provider_session_id}:run:{run_key}",
            provider.as_str()
        ),
        "run",
    )
}

pub(crate) fn provider_event_uuid(
    provider: CaptureProvider,
    provider_session_id: &str,
    provider_event_index: u64,
) -> Uuid {
    stable_capture_uuid(
        &format!(
            "provider:{}:{provider_session_id}:{provider_event_index}",
            provider.as_str()
        ),
        "event",
    )
}

pub(crate) fn provider_event_seq(
    provider: CaptureProvider,
    provider_session_id: &str,
    provider_event_index: u64,
) -> u64 {
    let session_key = format!("provider:{}:{provider_session_id}", provider.as_str());
    ((fnv1a64(session_key.as_bytes()) & 0x0000_07ff_ffff_ffff) << 20)
        | (provider_event_index & 0x000f_ffff)
}

pub(crate) fn provider_file_touch_uuid(
    provider: CaptureProvider,
    provider_session_id: &str,
    provider_touch_index: u64,
) -> Uuid {
    stable_capture_uuid(
        &format!(
            "provider:{}:{provider_session_id}:file-touch:{provider_touch_index}",
            provider.as_str()
        ),
        "file-touch",
    )
}

pub(crate) fn provider_edge_uuid(
    provider: CaptureProvider,
    provider_session_id: &str,
    edge_kind: &str,
) -> Uuid {
    stable_capture_uuid(
        &format!(
            "provider:{}:{provider_session_id}:{edge_kind}",
            provider.as_str()
        ),
        "session-edge",
    )
}
