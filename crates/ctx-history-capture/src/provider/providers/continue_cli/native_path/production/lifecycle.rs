use super::*;

pub(super) fn known_continue_routes(
    store: &Store,
    machine_id: &str,
    configured_source_root: &Path,
) -> Result<Vec<KnownContinueRoute>> {
    let source_root = configured_source_root.display().to_string();
    let mut routes = BTreeMap::<String, KnownContinueRoute>::new();
    for source in store.list_capture_sources()? {
        if source.descriptor.provider != CaptureProvider::Continue
            || source.descriptor.machine_id != machine_id
            || source.descriptor.source_format.as_deref() != Some(CONTINUE_CLI_SOURCE_FORMAT)
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
            CaptureProvider::Continue,
            CONTINUE_CLI_SOURCE_FORMAT,
            &locator_identity,
        );
        let Some(current_cursor) = store.get_sync_cursor(None, machine_id, &stream)? else {
            continue;
        };
        let (provider_cursor, committed_publication_id) = match decode_native_path_committed_cursor(
            &current_cursor.cursor,
        ) {
            Ok(cursor) => (
                cursor.provider_cursor().to_owned(),
                Some(cursor.publication_id().to_owned()),
            ),
            Err(_) => {
                if CertifiedProviderCursor::decode_if_certified(&current_cursor.cursor)?.is_none() {
                    return Err(CaptureError::InvalidPayload(
                            "Continue NativePath cursor is neither a publication envelope nor a released migration cursor"
                                .to_owned(),
                        ));
                }
                (current_cursor.cursor.clone(), None)
            }
        };
        let cursor_revision = ContinueNativeStoreCursor::decode(&provider_cursor)
            .ok()
            .map(|cursor| cursor.source_revision)
            .or_else(|| {
                CertifiedProviderCursor::decode_if_certified(&provider_cursor)
                    .ok()
                    .flatten()
                    .map(|cursor| cursor.source_revision().to_owned())
            });
        let Some(source_revision) = source
            .sync
            .metadata
            .get("source_revision")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or(cursor_revision)
        else {
            continue;
        };
        let route = KnownContinueRoute {
            path,
            locator_identity: locator_identity.clone(),
            canonical_source_identity: canonical_source_identity.to_owned(),
            source_revision,
            current_cursor,
            provider_cursor,
            committed_publication_id,
        };
        if let Some(existing) = routes.get(&locator_identity) {
            if existing.canonical_source_identity != route.canonical_source_identity
                || existing.source_revision != route.source_revision
            {
                return Err(CaptureError::SystemInvariant(
                    "Continue persisted conflicting routes for one locator",
                ));
            }
            continue;
        }
        routes.insert(locator_identity, route);
    }
    Ok(routes.into_values().collect())
}

pub(super) fn retire_missing_routes(
    store: &mut Store,
    context: &ProviderAdapterContext,
    known_routes: &[KnownContinueRoute],
    live_paths: &BTreeSet<PathBuf>,
    reason: ProviderSourceRouteRetirementReason,
    work_limit: CaptureWorkLimit,
) -> Result<ProviderImportSummary> {
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = retire_missing_routes_in_bulk(
        store,
        &bulk_guard,
        context,
        known_routes,
        live_paths,
        reason,
        work_limit,
    );
    let finish = store
        .finish_event_search_bulk_mode(&bulk_guard)
        .map_err(CaptureError::from);
    match (operation, finish) {
        (Ok(summary), Ok(())) => Ok(summary),
        (_, Err(error)) => Err(error),
        (Err(error), Ok(())) => Err(error),
    }
}

pub(super) fn retire_missing_routes_in_bulk(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    known_routes: &[KnownContinueRoute],
    live_paths: &BTreeSet<PathBuf>,
    reason: ProviderSourceRouteRetirementReason,
    work_limit: CaptureWorkLimit,
) -> Result<ProviderImportSummary> {
    let mut summary = ProviderImportSummary::default();
    let mut missing_routes = known_routes
        .iter()
        .filter(|route| !live_paths.contains(&route.path))
        .peekable();
    while let Some(route) = missing_routes.next() {
        if retire_route(store, bulk_guard, context, route, reason)? {
            summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
            summary.set_work_result(ProviderImportWorkResult::Changed);
            if work_limit == CaptureWorkLimit::OneSafeGroup {
                summary.work_remaining = missing_routes.peek().is_some();
                break;
            }
        }
    }
    Ok(summary)
}

pub(super) fn retire_route(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    route: &KnownContinueRoute,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<bool> {
    let stream = route.current_cursor.stream.clone();
    let transition = NativePathCursorTransition::new(
        Some(route.current_cursor.cursor.clone()),
        provider_sync_cursor(
            &context.machine_id,
            stream.clone(),
            route.provider_cursor.clone(),
            context.imported_at,
        ),
    );
    let retirement = ProviderSourceRouteRetirement {
        provider: CaptureProvider::Continue,
        source_format: CONTINUE_CLI_SOURCE_FORMAT.to_owned(),
        machine_id: context.machine_id.clone(),
        locator_identity: route.locator_identity.clone(),
        cursor_stream: stream,
        expected_canonical_source_identity: route.canonical_source_identity.clone(),
        expected_source_revision: route.source_revision.clone(),
        retired_at_ms: context.imported_at.timestamp_millis(),
        reason,
    };
    let publication_id = retirement_publication_id(&retirement);
    if route.committed_publication_id.as_deref() == Some(publication_id.as_str()) {
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

pub(super) fn source_cursor_stream(source: &ContinueSourceObservation) -> Result<String> {
    let identity = provider_path_identity(source.canonical_path())?;
    Ok(provider_source_cursor_stream_for_path(
        CaptureProvider::Continue,
        CONTINUE_CLI_SOURCE_FORMAT,
        &identity,
    ))
}

pub(super) fn source_revision(source: &ContinuePublicationSource) -> String {
    format!(
        "{};index={}",
        source.observation.session_revision(),
        source.index_dependency.dependency_revision()
    )
}

pub(super) fn decode_frontier(frontier: &NativeSafeFrontier) -> Result<ContinuePageFrontier> {
    if frontier.version != 1 {
        return Err(CaptureError::InvalidPayload(
            "unsupported Continue NativePath frontier version".to_owned(),
        ));
    }
    serde_json::from_slice(&frontier.bytes)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
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
                CaptureProvider::Continue.as_str(),
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

pub(super) fn page_publication_id(
    source: &NativeSourceIdentity,
    page: &NativeIngestionPage<super::super::ContinuePreparedPage>,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(CONTINUE_PAGE_PUBLICATION_DOMAIN);
    hash_publication_common(&mut digest, source, transition);
    digest.update(page.expected_frontier.version.to_le_bytes());
    hash_field(&mut digest, &page.expected_frontier.bytes);
    digest.update(page.next_safe_frontier.version.to_le_bytes());
    hash_field(&mut digest, &page.next_safe_frontier.bytes);
    digest.update([u8::from(page.terminal)]);
    format!("continue-nativepath-page:{}", hex(&digest.finalize()))
}

pub(super) fn terminal_publication_id(
    source: &NativeSourceIdentity,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(CONTINUE_TERMINAL_PUBLICATION_DOMAIN);
    hash_publication_common(&mut digest, source, transition);
    format!("continue-nativepath-terminal:{}", hex(&digest.finalize()))
}

pub(super) fn hash_publication_common(
    digest: &mut Sha256,
    source: &NativeSourceIdentity,
    transition: &NativePathCursorTransition,
) {
    hash_field(digest, source.provider().as_bytes());
    hash_field(digest, source.source_identity().as_bytes());
    hash_field(digest, transition.next().stream.as_bytes());
    hash_field(digest, transition.next().cursor.as_bytes());
}

pub(super) fn retirement_publication_id(retirement: &ProviderSourceRouteRetirement) -> String {
    let mut digest = Sha256::new();
    digest.update(CONTINUE_RETIREMENT_PUBLICATION_DOMAIN);
    hash_field(&mut digest, retirement.provider.as_str().as_bytes());
    hash_field(&mut digest, retirement.source_format.as_bytes());
    hash_field(&mut digest, retirement.machine_id.as_bytes());
    hash_field(&mut digest, retirement.locator_identity.as_bytes());
    hash_field(&mut digest, retirement.cursor_stream.as_bytes());
    hash_field(
        &mut digest,
        retirement.expected_canonical_source_identity.as_bytes(),
    );
    hash_field(&mut digest, retirement.expected_source_revision.as_bytes());
    format!("continue-nativepath-retire:{}", hex(&digest.finalize()))
}

pub(super) fn hash_field(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_le_bytes());
    digest.update(value);
}

pub(super) fn hex(bytes: &[u8]) -> String {
    let mut value = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(value, "{byte:02x}");
    }
    value
}

pub(super) fn map_native_error(error: ContinueNativePathError) -> CaptureError {
    match error {
        ContinueNativePathError::SourceChanged { .. } => CaptureError::SourceChangedDuringCapture,
        ContinueNativePathError::SourceIo {
            path,
            operation,
            kind,
            raw_os_error,
            message,
        } => CaptureError::Io(io::Error::new(
            kind,
            format!(
                "Continue source I/O failed during {operation} for `{}` (os={raw_os_error:?}): {message}",
                path.display()
            ),
        )),
        ContinueNativePathError::SystemIo { operation, source } => {
            CaptureError::SystemIo { operation, source }
        }
        ContinueNativePathError::Invariant { message } => CaptureError::SystemInvariant(message),
        other => CaptureError::InvalidPayload(other.to_string()),
    }
}
