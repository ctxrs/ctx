use super::*;

pub(super) fn import_core(
    path: &Path,
    store: &mut Store,
    conn: &Connection,
    sql: &AstrBotSql,
    snapshot: &crate::provider::sqlite::ProviderSqliteSourceSnapshot,
    authority: &SourceAuthority,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    prior: PriorCursor,
) -> Result<ProviderImportSummary> {
    let (start, generation, mut rejected_records, mut expected_encoded) =
        classify_core_start(conn, sql, authority, prior)?;
    let mut reader = AstrBotReader::new(conn, AstrBotSql::new(conn)?, start);
    let committed_store = Store::open_read_only(store.path())?;
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let mut summary = ProviderImportSummary::default();
        let mut changed_groups = 0_usize;
        let mut accounted_sessions = BTreeSet::new();
        while let Some(page) = reader.next_page(false)? {
            if !snapshot.revalidate(path)? {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            rejected_records = rejected_records
                .saturating_add(u64::try_from(page.rejections.len()).unwrap_or(u64::MAX));
            let next_cursor = AstrBotStoreCursor {
                version: CURSOR_VERSION,
                provider: CaptureProvider::AstrBot.as_str().to_owned(),
                source_format: ASTRBOT_SQLITE_SOURCE_FORMAT.to_owned(),
                locator_identity: authority.locator_identity.clone(),
                source_identity: authority.source_identity.clone(),
                source_revision: authority.source_revision.clone(),
                source_incarnation: authority.source_incarnation.clone(),
                schema_authority: authority.schema_authority.clone(),
                frontier: page.next_frontier.clone(),
                terminal: page.terminal,
                generation,
                rejected_records,
                retired: false,
            };
            let page_summary = publish_core_page(
                store,
                &committed_store,
                &bulk_guard,
                snapshot,
                path,
                authority,
                context,
                options,
                &page,
                expected_encoded.clone(),
                &next_cursor,
                &mut accounted_sessions,
            )?;
            if page_summary.work_result() == ProviderImportWorkResult::Changed {
                changed_groups = changed_groups.saturating_add(1);
            }
            summary.merge_from(page_summary);
            expected_encoded = store
                .get_sync_cursor(None, &context.machine_id, &authority.cursor_stream)?
                .map(|cursor| cursor.cursor);
            if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup
                && changed_groups != 0
                && !page.terminal
            {
                summary.work_remaining = true;
                break;
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

pub(super) fn classify_core_start(
    conn: &Connection,
    sql: &AstrBotSql,
    authority: &SourceAuthority,
    prior: PriorCursor,
) -> Result<(AstrBotFrontier, u64, u64, Option<String>)> {
    match prior {
        PriorCursor::None => Ok((AstrBotFrontier::initial(), 0, 0, None)),
        PriorCursor::Released {
            encoded,
            rejected_records,
        } => Ok((
            AstrBotFrontier::initial(),
            0,
            rejected_records,
            Some(encoded),
        )),
        PriorCursor::Native { encoded, cursor } => {
            if cursor.locator_identity != authority.locator_identity
                || cursor.source_identity != authority.source_identity
            {
                return Err(CaptureError::InvalidPayload(
                    "AstrBot NativePath cursor route does not match this source".to_owned(),
                ));
            }
            if cursor.retired {
                return Ok((
                    AstrBotFrontier::initial(),
                    cursor.generation.saturating_add(1),
                    0,
                    Some(encoded),
                ));
            }
            if cursor.schema_authority == authority.schema_authority
                && cursor.source_revision == authority.source_revision
                && !cursor.terminal
            {
                return Ok((
                    cursor.frontier,
                    cursor.generation,
                    cursor.rejected_records,
                    Some(encoded),
                ));
            }
            let same_incarnation = cursor.source_incarnation == authority.source_incarnation;
            let append_safe = same_incarnation
                && cursor.schema_authority == authority.schema_authority
                && validate_frontier(conn, sql, &cursor.frontier)?;
            if append_safe {
                return Ok((
                    cursor.frontier.append_start(),
                    cursor.generation,
                    cursor.rejected_records,
                    Some(encoded),
                ));
            }
            Ok((
                AstrBotFrontier::initial(),
                cursor
                    .generation
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "AstrBot NativePath generation exhausted",
                    ))?,
                0,
                Some(encoded),
            ))
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_core_page(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    snapshot: &crate::provider::sqlite::ProviderSqliteSourceSnapshot,
    path: &Path,
    authority: &SourceAuthority,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    page: &AstrBotPage,
    expected_encoded: Option<String>,
    next_cursor: &AstrBotStoreCursor,
    accounted_sessions: &mut BTreeSet<String>,
) -> Result<ProviderImportSummary> {
    let provider_cursor = next_cursor.encode()?;
    let next = SyncCursor {
        id: Uuid::new_v4(),
        team_id: None,
        device_id: context.machine_id.clone(),
        stream: authority.cursor_stream.clone(),
        cursor: provider_cursor,
        last_synced_at: Some(context.imported_at),
        timestamps: timestamps(context.imported_at),
    };
    let transition = NativePathCursorTransition::new(expected_encoded, next);
    let publication_id = page_publication_id(authority, page, next_cursor)?;
    let retained_bytes = page
        .retained_core_bytes
        .saturating_add(transition.next().cursor.len())
        .min(PAGE_MAX_CORE_BYTES);
    let accounting = NativePathGroupAccounting::new(1, 1, retained_bytes)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
        NativePathCursorSetClassification::AllNextSameGroup { .. } => {
            group.commit()?;
            let mut summary = ProviderImportSummary::default();
            summary.skipped_sessions = page
                .units
                .iter()
                .filter(|unit| accounted_sessions.insert(unit.session.provider_session_id.clone()))
                .count();
            summary.skipped_events = page
                .units
                .iter()
                .filter(|unit| unit.event.is_some())
                .count();
            summary.skipped = summary
                .skipped_sessions
                .saturating_add(summary.skipped_events);
            summary.set_work_result(ProviderImportWorkResult::NoOp);
            return Ok(summary);
        }
        NativePathCursorSetClassification::AllExpected => {}
    }

    let resolution =
        group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::AstrBot,
            source_format: ASTRBOT_SQLITE_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity: authority.locator_identity.clone(),
            cursor_stream: authority.cursor_stream.clone(),
            proposed_source_identity: authority.source_identity.clone(),
            raw_source_path: Some(authority.raw_source_path.clone()),
            source_revision: authority.source_revision.clone(),
            observed_at_ms: context.imported_at.timestamp_millis(),
        })?;

    let mut summary = ProviderImportSummary::default();
    let mut sessions = BTreeMap::<String, (SessionFact, Uuid, Session)>::new();
    for unit in &page.units {
        if sessions.contains_key(&unit.session.provider_session_id) {
            continue;
        }
        let source_id = provider_scoped_source_uuid(
            CaptureProvider::AstrBot,
            &unit.session.provider_session_id,
            ASTRBOT_SQLITE_SOURCE_FORMAT,
            Some(&authority.raw_source_path),
        );
        let source = if unit.session.preserve_existing {
            match committed_store.get_capture_source(source_id) {
                Ok(existing) => existing,
                Err(StoreError::NotFound(_)) => capture_source(
                    authority,
                    context,
                    &unit.session,
                    source_id,
                    &resolution.canonical_source_identity,
                ),
                Err(error) => return Err(error.into()),
            }
        } else {
            capture_source(
                authority,
                context,
                &unit.session,
                source_id,
                &resolution.canonical_source_identity,
            )
        };
        group.upsert_capture_source(&source)?;
        group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;
        let session = session(
            committed_store,
            authority,
            context,
            options,
            &unit.session,
            source_id,
            &resolution.canonical_source_identity,
        )?;
        let existed = committed_store.get_session(session.id).is_ok();
        group.upsert_session(&session)?;
        if accounted_sessions.insert(unit.session.provider_session_id.clone()) {
            if existed {
                summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
                summary.skipped = summary.skipped.saturating_add(1);
            } else {
                summary.imported_sessions = summary.imported_sessions.saturating_add(1);
                summary.imported = summary.imported.saturating_add(1);
            }
        }
        sessions.insert(
            unit.session.provider_session_id.clone(),
            (unit.session.clone(), source_id, session),
        );
    }

    for unit in &page.units {
        let Some(fact) = &unit.event else {
            continue;
        };
        let (_, source_id, session) = sessions.get(&unit.session.provider_session_id).ok_or(
            CaptureError::SystemInvariant("AstrBot NativePath event lost its session"),
        )?;
        let normalized = normalized_event(
            committed_store,
            options,
            &unit.session,
            *source_id,
            session,
            fact,
        )?;
        if reconcile_astrbot_event(
            &mut group,
            committed_store,
            context,
            session,
            fact,
            normalized,
        )? {
            summary.imported_events = summary.imported_events.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        } else {
            summary.skipped_events = summary.skipped_events.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
        }
    }
    for rejection in &page.rejections {
        summary.record_failure(ProviderImportFailure {
            line: rejection.line,
            error: rejection.detail.clone(),
        });
    }

    if !snapshot.revalidate(path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}
