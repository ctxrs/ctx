use super::*;

pub(crate) fn import_codebuddy_nativepath(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    import_options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let configured_root = context
        .source_root
        .clone()
        .or_else(|| context.source_path.clone())
        .unwrap_or_else(|| path.to_path_buf());
    let inventory = discover_sources(path, &configured_root, &import_options)?;
    let committed_store = Store::open_read_only(store.path())?;
    let known_routes = known_routes(&committed_store, &context, &configured_root)?;

    if inventory.sources.is_empty() && known_routes.is_empty() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "no CodeBuddy history sessions with index.json and messages/*.json or CLI project JSONL files were found",
        });
    }

    if import_options.import_profile.is_replay_only() {
        replay_outputs_or_mark_behind(
            &inventory.sources,
            &committed_store,
            &context,
            &import_options.import_profile,
        );
        return Ok(ProviderImportSummary::default());
    }

    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let mut summary = ProviderImportSummary::default();
    let operation = (|| {
        let mut changed_groups = 0_usize;
        let live_locators = inventory
            .sources
            .iter()
            .map(|source| source.locator_identity.as_str())
            .collect::<BTreeSet<_>>();
        let pending_retirement = known_routes
            .iter()
            .any(|route| !live_locators.contains(route.locator_identity.as_str()));
        for (source_index, source) in inventory.sources.iter().enumerate() {
            let source_summary = import_source_core(
                store,
                &committed_store,
                &bulk_guard,
                source,
                &context,
                &import_options,
                &mut changed_groups,
            )?;
            let stop = source_summary.work_remaining;
            summary.merge_from(source_summary);
            if stop {
                return Ok(summary);
            }
            if import_options.capture_work_limit == CaptureWorkLimit::OneSafeGroup
                && changed_groups != 0
            {
                summary.work_remaining =
                    source_index.saturating_add(1) < inventory.sources.len() || pending_retirement;
                return Ok(summary);
            }
        }

        summary.merge_from(retire_missing_routes(
            store,
            &bulk_guard,
            &context,
            &known_routes,
            &inventory.sources,
            import_options.capture_work_limit,
            if inventory.root_missing {
                ProviderSourceRouteRetirementReason::RootMissing
            } else {
                ProviderSourceRouteRetirementReason::SourceMissing
            },
        )?);
        Ok(summary)
    })();
    let finish = store
        .finish_event_search_bulk_mode(&bulk_guard)
        .map_err(CaptureError::from);
    let mut summary = match (operation, finish) {
        (Ok(summary), Ok(())) => summary,
        (_, Err(error)) => return Err(error),
        (Err(error), Ok(())) => return Err(error),
    };

    if !summary.work_remaining {
        replay_outputs_or_mark_behind(
            &inventory.sources,
            &committed_store,
            &context,
            &import_options.import_profile,
        );
    }
    if !inventory.sources.is_empty()
        && !summary.has_accepted_content()
        && summary.failed == 0
        && !summary.work_remaining
    {
        summary.record_failure(ProviderImportFailure {
            line: 0,
            error: "CodeBuddy history contained no real conversation messages".to_owned(),
        });
    }
    Ok(summary)
}

pub(super) fn import_source_core(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    source: &CodeBuddySource,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    changed_groups: &mut usize,
) -> Result<ProviderImportSummary> {
    let mut plan = plan_source(store, source, context)?;
    if plan.change == CodeBuddySourceChange::Resume && plan.cursor.terminal {
        return plan.cursor.replay_summary();
    }

    let mut summary = ProviderImportSummary::default();
    loop {
        let expected = plan.cursor.clone();
        let Some(page) = next_source_page(source, &expected, context)? else {
            break;
        };
        if !source.revalidate()? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let mut page_summary = publish_core_page(
            store,
            committed_store,
            bulk_guard,
            source,
            context,
            options,
            plan.expected_store_cursor.as_deref(),
            page,
        )?;
        if page_summary.work_result() == ProviderImportWorkResult::Changed {
            *changed_groups = changed_groups.saturating_add(1);
        }
        page_summary.work_remaining = false;
        summary.merge_from(page_summary);

        let stored = store
            .get_sync_cursor(None, &context.machine_id, &source.cursor_stream)?
            .ok_or(CaptureError::SystemInvariant(
                "CodeBuddy NativePath commit did not publish its cursor",
            ))?;
        let committed = decode_native_path_committed_cursor(&stored.cursor)?;
        plan.cursor = CodeBuddyNativeCursor::decode(committed.provider_cursor())?;
        plan.expected_store_cursor = Some(stored.cursor);

        if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup && *changed_groups != 0 {
            summary.work_remaining = !plan.cursor.terminal;
            return Ok(summary);
        }
        if plan.cursor.terminal {
            break;
        }
    }
    Ok(summary)
}

#[derive(Debug)]
pub(super) struct ResolvedCodeBuddySource {
    pub(super) source_id: Uuid,
    pub(super) session: Session,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_core_page(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    source: &CodeBuddySource,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    expected_store_cursor: Option<&str>,
    page: CodeBuddyPage,
) -> Result<ProviderImportSummary> {
    let next_cursor = page.next_cursor.encode()?;
    let next = SyncCursor {
        id: Uuid::new_v4(),
        team_id: None,
        device_id: context.machine_id.clone(),
        stream: source.cursor_stream.clone(),
        cursor: next_cursor,
        last_synced_at: Some(context.imported_at),
        timestamps: timestamps(context.imported_at),
    };
    let transition =
        NativePathCursorTransition::new(expected_store_cursor.map(str::to_owned), next);
    let publication_id = publication_id(source, &page, &transition);
    let accounting = NativePathGroupAccounting::new(1, 1, page.retained_bytes)?;
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

    let mut summary = ProviderImportSummary::default();
    let has_accepted_core = page.next_cursor.accepted_events != 0
        || page
            .records
            .iter()
            .any(|record| record.classification.core().is_some());
    let template = if has_accepted_core {
        cursor_session_draft(source, context, &page.next_cursor)?.or_else(|| {
            page.records.iter().find_map(|record| {
                record
                    .classification
                    .core()
                    .map(|core| core.session.clone())
            })
        })
    } else {
        None
    };
    let resolved = template
        .as_ref()
        .map(|session| {
            resolve_source(
                committed_store,
                &mut group,
                source,
                context,
                options,
                session,
                &mut summary,
            )
        })
        .transpose()?;

    if let Some(resolved) = resolved.as_ref() {
        for record in &page.records {
            let Some(core) = record.classification.core() else {
                continue;
            };
            publish_event(
                committed_store,
                &mut group,
                context,
                options,
                &core.event,
                record.native_ordinal,
                record.physical_line,
                resolved,
                &mut summary,
            )?;
        }
    }

    let prior_rejected = page.expected_cursor.rejected_records;
    let new_rejected = page
        .next_cursor
        .rejected_records
        .saturating_sub(prior_rejected);
    summary.failed = summary
        .failed
        .saturating_add(usize::try_from(new_rejected).unwrap_or(usize::MAX));
    let new_skipped_metadata = page
        .next_cursor
        .skipped_metadata
        .saturating_sub(page.expected_cursor.skipped_metadata);
    summary.skipped = summary
        .skipped
        .saturating_add(usize::try_from(new_skipped_metadata).unwrap_or(usize::MAX));
    let prior_failure_count = page.expected_cursor.failures.len();
    summary.failures.extend(
        page.next_cursor
            .failures
            .iter()
            .skip(prior_failure_count)
            .map(|failure| ProviderImportFailure {
                line: failure.line,
                error: failure.error.clone(),
            }),
    );
    if page.expected_cursor.incomplete_tail != page.next_cursor.incomplete_tail {
        if let Some(failure) = page.next_cursor.incomplete_tail.as_ref() {
            summary.failed = summary.failed.saturating_add(1);
            summary.failures.push(ProviderImportFailure {
                line: failure.line,
                error: failure.error.clone(),
            });
        }
    }

    if !source.revalidate()? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn resolve_source(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    source: &CodeBuddySource,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    session_draft: &CodeBuddySessionDraft,
    summary: &mut ProviderImportSummary,
) -> Result<ResolvedCodeBuddySource> {
    let raw_source_path = source.canonical_path.display().to_string();
    let source_root = source.configured_root.display().to_string();
    let resolution =
        group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::CodeBuddy,
            source_format: CODEBUDDY_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity: source.locator_identity.clone(),
            cursor_stream: source.cursor_stream.clone(),
            proposed_source_identity: source.proposed_source_identity.clone(),
            raw_source_path: Some(raw_source_path.clone()),
            source_revision: source.source_revision.clone(),
            observed_at_ms: context.imported_at.timestamp_millis(),
        })?;
    let provider_session_id = &session_draft.provider_session_id;
    let existing_source = committed_store.capture_source_by_canonical_identity_session(
        CaptureProvider::CodeBuddy,
        CODEBUDDY_SOURCE_FORMAT,
        &context.machine_id,
        &resolution.canonical_source_identity,
        provider_session_id.as_str(),
    )?;
    let source_id = existing_source
        .as_ref()
        .map(|source| source.id)
        .unwrap_or_else(|| {
            provider_scoped_source_uuid(
                CaptureProvider::CodeBuddy,
                provider_session_id,
                CODEBUDDY_SOURCE_FORMAT,
                Some(&raw_source_path),
            )
        });
    let source_record = capture_source(
        source_id,
        source,
        context,
        session_draft,
        &resolution.canonical_source_identity,
        &source_root,
    );
    group.upsert_capture_source(&source_record)?;
    group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;

    let session_id = provider_import_session_uuid(
        committed_store,
        CaptureProvider::CodeBuddy,
        provider_session_id.as_str(),
        source_id,
        Some(&resolution.canonical_source_identity),
    )?;
    let session_existed = committed_store.get_session(session_id).is_ok();
    let session = normalized_session(session_id, source_id, context, options, session_draft);
    group.upsert_session(&session)?;
    if session_existed {
        summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    } else {
        summary.imported_sessions = summary.imported_sessions.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    }
    Ok(ResolvedCodeBuddySource { source_id, session })
}

pub(super) fn capture_source(
    source_id: Uuid,
    source: &CodeBuddySource,
    context: &ProviderAdapterContext,
    session: &CodeBuddySessionDraft,
    canonical_source_identity: &str,
    source_root: &str,
) -> CaptureSource {
    let raw_source_path = source.canonical_path.display().to_string();
    let source_identity_key = provider_scoped_source_identity_key(
        CaptureProvider::CodeBuddy,
        &session.provider_session_id,
        CODEBUDDY_SOURCE_FORMAT,
        Some(&raw_source_path),
    );
    CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::CodeBuddy,
            machine_id: context.machine_id.clone(),
            process_id: None,
            cwd: session.cwd.clone(),
            raw_source_path: Some(raw_source_path.clone()),
            source_format: Some(CODEBUDDY_SOURCE_FORMAT.to_owned()),
            source_root: Some(source_root.to_owned()),
            source_identity: Some(canonical_source_identity.to_owned()),
            external_session_id: Some(session.provider_session_id.clone()),
        },
        started_at: session.started_at,
        ended_at: session.ended_at,
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": session.provider_session_id,
                "source_format": CODEBUDDY_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "source_identity": canonical_source_identity,
                "source_root": source_root,
                "source_revision": source.source_revision,
                "source_identity_key": source_identity_key,
                "source_metadata": session.source_metadata,
                "session_metadata": session.session_metadata,
                "nativepath_publication": CODEBUDDY_NATIVE_PUBLICATION_REVISION,
            }),
        ),
    }
}

pub(super) fn normalized_session(
    session_id: Uuid,
    source_id: Uuid,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    session: &CodeBuddySessionDraft,
) -> Session {
    Session {
        id: session_id,
        history_record_id: options.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::CodeBuddy,
        external_session_id: Some(session.provider_session_id.clone()),
        external_agent_id: None,
        agent_type: AgentType::Primary,
        role_hint: Some("primary".to_owned()),
        is_primary: true,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: session.started_at,
        ended_at: session.ended_at,
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": session.provider_session_id,
                "source_format": CODEBUDDY_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "session_idempotency_key":
                    format!("provider-session:codebuddy:{}", session.provider_session_id),
                "artifacts": [],
                "metadata": session.session_metadata,
                "nativepath_publication": CODEBUDDY_NATIVE_PUBLICATION_REVISION,
            }),
        ),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_event(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    event: &CodeBuddyEventDraft,
    native_ordinal: u64,
    line_number: usize,
    resolved: &ResolvedCodeBuddySource,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let provider_session_id =
        resolved
            .session
            .external_session_id
            .as_deref()
            .ok_or(CaptureError::SystemInvariant(
                "CodeBuddy NativePath session lost its provider identity",
            ))?;
    let event_hash = &event.event_hash;
    let provider_event_index = event.provider_event_index;
    let identity = provider_event_import_identity_with_exact_legacy_source(
        committed_store,
        CaptureProvider::CodeBuddy,
        provider_session_id,
        resolved.source_id,
        provider_event_index,
        native_ordinal,
        event_hash,
        None,
        Some(event.legacy_provider_event_index),
        resolved.session.id
            == provider_session_uuid(CaptureProvider::CodeBuddy, provider_session_id),
    )?;

    let mut provider_metadata = event.metadata.clone();
    let verified_content_locators = provider_metadata
        .as_object_mut()
        .and_then(|metadata| metadata.remove(VERIFIED_CONTENT_LOCATORS_METADATA_KEY))
        .map(|value| {
            VerifiedContentLocatorsV1::from_metadata_value(&value).ok_or_else(|| {
                CaptureError::InvalidPayload(
                    "verified content locator annotation is malformed".to_owned(),
                )
            })
        })
        .transpose()?;
    let mut sync_metadata = json!({
        "provider_session_id": provider_session_id,
        "provider_event_index": provider_event_index,
        "provider_event_hash": event_hash,
        "provider_event_hash_authority": ProviderEventHashAuthority::NormalizedPayloadFallback.as_str(),
        "cursor": event_hash,
        "source_format": CODEBUDDY_SOURCE_FORMAT,
        "source_trust": "provider_native",
        "fixture_line": line_number,
        "imported_at": context.imported_at,
        "event_idempotency_key":
            format!("provider-event:codebuddy:{CODEBUDDY_SOURCE_FORMAT}:{provider_event_index}"),
        "source_record_ordinal": native_ordinal,
        "source_record_subrecord_index": 0,
        "metadata": provider_metadata,
    });
    if let (Some(metadata), Some(locators)) = (
        sync_metadata.as_object_mut(),
        verified_content_locators.as_ref(),
    ) {
        metadata.insert(
            VERIFIED_CONTENT_LOCATORS_METADATA_KEY.to_owned(),
            locators.to_metadata_value(),
        );
    }
    let dedupe_key =
        Store::provider_event_dedupe_key_with_payload_hash(&identity.dedupe_key, event_hash)
            .unwrap_or_else(|| identity.dedupe_key.clone());
    let normalized = Event {
        id: identity.id,
        seq: identity.seq,
        history_record_id: options.history_record_id,
        session_id: Some(resolved.session.id),
        run_id: None,
        event_type: event.event_type,
        role: Some(event.role),
        occurred_at: event.occurred_at,
        capture_source_id: Some(resolved.source_id),
        payload: json!({
            "provider": CaptureProvider::CodeBuddy.as_str(),
            "provider_session_id": provider_session_id,
            "provider_event_index": provider_event_index,
            "provider_event_hash": event_hash,
            "cursor": event_hash,
            "artifacts": [],
            "body": compact_provider_result_payload(event.event_type, &event.payload),
        }),
        payload_blob_id: None,
        dedupe_key: Some(dedupe_key),
        sync: provider_sync_metadata(Fidelity::Imported, sync_metadata),
    };
    if group.reconcile_provider_event_migrating_exact_legacy_provider_hash(
        &normalized,
        &event.legacy_provider_event_hash,
    )? {
        summary.imported_events = summary.imported_events.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    } else {
        summary.skipped_events = summary.skipped_events.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    }
    summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
    Ok(())
}

pub(super) fn cursor_session_draft(
    source: &CodeBuddySource,
    context: &ProviderAdapterContext,
    cursor: &CodeBuddyNativeCursor,
) -> Result<Option<CodeBuddySessionDraft>> {
    let started_at = cursor.session.started_at()?.unwrap_or(context.imported_at);
    let provider_session_id = cursor.session.provider_session_id();
    let source_path = source.canonical_path.display().to_string();
    let session_title = codebuddy_session_title(source, &cursor.session)?;
    match source.shape {
        CodeBuddySourceShape::Cli => {
            let session_index = json!({
                "source": "codebuddy_cli_jsonl",
                "path": source_path,
                "rows": cursor.session.row_count,
            });
            Ok(Some(codebuddy_session_draft(&CodeBuddySessionInput {
                provider_session_id: &provider_session_id,
                native_session_id: &cursor.session.native_session_id,
                project_hash: &cursor.session.project_hash,
                started_at,
                ended_at: cursor.session.ended_at()?,
                title: session_title.as_deref(),
                cwd: cursor.session.cwd.as_deref(),
                project_index: None,
                conversation: None,
                session_index: &session_index,
                file_names: &["projects/*/*.jsonl"],
                shape: CodeBuddyNativeShape::Cli,
            })))
        }
        CodeBuddySourceShape::Extension => {
            let (metadata, _) = codebuddy_extension_metadata(&source.path, source.session_ordinal)?;
            let Some(metadata) = metadata else {
                return Ok(None);
            };
            let cwd = codebuddy_extension_metadata_text(
                &metadata,
                &["projectPath", "project_path", "cwd", "workspace"],
            );
            Ok(Some(codebuddy_session_draft(&CodeBuddySessionInput {
                provider_session_id: &provider_session_id,
                native_session_id: &cursor.session.native_session_id,
                project_hash: &cursor.session.project_hash,
                started_at,
                ended_at: cursor.session.ended_at()?,
                title: session_title.as_deref(),
                cwd: cwd.as_deref(),
                project_index: metadata.project_index.as_ref(),
                conversation: metadata.conversation.as_ref(),
                session_index: &metadata.session_index,
                file_names: &["index.json", "messages/*.json"],
                shape: CodeBuddyNativeShape::Extension,
            })))
        }
    }
}

pub(super) fn publication_id(
    source: &CodeBuddySource,
    page: &CodeBuddyPage,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(CODEBUDDY_PUBLICATION_DOMAIN);
    digest.update(source.shape.cursor_tag().as_bytes());
    digest.update(source.locator_identity.as_bytes());
    digest.update(source.source_revision.as_bytes());
    digest.update(page.expected_cursor.next_native_offset.to_be_bytes());
    digest.update(page.expected_cursor.next_native_ordinal.to_be_bytes());
    digest.update(page.next_cursor.next_native_offset.to_be_bytes());
    digest.update(page.next_cursor.next_native_ordinal.to_be_bytes());
    digest.update([u8::from(page.next_cursor.terminal)]);
    for record in &page.records {
        digest.update(record.native_ordinal.to_be_bytes());
        digest.update((record.native_bytes.len() as u64).to_be_bytes());
        digest.update(Sha256::digest(&record.native_bytes));
    }
    digest.update(transition.next().stream.as_bytes());
    digest.update(transition.next().cursor.as_bytes());
    format!("codebuddy-nativepath:{}", hex(&digest.finalize()))
}
