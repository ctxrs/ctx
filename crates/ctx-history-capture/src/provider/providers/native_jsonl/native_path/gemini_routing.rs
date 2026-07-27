use super::*;

pub(super) fn revalidate_gemini_source(pages: &[GeminiPendingPage], path: &Path) -> Result<()> {
    let expected = &pages
        .iter()
        .find(|pending| pending.source.path == path)
        .ok_or(CaptureError::SystemInvariant(
            "Gemini revalidation path has no pending source",
        ))?
        .source
        .observation;
    if &GeminiFileObservation::read(path)? != expected {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    Ok(())
}

pub(super) fn gemini_source_revision(observation: &GeminiFileObservation) -> String {
    let (side, seconds, nanos) = match observation.modified.duration_since(UNIX_EPOCH) {
        Ok(duration) => ('+', duration.as_secs(), duration.subsec_nanos()),
        Err(error) => {
            let duration = error.duration();
            ('-', duration.as_secs(), duration.subsec_nanos())
        }
    };
    format!(
        "gemini-nativepath-metadata-v1:length={};modified={side}{seconds}.{nanos:09};readonly={};device={};inode={}",
        observation.length,
        observation.readonly,
        observation
            .device
            .map_or_else(|| "none".to_owned(), |value| value.to_string()),
        observation
            .inode
            .map_or_else(|| "none".to_owned(), |value| value.to_string()),
    )
}

pub(super) fn encode_gemini_cursor(checkpoint: &GeminiCheckpoint) -> Result<String> {
    Ok(serde_json::to_string(&GeminiCursorWire {
        version: GEMINI_CURSOR_VERSION,
        kind: "gemini-nativepath".to_owned(),
        checkpoint: checkpoint.clone(),
    })?)
}

pub(super) fn decode_gemini_cursor(encoded_store_cursor: &str) -> Result<Option<GeminiCheckpoint>> {
    let encoded = ctx_history_store::decode_native_path_committed_cursor(encoded_store_cursor)
        .map(|cursor| cursor.provider_cursor().to_owned())
        .unwrap_or_else(|_| encoded_store_cursor.to_owned());
    let Ok(wire) = serde_json::from_str::<GeminiCursorWire>(&encoded) else {
        // Non-NativePath cursors reset into one authoritative NativePath scan.
        // The resulting commit emits only the current cursor format.
        return Ok(None);
    };
    Ok((wire.version == GEMINI_CURSOR_VERSION
        && wire.kind == "gemini-nativepath"
        && wire.checkpoint.parser_revision == GEMINI_NATIVEPATH_PARSER_REVISION
        && wire.checkpoint.policy_revision == GEMINI_NATIVEPATH_POLICY_REVISION)
        .then_some(wire.checkpoint))
}

pub(super) fn known_gemini_routes(
    store: &Store,
    machine_id: &str,
    source_root: &Path,
) -> Result<Vec<GeminiKnownRoute>> {
    let source_root = source_root.display().to_string();
    let mut routes = BTreeMap::<String, GeminiKnownRoute>::new();
    for source in store.list_capture_sources()? {
        if source.descriptor.provider != CaptureProvider::Gemini
            || source.descriptor.machine_id != machine_id
            || source.descriptor.source_format.as_deref() != Some(GEMINI_CLI_SOURCE_FORMAT)
            || source.descriptor.source_root.as_deref() != Some(source_root.as_str())
        {
            continue;
        }
        let (Some(raw_source_path), Some(canonical_source_identity)) = (
            source.descriptor.raw_source_path.as_deref(),
            source.descriptor.source_identity.as_deref(),
        ) else {
            continue;
        };
        let path = PathBuf::from(raw_source_path);
        let locator_identity = provider_path_identity(&path)?;
        let stream = provider_source_cursor_stream_for_path(
            CaptureProvider::Gemini,
            GEMINI_CLI_SOURCE_FORMAT,
            &locator_identity,
        );
        let Some(current_cursor) = store.get_sync_cursor(None, machine_id, &stream)? else {
            continue;
        };
        let Some(checkpoint) = decode_gemini_cursor(&current_cursor.cursor)? else {
            continue;
        };
        if checkpoint.source_path != path {
            continue;
        }
        let Some(source_revision) = source
            .sync
            .metadata
            .get("source_revision")
            .and_then(Value::as_str)
        else {
            continue;
        };
        let route = GeminiKnownRoute {
            path,
            locator_identity: locator_identity.clone(),
            canonical_source_identity: canonical_source_identity.to_owned(),
            source_revision: source_revision.to_owned(),
            current_cursor,
            checkpoint,
        };
        if routes.insert(locator_identity, route).is_some() {
            return Err(CaptureError::SystemInvariant(
                "Gemini persisted duplicate current routes for one transcript",
            ));
        }
    }
    Ok(routes.into_values().collect())
}

pub(super) fn retire_missing_gemini_routes(
    store: &mut Store,
    machine_id: &str,
    retired_at: DateTime<Utc>,
    known_routes: &[GeminiKnownRoute],
    live_paths: &BTreeSet<PathBuf>,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<ProviderImportSummary> {
    let missing = known_routes
        .iter()
        .filter(|route| !live_paths.contains(&route.path))
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(ProviderImportSummary::default());
    }
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let mut summary = ProviderImportSummary::default();
        for route in missing {
            if retire_gemini_route(store, &bulk_guard, machine_id, retired_at, route, reason)? {
                summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
                summary.skipped = summary.skipped.saturating_add(1);
                summary.set_work_result(ProviderImportWorkResult::Changed);
            }
        }
        Ok(summary)
    })();
    let finish = store
        .finish_event_search_bulk_mode(&bulk_guard)
        .map_err(CaptureError::from);
    match (operation, finish) {
        (Ok(summary), Ok(())) => Ok(summary),
        (_, Err(error)) => Err(error),
        (Err(error), Ok(())) => Err(error),
    }
}

fn retire_gemini_route(
    store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    machine_id: &str,
    retired_at: DateTime<Utc>,
    route: &GeminiKnownRoute,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<bool> {
    let provider_cursor = match ctx_history_store::decode_native_path_committed_cursor(
        &route.current_cursor.cursor,
    ) {
        Ok(cursor) => cursor.provider_cursor().to_owned(),
        Err(_) => encode_gemini_cursor(&route.checkpoint)?,
    };
    let transition = NativePathCursorTransition::new(
        Some(route.current_cursor.cursor.clone()),
        provider_sync_cursor(
            machine_id,
            route.current_cursor.stream.clone(),
            provider_cursor.clone(),
            retired_at,
        ),
    );
    let retirement = ProviderSourceRouteRetirement {
        provider: CaptureProvider::Gemini,
        source_format: GEMINI_CLI_SOURCE_FORMAT.to_owned(),
        machine_id: machine_id.to_owned(),
        locator_identity: route.locator_identity.clone(),
        cursor_stream: route.current_cursor.stream.clone(),
        expected_canonical_source_identity: route.canonical_source_identity.clone(),
        expected_source_revision: route.source_revision.clone(),
        retired_at_ms: retired_at.timestamp_millis(),
        reason,
    };
    let publication_id = gemini_retirement_publication_id(&retirement);
    if ctx_history_store::decode_native_path_committed_cursor(&route.current_cursor.cursor)
        .ok()
        .is_some_and(|cursor| {
            cursor.publication_id() == publication_id && cursor.provider_cursor() == provider_cursor
        })
    {
        return Ok(false);
    }
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store
        .begin_native_path_publication_group(admission, NativePathGroupAccounting::new(0, 1, 0)?)?;
    let changed =
        match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
            NativePathCursorSetClassification::AllExpected => {
                let disposition = group.retire_provider_source_route(&retirement)?;
                group.prepare_journal_checkpoint()?;
                group.publish_cursor_set()?;
                matches!(
                    disposition,
                    ProviderSourceRouteRetirementDisposition::Retired
                )
            }
            NativePathCursorSetClassification::AllNextSameGroup { .. } => false,
        };
    group.commit()?;
    Ok(changed)
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
                CaptureProvider::Gemini.as_str(),
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

pub(super) fn gemini_publication_id(
    pages: &[GeminiPendingPage],
    transitions: &[NativePathCursorTransition],
) -> String {
    let mut digest = Sha256::new();
    digest.update(GEMINI_PUBLICATION_DOMAIN);
    digest.update((pages.len() as u64).to_be_bytes());
    for pending in pages {
        digest.update(pending.page.identity.as_bytes());
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
    format!("gemini-nativepath-v1:{:x}", digest.finalize())
}

pub(super) fn gemini_retirement_publication_id(
    retirement: &ProviderSourceRouteRetirement,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-gemini-nativepath-retirement-v1\0");
    digest.update(retirement.machine_id.as_bytes());
    digest.update(retirement.locator_identity.as_bytes());
    digest.update(retirement.cursor_stream.as_bytes());
    digest.update(retirement.expected_canonical_source_identity.as_bytes());
    digest.update(retirement.expected_source_revision.as_bytes());
    format!("gemini-nativepath-retired-v1:{:x}", digest.finalize())
}

pub(super) fn gemini_scan_error(error: GeminiScanError) -> CaptureError {
    match error {
        GeminiScanError::Capture(error) => error,
        error => CaptureError::InvalidPayload(error.to_string()),
    }
}

pub(super) fn hex_digest(digest: [u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}
