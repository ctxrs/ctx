use super::*;

pub(super) fn import_task_json_nativepath_history(
    path: &Path,
    store: &mut Store,
    options: TaskJsonNativeImportOptions,
    dialect: TaskJsonNativeDialect,
) -> Result<ProviderImportSummary> {
    let configured_source_root = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let discovery = if dialect == TaskJsonNativeDialect::CLINE {
        super::super::discover_cline_root(path)
    } else {
        super::super::discover_roo_root(path)
    }
    .map_err(map_source_error)?;
    let committed_store = Store::open_read_only(store.path())?;
    let tasks_root = discovery.root_authority().tasks_root().to_path_buf();
    let prior_manifest =
        load_cline_root_manifest(&committed_store, &options.machine_id, &tasks_root, dialect)?;
    let current_task_names = cline_task_names(&discovery)?;
    if current_task_names.is_empty() && prior_manifest.is_none() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: if dialect == TaskJsonNativeDialect::CLINE {
                "Cline task history root contains no task directories"
            } else {
                "Roo Code task history root contains no task directories"
            },
        });
    }
    let mut previous = Vec::new();
    if !matches!(&options.import_profile, ImportProfile::ProReplayOnly(_)) {
        for route in discovery.task_routes() {
            if let Some(checkpoint) =
                load_cline_task_checkpoint(&committed_store, &options.machine_id, route)?
            {
                previous.push(checkpoint);
            }
        }
        if let Some(manifest) = &prior_manifest {
            let current = current_task_names.iter().collect::<BTreeSet<_>>();
            for task_name in &manifest.task_names {
                if current.contains(task_name) {
                    continue;
                }
                let task_path = manifest.tasks_root.join(task_name);
                let checkpoint = load_cline_task_checkpoint_by_path(
                    &committed_store,
                    &options.machine_id,
                    &task_path,
                    dialect,
                )
                .map_err(map_vertical_error)?
                .ok_or_else(|| {
                    CaptureError::InvalidPayload(
                        "Cline root manifest references a missing task checkpoint".to_owned(),
                    )
                })?;
                previous.push(checkpoint);
            }
        }
    }
    let native_profile = match &options.import_profile {
        ImportProfile::CoreOnly => ClineNativeProfile::CoreOnly,
        ImportProfile::CoreAndPro(_) | ImportProfile::ProReplayOnly(_) => {
            ClineNativeProfile::CoreAndPro
        }
    };
    let replay_only = matches!(&options.import_profile, ImportProfile::ProReplayOnly(_));
    let mut reader = ClineNativeReader::new(discovery, &previous, native_profile);
    let mut adapter = ClineNativePageAdapter::new(dialect.provider, &options.import_profile);
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let mut summary = ProviderImportSummary::default();
    let operation = (|| {
        let mut changed_groups = 0_usize;
        let mut relocated_task_identities = BTreeSet::new();
        while let Some(page) = reader.next_page().map_err(map_source_error)? {
            let adapted = adapter
                .adapt(page)
                .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
            let pending_output = adapted.output;
            if replay_only {
                verify_cline_core_page_committed(store, &options, dialect, adapted.core)?;
            } else {
                let core = publish_task_json_core_page(
                    store,
                    &committed_store,
                    &bulk_guard,
                    &configured_source_root,
                    &options,
                    dialect,
                    adapted.core,
                )?;
                if core.summary.work_result() == ProviderImportWorkResult::Changed {
                    changed_groups = changed_groups.saturating_add(1);
                }
                relocated_task_identities.extend(core.relocated_task_identities);
                summary.merge_from(core.summary);
            }
            if let Some(pending_output) = pending_output {
                let sink = options
                    .import_profile
                    .sink()
                    .ok_or(CaptureError::SystemInvariant(
                        "task JSON NativePath output page has no output sink",
                    ))?;
                match adapter.adapt_output_after_core(pending_output) {
                    Ok(Some(output)) => {
                        if let Err(error) =
                            crate::provider::native_ingestion::process_pro_replay_only(
                                output,
                                sink.as_ref(),
                            )
                        {
                            sink.mark_behind(ProOutputSinkError::new(
                                "task_json_nativepath_output_replay",
                                format!("{:?}", error.output_error),
                            ));
                        }
                    }
                    Ok(None) => {}
                    Err(error) => sink.mark_behind(ProOutputSinkError::new(
                        "task_json_nativepath_output",
                        error.to_string(),
                    )),
                }
            }
            if !replay_only
                && options.capture_work_limit == CaptureWorkLimit::OneSafeGroup
                && changed_groups != 0
            {
                summary.work_remaining = true;
                return Ok(summary);
            }
        }
        let completion = reader.finish_catalog().map_err(map_source_error)?;
        record_catalog_failures(&completion, &mut summary);
        if !replay_only {
            for checkpoint in &completion.live_checkpoints {
                summary.merge_from(publish_task_json_task_checkpoint(
                    store,
                    &bulk_guard,
                    &options,
                    dialect,
                    checkpoint,
                )?);
            }
            let retirement_source_root = prior_manifest.as_ref().map_or_else(
                || configured_source_root.display().to_string(),
                |manifest| manifest.source_root.clone(),
            );
            let live_task_identities = completion
                .live_checkpoints
                .iter()
                .flat_map(|checkpoint| {
                    std::iter::once(checkpoint.identity.as_str()).chain(
                        checkpoint
                            .task_metadata
                            .session
                            .identity_aliases
                            .iter()
                            .map(ClineTaskIdentity::as_str),
                    )
                })
                .collect::<BTreeSet<_>>();
            for missing_path in &completion.missing_task_paths {
                let checkpoint = previous
                    .iter()
                    .find(|checkpoint| &checkpoint.canonical_task_path == missing_path)
                    .ok_or(CaptureError::SystemInvariant(
                        "Cline catalog retirement lost its prior task checkpoint",
                    ))?;
                if relocated_task_identities.contains(checkpoint.identity.as_str())
                    || live_task_identities.contains(checkpoint.identity.as_str())
                    || checkpoint
                        .task_metadata
                        .session
                        .identity_aliases
                        .iter()
                        .any(|alias| relocated_task_identities.contains(alias.as_str()))
                {
                    continue;
                }
                summary.merge_from(retire_cline_task_routes(
                    store,
                    &bulk_guard,
                    &options,
                    dialect,
                    &retirement_source_root,
                    checkpoint,
                )?);
            }
            summary.merge_from(publish_cline_root_manifest(
                store,
                &bulk_guard,
                &options,
                dialect,
                &tasks_root,
                &configured_source_root,
                &current_task_names,
            )?);
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

pub(super) fn verify_cline_core_page_committed(
    store: &Store,
    options: &TaskJsonNativeImportOptions,
    dialect: TaskJsonNativeDialect,
    publication_page: NativePublicationPage<ClineCertifiedPage>,
) -> Result<()> {
    let (source_identity, page) = publication_page.into_parts();
    validate_source_identity(dialect, &source_identity, &page).map_err(map_vertical_error)?;
    revalidate_page_source(&page).map_err(map_vertical_error)?;
    let stream = component_cursor_stream(dialect, &page.core.source.canonical_path)
        .map_err(map_vertical_error)?;
    let stored = store
        .get_sync_cursor(None, &options.machine_id, &stream)?
        .ok_or_else(|| {
            CaptureError::InvalidPayload(format!(
                "{} output replay requires committed NativePath Core",
                dialect.display_name
            ))
        })?;
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    let prior = ClineNativeStoreCursor::decode(committed.provider_cursor())
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let source_revision = revision(&page.core.source_revision.revision_sha256);
    if !matches!(
        prior.version,
        ClineNativeStoreCursor::LEGACY_VERSION | ClineNativeStoreCursor::VERSION
    ) || prior.provider != dialect.provider.as_str()
        || prior.source_identity != page.core.source.stable_id.as_ref()
        || !cursor_task_authority_matches(&prior, &page.core.source)
        || prior.source_revision != source_revision
        || prior.frontier.next_native_index < page.core.next_safe_frontier.next_native_index
        || (prior.frontier.next_native_index == page.core.next_safe_frontier.next_native_index
            && prior.frontier != page.core.next_safe_frontier)
    {
        return Err(CaptureError::InvalidPayload(format!(
            "{} output replay source no longer matches committed Core authority",
            dialect.display_name
        )));
    }
    Ok(())
}

pub(super) fn record_catalog_failures(
    completion: &ClineCatalogCompletion,
    summary: &mut ProviderImportSummary,
) {
    let rejection = match &completion.root_index {
        ClineCatalogIndex::Incomplete(rejection)
        | ClineCatalogIndex::Malformed(rejection)
        | ClineCatalogIndex::Unavailable(rejection) => Some(rejection),
        ClineCatalogIndex::Missing | ClineCatalogIndex::Parsed { .. } => None,
    };
    if let Some(rejection) = rejection {
        summary.record_failure(crate::ProviderImportFailure {
            line: 0,
            error: format!("{}: {}", rejection.path.display(), rejection.message),
        });
    }
    for failure in completion
        .component_outcomes
        .iter()
        .filter_map(|outcome| outcome.failure.as_ref())
    {
        summary.record_failure(crate::ProviderImportFailure {
            line: 0,
            error: format!("{}: {}", failure.path.display(), failure.message),
        });
    }
    for checkpoint in &completion.live_checkpoints {
        let retained_rows = [
            checkpoint.api_history.as_ref(),
            checkpoint.ui_messages.as_ref(),
            checkpoint.fallback_history.as_ref(),
        ]
        .into_iter()
        .flatten()
        .map(|component| component.retained_rows)
        .sum::<u64>();
        let has_component_failure = completion.component_outcomes.iter().any(|outcome| {
            outcome.failure.is_some() && outcome.path.starts_with(&checkpoint.canonical_task_path)
        });
        if retained_rows == 0 && !has_component_failure {
            summary.record_failure(crate::ProviderImportFailure {
                line: 0,
                error: format!(
                    "{}: provider source contained no real conversation message",
                    checkpoint.canonical_task_path.display()
                ),
            });
        }
    }
}

pub(super) fn cline_task_names(
    discovery: &ClineDiscovery,
) -> std::result::Result<Vec<String>, CaptureError> {
    let mut names = discovery
        .task_routes()
        .iter()
        .map(|task| {
            task.canonical_task_path
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
                .ok_or(CaptureError::SystemInvariant(
                    "Cline task route has no UTF-8 direct-child name",
                ))
        })
        .collect::<Result<Vec<_>>>()?;
    names.sort();
    names.dedup();
    Ok(names)
}

pub(super) fn load_cline_root_manifest(
    store: &Store,
    machine_id: &str,
    tasks_root: &Path,
    dialect: TaskJsonNativeDialect,
) -> Result<Option<ClineRootManifestWire>> {
    let stream = root_cursor_stream(dialect, tasks_root).map_err(map_vertical_error)?;
    let Some(stored) = store.get_sync_cursor(None, machine_id, &stream)? else {
        return Ok(None);
    };
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    let manifest: ClineRootManifestWire = serde_json::from_str(committed.provider_cursor())
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    if manifest.version != CLINE_TASK_CURSOR_VERSION || manifest.tasks_root != tasks_root {
        return Err(CaptureError::InvalidPayload(
            "Cline NativePath root manifest is inconsistent".to_owned(),
        ));
    }
    Ok(Some(manifest))
}

pub(super) fn publish_cline_root_manifest(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    options: &TaskJsonNativeImportOptions,
    dialect: TaskJsonNativeDialect,
    tasks_root: &Path,
    configured_source_root: &Path,
    task_names: &[String],
) -> Result<ProviderImportSummary> {
    let stream = root_cursor_stream(dialect, tasks_root).map_err(map_vertical_error)?;
    let stored = store.get_sync_cursor(None, &options.machine_id, &stream)?;
    let wire = ClineRootManifestWire {
        version: CLINE_TASK_CURSOR_VERSION,
        tasks_root: tasks_root.to_path_buf(),
        source_root: configured_source_root.display().to_string(),
        task_names: task_names.to_vec(),
    };
    let encoded = serde_json::to_string(&wire)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    if encoded.len() > ctx_history_store::NATIVE_PATH_MAX_RETAINED_PAGE_BYTES {
        return Err(CaptureError::InvalidPayload(
            "Cline NativePath root manifest exceeds the bounded Store page".to_owned(),
        ));
    }
    if let Some(stored) = &stored {
        let committed = decode_native_path_committed_cursor(&stored.cursor)?;
        if committed.provider_cursor() == encoded {
            let mut summary = ProviderImportSummary::default();
            summary.set_work_result(ProviderImportWorkResult::NoOp);
            return Ok(summary);
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
    let publication_id = root_manifest_publication_id(dialect, &wire, &transition);
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

pub(super) fn retire_cline_task_routes(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    options: &TaskJsonNativeImportOptions,
    dialect: TaskJsonNativeDialect,
    source_root: &str,
    checkpoint: &ClineTaskCheckpoint,
) -> Result<ProviderImportSummary> {
    let task_path = &checkpoint.canonical_task_path;
    let stream = task_cursor_stream(dialect, task_path).map_err(map_vertical_error)?;
    let stored = store
        .get_sync_cursor(None, &options.machine_id, &stream)?
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "task JSON route retirement requires its committed task cursor".to_owned(),
            )
        })?;
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    let locator_identity = provider_path_identity(task_path)?;
    let raw_source_path = task_path.display().to_string();
    let canonical_source_identity = provider_source_identity(
        dialect.provider,
        dialect.source_format,
        Some(source_root),
        Some(&raw_source_path),
        None,
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Cline route retirement has no canonical source identity",
    ))?;
    let retirement = ProviderSourceRouteRetirement {
        provider: dialect.provider,
        source_format: dialect.source_format.to_owned(),
        machine_id: options.machine_id.clone(),
        locator_identity,
        cursor_stream: stream.clone(),
        expected_canonical_source_identity: canonical_source_identity,
        expected_source_revision: task_route_revision(dialect, checkpoint.identity.as_str()),
        retired_at_ms: options.imported_at.timestamp_millis(),
        reason: ProviderSourceRouteRetirementReason::SourceMissing,
    };
    let publication_id = retirement_publication_id(dialect, &retirement);
    if committed.publication_id() == publication_id {
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }
    let next = SyncCursor {
        id: Uuid::new_v4(),
        team_id: None,
        device_id: options.machine_id.clone(),
        stream,
        cursor: committed.provider_cursor().to_owned(),
        last_synced_at: Some(options.imported_at),
        timestamps: timestamps(options.imported_at),
    };
    let transition = NativePathCursorTransition::new(Some(stored.cursor), next);
    let accounting = NativePathGroupAccounting::new(0, 1, 0)?;
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
    let disposition = group.retire_provider_source_route(&retirement)?;
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    let mut summary = ProviderImportSummary::default();
    match disposition {
        ProviderSourceRouteRetirementDisposition::Retired => {
            summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
            summary.set_work_result(ProviderImportWorkResult::Changed);
        }
        ProviderSourceRouteRetirementDisposition::AlreadyRetired => {
            summary.set_work_result(ProviderImportWorkResult::NoOp);
        }
    }
    Ok(summary)
}

pub(super) fn root_cursor_stream(
    dialect: TaskJsonNativeDialect,
    path: &Path,
) -> std::result::Result<String, ClineNativeVerticalError> {
    let identity = provider_path_identity(path)?;
    Ok(provider_source_cursor_stream_for_path(
        dialect.provider,
        dialect.root_cursor_stream_format,
        &identity,
    ))
}

pub(super) fn root_manifest_publication_id(
    dialect: TaskJsonNativeDialect,
    wire: &ClineRootManifestWire,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(dialect.root_publication_domain);
    digest.update(wire.tasks_root.as_os_str().as_encoded_bytes());
    digest.update(wire.source_root.as_bytes());
    for task_name in &wire.task_names {
        digest.update((task_name.len() as u64).to_le_bytes());
        digest.update(task_name.as_bytes());
    }
    digest.update(transition.next().cursor.as_bytes());
    format!(
        "{}{}",
        dialect.root_publication_prefix,
        hex(&digest.finalize())
    )
}

pub(super) fn retirement_publication_id(
    dialect: TaskJsonNativeDialect,
    retirement: &ProviderSourceRouteRetirement,
) -> String {
    let mut digest = Sha256::new();
    digest.update(dialect.retirement_publication_domain);
    digest.update(retirement.provider.as_str().as_bytes());
    digest.update(retirement.source_format.as_bytes());
    digest.update(retirement.machine_id.as_bytes());
    digest.update(retirement.locator_identity.as_bytes());
    digest.update(retirement.cursor_stream.as_bytes());
    digest.update(retirement.expected_canonical_source_identity.as_bytes());
    digest.update(retirement.expected_source_revision.as_bytes());
    format!(
        "{}{}",
        dialect.retirement_publication_prefix,
        hex(&digest.finalize())
    )
}
