use super::*;

// Publication needs the certified source, cursor, Store, and import policy
// authorities together; grouping them would obscure their ownership.
#[allow(clippy::too_many_arguments)]
pub(super) fn publish_core_page(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    configured_source_root: &Path,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    source: &ContinuePublicationSource,
    index: &ContinueIndexSnapshot,
    publication_page: NativePublicationPage<super::super::ContinuePreparedPage>,
) -> Result<ProviderImportSummary> {
    if !index.revalidate() || !source.observation.revalidate().map_err(map_native_error)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let (source_identity, page) = publication_page.into_parts();
    validate_page_source_identity(&source_identity, source)?;
    let stream = source_cursor_stream(&source.observation)?;
    let stored = store.get_sync_cursor(None, &context.machine_id, &stream)?;
    let plan = classify_cursor(stored.as_ref(), &source_identity, source, &page)?;
    let CursorPlan::Publish {
        cursor,
        terminal_reconciliation,
    } = plan
    else {
        return Ok(already_committed_summary(&page));
    };

    let transition = NativePathCursorTransition::new(
        stored.as_ref().map(|cursor| cursor.cursor.clone()),
        provider_sync_cursor(
            &context.machine_id,
            stream,
            cursor.encode()?,
            context.imported_at,
        ),
    );
    let publication_id = if terminal_reconciliation {
        terminal_publication_id(&source_identity, &transition)
    } else {
        page_publication_id(&source_identity, &page, &transition)
    };
    if terminal_reconciliation
        && stored
            .as_ref()
            .map(|cursor| decode_native_path_committed_cursor(&cursor.cursor))
            .transpose()?
            .is_some_and(|committed| committed.publication_id() == publication_id)
    {
        return Ok(already_committed_summary(&page));
    }
    let accounting =
        NativePathGroupAccounting::new(1, 1, page.accounting.conservative_serialized_bytes)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
        NativePathCursorSetClassification::AllNextSameGroup { .. } => {
            group.commit()?;
            return Ok(already_committed_summary(&page));
        }
        NativePathCursorSetClassification::AllExpected => {}
    }

    let mut summary = ProviderImportSummary::default();
    let resolved = resolve_source(
        committed_store,
        &mut group,
        configured_source_root,
        context,
        options,
        source,
        &mut summary,
    )?;
    publish_events(
        committed_store,
        &mut group,
        options,
        &resolved,
        &page.core.events,
        &mut summary,
    )?;
    if let Some(authority) = page.core.authority.as_ref() {
        for _ in 0..authority.rejected_items {
            summary.record_failure(ProviderImportFailure {
                line: 0,
                error: "Continue history item was rejected during bounded native parsing"
                    .to_owned(),
            });
        }
    }

    if !index.revalidate() || !source.observation.revalidate().map_err(map_native_error)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

pub(super) fn classify_cursor(
    stored: Option<&SyncCursor>,
    source_identity: &NativeSourceIdentity,
    source: &ContinuePublicationSource,
    page: &NativeIngestionPage<super::super::ContinuePreparedPage>,
) -> Result<CursorPlan> {
    let expected = decode_frontier(&page.expected_frontier)?;
    let next = decode_frontier(&page.next_safe_frontier)?;
    let revision = source_revision(source);
    let rejected_records = page
        .core
        .authority
        .as_ref()
        .and_then(|authority| u64::try_from(authority.rejected_items).ok());

    let Some(stored) = stored else {
        ensure_initial_frontier(&expected)?;
        return Ok(CursorPlan::Publish {
            cursor: ContinueNativeStoreCursor {
                version: ContinueNativeStoreCursor::VERSION,
                source_identity: source_identity.source_identity().to_owned(),
                source_revision: revision,
                frontier: next,
                terminal: page.terminal,
                generation: 0,
                rejected_records: rejected_records.unwrap_or(0),
            },
            terminal_reconciliation: page.terminal,
        });
    };

    let provider_cursor = decode_native_path_committed_cursor(&stored.cursor)
        .map(|cursor| cursor.provider_cursor().to_owned())
        .unwrap_or_else(|_| stored.cursor.clone());
    let prior = match ContinueNativeStoreCursor::decode(&provider_cursor) {
        Ok(prior) => Some(prior),
        Err(_) => {
            if CertifiedProviderCursor::decode_if_certified(&provider_cursor)?.is_none() {
                return Err(CaptureError::InvalidPayload(
                    "Continue NativePath cursor is neither current nor a released migration cursor"
                        .to_owned(),
                ));
            }
            None
        }
    };
    let Some(prior) = prior else {
        ensure_initial_frontier(&expected)?;
        return Ok(CursorPlan::Publish {
            cursor: ContinueNativeStoreCursor {
                version: ContinueNativeStoreCursor::VERSION,
                source_identity: source_identity.source_identity().to_owned(),
                source_revision: revision,
                frontier: next,
                terminal: page.terminal,
                generation: 0,
                rejected_records: rejected_records.unwrap_or(0),
            },
            terminal_reconciliation: page.terminal,
        });
    };
    if prior.version != ContinueNativeStoreCursor::VERSION {
        return Err(CaptureError::InvalidPayload(
            "unsupported Continue NativePath cursor version".to_owned(),
        ));
    }

    if prior.source_identity == source_identity.source_identity()
        && prior.source_revision == revision
    {
        if prior.frontier == next || prior.frontier.next_page_ordinal > next.next_page_ordinal {
            if page.core.source.is_some() && prior.terminal {
                return Ok(CursorPlan::Publish {
                    cursor: prior,
                    terminal_reconciliation: true,
                });
            }
            return Ok(CursorPlan::AlreadyCommitted);
        }
        if prior.frontier.next_page_ordinal == next.next_page_ordinal {
            return Err(CaptureError::InvalidPayload(
                "Continue NativePath cursor conflicts at the same page frontier".to_owned(),
            ));
        }
        if prior.frontier != expected {
            return Err(CaptureError::InvalidPayload(
                "Continue NativePath cursor is discontinuous".to_owned(),
            ));
        }
        return Ok(CursorPlan::Publish {
            cursor: ContinueNativeStoreCursor {
                version: ContinueNativeStoreCursor::VERSION,
                source_identity: source_identity.source_identity().to_owned(),
                source_revision: revision,
                frontier: next,
                terminal: page.terminal,
                generation: prior.generation,
                rejected_records: rejected_records.unwrap_or(prior.rejected_records),
            },
            terminal_reconciliation: page.terminal,
        });
    }

    ensure_initial_frontier(&expected)?;
    let generation = prior
        .generation
        .checked_add(1)
        .ok_or(CaptureError::SystemInvariant(
            "Continue NativePath generation is exhausted",
        ))?;
    Ok(CursorPlan::Publish {
        cursor: ContinueNativeStoreCursor {
            version: ContinueNativeStoreCursor::VERSION,
            source_identity: source_identity.source_identity().to_owned(),
            source_revision: revision,
            frontier: next,
            terminal: page.terminal,
            generation,
            rejected_records: rejected_records.unwrap_or(0),
        },
        terminal_reconciliation: page.terminal,
    })
}

pub(super) fn ensure_initial_frontier(frontier: &ContinuePageFrontier) -> Result<()> {
    if frontier.next_page_ordinal != 0 || frontier.next_history_ordinal != 0 {
        return Err(CaptureError::InvalidPayload(
            "Continue NativePath reset did not begin at the initial frontier".to_owned(),
        ));
    }
    Ok(())
}

pub(super) fn verify_core_page_committed(
    store: &Store,
    context: &ProviderAdapterContext,
    source: &ContinuePublicationSource,
    publication_page: NativePublicationPage<super::super::ContinuePreparedPage>,
) -> Result<()> {
    let (source_identity, page) = publication_page.into_parts();
    validate_page_source_identity(&source_identity, source)?;
    if !source.observation.revalidate().map_err(map_native_error)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let stream = source_cursor_stream(&source.observation)?;
    let stored = store
        .get_sync_cursor(None, &context.machine_id, &stream)?
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Continue output replay requires committed NativePath Core".to_owned(),
            )
        })?;
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    let prior = ContinueNativeStoreCursor::decode(committed.provider_cursor())
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let next = decode_frontier(&page.next_safe_frontier)?;
    if prior.version != ContinueNativeStoreCursor::VERSION
        || prior.source_identity != source_identity.source_identity()
        || prior.source_revision != source_revision(source)
        || prior.frontier.next_page_ordinal < next.next_page_ordinal
        || (prior.frontier.next_page_ordinal == next.next_page_ordinal && prior.frontier != next)
    {
        return Err(CaptureError::InvalidPayload(
            "Continue output replay no longer matches committed Core authority".to_owned(),
        ));
    }
    Ok(())
}

pub(super) fn validate_page_source_identity(
    identity: &NativeSourceIdentity,
    source: &ContinuePublicationSource,
) -> Result<()> {
    if identity.provider() != CaptureProvider::Continue.as_str()
        || identity.source_identity() != format!("continue-session:{}", source.session.identity.0)
    {
        return Err(CaptureError::InvalidPayload(
            "Continue NativePath page source identity mismatch".to_owned(),
        ));
    }
    Ok(())
}

pub(super) fn resolve_source(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    configured_source_root: &Path,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    source: &ContinuePublicationSource,
    summary: &mut ProviderImportSummary,
) -> Result<ResolvedContinueSource> {
    let raw_source_path = source.observation.canonical_path().display().to_string();
    let source_root = configured_source_root.display().to_string();
    let locator_identity = provider_path_identity(source.observation.canonical_path())?;
    let proposed_source_identity = provider_source_identity(
        CaptureProvider::Continue,
        CONTINUE_CLI_SOURCE_FORMAT,
        Some(&source_root),
        Some(&raw_source_path),
        Some(&source.session.identity.0),
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Continue NativePath source has no canonical identity",
    ))?;
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Continue,
        CONTINUE_CLI_SOURCE_FORMAT,
        &locator_identity,
    );
    let revision = source_revision(source);
    let resolution =
        group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::Continue,
            source_format: CONTINUE_CLI_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity,
            cursor_stream: stream,
            proposed_source_identity,
            raw_source_path: Some(raw_source_path.clone()),
            source_revision: revision.clone(),
            observed_at_ms: context.imported_at.timestamp_millis(),
        })?;
    let existing = committed_store.capture_source_by_canonical_identity_session(
        CaptureProvider::Continue,
        CONTINUE_CLI_SOURCE_FORMAT,
        &context.machine_id,
        &resolution.canonical_source_identity,
        &source.session.identity.0,
    )?;
    let source_id = existing
        .as_ref()
        .map(|source| source.id)
        .unwrap_or_else(|| {
            provider_scoped_source_uuid(
                CaptureProvider::Continue,
                &source.session.identity.0,
                CONTINUE_CLI_SOURCE_FORMAT,
                Some(&raw_source_path),
            )
        });
    group.upsert_capture_source(&continue_capture_source(
        context,
        source,
        source_id,
        &raw_source_path,
        &source_root,
        &resolution.canonical_source_identity,
        &revision,
    ))?;
    group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;

    let session_id = provider_import_session_uuid(
        committed_store,
        CaptureProvider::Continue,
        &source.session.identity.0,
        source_id,
        Some(&resolution.canonical_source_identity),
    )?;
    let session = continue_session(
        context,
        options,
        source,
        source_id,
        session_id,
        &resolution.canonical_source_identity,
    );
    let existed = committed_store.get_session(session.id).is_ok();
    group.upsert_session(&session)?;
    if existed {
        summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    } else {
        summary.imported_sessions = summary.imported_sessions.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    }
    Ok(ResolvedContinueSource { source_id, session })
}

pub(super) fn continue_capture_source(
    context: &ProviderAdapterContext,
    source: &ContinuePublicationSource,
    source_id: Uuid,
    raw_source_path: &str,
    source_root: &str,
    canonical_source_identity: &str,
    revision: &str,
) -> CaptureSource {
    CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Continue,
            machine_id: context.machine_id.clone(),
            process_id: None,
            cwd: source.session.workspace_directory.clone(),
            raw_source_path: Some(raw_source_path.to_owned()),
            source_format: Some(CONTINUE_CLI_SOURCE_FORMAT.to_owned()),
            source_root: Some(source_root.to_owned()),
            source_identity: Some(canonical_source_identity.to_owned()),
            external_session_id: Some(source.session.identity.0.clone()),
        },
        started_at: source.session.started_at.unwrap_or(context.imported_at),
        ended_at: None,
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": source.session.identity.0,
                "source_format": CONTINUE_CLI_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "source_identity": canonical_source_identity,
                "source_revision": revision,
                "source_identity_key": provider_scoped_source_identity_key(
                    CaptureProvider::Continue,
                    &source.session.identity.0,
                    CONTINUE_CLI_SOURCE_FORMAT,
                    Some(raw_source_path),
                ),
                "nativepath_publication": 1,
            }),
        ),
    }
}

pub(super) fn continue_session(
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    source: &ContinuePublicationSource,
    source_id: Uuid,
    session_id: Uuid,
    canonical_source_identity: &str,
) -> Session {
    let metadata = serde_json::from_str::<Value>(&source.session.metadata_json)
        .unwrap_or_else(|_| Value::Object(Default::default()));
    Session {
        id: session_id,
        history_record_id: options.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Continue,
        external_session_id: Some(source.session.identity.0.clone()),
        external_agent_id: None,
        agent_type: AgentType::Primary,
        role_hint: Some("continue-cli".to_owned()),
        is_primary: true,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: source.session.started_at.unwrap_or(context.imported_at),
        ended_at: None,
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": source.session.identity.0,
                "source_format": CONTINUE_CLI_SOURCE_FORMAT,
                "session_idempotency_key": format!(
                    "provider-session:{}:{}",
                    CaptureProvider::Continue.as_str(),
                    source.session.identity.0,
                ),
                "canonical_source_identity": canonical_source_identity,
                "metadata": metadata,
                "metadata_hash": source.session.metadata_hash,
            }),
        ),
    }
}

pub(super) fn publish_events(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    options: &ProviderImportOptions,
    resolved: &ResolvedContinueSource,
    events: &[ContinueEventRow],
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let publications = prepare_event_publications(committed_store, resolved, events)?;
    for publication in publications {
        let event = publication.event;
        let provider_event_index = publication.provider_event_index;
        let identity = publication.identity;
        let dedupe_key = Store::provider_event_dedupe_key_with_payload_hash(
            &identity.dedupe_key,
            &event.content_hash,
        )
        .unwrap_or(identity.dedupe_key);
        let body = serde_json::from_str::<Value>(&event.body_json).map_err(|error| {
            CaptureError::InvalidPayload(format!(
                "Continue sanitized event body is invalid: {error}"
            ))
        })?;
        let occurred_at = event.occurred_at.unwrap_or(resolved.session.started_at);
        let normalized = Event {
            id: identity.id,
            seq: identity.seq,
            history_record_id: options.history_record_id,
            session_id: Some(resolved.session.id),
            run_id: None,
            event_type: match event.kind {
                ContinueEventKind::Message => EventType::Message,
                ContinueEventKind::ToolCall => EventType::ToolCall,
            },
            role: Some(match event.role {
                ContinueEventRole::User => EventRole::User,
                ContinueEventRole::Assistant => EventRole::Assistant,
                ContinueEventRole::System => EventRole::System,
                ContinueEventRole::Tool => EventRole::Tool,
                ContinueEventRole::Unknown => EventRole::Unknown,
            }),
            occurred_at,
            capture_source_id: Some(resolved.source_id),
            payload: json!({
                "provider": CaptureProvider::Continue.as_str(),
                "provider_session_id": resolved.session.external_session_id,
                "provider_event_index": provider_event_index,
                "provider_event_hash": event.content_hash,
                "native_item_id": event.native_item_id,
                "body": body,
                "preview": event.preview,
                "searchable_text": event.search_text,
                "calls": event.calls.iter().map(|call| json!({
                    "state_ordinal": call.state_ordinal,
                    "call_id": call.call_id,
                    "nested_call_id": call.nested_call_id,
                    "tool_name": call.tool_name,
                    "status": call.status,
                })).collect::<Vec<_>>(),
                "artifacts": [],
            }),
            payload_blob_id: None,
            dedupe_key: Some(dedupe_key),
            sync: provider_sync_metadata(
                Fidelity::Imported,
                json!({
                    "provider_session_id": resolved.session.external_session_id,
                    "provider_event_index": provider_event_index,
                    "provider_event_hash": event.content_hash,
                    "provider_event_hash_authority": "normalized_payload_fallback",
                    "source_format": CONTINUE_CLI_SOURCE_FORMAT,
                    "source_trust": "provider_native",
                    "source_record_ordinal": event.identity.history_ordinal,
                    "source_record_subrecord_index": 0,
                    "native_item_id": event.native_item_id,
                }),
            ),
        };
        if group.reconcile_provider_event(
            &normalized,
            ProviderEventHashAuthority::NormalizedPayloadFallback,
        )? {
            summary.imported_events = summary.imported_events.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        } else {
            summary.skipped_events = summary.skipped_events.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
        }
        summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
        for (touch, id) in event
            .file_touches
            .iter()
            .zip(publication.touch_ids.iter().copied())
        {
            group.upsert_file_touched(&FileTouched {
                id,
                history_record_id: options.history_record_id,
                run_id: None,
                event_id: Some(normalized.id),
                vcs_workspace_id: None,
                path: touch.path.clone(),
                change_kind: touch.change_kind,
                old_path: touch.old_path.clone(),
                line_count_delta: None,
                confidence: touch.confidence,
                timestamps: timestamps(occurred_at),
                source_id: Some(resolved.source_id),
                sync: provider_sync_metadata(
                    Fidelity::Imported,
                    json!({
                        "provider": CaptureProvider::Continue.as_str(),
                        "provider_session_id": resolved.session.external_session_id,
                        "provider_event_index": provider_event_index,
                        "source_format": CONTINUE_CLI_SOURCE_FORMAT,
                        "metadata": touch.metadata,
                    }),
                ),
            })?;
        }
        for id in publication.touch_ids[event.file_touches.len()..]
            .iter()
            .copied()
        {
            let retired_at = resolved.session.timestamps.updated_at;
            let mut sync = provider_sync_metadata(
                Fidelity::Imported,
                json!({
                    "provider": CaptureProvider::Continue.as_str(),
                    "provider_session_id": resolved.session.external_session_id,
                    "provider_event_index": provider_event_index,
                    "source_format": CONTINUE_CLI_SOURCE_FORMAT,
                    "retired_by": "continue_file_touch_rewrite",
                }),
            );
            sync.deleted_at = Some(retired_at);
            group.upsert_file_touched(&FileTouched {
                id,
                history_record_id: options.history_record_id,
                run_id: None,
                event_id: Some(normalized.id),
                vcs_workspace_id: None,
                path: CONTINUE_RETIRED_FILE_TOUCH_PATH.to_owned(),
                change_kind: None,
                old_path: None,
                line_count_delta: None,
                confidence: Confidence::Unknown,
                timestamps: timestamps(retired_at),
                source_id: Some(resolved.source_id),
                sync,
            })?;
        }
    }
    Ok(())
}

pub(super) fn prepare_event_publications<'event>(
    committed_store: &Store,
    resolved: &ResolvedContinueSource,
    events: &'event [ContinueEventRow],
) -> Result<Vec<ContinueEventPublication<'event>>> {
    let provider_session_id = resolved
        .session
        .external_session_id
        .as_deref()
        .unwrap_or_default();
    let allow_legacy_provider_identity = resolved.session.id
        == crate::provider::importer::provider_session_uuid(
            CaptureProvider::Continue,
            provider_session_id,
        );
    let mut mutation_units = CONTINUE_CORE_PAGE_FIXED_MUTATION_UNITS;
    let mut publications = Vec::with_capacity(events.len());
    for event in events {
        let provider_event_index =
            event
                .identity
                .history_ordinal
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "Continue event index is exhausted",
                ))?;
        let identity = provider_event_import_identity_with_exact_legacy_source(
            committed_store,
            CaptureProvider::Continue,
            provider_session_id,
            resolved.source_id,
            provider_event_index,
            provider_event_index,
            &event.content_hash,
            None,
            Some(provider_event_index),
            allow_legacy_provider_identity,
        )?;
        let mut touch_ids = Vec::new();
        for touch_index in 0..=CONTINUE_NATIVE_MAX_FILE_TOUCHES_PER_EVENT {
            let id = continue_file_touch_id(
                committed_store,
                resolved,
                provider_event_index,
                touch_index,
            )?;
            if !committed_store.file_touched_exists(id)? {
                break;
            }
            if touch_index == CONTINUE_NATIVE_MAX_FILE_TOUCHES_PER_EVENT {
                return Err(CaptureError::InvalidPayload(format!(
                    "stored Continue event {provider_event_index} exceeds the {} file-touch \
                     transaction bound",
                    CONTINUE_NATIVE_MAX_FILE_TOUCHES_PER_EVENT
                )));
            }
            touch_ids.push(id);
        }
        while touch_ids.len() < event.file_touches.len() {
            touch_ids.push(continue_file_touch_id(
                committed_store,
                resolved,
                provider_event_index,
                touch_ids.len(),
            )?);
        }
        mutation_units = mutation_units
            .checked_add(1_usize.saturating_add(touch_ids.len()))
            .ok_or(CaptureError::SystemInvariant(
                "Continue publication mutation accounting overflowed",
            ))?;
        publications.push(ContinueEventPublication {
            event,
            provider_event_index,
            identity,
            touch_ids,
        });
    }
    if mutation_units > NATIVE_PATH_MAX_MUTATION_UNITS {
        return Err(CaptureError::InvalidPayload(format!(
            "Continue page requires {mutation_units} Store mutation units, exceeding the \
             {NATIVE_PATH_MAX_MUTATION_UNITS} unit transaction bound"
        )));
    }
    Ok(publications)
}

pub(super) fn continue_file_touch_id(
    committed_store: &Store,
    resolved: &ResolvedContinueSource,
    provider_event_index: u64,
    touch_index: usize,
) -> Result<Uuid> {
    let packed_touch_index = provider_event_index
        .checked_mul(u64::from(u16::MAX) + 1)
        .and_then(|base| base.checked_add(u64::try_from(touch_index).ok()?))
        .ok_or(CaptureError::SystemInvariant(
            "Continue file-touch identity overflowed",
        ))?;
    let provider_session_id = resolved
        .session
        .external_session_id
        .as_deref()
        .unwrap_or_default();
    provider_file_touch_import_id(
        committed_store,
        CaptureProvider::Continue,
        provider_session_id,
        resolved.source_id,
        Some(provider_event_index),
        packed_touch_index,
        resolved.session.id
            == crate::provider::importer::provider_session_uuid(
                CaptureProvider::Continue,
                provider_session_id,
            ),
    )
}

pub(super) fn already_committed_summary(
    page: &NativeIngestionPage<super::super::ContinuePreparedPage>,
) -> ProviderImportSummary {
    let mut summary = ProviderImportSummary::default();
    summary.skipped_events = page.core.events.len();
    summary.skipped = summary.skipped.saturating_add(summary.skipped_events);
    if page.core.source.is_some() {
        summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    }
    summary.accepted_content_records = summary
        .accepted_content_records
        .saturating_add(page.core.events.len());
    summary.set_work_result(ProviderImportWorkResult::NoOp);
    summary
}
