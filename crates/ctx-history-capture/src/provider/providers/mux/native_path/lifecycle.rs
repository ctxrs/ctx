use super::*;

pub(super) fn root_cursor_stream(configured_root: &Path) -> String {
    provider_source_cursor_stream_for_path(
        CaptureProvider::Mux,
        MUX_SOURCE_FORMAT,
        &format!("mux-nativepath-root:{}", configured_root.display()),
    )
}

pub(super) fn load_root_manifest(
    store: &Store,
    machine_id: &str,
    configured_root: &Path,
) -> Result<Option<MuxRootManifest>> {
    let stream = root_cursor_stream(configured_root);
    let Some(stored) = store.get_sync_cursor(None, machine_id, &stream)? else {
        return Ok(None);
    };
    let committed = decode_native_path_committed_cursor(&stored.cursor).map_err(|_| {
        CaptureError::InvalidPayload("Mux NativePath root cursor is corrupt".to_owned())
    })?;
    let manifest: MuxRootManifest = serde_json::from_str(committed.provider_cursor())
        .map_err(|_| CaptureError::InvalidPayload("Mux root manifest is corrupt".to_owned()))?;
    if manifest.version != MUX_ROOT_MANIFEST_VERSION || manifest.configured_root != configured_root
    {
        return Err(CaptureError::InvalidPayload(
            "Mux root manifest identity is inconsistent".to_owned(),
        ));
    }
    Ok(Some(manifest))
}

pub(super) fn publish_root_manifest(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    manifest: MuxRootManifest,
) -> Result<ProviderImportSummary> {
    let stream = root_cursor_stream(&manifest.configured_root);
    let stored = store.get_sync_cursor(None, &context.machine_id, &stream)?;
    let encoded = serde_json::to_string(&manifest)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    if let Some(stored) = stored.as_ref() {
        if let Ok(committed) = decode_native_path_committed_cursor(&stored.cursor) {
            if committed.provider_cursor() == encoded {
                let mut summary = ProviderImportSummary::default();
                summary.set_work_result(ProviderImportWorkResult::NoOp);
                return Ok(summary);
            }
        }
    }
    let next = SyncCursor {
        id: stable_capture_uuid(
            &format!(
                "provider-cursor:{}:{}:{}",
                CaptureProvider::Mux.as_str(),
                context.machine_id,
                stream
            ),
            "provider-sync-cursor",
        ),
        team_id: None,
        device_id: context.machine_id.clone(),
        stream: stream.clone(),
        cursor: encoded,
        last_synced_at: Some(context.imported_at),
        timestamps: timestamps(context.imported_at),
    };
    let transition =
        NativePathCursorTransition::new(stored.as_ref().map(|cursor| cursor.cursor.clone()), next);
    let accounting =
        NativePathGroupAccounting::new(1, 1, transition.next().cursor.len().saturating_add(256))?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    let publication_id = manifest_publication_id(&manifest);
    let already = matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    );
    if !already {
        group.prepare_journal_checkpoint()?;
        group.publish_cursor_set()?;
    }
    group.commit()?;
    let mut summary = ProviderImportSummary::default();
    summary.set_work_result(if already {
        ProviderImportWorkResult::NoOp
    } else {
        ProviderImportWorkResult::Changed
    });
    Ok(summary)
}

pub(super) fn manifest_publication_id(manifest: &MuxRootManifest) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx/mux-nativepath/root-manifest/v1\0");
    digest.update(serde_json::to_vec(manifest).unwrap_or_default());
    format!(
        "{MUX_PUBLICATION_PREFIX}manifest:{}",
        hex(&digest.finalize())
    )
}

pub(super) fn retire_missing_sources(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    prior: &MuxRootManifest,
    current: &[MuxManifestSource],
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let live = current
        .iter()
        .map(|source| (source.path.clone(), source.kind))
        .collect::<BTreeSet<_>>();
    for missing in prior
        .sources
        .iter()
        .filter(|source| !live.contains(&(source.path.clone(), source.kind)))
    {
        summary.merge_from(retire_missing_source(store, bulk_guard, context, missing)?);
    }
    Ok(())
}

pub(super) fn retire_missing_source(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    missing: &MuxManifestSource,
) -> Result<ProviderImportSummary> {
    let stored = store
        .get_sync_cursor(None, &context.machine_id, &missing.cursor_stream)?
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Mux manifest source is missing its committed cursor".to_owned(),
            )
        })?;
    let committed = decode_native_path_committed_cursor(&stored.cursor).map_err(|_| {
        CaptureError::InvalidPayload("Mux route retirement requires a NativePath cursor".to_owned())
    })?;
    let prior: MuxCursorWire = serde_json::from_str(committed.provider_cursor())
        .map_err(|_| CaptureError::InvalidPayload("Mux NativePath cursor is corrupt".to_owned()))?;
    if prior.retired {
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }
    let next_wire = MuxCursorWire {
        version: MUX_CURSOR_VERSION,
        capture_revision: MUX_CAPTURE_REVISION,
        policy_revision: MUX_POLICY_REVISION,
        kind: missing.kind,
        canonical_path: missing.path.clone(),
        source_revision: format!("retired:{}", missing.source_revision),
        metadata_revision: prior.metadata_revision.clone(),
        generation: prior
            .generation
            .checked_add(1)
            .ok_or(CaptureError::InvalidPayload(
                "Mux NativePath source generation is exhausted".to_owned(),
            ))?,
        frontier: prior.frontier.clone(),
        terminal: true,
        retired: true,
        accepted_events: prior.accepted_events,
        rejected_records: prior.rejected_records,
        first_failure: prior.first_failure.clone(),
    };
    let next = mux_sync_cursor(context, &missing.cursor_stream, &next_wire)?;
    let transition = NativePathCursorTransition::new(Some(stored.cursor.clone()), next);
    let retirement = ProviderSourceRouteRetirement {
        provider: CaptureProvider::Mux,
        source_format: MUX_SOURCE_FORMAT.to_owned(),
        machine_id: context.machine_id.clone(),
        locator_identity: missing.locator_identity.clone(),
        cursor_stream: missing.cursor_stream.clone(),
        expected_canonical_source_identity: missing.canonical_source_identity.clone(),
        expected_source_revision: missing.source_revision.clone(),
        retired_at_ms: context.imported_at.timestamp_millis(),
        reason: if context
            .source_path
            .as_ref()
            .is_some_and(|root| !root.exists())
        {
            ProviderSourceRouteRetirementReason::RootMissing
        } else {
            ProviderSourceRouteRetirementReason::SourceMissing
        },
    };
    let accounting = NativePathGroupAccounting::new(1, 1, 1024)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    let publication_id = retirement_publication_id(missing, &next_wire);
    let classification =
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?;
    let mut changed = false;
    if matches!(
        classification,
        NativePathCursorSetClassification::AllExpected
    ) {
        changed = matches!(
            group.retire_provider_source_route(&retirement)?,
            ctx_history_store::ProviderSourceRouteRetirementDisposition::Retired
        );
        group.prepare_journal_checkpoint()?;
        group.publish_cursor_set()?;
    }
    group.commit()?;
    let mut summary = ProviderImportSummary::default();
    summary.set_work_result(if changed {
        ProviderImportWorkResult::Changed
    } else {
        ProviderImportWorkResult::NoOp
    });
    Ok(summary)
}

pub(super) fn retirement_publication_id(
    source: &MuxManifestSource,
    wire: &MuxCursorWire,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx/mux-nativepath/retire/v1\0");
    digest.update(source.cursor_stream.as_bytes());
    digest.update(source.canonical_source_identity.as_bytes());
    digest.update(wire.generation.to_le_bytes());
    format!("{MUX_PUBLICATION_PREFIX}retire:{}", hex(&digest.finalize()))
}

pub(super) fn ensure_active_journal(store: &Store) -> Result<()> {
    match store.projection_journal_snapshot(None) {
        Ok(_) => Ok(()),
        Err(ctx_history_store::StoreError::ProjectionJournalInactive) => {
            store.activate_projection_journal(ctx_pro_host_protocol::PROTOCOL_FINGERPRINT)?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

pub(super) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        encoded.push(char::from(DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    encoded
}
