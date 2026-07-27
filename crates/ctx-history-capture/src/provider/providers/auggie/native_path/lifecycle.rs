use super::*;

pub(crate) fn import_auggie_sessions_nativepath(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    import_options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let configured_source_root = context
        .source_root
        .clone()
        .or_else(|| context.source_path.clone())
        .unwrap_or_else(|| path.to_path_buf());
    let inventory = discover_auggie_sources(path)?;
    let known_routes = known_auggie_routes(store, &context.machine_id, &configured_source_root)?;

    if import_options.import_profile.is_replay_only() {
        replay_outputs_or_mark_behind(
            store,
            &inventory.paths,
            &configured_source_root,
            &context,
            &import_options,
        );
        return Ok(ProviderImportSummary::default());
    }

    if inventory.paths.is_empty() && known_routes.is_empty() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "no Auggie session JSON files were found",
        });
    }

    let committed_store = Store::open_read_only(store.path())?;
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let mut summary = ProviderImportSummary::default();
        let mut changed_groups = 0_usize;
        let mut relationships = Vec::new();
        let mut session_index = current_session_index(&known_routes, &inventory.paths);
        let known_by_path = known_routes
            .iter()
            .map(|route| (route.path.clone(), route))
            .collect::<BTreeMap<_, _>>();

        for source_path in &inventory.paths {
            let parsed = match parse_auggie_source(
                source_path,
                &context,
                import_options.inventory_observation_token.as_deref(),
                import_options.import_profile.sink().is_some(),
            ) {
                Ok(parsed) => parsed,
                Err(error) => {
                    record_auggie_source_parse_error(&mut summary, 1, error)?;
                    continue;
                }
            };
            let completion = import_auggie_source(
                store,
                &committed_store,
                &bulk_guard,
                &configured_source_root,
                &context,
                &import_options,
                &parsed,
                known_by_path.get(source_path).copied(),
                &session_index,
                &mut summary,
            )?;
            changed_groups = changed_groups.saturating_add(completion.changed_groups);
            session_index_insert(
                &mut session_index,
                parsed.session.provider_session_id.clone(),
                completion.session_id,
            );
            relationships.push(RelationshipFact {
                path: source_path.clone(),
                stamp: parsed.stamp.clone(),
                provider_session_id: parsed.session.provider_session_id.clone(),
                parent_provider_session_id: parsed.session.parent_provider_session_id.clone(),
                root_provider_session_id: parsed.session.root_provider_session_id.clone(),
                session_id: completion.session_id,
            });

            if completion.terminal {
                replay_parsed_outputs_or_mark_behind(
                    &parsed,
                    &configured_source_root,
                    import_options.import_profile.sink().map(AsRef::as_ref),
                );
            }
            if stop_after_changed_group(&import_options, changed_groups) {
                summary.work_remaining = true;
                return Ok(summary);
            }
        }

        for route in known_routes
            .iter()
            .filter(|route| !inventory.paths.contains(&route.path))
        {
            let changed = retire_auggie_route(
                store,
                &bulk_guard,
                &context,
                route,
                if inventory.root_missing {
                    ProviderSourceRouteRetirementReason::RootMissing
                } else {
                    ProviderSourceRouteRetirementReason::SourceMissing
                },
            )?;
            if changed {
                summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
                summary.skipped = summary.skipped.saturating_add(1);
                summary.set_work_result(ProviderImportWorkResult::Changed);
                changed_groups = changed_groups.saturating_add(1);
            }
            if stop_after_changed_group(&import_options, changed_groups) {
                summary.work_remaining = true;
                return Ok(summary);
            }
        }

        for relationship in &relationships {
            if reconcile_auggie_relationship(
                store,
                &bulk_guard,
                &context,
                relationship,
                &session_index,
            )? {
                summary.set_work_result(ProviderImportWorkResult::Changed);
                changed_groups = changed_groups.saturating_add(1);
            }
            if stop_after_changed_group(&import_options, changed_groups) {
                summary.work_remaining = true;
                return Ok(summary);
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

pub(in crate::provider::providers::auggie) fn record_auggie_source_parse_error(
    summary: &mut ProviderImportSummary,
    line: usize,
    error: CaptureError,
) -> Result<()> {
    match error {
        error @ CaptureError::InvalidPayload(_) => {
            summary.record_failure(ProviderImportFailure {
                line,
                error: error.to_string(),
            });
            Ok(())
        }
        error => Err(error),
    }
}

pub(super) fn stop_after_changed_group(
    options: &ProviderImportOptions,
    changed_groups: usize,
) -> bool {
    options.capture_work_limit == CaptureWorkLimit::OneSafeGroup && changed_groups != 0
}

pub(super) fn classify_cursor(
    stored: Option<&SyncCursor>,
    parsed: &ParsedAuggieSource,
) -> Result<CursorPlan> {
    let Some(stored) = stored else {
        return Ok(CursorPlan::Publish {
            expected_cursor: None,
            generation: 0,
            next_event: 0,
            rejected_records: 0,
        });
    };
    if let Ok(committed) = decode_native_path_committed_cursor(&stored.cursor) {
        let prior = decode_cursor(committed.provider_cursor())?;
        validate_native_cursor(&prior, &parsed.stamp.canonical_path)?;
        if prior.provider_session_id != parsed.session.provider_session_id {
            return Ok(CursorPlan::Publish {
                expected_cursor: Some(stored.cursor.clone()),
                generation: prior.generation.checked_add(1).ok_or(
                    CaptureError::SystemInvariant("Auggie source generation exhausted"),
                )?,
                next_event: 0,
                rejected_records: 0,
            });
        }
        let prior_next = usize::try_from(prior.next_event).map_err(|_| {
            CaptureError::InvalidPayload(
                "Auggie cursor event frontier exceeds platform limits".into(),
            )
        })?;
        if prior.source_revision == parsed.source_revision {
            if prior_next > parsed.events.len()
                || event_prefix_digest(&parsed.events[..prior_next])? != prior.prefix_sha256
            {
                return Err(CaptureError::InvalidPayload(
                    "Auggie NativePath cursor does not match its certified event prefix".to_owned(),
                ));
            }
            if prior.terminal {
                if prior_next != parsed.events.len()
                    || prior.event_count != u64::try_from(parsed.events.len()).unwrap_or(u64::MAX)
                {
                    return Err(CaptureError::InvalidPayload(
                        "Auggie terminal cursor does not match its source".to_owned(),
                    ));
                }
                return Ok(CursorPlan::AlreadyCommitted(prior));
            }
            return Ok(CursorPlan::Publish {
                expected_cursor: Some(stored.cursor.clone()),
                generation: prior.generation,
                next_event: prior_next,
                rejected_records: prior.rejected_records,
            });
        }
        let prefix_matches = prior_next <= parsed.events.len()
            && event_prefix_digest(&parsed.events[..prior_next])? == prior.prefix_sha256;
        if prefix_matches {
            return Ok(CursorPlan::Publish {
                expected_cursor: Some(stored.cursor.clone()),
                generation: prior.generation,
                next_event: prior_next,
                rejected_records: prior.rejected_records,
            });
        }
        return Ok(CursorPlan::Publish {
            expected_cursor: Some(stored.cursor.clone()),
            generation: prior
                .generation
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "Auggie source generation exhausted",
                ))?,
            next_event: 0,
            rejected_records: 0,
        });
    }

    if CertifiedProviderCursor::decode_if_certified(&stored.cursor)?.is_none() {
        return Err(CaptureError::InvalidPayload(
            "Auggie cursor is neither NativePath nor a released migration cursor".to_owned(),
        ));
    }
    Ok(CursorPlan::Publish {
        expected_cursor: Some(stored.cursor.clone()),
        generation: 0,
        next_event: 0,
        rejected_records: 0,
    })
}

fn known_auggie_routes(
    store: &Store,
    machine_id: &str,
    source_root: &Path,
) -> Result<Vec<KnownAuggieRoute>> {
    let source_root = source_root.display().to_string();
    let mut routes = BTreeMap::<String, KnownAuggieRoute>::new();
    for source in store.list_capture_sources()? {
        if source.descriptor.provider != CaptureProvider::Auggie
            || source.descriptor.machine_id != machine_id
            || source.descriptor.source_format.as_deref() != Some(AUGGIE_SESSION_JSON_SOURCE_FORMAT)
            || source.descriptor.source_root.as_deref() != Some(source_root.as_str())
        {
            continue;
        }
        let (Some(raw_source_path), Some(canonical_source_identity), Some(provider_session_id)) = (
            source.descriptor.raw_source_path.as_deref(),
            source.descriptor.source_identity.as_deref(),
            source.descriptor.external_session_id.as_deref(),
        ) else {
            continue;
        };
        let path = PathBuf::from(raw_source_path);
        let locator_identity = provider_path_identity(&path)?;
        let stream = provider_source_cursor_stream_for_path(
            CaptureProvider::Auggie,
            AUGGIE_SESSION_JSON_SOURCE_FORMAT,
            &locator_identity,
        );
        let Some(current_cursor) = store.get_sync_cursor(None, machine_id, &stream)? else {
            continue;
        };
        let source_revision = source
            .sync
            .metadata
            .get("source_revision")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                CaptureError::InvalidPayload(
                    "Auggie persisted source is missing its source revision".to_owned(),
                )
            })?
            .to_owned();
        let provider_cursor = migrate_or_decode_known_cursor(
            &current_cursor.cursor,
            &path,
            &source_revision,
            provider_session_id,
        )?;
        let session = store
            .session_by_capture_source_and_external_session(
                source.id,
                CaptureProvider::Auggie,
                provider_session_id,
            )?
            .ok_or_else(|| {
                CaptureError::InvalidPayload(
                    "Auggie persisted source has no canonical session".to_owned(),
                )
            })?;
        let route = KnownAuggieRoute {
            path,
            locator_identity: locator_identity.clone(),
            canonical_source_identity: canonical_source_identity.to_owned(),
            source_revision,
            session_id: session.id,
            provider_session_id: provider_session_id.to_owned(),
            current_cursor,
            provider_cursor,
        };
        if routes.insert(locator_identity, route).is_some() {
            return Err(CaptureError::SystemInvariant(
                "Auggie persisted duplicate current routes for one transcript",
            ));
        }
    }
    Ok(routes.into_values().collect())
}

fn migrate_or_decode_known_cursor(
    encoded: &str,
    path: &Path,
    source_revision: &str,
    provider_session_id: &str,
) -> Result<AuggieNativeCursor> {
    if let Ok(committed) = decode_native_path_committed_cursor(encoded) {
        let cursor = decode_cursor(committed.provider_cursor())?;
        validate_native_cursor(&cursor, path)?;
        return Ok(cursor);
    }
    if CertifiedProviderCursor::decode_if_certified(encoded)?.is_none() {
        return Err(CaptureError::InvalidPayload(
            "Auggie persisted route has an unsupported released cursor".to_owned(),
        ));
    }
    Ok(AuggieNativeCursor {
        version: AUGGIE_NATIVE_CURSOR_VERSION,
        parser_revision: AUGGIE_PARSER_REVISION.to_owned(),
        policy_revision: AUGGIE_POLICY_REVISION.to_owned(),
        source_path: path.to_path_buf(),
        source_revision: source_revision.to_owned(),
        generation: 0,
        next_event: 0,
        prefix_sha256: empty_digest(),
        terminal: true,
        event_count: 0,
        provider_session_id: provider_session_id.to_owned(),
        rejected_records: 0,
    })
}

fn retire_auggie_route(
    store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    route: &KnownAuggieRoute,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<bool> {
    let stream = route.current_cursor.stream.clone();
    let transition = NativePathCursorTransition::new(
        Some(route.current_cursor.cursor.clone()),
        provider_sync_cursor(
            &context.machine_id,
            stream.clone(),
            encode_cursor(&route.provider_cursor)?,
            context.imported_at,
        ),
    );
    let retirement = ProviderSourceRouteRetirement {
        provider: CaptureProvider::Auggie,
        source_format: AUGGIE_SESSION_JSON_SOURCE_FORMAT.to_owned(),
        machine_id: context.machine_id.clone(),
        locator_identity: route.locator_identity.clone(),
        cursor_stream: stream,
        expected_canonical_source_identity: route.canonical_source_identity.clone(),
        expected_source_revision: route.source_revision.clone(),
        retired_at_ms: context.imported_at.timestamp_millis(),
        reason,
    };
    let publication_id = retirement_publication_id(&retirement);
    if decode_native_path_committed_cursor(&route.current_cursor.cursor)
        .is_ok_and(|committed| committed.publication_id() == publication_id)
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

fn current_session_index(
    known_routes: &[KnownAuggieRoute],
    live_paths: &BTreeSet<PathBuf>,
) -> BTreeMap<String, Option<Uuid>> {
    let mut index = BTreeMap::new();
    for route in known_routes
        .iter()
        .filter(|route| live_paths.contains(&route.path))
    {
        session_index_insert(
            &mut index,
            route.provider_session_id.clone(),
            route.session_id,
        );
    }
    index
}

fn session_index_insert(
    index: &mut BTreeMap<String, Option<Uuid>>,
    provider_session_id: String,
    session_id: Uuid,
) {
    match index.entry(provider_session_id) {
        std::collections::btree_map::Entry::Vacant(entry) => {
            entry.insert(Some(session_id));
        }
        std::collections::btree_map::Entry::Occupied(mut entry) => {
            if entry.get().is_some_and(|existing| existing != session_id) {
                entry.insert(None);
            }
        }
    }
}

pub(super) fn unique_session_id(
    index: &BTreeMap<String, Option<Uuid>>,
    provider_session_id: &str,
) -> Option<Uuid> {
    index.get(provider_session_id).copied().flatten()
}

fn reconcile_auggie_relationship(
    store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    relationship: &RelationshipFact,
    session_index: &BTreeMap<String, Option<Uuid>>,
) -> Result<bool> {
    let mut session = store.get_session(relationship.session_id)?;
    let parent_session_id = relationship
        .parent_provider_session_id
        .as_ref()
        .and_then(|provider_id| unique_session_id(session_index, provider_id));
    let root_session_id = relationship
        .root_provider_session_id
        .as_ref()
        .and_then(|provider_id| unique_session_id(session_index, provider_id))
        .or(parent_session_id);
    if session.parent_session_id == parent_session_id && session.root_session_id == root_session_id
    {
        return Ok(false);
    }
    if !relationship.stamp.revalidate()? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    session.parent_session_id = parent_session_id;
    session.root_session_id = root_session_id;
    session.timestamps.updated_at = context.imported_at;
    let locator_identity = provider_path_identity(&relationship.path)?;
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Auggie,
        AUGGIE_SESSION_JSON_SOURCE_FORMAT,
        &locator_identity,
    );
    let stored = store
        .get_sync_cursor(None, &context.machine_id, &stream)?
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Auggie relationship reconciliation requires committed Core".to_owned(),
            )
        })?;
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    let provider_cursor = decode_cursor(committed.provider_cursor())?;
    if !provider_cursor.terminal || !relationship.stamp.revalidate()? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let transition = NativePathCursorTransition::new(
        Some(stored.cursor),
        provider_sync_cursor(
            &context.machine_id,
            stream,
            encode_cursor(&provider_cursor)?,
            context.imported_at,
        ),
    );
    let publication_id = relationship_publication_id(
        relationship,
        parent_session_id,
        root_session_id,
        &transition,
    );
    let retained_bytes = serde_json::to_vec(&session)?
        .len()
        .saturating_add(PAGE_ACCOUNTING_OVERHEAD_BYTES)
        .min(ctx_history_store::NATIVE_PATH_MAX_RETAINED_PAGE_BYTES);
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(
        admission,
        NativePathGroupAccounting::new(1, 1, retained_bytes)?,
    )?;
    let changed =
        match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
            NativePathCursorSetClassification::AllExpected => {
                group.upsert_session(&session)?;
                group.prepare_journal_checkpoint()?;
                group.publish_cursor_set()?;
                true
            }
            NativePathCursorSetClassification::AllNextSameGroup { .. } => false,
        };
    group.commit()?;
    Ok(changed)
}
