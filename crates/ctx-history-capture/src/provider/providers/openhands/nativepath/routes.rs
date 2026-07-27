use super::*;

pub(super) fn known_openhands_routes(
    store: &Store,
    machine_id: &str,
) -> Result<Vec<KnownOpenHandsRoute>> {
    let mut routes = BTreeMap::<String, KnownOpenHandsRoute>::new();
    for source in store.list_capture_sources()? {
        if source.descriptor.provider != CaptureProvider::OpenHands
            || source.descriptor.machine_id != machine_id
            || source.descriptor.source_format.as_deref()
                != Some(OPENHANDS_FILE_EVENTS_SOURCE_FORMAT)
        {
            continue;
        }
        let (Some(raw_source_path), Some(canonical_source_identity), Some(locator_revision)) = (
            source.descriptor.raw_source_path.as_deref(),
            source.descriptor.source_identity.as_deref(),
            source
                .sync
                .metadata
                .get("source_revision")
                .and_then(Value::as_str),
        ) else {
            continue;
        };
        let path = PathBuf::from(raw_source_path);
        let locator_identity = source
            .sync
            .metadata
            .pointer("/source_metadata/native_locator_identity")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or(provider_path_identity(&path)?);
        let path_identity = provider_path_identity(&path)?;
        let stream = provider_source_cursor_stream_for_path(
            CaptureProvider::OpenHands,
            OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
            &path_identity,
        );
        let Some(current_cursor) = store.get_sync_cursor(None, machine_id, &stream)? else {
            continue;
        };
        let checkpoint: Option<OpenHandsNativeCursor> =
            decode_native_path_committed_cursor(&current_cursor.cursor)
                .ok()
                .and_then(|committed| serde_json::from_str(committed.provider_cursor()).ok());
        let cursor_revision = source
            .sync
            .metadata
            .get("cursor_revision")
            .and_then(Value::as_str)
            .unwrap_or(locator_revision)
            .to_owned();
        let physical_fingerprint = source
            .sync
            .metadata
            .get("physical_source_fingerprint")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| {
                checkpoint.as_ref().and_then(|cursor| {
                    cursor.observation.as_ref().map(|observation| {
                        openhands_physical_fingerprint(observation, cursor.content_sha256)
                    })
                })
            });
        let identity_path = source
            .sync
            .metadata
            .pointer("/source_metadata/native_identity_path")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| path_identity.clone());
        let identity_raw_path = source
            .sync
            .metadata
            .pointer("/source_metadata/native_identity_raw_path")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .unwrap_or_else(|| path.clone());
        let route = KnownOpenHandsRoute {
            source_id: source.id,
            source_root: source.descriptor.source_root.clone(),
            path,
            path_identity,
            identity_path,
            identity_raw_path,
            locator_identity: locator_identity.clone(),
            canonical_source_identity: canonical_source_identity.to_owned(),
            locator_revision: locator_revision.to_owned(),
            cursor_revision,
            physical_fingerprint,
            current_cursor,
            checkpoint,
        };
        if routes.insert(locator_identity, route).is_some() {
            return Err(CaptureError::SystemInvariant(
                "OpenHands persisted duplicate current routes for one event file",
            ));
        }
    }
    Ok(routes.into_values().collect())
}

pub(super) fn current_route_for_source<'a>(
    routes: &'a [KnownOpenHandsRoute],
    source: &OpenHandsObservedFile,
) -> Result<Option<&'a KnownOpenHandsRoute>> {
    let matches = routes
        .iter()
        .filter(|route| route.path == source.canonical_path)
        .take(2)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => Ok(None),
        [route] => Ok(Some(*route)),
        _ => Err(CaptureError::SystemInvariant(
            "OpenHands persisted multiple sources for one current event path",
        )),
    }
}

pub(super) fn relocation_route_for_source<'a>(
    routes: &'a [KnownOpenHandsRoute],
    source: &OpenHandsObservedFile,
) -> Result<Option<&'a KnownOpenHandsRoute>> {
    let physical_fingerprint = source.physical_fingerprint();
    let matches = routes
        .iter()
        .filter(|route| {
            !route
                .checkpoint
                .as_ref()
                .is_some_and(|checkpoint| checkpoint.deleted)
                && route.physical_fingerprint.as_deref() == Some(physical_fingerprint.as_str())
        })
        .take(2)
        .collect::<Vec<_>>();
    let [route] = matches.as_slice() else {
        return Ok(None);
    };
    if route.path == source.canonical_path || route.path.try_exists()? {
        return Ok(None);
    }
    Ok(Some(*route))
}

pub(super) fn retire_missing_routes(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    known_routes: &[KnownOpenHandsRoute],
    live_paths: &BTreeSet<PathBuf>,
    relocated_locators: &BTreeSet<String>,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<ProviderImportSummary> {
    let mut summary = ProviderImportSummary::default();
    for route in known_routes.iter().filter(|route| {
        !live_paths.contains(&route.path) && !relocated_locators.contains(&route.locator_identity)
    }) {
        if retire_route(store, bulk_guard, context, route, reason)? {
            summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
            summary.set_work_result(ProviderImportWorkResult::Changed);
        }
    }
    Ok(summary)
}

fn retire_route(
    store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    route: &KnownOpenHandsRoute,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<bool> {
    if route
        .checkpoint
        .as_ref()
        .is_some_and(|checkpoint| checkpoint.deleted)
    {
        return Ok(false);
    }
    let generation = route.checkpoint.as_ref().map_or(0, |cursor| {
        if cursor.deleted {
            cursor.generation
        } else {
            cursor.generation.saturating_add(1)
        }
    });
    let route_sha256 = route.checkpoint.as_ref().map_or_else(
        || route_hash(&route.locator_identity),
        |cursor| cursor.route_sha256,
    );
    let tombstone = OpenHandsNativeCursor {
        version: OPENHANDS_NATIVE_CURSOR_VERSION,
        parser_revision: OPENHANDS_NATIVE_PARSER_REVISION,
        policy_revision: OPENHANDS_NATIVE_POLICY_REVISION,
        route_sha256,
        locator_identity: route.locator_identity.clone(),
        legacy_source_layout: route
            .checkpoint
            .as_ref()
            .is_some_and(|cursor| cursor.legacy_source_layout),
        source_revision: route.cursor_revision.clone(),
        observation: None,
        content_sha256: None,
        generation,
        next_touch: 0,
        accepted_event: false,
        accepted_file_touches: 0,
        rejected_records: 0,
        terminal: true,
        deleted: true,
    };
    let transition = NativePathCursorTransition::new(
        Some(route.current_cursor.cursor.clone()),
        provider_sync_cursor(
            &context.machine_id,
            route.current_cursor.stream.clone(),
            tombstone.encode()?,
            context.imported_at,
        ),
    );
    let retirement = ProviderSourceRouteRetirement {
        provider: CaptureProvider::OpenHands,
        source_format: OPENHANDS_FILE_EVENTS_SOURCE_FORMAT.to_owned(),
        machine_id: context.machine_id.clone(),
        locator_identity: route.locator_identity.clone(),
        cursor_stream: route.current_cursor.stream.clone(),
        expected_canonical_source_identity: route.canonical_source_identity.clone(),
        expected_source_revision: route.locator_revision.clone(),
        retired_at_ms: context.imported_at.timestamp_millis(),
        reason,
    };
    let publication_id = retirement_publication_id(&retirement);
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
                CaptureProvider::OpenHands.as_str(),
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

pub(super) fn publication_id(
    source: &OpenHandsObservedFile,
    page: &PreparedCorePage,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(OPENHANDS_NATIVE_PUBLICATION_DOMAIN);
    digest.update(source.route_sha256);
    digest.update(page.cursor_revision.as_bytes());
    digest.update(page.next_cursor.generation.to_be_bytes());
    digest.update(page.next_cursor.next_touch.to_be_bytes());
    digest.update([u8::from(page.next_cursor.terminal)]);
    digest.update([source_change_code(page.source_change)]);
    digest.update(transition.next().cursor.as_bytes());
    format!("openhands-nativepath-v1:{}", hex(&digest.finalize()))
}

const fn source_change_code(change: OpenHandsSourceChange) -> u8 {
    match change {
        OpenHandsSourceChange::Fresh => 0,
        OpenHandsSourceChange::Unchanged => 1,
        OpenHandsSourceChange::Append => 2,
        OpenHandsSourceChange::Rewrite => 3,
        OpenHandsSourceChange::Truncation => 4,
        OpenHandsSourceChange::Replacement => 5,
        OpenHandsSourceChange::Migrated => 6,
    }
}

fn retirement_publication_id(retirement: &ProviderSourceRouteRetirement) -> String {
    let mut digest = Sha256::new();
    digest.update(OPENHANDS_NATIVE_RETIREMENT_DOMAIN);
    digest.update(retirement.provider.as_str().as_bytes());
    digest.update(retirement.source_format.as_bytes());
    digest.update(retirement.machine_id.as_bytes());
    digest.update(retirement.locator_identity.as_bytes());
    digest.update(retirement.cursor_stream.as_bytes());
    digest.update(retirement.expected_canonical_source_identity.as_bytes());
    digest.update(retirement.expected_source_revision.as_bytes());
    digest.update(format!("{:?}", retirement.reason).as_bytes());
    format!(
        "openhands-nativepath-retirement-v1:{}",
        hex(&digest.finalize())
    )
}

pub(super) fn route_hash(locator_identity: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"ctx-openhands-nativepath-route-v1\0");
    digest.update((locator_identity.len() as u64).to_be_bytes());
    digest.update(locator_identity.as_bytes());
    digest.finalize().into()
}
