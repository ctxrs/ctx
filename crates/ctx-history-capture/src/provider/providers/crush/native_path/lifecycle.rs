use super::event_projection::{encode_core_cursor, provider_sync_cursor};
use super::*;

#[derive(Clone)]
struct KnownCrushRoute {
    locator_identity: String,
    canonical_source_identity: String,
    source_revision: String,
    current_cursor: SyncCursor,
}

pub(super) fn retire_missing_crush_source(
    requested_path: &Path,
    store: &mut Store,
    context: &ProviderAdapterContext,
) -> Result<ProviderImportSummary> {
    let routes = known_crush_routes(store, requested_path, context)?;
    if routes.is_empty() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: requested_path.to_path_buf(),
            reason: "Crush SQLite source does not exist",
        });
    }
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let mut summary = ProviderImportSummary::default();
        for route in routes.values() {
            if retire_crush_route(store, &bulk_guard, context, route)? {
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

fn known_crush_routes(
    store: &Store,
    requested_path: &Path,
    context: &ProviderAdapterContext,
) -> Result<BTreeMap<String, KnownCrushRoute>> {
    let requested_absolute = lexical_absolute_path(requested_path)?;
    let requested_is_source_root = context
        .source_root
        .as_deref()
        .map(lexical_absolute_path)
        .transpose()?
        .is_some_and(|root| root == requested_absolute);
    let mut routes = BTreeMap::new();
    for source in store.list_capture_sources()? {
        if source.descriptor.provider != CaptureProvider::Crush
            || source.descriptor.machine_id != context.machine_id
            || source.descriptor.source_format.as_deref() != Some(CRUSH_SQLITE_SOURCE_FORMAT)
        {
            continue;
        }
        let Some(raw_path) = source.descriptor.raw_source_path.as_deref() else {
            continue;
        };
        let raw_path = PathBuf::from(raw_path);
        let raw_matches =
            raw_path == requested_path || lexical_absolute_path(&raw_path)? == requested_absolute;
        let root_matches = requested_is_source_root
            && source
                .descriptor
                .source_root
                .as_deref()
                .map(Path::new)
                .map(lexical_absolute_path)
                .transpose()?
                .is_some_and(|root| root == requested_absolute);
        if !raw_matches && !root_matches {
            continue;
        }
        let locator_identity = provider_path_identity(&raw_path)?;
        let stream = provider_source_cursor_stream_for_path(
            CaptureProvider::Crush,
            CRUSH_SQLITE_SOURCE_FORMAT,
            &locator_identity,
        );
        let Some(current_cursor) = store.get_sync_cursor(None, &context.machine_id, &stream)?
        else {
            continue;
        };
        let Some(canonical_source_identity) = source.descriptor.source_identity.clone() else {
            continue;
        };
        let Some(source_revision) = source
            .sync
            .metadata
            .get("source_revision")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            continue;
        };
        routes
            .entry(locator_identity.clone())
            .or_insert(KnownCrushRoute {
                locator_identity,
                canonical_source_identity,
                source_revision,
                current_cursor,
            });
    }
    Ok(routes)
}

fn lexical_absolute_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    Ok(normalized)
}

fn retire_crush_route(
    store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    route: &KnownCrushRoute,
) -> Result<bool> {
    let provider_cursor = match decode_native_path_committed_cursor(&route.current_cursor.cursor) {
        Ok(committed) => committed.provider_cursor().to_owned(),
        Err(_) => encode_core_cursor(&CrushNativeCursor {
            version: CRUSH_NATIVE_CURSOR_VERSION,
            parser_revision: CRUSH_NATIVE_PARSER_REVISION.to_owned(),
            policy_revision: CRUSH_POLICY_REVISION,
            locator_identity: route.locator_identity.clone(),
            source_revision: route.source_revision.clone(),
            frontier: CrushNativeFrontier::default(),
            generation: 1,
            terminal: true,
            rejected_records: 0,
            rejections: Vec::new(),
            retained_events: 0,
        })?,
    };
    let transition = NativePathCursorTransition::new(
        Some(route.current_cursor.cursor.clone()),
        provider_sync_cursor(
            &context.machine_id,
            route.current_cursor.stream.clone(),
            provider_cursor,
            context.imported_at,
        ),
    );
    let retirement = ProviderSourceRouteRetirement {
        provider: CaptureProvider::Crush,
        source_format: CRUSH_SQLITE_SOURCE_FORMAT.to_owned(),
        machine_id: context.machine_id.clone(),
        locator_identity: route.locator_identity.clone(),
        cursor_stream: route.current_cursor.stream.clone(),
        expected_canonical_source_identity: route.canonical_source_identity.clone(),
        expected_source_revision: route.source_revision.clone(),
        retired_at_ms: context.imported_at.timestamp_millis(),
        reason: ProviderSourceRouteRetirementReason::RootMissing,
    };
    let mut digest = Sha256::new();
    digest.update(CRUSH_NATIVE_RETIREMENT_DOMAIN);
    hash_field(&mut digest, route.locator_identity.as_bytes());
    hash_field(&mut digest, route.canonical_source_identity.as_bytes());
    hash_field(&mut digest, route.source_revision.as_bytes());
    hash_field(&mut digest, route.current_cursor.cursor.as_bytes());
    let publication_id = format!("crush-nativepath-retire-v1:{:x}", digest.finalize());
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store
        .begin_native_path_publication_group(admission, NativePathGroupAccounting::new(0, 1, 0)?)?;
    let changed =
        match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
            NativePathCursorSetClassification::AllExpected => {
                let disposition = group.retire_provider_source_route(&retirement)?;
                if disposition == ProviderSourceRouteRetirementDisposition::Retired {
                    group.prepare_journal_checkpoint()?;
                    group.publish_cursor_set()?;
                    true
                } else {
                    group.rollback()?;
                    return Ok(false);
                }
            }
            NativePathCursorSetClassification::AllNextSameGroup { .. } => false,
        };
    group.commit()?;
    Ok(changed)
}

pub(super) fn hash_field(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_le_bytes());
    digest.update(bytes);
}
