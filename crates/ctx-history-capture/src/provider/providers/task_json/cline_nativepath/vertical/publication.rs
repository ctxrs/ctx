use super::*;

pub(super) fn publish_task_json_core_page(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    configured_source_root: &Path,
    options: &TaskJsonNativeImportOptions,
    dialect: TaskJsonNativeDialect,
    publication_page: NativePublicationPage<ClineCertifiedPage>,
) -> Result<CorePublicationOutcome> {
    publish_cline_core_page_inner(
        store,
        committed_store,
        bulk_guard,
        &ClineFreshPublicationContext {
            options,
            configured_source_root,
            dialect,
        },
        publication_page,
    )
    .map_err(map_vertical_error)
}

pub(super) fn publish_cline_core_page_inner(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ClineFreshPublicationContext<'_>,
    publication_page: NativePublicationPage<ClineCertifiedPage>,
) -> std::result::Result<CorePublicationOutcome, ClineNativeVerticalError> {
    let (source_identity, page) = publication_page.into_parts();
    validate_source_identity(context.dialect, &source_identity, &page)?;
    revalidate_page_source(&page)?;

    let stream = component_cursor_stream(context.dialect, &page.core.source.canonical_path)?;
    let stored = store.get_sync_cursor(None, &context.options.machine_id, &stream)?;
    let plan = classify_component_cursor(stored.as_ref(), context, &stream, &page)?;
    let ComponentCursorPlan::Publish {
        transition,
        generation,
        rejected_records,
    } = plan
    else {
        let mut summary = ProviderImportSummary::default();
        summary.skipped_events = page.core.core.events.len();
        summary.skipped = summary.skipped.saturating_add(summary.skipped_events);
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(CorePublicationOutcome {
            summary,
            relocated_task_identities: Box::new([]),
        });
    };
    let next = component_sync_cursor(context, &stream, &page, generation, rejected_records)?;
    let transition =
        NativePathCursorTransition::new(transition.expected_cursor().map(str::to_owned), next);
    let publication_id = page_publication_id(context.dialect, &source_identity, &page, &transition);
    let accounting =
        NativePathGroupAccounting::new(1, 1, page.accounting.conservative_serialized_bytes)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
        NativePathCursorSetClassification::AllNextSameGroup { .. } => {
            group.commit()?;
            let mut summary = ProviderImportSummary::default();
            summary.set_work_result(ProviderImportWorkResult::NoOp);
            return Ok(CorePublicationOutcome {
                summary,
                relocated_task_identities: Box::new([]),
            });
        }
        NativePathCursorSetClassification::AllExpected => {}
    }

    let mut summary = ProviderImportSummary::default();
    let resolved = resolve_fresh_source(
        committed_store,
        &mut group,
        context,
        &page.core,
        &mut summary,
    )?;
    publish_page_events(
        committed_store,
        &mut group,
        context,
        &resolved,
        generation,
        &page.core.source,
        &page.core.core.events,
        &mut summary,
    )?;
    for rejection in &page.core.core.rejections {
        summary.record_failure(crate::ProviderImportFailure {
            line: usize::try_from(rejection.native_index)
                .unwrap_or(usize::MAX)
                .saturating_add(1),
            error: rejection.detail.to_string(),
        });
    }

    revalidate_page_source(&page)?;
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    let relocated_task_identities = if resolved.relocated {
        std::iter::once(page.core.source.task.as_str().to_owned())
            .chain(
                page.core
                    .source
                    .task_aliases
                    .iter()
                    .map(|alias| alias.as_str().to_owned()),
            )
            .collect::<Vec<_>>()
            .into_boxed_slice()
    } else {
        Box::new([])
    };
    Ok(CorePublicationOutcome {
        summary,
        relocated_task_identities,
    })
}

pub(super) fn classify_component_cursor(
    stored: Option<&SyncCursor>,
    context: &ClineFreshPublicationContext<'_>,
    stream: &str,
    page: &NativeIngestionPage<ClineCertifiedPage>,
) -> std::result::Result<ComponentCursorPlan, ClineNativeVerticalError> {
    let page_revision = revision(&page.core.source_revision.revision_sha256);
    let Some(stored) = stored else {
        if page.core.expected_frontier.next_native_index != 0 {
            return Err(ClineNativeVerticalError::CorruptCursor);
        }
        return Ok(ComponentCursorPlan::Publish {
            transition: NativePathCursorTransition::new(
                None,
                component_sync_cursor(context, stream, page, 0, 0)?,
            ),
            generation: 0,
            rejected_records: 0,
        });
    };
    let committed = decode_native_path_committed_cursor(&stored.cursor)
        .map_err(|_| ClineNativeVerticalError::CorruptCursor)?;
    let prior = ClineNativeStoreCursor::decode(committed.provider_cursor())
        .map_err(|_| ClineNativeVerticalError::CorruptCursor)?;
    if !matches!(
        prior.version,
        ClineNativeStoreCursor::LEGACY_VERSION | ClineNativeStoreCursor::VERSION
    ) || prior.provider != context.dialect.provider.as_str()
        || prior.source_identity != page.core.source.stable_id.as_ref()
        || !cursor_task_authority_matches(&prior, &page.core.source)
    {
        return Err(ClineNativeVerticalError::CorruptCursor);
    }
    if prior.source_revision == page_revision {
        if prior.frontier == page.core.next_safe_frontier
            || prior.frontier.next_native_index > page.core.next_safe_frontier.next_native_index
        {
            return Ok(ComponentCursorPlan::AlreadyCommitted);
        }
        if prior.frontier != page.core.expected_frontier {
            return Err(ClineNativeVerticalError::CorruptCursor);
        }
        return Ok(ComponentCursorPlan::Publish {
            transition: NativePathCursorTransition::new(
                Some(stored.cursor.clone()),
                component_sync_cursor(
                    context,
                    stream,
                    page,
                    prior.generation,
                    prior.rejected_records,
                )?,
            ),
            generation: prior.generation,
            rejected_records: prior.rejected_records,
        });
    }
    if prior.frontier == page.core.expected_frontier
        && page.core.expected_frontier.next_native_index != 0
    {
        return Ok(ComponentCursorPlan::Publish {
            transition: NativePathCursorTransition::new(
                Some(stored.cursor.clone()),
                component_sync_cursor(
                    context,
                    stream,
                    page,
                    prior.generation,
                    prior.rejected_records,
                )?,
            ),
            generation: prior.generation,
            rejected_records: prior.rejected_records,
        });
    }
    if page.core.expected_frontier.next_native_index != 0 {
        return Err(ClineNativeVerticalError::CorruptCursor);
    }
    let generation = prior
        .generation
        .checked_add(1)
        .ok_or(ClineNativeVerticalError::GenerationExhausted)?;
    Ok(ComponentCursorPlan::Publish {
        transition: NativePathCursorTransition::new(
            Some(stored.cursor.clone()),
            component_sync_cursor(context, stream, page, generation, 0)?,
        ),
        generation,
        rejected_records: 0,
    })
}

pub(super) fn validate_source_identity(
    dialect: TaskJsonNativeDialect,
    source_identity: &NativeSourceIdentity,
    page: &NativeIngestionPage<ClineCertifiedPage>,
) -> std::result::Result<(), ClineNativeVerticalError> {
    if source_identity.provider() != dialect.provider.as_str()
        || source_identity.source_identity() != page.core.source.stable_id.as_ref()
        || page.core.source.provider != dialect.provider.as_str()
    {
        return Err(ClineNativeVerticalError::SourceIdentityMismatch);
    }
    Ok(())
}

pub(super) fn cursor_task_authority_matches(
    prior: &ClineNativeStoreCursor,
    source: &ClineFileSourceIdentity,
) -> bool {
    if prior.version == ClineNativeStoreCursor::LEGACY_VERSION {
        return prior.task_identity.is_none()
            && prior.task_identity_origin.is_none()
            && prior.task_identity_aliases.is_empty();
    }
    let (Some(prior_identity), Some(prior_origin)) =
        (prior.task_identity.as_deref(), prior.task_origin())
    else {
        return false;
    };
    let current_aliases = source
        .task_aliases
        .iter()
        .map(ClineTaskIdentity::as_str)
        .collect::<BTreeSet<_>>();
    if !prior
        .task_identity_aliases
        .iter()
        .all(|alias| alias == source.task.as_str() || current_aliases.contains(alias.as_str()))
    {
        return false;
    }
    if prior_identity == source.task.as_str() {
        return !matches!(
            (prior_origin, source.task_origin),
            (
                ClineTaskIdentityOrigin::TaskMetadata,
                ClineTaskIdentityOrigin::DirectoryNameDegraded
            )
        );
    }
    prior_origin == ClineTaskIdentityOrigin::DirectoryNameDegraded
        && source.task_origin == ClineTaskIdentityOrigin::TaskMetadata
        && current_aliases.contains(prior_identity)
}

pub(super) fn revalidate_page_source(
    page: &NativeIngestionPage<ClineCertifiedPage>,
) -> std::result::Result<(), ClineNativeVerticalError> {
    if !super::super::revalidate_cline_component_source(
        &page.core.source.canonical_path,
        page.core.source.component,
        &page.core.source_revision.observed_stamp_token,
    )? {
        return Err(ClineNativeVerticalError::SourceChanged);
    }
    Ok(())
}

pub(super) fn resolve_fresh_source(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    context: &ClineFreshPublicationContext<'_>,
    page: &ClineCertifiedPage,
    summary: &mut ProviderImportSummary,
) -> std::result::Result<ResolvedClineSource, ClineNativeVerticalError> {
    let session_fact = page.core.session.as_ref().or_else(|| {
        page.core
            .terminal_metadata_checkpoint
            .as_deref()
            .map(|checkpoint| &checkpoint.session)
    });
    let task_path = page
        .source
        .canonical_path
        .parent()
        .ok_or(CaptureError::SystemInvariant(
            "Cline component path has no task directory",
        ))?;
    let raw_source_path = task_path.display().to_string();
    let source_root = context.configured_source_root.display().to_string();
    let task_id = page.source.task.as_str();
    let locator_identity = provider_path_identity(task_path)?;
    let proposed_source_identity = provider_source_identity(
        context.dialect.provider,
        context.dialect.source_format,
        Some(&source_root),
        Some(&raw_source_path),
        None,
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Cline NativePath source has no canonical identity",
    ))?;
    let route_stream = task_cursor_stream(context.dialect, task_path)?;
    let source_revision = task_route_revision(context.dialect, task_id);
    let resolution =
        group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: context.dialect.provider,
            source_format: context.dialect.source_format.to_owned(),
            machine_id: context.options.machine_id.clone(),
            locator_identity,
            cursor_stream: route_stream,
            proposed_source_identity,
            raw_source_path: Some(raw_source_path.clone()),
            source_revision: source_revision.clone(),
            observed_at_ms: context.options.imported_at.timestamp_millis(),
        })?;
    let task_id_candidates = std::iter::once(task_id)
        .chain(
            page.source
                .task_aliases
                .iter()
                .map(ClineTaskIdentity::as_str),
        )
        .collect::<Vec<_>>();
    let mut existing_source = None;
    for candidate in &task_id_candidates {
        existing_source = committed_store.capture_source_by_canonical_identity_session(
            context.dialect.provider,
            context.dialect.source_format,
            &context.options.machine_id,
            &resolution.canonical_source_identity,
            candidate,
        )?;
        if existing_source.is_some() {
            break;
        }
    }
    let source_id = existing_source
        .as_ref()
        .map(|source| source.id)
        .unwrap_or_else(|| {
            provider_scoped_source_uuid(
                context.dialect.provider,
                task_id,
                context.dialect.source_format,
                Some(&raw_source_path),
            )
        });
    if let Some(session_fact) = session_fact {
        group.upsert_capture_source(&cline_capture_source(
            context,
            session_fact.identity.as_str(),
            source_id,
            &raw_source_path,
            &source_root,
            &resolution.canonical_source_identity,
            &source_revision,
            session_fact.workspace_directory.as_deref(),
            parse_timestamp(
                session_fact.created_at.as_deref(),
                context.options.imported_at,
            ),
        ))?;
    } else {
        let source = existing_source.ok_or(ClineNativeVerticalError::MissingSession)?;
        group.upsert_capture_source(&source)?;
    }
    group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;
    let session = if let Some(session_fact) = session_fact {
        let mut existing_session = None;
        for candidate in &task_id_candidates {
            existing_session = committed_store.session_by_capture_source_and_external_session(
                source_id,
                context.dialect.provider,
                candidate,
            )?;
            if existing_session.is_some() {
                break;
            }
        }
        let session = cline_session(
            committed_store,
            context,
            session_fact,
            source_id,
            &resolution.canonical_source_identity,
            existing_session.as_ref().map(|session| session.id),
        )?;
        let existed = existing_session.is_some() || committed_store.get_session(session.id).is_ok();
        group.upsert_session(&session)?;
        if existed {
            summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
        } else {
            summary.imported_sessions = summary.imported_sessions.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        }
        session
    } else {
        let mut session = None;
        for candidate in &task_id_candidates {
            session = committed_store.session_by_capture_source_and_external_session(
                source_id,
                context.dialect.provider,
                candidate,
            )?;
            if session.is_some() {
                break;
            }
        }
        session.ok_or(ClineNativeVerticalError::MissingSession)?
    };
    Ok(ResolvedClineSource {
        source_id,
        session,
        relocated: resolution.relocated,
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn cline_capture_source(
    context: &ClineFreshPublicationContext<'_>,
    task_id: &str,
    source_id: Uuid,
    raw_source_path: &str,
    source_root: &str,
    canonical_source_identity: &str,
    source_revision: &str,
    cwd: Option<&str>,
    started_at: DateTime<Utc>,
) -> CaptureSource {
    CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: context.dialect.provider,
            machine_id: context.options.machine_id.clone(),
            process_id: None,
            cwd: cwd.map(str::to_owned),
            raw_source_path: Some(raw_source_path.to_owned()),
            source_format: Some(context.dialect.source_format.to_owned()),
            source_root: Some(source_root.to_owned()),
            source_identity: Some(canonical_source_identity.to_owned()),
            external_session_id: Some(task_id.to_owned()),
        },
        started_at,
        ended_at: None,
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": task_id,
                "source_format": context.dialect.source_format,
                "source_trust": "provider_native",
                "imported_at": context.options.imported_at,
                "source_identity": canonical_source_identity,
                "source_root": source_root,
                "source_revision": source_revision,
                "source_identity_key": provider_scoped_source_identity_key(
                    context.dialect.provider,
                    task_id,
                    context.dialect.source_format,
                    Some(raw_source_path),
                ),
                "nativepath_publication": context.dialect.publication_revision,
            }),
        ),
    }
}

pub(super) fn cline_session(
    committed_store: &Store,
    context: &ClineFreshPublicationContext<'_>,
    fact: &super::ClineSessionRow,
    source_id: Uuid,
    canonical_source_identity: &str,
    existing_id: Option<Uuid>,
) -> std::result::Result<Session, ClineNativeVerticalError> {
    let id = match existing_id {
        Some(id) => id,
        None => provider_import_session_uuid(
            committed_store,
            context.dialect.provider,
            fact.identity.as_str(),
            source_id,
            Some(canonical_source_identity),
        )?,
    };
    let started_at = parse_timestamp(fact.created_at.as_deref(), context.options.imported_at);
    let ended_at = fact
        .last_modified
        .as_deref()
        .and_then(crate::common::time::parse_rfc3339_utc);
    Ok(Session {
        id,
        history_record_id: context.options.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: context.dialect.provider,
        external_session_id: Some(fact.identity.as_str().to_owned()),
        external_agent_id: None,
        agent_type: AgentType::Primary,
        role_hint: Some("primary".to_owned()),
        is_primary: true,
        status: if ended_at.is_some() {
            SessionStatus::Completed
        } else {
            SessionStatus::Imported
        },
        transcript_blob_id: None,
        started_at,
        ended_at,
        timestamps: timestamps(context.options.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": fact.identity.as_str(),
                "source_format": context.dialect.source_format,
                "source_trust": "provider_native",
                "imported_at": context.options.imported_at,
                "session_idempotency_key":
                    format!(
                        "provider-session:{}:{}",
                        context.dialect.provider.as_str(),
                        fact.identity.as_str()
                    ),
                "metadata": {
                    "title": fact.title,
                    "workspace_directory": fact.workspace_directory,
                    "created_at": fact.created_at,
                    "last_modified": fact.last_modified,
                    "model_id": fact.model_id,
                    "model_provider": fact.model_provider,
                    "tokens_input": fact.tokens_input,
                    "tokens_output": fact.tokens_output,
                    "nativepath_publication": context.dialect.publication_revision,
                },
            }),
        ),
    })
}
