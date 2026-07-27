use super::*;

pub(super) fn publish_manifest(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    stream: &str,
    initial_stored: Option<&SyncCursor>,
    manifest: &RovoDevRootManifest,
) -> Result<ProviderImportSummary> {
    let stored = store.get_sync_cursor(None, &context.machine_id, stream)?;
    if initial_stored.is_some() && stored.is_none() {
        return Err(CaptureError::InvalidPayload(
            "RovoDev root cursor disappeared during import".to_owned(),
        ));
    }
    let encoded = serde_json::to_string(manifest)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    if let Some(stored) = stored.as_ref() {
        let committed = decode_native_path_committed_cursor(&stored.cursor)?;
        if committed.provider_cursor() == encoded {
            let mut summary = ProviderImportSummary::default();
            summary.set_work_result(ProviderImportWorkResult::NoOp);
            return Ok(summary);
        }
    }
    let transition = NativePathCursorTransition::new(
        stored.as_ref().map(|cursor| cursor.cursor.clone()),
        sync_cursor(context, stream, encoded, CaptureProvider::RovoDev),
    );
    let publication_id = root_publication_id(manifest, &transition);
    let accounting = NativePathGroupAccounting::new(1, 1, transition.next().cursor.len())?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllExpected
    ) {
        group.prepare_journal_checkpoint()?;
        group.publish_cursor_set()?;
    }
    group.commit()?;
    let mut summary = ProviderImportSummary::default();
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

pub(super) fn load_manifest(
    stored: Option<&SyncCursor>,
    root_identity: &str,
) -> Result<RovoDevRootManifest> {
    let Some(stored) = stored else {
        return Ok(RovoDevRootManifest {
            version: ROVODEV_NATIVE_CURSOR_VERSION,
            root_identity: root_identity.to_owned(),
            sources: Vec::new(),
        });
    };
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    let manifest: RovoDevRootManifest = serde_json::from_str(committed.provider_cursor())
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    if manifest.version != ROVODEV_NATIVE_CURSOR_VERSION
        || manifest.root_identity != root_identity
        || manifest
            .sources
            .windows(2)
            .any(|sources| sources[0].source_identity >= sources[1].source_identity)
    {
        return Err(CaptureError::InvalidPayload(
            "RovoDev NativePath root manifest is inconsistent".to_owned(),
        ));
    }
    Ok(manifest)
}

pub(super) fn manifest_entry(
    store: &Store,
    source: &RovoDevSessionSource,
    cursor: &RovoDevNativeCursor,
) -> Result<RovoDevManifestEntry> {
    let canonical_source_identity = cursor
        .source_id
        .map(|source_id| store.get_capture_source(source_id))
        .transpose()?
        .and_then(|source| source.descriptor.source_identity);
    manifest_entry_with_canonical(source, cursor, canonical_source_identity)
}

pub(super) fn manifest_entry_with_canonical(
    source: &RovoDevSessionSource,
    cursor: &RovoDevNativeCursor,
    canonical_source_identity: Option<String>,
) -> Result<RovoDevManifestEntry> {
    let canonical = fs::canonicalize(&source.context_path)?;
    let path_identity = provider_path_identity(&canonical)?;
    Ok(RovoDevManifestEntry {
        source_identity: cursor.source_identity.clone(),
        cursor_stream: provider_source_cursor_stream_for_path(
            CaptureProvider::RovoDev,
            ROVODEV_SOURCE_FORMAT,
            &path_identity,
        ),
        locator_identity: cursor.locator_identity.clone(),
        canonical_source_identity,
        source_revision: cursor.source_revision.clone(),
    })
}

pub(super) fn manifest_with_entry(
    manifest: &RovoDevRootManifest,
    entry: RovoDevManifestEntry,
) -> RovoDevRootManifest {
    let mut next = manifest.clone();
    match next
        .sources
        .iter_mut()
        .find(|prior| prior.source_identity == entry.source_identity)
    {
        Some(prior) => *prior = entry,
        None => next.sources.push(entry),
    }
    next.sources
        .sort_by(|left, right| left.source_identity.cmp(&right.source_identity));
    next
}

pub(super) fn manifest_transition(
    store: &Store,
    context: &ProviderAdapterContext,
    stream: &str,
    expected_manifest: &RovoDevRootManifest,
    next_manifest: &RovoDevRootManifest,
) -> Result<Option<NativePathCursorTransition>> {
    let expected_encoded = serde_json::to_string(expected_manifest)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let next_encoded = serde_json::to_string(next_manifest)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    if expected_encoded == next_encoded {
        return Ok(None);
    }
    let stored = store.get_sync_cursor(None, &context.machine_id, stream)?;
    match stored.as_ref() {
        Some(stored) => {
            let committed = decode_native_path_committed_cursor(&stored.cursor)?;
            if committed.provider_cursor() != expected_encoded {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
        }
        None if !expected_manifest.sources.is_empty() => {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        None => {}
    }
    Ok(Some(NativePathCursorTransition::new(
        stored.map(|cursor| cursor.cursor),
        sync_cursor(context, stream, next_encoded, CaptureProvider::RovoDev),
    )))
}
