use super::*;

pub(super) fn load_core_state(
    store: &Store,
    machine_id: &str,
    stream: &str,
) -> Result<PiCoreState> {
    let stored = store.get_sync_cursor(None, machine_id, stream)?;
    let prior = stored
        .as_ref()
        .map(|cursor| decode_core_cursor(&cursor.cursor))
        .transpose()?
        .flatten();
    Ok(PiCoreState {
        expected_store_cursor: stored,
        prior,
    })
}

pub(super) fn decode_core_cursor(encoded: &str) -> Result<Option<PiStoreCursorWire>> {
    let provider_cursor = match decode_native_path_committed_cursor(encoded) {
        Ok(committed) => committed.provider_cursor().to_owned(),
        Err(error) => {
            let resembles_native_envelope = serde_json::from_str::<Value>(encoded)
                .ok()
                .and_then(|value| value.as_object().cloned())
                .is_some_and(|object| {
                    object.contains_key("publication_id") || object.contains_key("provider_cursor")
                });
            if resembles_native_envelope {
                return Err(CaptureError::Store(error));
            }
            encoded.to_owned()
        }
    };
    if let Ok(wire) = serde_json::from_str::<PiStoreCursorWire>(&provider_cursor) {
        if wire.version != PI_STORE_CURSOR_VERSION {
            return Err(CaptureError::InvalidPayload(
                "unsupported Pi NativePath Store cursor".to_owned(),
            ));
        }
        return Ok(Some(wire));
    }
    validate_released_cursor(&provider_cursor)?;
    Ok(None)
}

pub(super) fn validate_released_cursor(encoded: &str) -> Result<()> {
    let cursor = CertifiedProviderCursor::decode_if_certified(encoded)?.ok_or_else(|| {
        CaptureError::InvalidPayload(
            "Pi cursor is neither NativePath nor a released certified cursor".to_owned(),
        )
    })?;
    if cursor.parser_revision() != PI_RELEASED_CAPTURE_REVISION
        || cursor.policy_revision() != PI_RELEASED_POLICY_REVISION
    {
        return Err(CaptureError::InvalidPayload(
            "Pi released cursor has unsupported revisions".to_owned(),
        ));
    }
    crate::released_jsonl_cursor::released_jsonl_position_offset(cursor.native_position())
        .map_err(|_| {
            CaptureError::InvalidPayload("Pi released cursor position is malformed".to_owned())
        })?;
    let checkpoint: ReleasedPiParserCheckpoint = cursor.parser_checkpoint().deserialize()?;
    validate_released_checkpoint(&checkpoint)
}

pub(super) fn validate_released_checkpoint(checkpoint: &ReleasedPiParserCheckpoint) -> Result<()> {
    if checkpoint.accepted_captures > checkpoint.next_ordinal
        || checkpoint.accepted_events > checkpoint.accepted_captures
    {
        return Err(CaptureError::InvalidPayload(
            "Pi released cursor checkpoint counters are inconsistent".to_owned(),
        ));
    }
    if let Some(header) = &checkpoint.header {
        if header.id.trim().is_empty() {
            return Err(CaptureError::InvalidPayload(
                "Pi released cursor session identity is empty".to_owned(),
            ));
        }
        let _ = (
            header.version,
            header.timestamp,
            &header.cwd,
            &header.parent_session,
        );
    }
    let _ = checkpoint.accepted_file_touches;
    Ok(())
}

pub(super) fn checkpoint_covers(
    committed: &PiNativeCheckpoint,
    candidate: &PiNativeCheckpoint,
) -> bool {
    committed.revisions_match()
        && candidate.revisions_match()
        && committed.route_sha256 == candidate.route_sha256
        && committed.physical_file_id == candidate.physical_file_id
        && committed.complete_offset >= candidate.complete_offset
        && (committed.complete_offset != candidate.complete_offset
            || committed.committed_prefix_sha256 == candidate.committed_prefix_sha256)
}

pub(super) struct PiPathDiscovery {
    pub(super) paths: Vec<PathBuf>,
    pub(super) root_missing: bool,
    pub(super) discovery: Option<super::PiDiscovery>,
}

pub(super) fn discover_paths(path: &Path) -> Result<PiPathDiscovery> {
    match discover_pi_sessions(path) {
        Ok(discovery) => Ok(PiPathDiscovery {
            paths: discovery.sessions.clone(),
            root_missing: false,
            discovery: Some(discovery),
        }),
        Err(PiNativePathError::Io { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            Ok(PiPathDiscovery {
                paths: Vec::new(),
                root_missing: true,
                discovery: None,
            })
        }
        Err(error) => Err(map_native_error(error)),
    }
}

pub(crate) fn source_cursor_stream(path: &Path) -> Result<String> {
    let identity = provider_path_identity(path)?;
    Ok(provider_source_cursor_stream_for_path(
        CaptureProvider::Pi,
        PI_SOURCE_FORMAT,
        &identity,
    ))
}

pub(super) fn output_source_identity(
    path: &Path,
    cursor_stream: &str,
) -> Result<crate::OutputSourceIdentity> {
    let canonical = std::path::absolute(path)?;
    Ok(crate::OutputSourceIdentity {
        provider: CaptureProvider::Pi.as_str().to_owned(),
        namespace_id: cursor_stream.to_owned(),
        source_id: format!("pi-jsonl-file:{}", provider_path_identity(&canonical)?),
    })
}

pub(super) fn root_stream(path: &Path) -> Result<String> {
    let identity = provider_path_identity(path)?;
    Ok(provider_source_cursor_stream_for_path(
        CaptureProvider::Pi,
        PI_ROOT_CURSOR_FORMAT,
        &identity,
    ))
}

pub(super) fn load_root_manifest(
    store: &Store,
    machine_id: &str,
    configured_root: &Path,
) -> Result<Option<PiRootManifest>> {
    let stream = root_stream(configured_root)?;
    let Some(stored) = store.get_sync_cursor(None, machine_id, &stream)? else {
        return Ok(None);
    };
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    let manifest: PiRootManifest = serde_json::from_str(committed.provider_cursor())?;
    if manifest.version != PI_ROOT_MANIFEST_VERSION || manifest.configured_root != configured_root {
        return Err(CaptureError::InvalidPayload(
            "Pi NativePath root manifest is inconsistent".to_owned(),
        ));
    }
    Ok(Some(manifest))
}

pub(super) fn root_entry_from_store(
    committed_store: &Store,
    live_store: &Store,
    machine_id: &str,
    path: &Path,
) -> Result<PiRootEntryState> {
    let cursor_stream = source_cursor_stream(path)?;
    let state = load_core_state(live_store, machine_id, &cursor_stream)?;
    let prior = state.prior;
    let locator_identity = provider_path_identity(path)?;
    let _ = committed_store;
    Ok(PiRootEntryState {
        source_id: prior.as_ref().and_then(|prior| prior.source_id),
        entry: PiRootEntry {
            path: path.to_path_buf(),
            locator_identity,
            cursor_stream,
            canonical_source_identity: prior
                .as_ref()
                .and_then(|prior| prior.canonical_source_identity.clone()),
            source_revision: prior
                .as_ref()
                .map_or_else(String::new, |prior| prior.source_revision.clone()),
        },
    })
}

pub(super) fn root_entry_was_superseded(
    store: &Store,
    machine_id: &str,
    prior_entry: &PiRootEntry,
    current_entries: &[PiRootEntryState],
    relocated_source_ids: &BTreeSet<Uuid>,
) -> Result<bool> {
    let prior = load_core_state(store, machine_id, &prior_entry.cursor_stream)?.prior;
    let Some(prior) = prior else {
        return Ok(false);
    };
    let Some(source_id) = prior.source_id else {
        return Ok(false);
    };
    Ok(current_entries.iter().any(|current| {
        current.source_id == Some(source_id)
            && current.entry.locator_identity != prior_entry.locator_identity
            && (relocated_source_ids.contains(&source_id)
                || current.entry.source_revision == prior.source_revision)
    }))
}

pub(super) fn publish_root_manifest(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    options: &PiSessionImportOptions,
    configured_root: &Path,
    source_root: &str,
    mut entries: Vec<PiRootEntry>,
) -> Result<ProviderImportSummary> {
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    let manifest = PiRootManifest {
        version: PI_ROOT_MANIFEST_VERSION,
        configured_root: configured_root.to_path_buf(),
        source_root: source_root.to_owned(),
        entries,
    };
    let encoded = serde_json::to_string(&manifest)?;
    if encoded.len() > ctx_history_store::NATIVE_PATH_MAX_RETAINED_PAGE_BYTES {
        return Err(CaptureError::InvalidPayload(
            "Pi NativePath root manifest exceeds the Store bound".to_owned(),
        ));
    }
    let stream = root_stream(configured_root)?;
    let stored = store.get_sync_cursor(None, &options.machine_id, &stream)?;
    if let Some(stored) = &stored {
        if let Ok(committed) = decode_native_path_committed_cursor(&stored.cursor) {
            if committed.provider_cursor() == encoded {
                let mut summary = ProviderImportSummary::default();
                summary.set_work_result(ProviderImportWorkResult::NoOp);
                return Ok(summary);
            }
        }
    }
    let next = SyncCursor {
        id: Uuid::new_v4(),
        team_id: None,
        device_id: options.machine_id.clone(),
        stream: stream.clone(),
        cursor: encoded,
        last_synced_at: Some(options.imported_at),
        timestamps: timestamps(options.imported_at),
    };
    let transition =
        NativePathCursorTransition::new(stored.as_ref().map(|cursor| cursor.cursor.clone()), next);
    let publication_id = root_publication_id(&manifest, &transition);
    let accounting = NativePathGroupAccounting::new(1, 1, transition.next().cursor.len())?;
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
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    let mut summary = ProviderImportSummary::default();
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

pub(super) fn retire_source_route(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    options: &PiSessionImportOptions,
    entry: &PiRootEntry,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<ProviderImportSummary> {
    let Some(canonical_source_identity) = entry.canonical_source_identity.as_ref() else {
        return Ok(ProviderImportSummary::default());
    };
    let stored = store
        .get_sync_cursor(None, &options.machine_id, &entry.cursor_stream)?
        .ok_or(CaptureError::SystemInvariant(
            "Pi route retirement lost its Core cursor",
        ))?;
    let retirement = ProviderSourceRouteRetirement {
        provider: CaptureProvider::Pi,
        source_format: PI_SOURCE_FORMAT.to_owned(),
        machine_id: options.machine_id.clone(),
        locator_identity: entry.locator_identity.clone(),
        cursor_stream: entry.cursor_stream.clone(),
        expected_canonical_source_identity: canonical_source_identity.clone(),
        expected_source_revision: entry.source_revision.clone(),
        retired_at_ms: options.imported_at.timestamp_millis(),
        reason,
    };
    let publication_id = retirement_publication_id(&retirement);
    if decode_native_path_committed_cursor(&stored.cursor)
        .is_ok_and(|committed| committed.publication_id() == publication_id)
    {
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }
    let provider_cursor = decode_native_path_committed_cursor(&stored.cursor)?
        .provider_cursor()
        .to_owned();
    let next = SyncCursor {
        id: Uuid::new_v4(),
        team_id: None,
        device_id: options.machine_id.clone(),
        stream: entry.cursor_stream.clone(),
        cursor: provider_cursor,
        last_synced_at: Some(options.imported_at),
        timestamps: timestamps(options.imported_at),
    };
    let transition = NativePathCursorTransition::new(Some(stored.cursor), next);
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store
        .begin_native_path_publication_group(admission, NativePathGroupAccounting::new(0, 1, 0)?)?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }
    let disposition = group.retire_provider_source_route(&retirement)?;
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    let mut summary = ProviderImportSummary::default();
    summary.set_work_result(match disposition {
        ProviderSourceRouteRetirementDisposition::Retired => ProviderImportWorkResult::Changed,
        ProviderSourceRouteRetirementDisposition::AlreadyRetired => ProviderImportWorkResult::NoOp,
    });
    Ok(summary)
}

pub(super) fn publication_id(
    path: &Path,
    page: &crate::provider::native_ingestion::NativeIngestionPage<PiNativeCorePage>,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-pi-nativepath-publication-v1\0");
    digest.update(path.as_os_str().as_encoded_bytes());
    digest.update(page.expected_frontier.version.to_le_bytes());
    digest.update(&page.expected_frontier.bytes);
    digest.update(page.next_safe_frontier.version.to_le_bytes());
    digest.update(&page.next_safe_frontier.bytes);
    digest.update(transition.next().cursor.as_bytes());
    format!("{PI_PUBLICATION_PREFIX}{:x}", digest.finalize())
}

pub(super) fn root_publication_id(
    manifest: &PiRootManifest,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-pi-nativepath-root-publication-v1\0");
    digest.update(manifest.configured_root.as_os_str().as_encoded_bytes());
    digest.update(transition.next().cursor.as_bytes());
    format!("{PI_ROOT_PUBLICATION_PREFIX}{:x}", digest.finalize())
}

pub(super) fn retirement_publication_id(retirement: &ProviderSourceRouteRetirement) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-pi-nativepath-retirement-v1\0");
    digest.update(retirement.locator_identity.as_bytes());
    digest.update(retirement.cursor_stream.as_bytes());
    digest.update(retirement.expected_canonical_source_identity.as_bytes());
    digest.update(retirement.expected_source_revision.as_bytes());
    format!("{PI_RETIREMENT_PUBLICATION_PREFIX}{:x}", digest.finalize())
}

pub(crate) fn map_native_error(error: PiNativePathError) -> CaptureError {
    match error {
        PiNativePathError::Io { source, .. } => CaptureError::Io(source),
        PiNativePathError::SourceChanged => CaptureError::SourceChangedDuringCapture,
        PiNativePathError::Encoding(error) => CaptureError::Json(error),
        PiNativePathError::Normalization(error) => error,
        error => CaptureError::InvalidPayload(error.to_string()),
    }
}
