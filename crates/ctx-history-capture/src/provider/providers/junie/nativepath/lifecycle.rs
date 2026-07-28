use super::*;

pub(crate) fn import_junie_nativepath(
    path: &Path,
    store: &mut Store,
    mut context: ProviderAdapterContext,
    options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let configured_source_root = context
        .source_root
        .clone()
        .or_else(|| context.source_path.clone())
        .unwrap_or_else(|| path.to_path_buf());
    context.source_path = Some(path.to_path_buf());
    context.source_root = Some(configured_source_root.clone());

    let inventory = discover(path)?;
    let known = known_routes(store, &context.machine_id, &configured_source_root)?;
    if inventory.sessions.is_empty() {
        if known.is_empty() {
            if inventory.index_rejection_count != 0 {
                let mut summary = ProviderImportSummary::default();
                merge_inventory_rejections(&mut summary, &inventory);
                return Ok(summary);
            }
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "no Junie index.jsonl entries with session events.jsonl files were found",
            });
        }
        if options.import_profile.is_replay_only() {
            return Ok(ProviderImportSummary::default());
        }
        return retire_missing(
            store,
            &context,
            &known,
            &BTreeSet::new(),
            if inventory.root_missing {
                ProviderSourceRouteRetirementReason::RootMissing
            } else {
                ProviderSourceRouteRetirementReason::SourceMissing
            },
            options.capture_work_limit,
        );
    }

    let replay_only = options.import_profile.is_replay_only();
    let mut summary = ProviderImportSummary::default();
    if !replay_only {
        merge_inventory_rejections(&mut summary, &inventory);
        let committed_store = Store::open_read_only(store.path())?;
        let bulk = store.begin_event_search_bulk_mode()?;
        let operation = (|| {
            let mut changed_groups = 0_usize;
            for session_path in &inventory.sessions {
                let source = import_core_source(
                    store,
                    &committed_store,
                    &bulk,
                    session_path,
                    &context,
                    &options,
                    &mut changed_groups,
                )?;
                summary.merge_from(source);
                if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup
                    && changed_groups != 0
                {
                    summary.work_remaining = true;
                    return Ok(());
                }
            }
            inventory.revalidate()?;
            summary.merge_from(retire_missing(
                store,
                &context,
                &known,
                &inventory.live_paths,
                ProviderSourceRouteRetirementReason::SourceMissing,
                options.capture_work_limit,
            )?);
            Ok(())
        })();
        let finish = store
            .finish_event_search_bulk_mode(&bulk)
            .map_err(CaptureError::from);
        match (operation, finish) {
            (Ok(()), Ok(())) => {}
            (_, Err(error)) => return Err(error),
            (Err(error), Ok(())) => return Err(error),
        }
    }

    if !summary.work_remaining {
        replay_outputs(
            store,
            &inventory.sessions,
            &configured_source_root,
            &context,
            &options.import_profile,
        );
    }
    Ok(summary)
}

pub(super) struct Inventory {
    pub(super) sessions: Vec<JunieSessionPath>,
    pub(super) live_paths: BTreeSet<PathBuf>,
    pub(super) root_missing: bool,
    pub(super) index_rejection_count: u64,
    pub(super) index_rejections: Vec<ProviderImportFailure>,
    authority: Option<crate::common::io::ProviderSourceRoot>,
}

impl Inventory {
    fn revalidate(&self) -> Result<()> {
        match self.authority.as_ref() {
            Some(authority) => authority.revalidate(),
            None if self.root_missing => Ok(()),
            None => Err(CaptureError::SystemInvariant(
                "Junie inventory has no retained root authority",
            )),
        }
    }
}

pub(super) fn discover(path: &Path) -> Result<Inventory> {
    let mut sessions = Vec::new();
    let visit = match super::super::session_tree::visit_junie_session_event_paths(
        path,
        &mut |session, _| {
            sessions.push(session);
            Ok(())
        },
    ) {
        Ok(visit) => visit,
        Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(Inventory {
                sessions: Vec::new(),
                live_paths: BTreeSet::new(),
                root_missing: true,
                index_rejection_count: 0,
                index_rejections: Vec::new(),
                authority: None,
            });
        }
        Err(error) => return Err(error.into()),
    };
    debug_assert_eq!(visit.visited, sessions.len());
    let live_paths = sessions
        .iter()
        .map(|session| session.events_path.clone())
        .collect();
    Ok(Inventory {
        sessions,
        live_paths,
        root_missing: false,
        index_rejection_count: visit.rejection_count,
        index_rejections: visit.rejections,
        authority: visit.authority,
    })
}

pub(super) fn merge_inventory_rejections(
    summary: &mut ProviderImportSummary,
    inventory: &Inventory,
) {
    for rejection in &inventory.index_rejections {
        summary.record_failure(rejection.clone());
    }
    summary.failed = summary.failed.saturating_add(
        usize::try_from(
            inventory
                .index_rejection_count
                .saturating_sub(inventory.index_rejections.len() as u64),
        )
        .unwrap_or(usize::MAX),
    );
}

#[derive(Clone)]
pub(super) struct KnownRoute {
    pub(super) path: PathBuf,
    pub(super) locator_identity: String,
    pub(super) canonical_source_identity: String,
    pub(super) current_cursor: SyncCursor,
    pub(super) cursor: JunieStoreCursor,
}

pub(super) fn known_routes(
    store: &Store,
    machine_id: &str,
    source_root: &Path,
) -> Result<Vec<KnownRoute>> {
    let source_root = source_root.display().to_string();
    let mut routes = BTreeMap::<String, KnownRoute>::new();
    for source in store.list_capture_sources()? {
        if source.descriptor.provider != CaptureProvider::Junie
            || source.descriptor.machine_id != machine_id
            || source.descriptor.source_format.as_deref()
                != Some(JUNIE_SESSION_EVENTS_SOURCE_FORMAT)
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
            CaptureProvider::Junie,
            JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
            &locator_identity,
        );
        let Some(current_cursor) = store.get_sync_cursor(None, machine_id, &stream)? else {
            continue;
        };
        let cursor = match decode_native_path_committed_cursor(&current_cursor.cursor) {
            Ok(committed) => {
                JunieStoreCursor::decode(committed.provider_cursor()).map_err(|_| {
                    CaptureError::SystemInvariant(
                        "Junie persisted NativePath route cursor is corrupt",
                    )
                })?
            }
            Err(_) if !looks_like_native_path_cursor(&current_cursor.cursor) => {
                let legacy = CertifiedProviderCursor::decode_if_certified(&current_cursor.cursor)
                    .map_err(|_| {
                        CaptureError::SystemInvariant("Junie persisted released cursor is corrupt")
                    })?
                    .ok_or(CaptureError::SystemInvariant(
                        "Junie persisted cursor has an unknown encoding",
                    ))?;
                released_cursor_for_retirement(canonical_source_identity, legacy)?
            }
            Err(error) => return Err(CaptureError::Store(error)),
        };
        let route = KnownRoute {
            path,
            locator_identity: locator_identity.clone(),
            canonical_source_identity: canonical_source_identity.to_owned(),
            current_cursor,
            cursor,
        };
        if routes.insert(locator_identity, route).is_some() {
            return Err(CaptureError::SystemInvariant(
                "Junie persisted duplicate current routes for one events file",
            ));
        }
    }
    Ok(routes.into_values().collect())
}

pub(super) fn retire_missing(
    store: &mut Store,
    context: &ProviderAdapterContext,
    known: &[KnownRoute],
    live_paths: &BTreeSet<PathBuf>,
    reason: ProviderSourceRouteRetirementReason,
    work_limit: CaptureWorkLimit,
) -> Result<ProviderImportSummary> {
    let missing = known
        .iter()
        .filter(|route| !live_paths.contains(&route.path))
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(ProviderImportSummary::default());
    }
    let bulk = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let mut summary = ProviderImportSummary::default();
        let missing_count = missing.len();
        for (index, route) in missing.into_iter().enumerate() {
            if retire_route(store, &bulk, context, route, reason)? {
                summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
                summary.skipped = summary.skipped.saturating_add(1);
                summary.set_work_result(ProviderImportWorkResult::Changed);
                if work_limit == CaptureWorkLimit::OneSafeGroup {
                    summary.work_remaining = index + 1 < missing_count;
                    break;
                }
            }
        }
        Ok(summary)
    })();
    let finish = store
        .finish_event_search_bulk_mode(&bulk)
        .map_err(CaptureError::from);
    match (operation, finish) {
        (Ok(summary), Ok(())) => Ok(summary),
        (_, Err(error)) => Err(error),
        (Err(error), Ok(())) => Err(error),
    }
}

pub(super) fn retire_route(
    store: &Store,
    bulk: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    route: &KnownRoute,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<bool> {
    if route.cursor.retired {
        return Ok(false);
    }
    let mut retired_cursor = route.cursor.clone();
    retired_cursor.retired = true;
    retired_cursor.terminal = true;
    let transition = NativePathCursorTransition::new(
        Some(route.current_cursor.cursor.clone()),
        SyncCursor {
            id: Uuid::new_v4(),
            team_id: None,
            device_id: context.machine_id.clone(),
            stream: route.current_cursor.stream.clone(),
            cursor: retired_cursor.encode()?,
            last_synced_at: Some(context.imported_at),
            timestamps: timestamps(context.imported_at),
        },
    );
    let retirement = ProviderSourceRouteRetirement {
        provider: CaptureProvider::Junie,
        source_format: JUNIE_SESSION_EVENTS_SOURCE_FORMAT.to_owned(),
        machine_id: context.machine_id.clone(),
        locator_identity: route.locator_identity.clone(),
        cursor_stream: route.current_cursor.stream.clone(),
        expected_canonical_source_identity: route.canonical_source_identity.clone(),
        expected_source_revision: route.cursor.source_revision.clone(),
        retired_at_ms: context.imported_at.timestamp_millis(),
        reason,
    };
    let publication_id = retirement_publication_id(&retirement, &transition);
    let admission = store.admit_event_search_bulk_group(bulk)?;
    let mut group = store
        .begin_native_path_publication_group(admission, NativePathGroupAccounting::new(0, 1, 0)?)?;
    let changed =
        match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
            NativePathCursorSetClassification::AllNextSameGroup { .. } => false,
            NativePathCursorSetClassification::AllExpected => {
                let disposition = group.retire_provider_source_route(&retirement)?;
                group.prepare_journal_checkpoint()?;
                group.publish_cursor_set()?;
                matches!(
                    disposition,
                    ProviderSourceRouteRetirementDisposition::Retired
                )
            }
        };
    group.commit()?;
    Ok(changed)
}

pub(super) fn retirement_publication_id(
    retirement: &ProviderSourceRouteRetirement,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-junie-nativepath-route-retirement-v1\0");
    digest.update(retirement.provider.as_str().as_bytes());
    digest.update(retirement.source_format.as_bytes());
    digest.update(retirement.machine_id.as_bytes());
    digest.update(retirement.locator_identity.as_bytes());
    digest.update(retirement.cursor_stream.as_bytes());
    digest.update(retirement.expected_canonical_source_identity.as_bytes());
    digest.update(retirement.expected_source_revision.as_bytes());
    digest.update(format!("{:?}", retirement.reason).as_bytes());
    digest.update(transition.next().cursor.as_bytes());
    format!("junie-nativepath-retirement-v1:{:x}", digest.finalize())
}
