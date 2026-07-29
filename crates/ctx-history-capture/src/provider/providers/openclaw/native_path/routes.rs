use super::*;

pub(super) fn source_locator_identity(path_identity: &str, generation: u64) -> String {
    format!("{path_identity}#openclaw-generation:{generation}")
}

pub(super) fn source_revision(
    observation: &OpenClawSessionObservation,
    inventory_token: Option<&str>,
) -> String {
    let revision = observation.source_revision();
    let Some(token) = inventory_token else {
        return revision;
    };
    let mut digest = Sha256::new();
    digest.update(b"ctx-openclaw-inventory-observation-v1\0");
    digest.update((revision.len() as u64).to_be_bytes());
    digest.update(revision.as_bytes());
    digest.update((token.len() as u64).to_be_bytes());
    digest.update(token.as_bytes());
    format!("inventory-observation-sha256-v1:{:x}", digest.finalize())
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
                CaptureProvider::OpenClaw.as_str(),
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
    context: &PublicationContext<'_>,
    pages: &[PendingPage],
    transitions: &[NativePathCursorTransition],
) -> Result<String> {
    let mut digest = Sha256::new();
    digest.update(PUBLICATION_DOMAIN);
    digest.update(context.machine_id.as_bytes());
    digest.update(context.source_root.as_os_str().as_encoded_bytes());
    digest.update((pages.len() as u64).to_be_bytes());
    for pending in pages {
        let path = provider_path_identity(&pending.path)?;
        digest.update((path.len() as u64).to_be_bytes());
        digest.update(path.as_bytes());
        let expected = serde_json::to_vec(&pending.page.expected_checkpoint)?;
        let next = serde_json::to_vec(&pending.page.next_checkpoint)?;
        digest.update((expected.len() as u64).to_be_bytes());
        digest.update(expected);
        digest.update((next.len() as u64).to_be_bytes());
        digest.update(next);
        digest.update([u8::from(pending.page.terminal)]);
        for event in &pending.page.events {
            digest.update(event.provider_event_index.to_be_bytes());
            digest.update((event.provider_event_hash.len() as u64).to_be_bytes());
            digest.update(event.provider_event_hash.as_bytes());
        }
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
    Ok(format!(
        "openclaw-nativepath-publication-sha256-v1:{:x}",
        digest.finalize()
    ))
}

pub(super) fn line_number(ordinal: u64) -> usize {
    usize::try_from(ordinal)
        .unwrap_or(usize::MAX)
        .saturating_add(1)
}

#[derive(Clone)]
pub(super) struct KnownRoute {
    pub(super) capture_source_id: Uuid,
    pub(super) raw_source_path: PathBuf,
    pub(super) locator_identity: String,
    pub(super) canonical_source_identity: String,
    pub(super) source_revision: String,
    pub(super) current_cursor: SyncCursor,
    pub(super) provider_cursor: String,
}

pub(super) fn known_routes(
    store: &Store,
    machine_id: &str,
    source_root: &Path,
) -> Result<Vec<KnownRoute>> {
    let source_root = source_root.display().to_string();
    let mut routes = BTreeMap::<String, KnownRoute>::new();
    for source in store.list_capture_sources()? {
        if source.descriptor.provider != CaptureProvider::OpenClaw
            || source.descriptor.machine_id != machine_id
            || source.descriptor.source_format.as_deref() != Some(OPENCLAW_SOURCE_FORMAT)
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
        let path_identity = provider_path_identity(&path)?;
        let stream = provider_source_cursor_stream_for_path(
            CaptureProvider::OpenClaw,
            OPENCLAW_SOURCE_FORMAT,
            &path_identity,
        );
        let Some(current_cursor) = store.get_sync_cursor(None, machine_id, &stream)? else {
            continue;
        };
        if cursor_was_retired(&current_cursor.cursor) {
            continue;
        }
        let provider_cursor = decode_native_path_committed_cursor(&current_cursor.cursor)
            .map(|cursor| cursor.provider_cursor().to_owned())
            .unwrap_or_else(|_| current_cursor.cursor.clone());
        let Some(checkpoint) = native_checkpoint_from_cursor(&provider_cursor) else {
            continue;
        };
        let Some(generation) = source
            .sync
            .metadata
            .get("nativepath_generation")
            .and_then(Value::as_u64)
        else {
            continue;
        };
        if checkpoint.generation != generation
            || source.descriptor.external_session_id.as_deref()
                != Some(checkpoint.session.provider_session_id.as_str())
        {
            continue;
        }
        let locator_identity = source_locator_identity(&path_identity, generation);
        let Some(source_revision) = source
            .sync
            .metadata
            .get("source_revision")
            .and_then(Value::as_str)
        else {
            continue;
        };
        let route = KnownRoute {
            capture_source_id: source.id,
            raw_source_path: path,
            locator_identity: locator_identity.clone(),
            canonical_source_identity: canonical_source_identity.to_owned(),
            source_revision: source_revision.to_owned(),
            current_cursor,
            provider_cursor,
        };
        if routes.insert(locator_identity, route).is_some() {
            return Err(CaptureError::SystemInvariant(
                "OpenClaw persisted duplicate current routes for one transcript",
            ));
        }
    }
    Ok(routes.into_values().collect())
}

pub(super) fn native_checkpoint_from_cursor(provider_cursor: &str) -> Option<Checkpoint> {
    let wire = serde_json::from_str::<CursorWire>(provider_cursor).ok()?;
    (wire.version == CURSOR_VERSION
        && wire.kind == "openclaw-nativepath-jsonl"
        && wire.checkpoint.supported())
    .then_some(wire.checkpoint)
}

pub(super) fn cursor_was_retired(encoded_store_cursor: &str) -> bool {
    decode_native_path_committed_cursor(encoded_store_cursor).is_ok_and(|cursor| {
        cursor
            .publication_id()
            .starts_with("openclaw-nativepath-retirement-sha256-v1:")
    })
}

pub(super) fn current_route_for_path(
    store: &Store,
    machine_id: &str,
    source_root: &Path,
    path: &Path,
) -> Result<Option<KnownRoute>> {
    let path_identity = provider_path_identity(path)?;
    let mut matches = known_routes(store, machine_id, source_root)?
        .into_iter()
        .filter(|route| {
            provider_path_identity(&route.raw_source_path)
                .is_ok_and(|identity| identity == path_identity)
        })
        .collect::<Vec<_>>();
    match matches.len() {
        0 => Ok(None),
        1 => Ok(matches.pop()),
        _ => Err(CaptureError::SystemInvariant(
            "OpenClaw has multiple current source generations for one route",
        )),
    }
}

pub(super) fn retire_missing_routes(
    store: &mut Store,
    machine_id: &str,
    retired_at: DateTime<Utc>,
    known_routes: &[KnownRoute],
    live_paths: &BTreeSet<PathBuf>,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<ProviderImportSummary> {
    let live_locators = live_paths
        .iter()
        .map(|path| provider_path_identity(path))
        .collect::<Result<BTreeSet<_>>>()?;
    let missing = known_routes
        .iter()
        .filter(|route| {
            provider_path_identity(&route.raw_source_path)
                .map(|identity| !live_locators.contains(&identity))
                .unwrap_or(true)
        })
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(ProviderImportSummary::default());
    }
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let mut summary = ProviderImportSummary::default();
        for route in missing {
            if retire_route(store, &bulk_guard, machine_id, retired_at, route, reason)? {
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

pub(super) fn retire_route(
    store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    machine_id: &str,
    retired_at: DateTime<Utc>,
    route: &KnownRoute,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<bool> {
    let stream = route.current_cursor.stream.clone();
    let transition = NativePathCursorTransition::new(
        Some(route.current_cursor.cursor.clone()),
        provider_sync_cursor(
            machine_id,
            stream.clone(),
            route.provider_cursor.clone(),
            retired_at,
        ),
    );
    let retirement = route_retirement(retired_at, route, reason);
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

pub(super) fn route_retirement(
    retired_at: DateTime<Utc>,
    route: &KnownRoute,
    reason: ProviderSourceRouteRetirementReason,
) -> ProviderSourceRouteRetirement {
    ProviderSourceRouteRetirement {
        provider: CaptureProvider::OpenClaw,
        source_format: OPENCLAW_SOURCE_FORMAT.to_owned(),
        machine_id: route.current_cursor.device_id.clone(),
        locator_identity: route.locator_identity.clone(),
        cursor_stream: route.current_cursor.stream.clone(),
        expected_canonical_source_identity: route.canonical_source_identity.clone(),
        expected_source_revision: route.source_revision.clone(),
        retired_at_ms: retired_at.timestamp_millis(),
        reason,
    }
}

pub(super) fn retirement_publication_id(retirement: &ProviderSourceRouteRetirement) -> String {
    let mut digest = Sha256::new();
    digest.update(RETIREMENT_DOMAIN);
    digest.update(retirement.provider.as_str().as_bytes());
    digest.update(retirement.source_format.as_bytes());
    digest.update(retirement.machine_id.as_bytes());
    digest.update(retirement.locator_identity.as_bytes());
    digest.update(retirement.cursor_stream.as_bytes());
    digest.update(retirement.expected_canonical_source_identity.as_bytes());
    digest.update(retirement.expected_source_revision.as_bytes());
    digest.update(retirement.retired_at_ms.to_be_bytes());
    digest.update([match retirement.reason {
        ProviderSourceRouteRetirementReason::SourceMissing => 0,
        ProviderSourceRouteRetirementReason::RootMissing => 1,
        ProviderSourceRouteRetirementReason::Replaced => 2,
    }]);
    format!(
        "openclaw-nativepath-retirement-sha256-v1:{:x}",
        digest.finalize()
    )
}
