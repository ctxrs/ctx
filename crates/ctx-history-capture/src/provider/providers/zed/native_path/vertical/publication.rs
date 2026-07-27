use super::*;

pub(super) fn publish_zed_core(
    store: &mut Store,
    staging: &ZedNativeStaging,
    context: &ZedPublicationContext<'_>,
    mut plan: CursorPlan,
) -> Result<ProviderImportSummary> {
    let committed_store = Store::open_read_only(store.path())?;
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let mut summary = ProviderImportSummary::default();
        let mut changed_groups = 0_usize;
        loop {
            let phase = plan.cursor.phase;
            if phase == ZedPublicationPhase::Complete {
                break;
            }
            let (sessions, events, next_phase, next_position, retained_bytes) = match phase {
                ZedPublicationPhase::Sessions => {
                    let sessions = staging
                        .session_batch(
                            plan.cursor.position,
                            NATIVE_PATH_MAX_MUTATION_UNITS,
                            NATIVE_PATH_MAX_RETAINED_PAGE_BYTES,
                        )
                        .map_err(map_native_error)?;
                    let consumed = u64::try_from(sessions.len()).unwrap_or(u64::MAX);
                    let next = plan.cursor.position.checked_add(consumed).ok_or(
                        CaptureError::SystemInvariant("Zed session publication cursor overflowed"),
                    )?;
                    let terminal = next >= plan.cursor.session_count;
                    let bytes = sessions.iter().fold(0_usize, |total, item| {
                        total.saturating_add(item.estimated_bytes)
                    });
                    (
                        sessions,
                        Vec::new(),
                        if terminal {
                            ZedPublicationPhase::Events
                        } else {
                            ZedPublicationPhase::Sessions
                        },
                        if terminal { 0 } else { next },
                        bytes,
                    )
                }
                ZedPublicationPhase::Events => {
                    let events = staging
                        .event_batch(
                            plan.cursor.position,
                            NATIVE_PATH_MAX_MUTATION_UNITS,
                            NATIVE_PATH_MAX_RETAINED_PAGE_BYTES,
                        )
                        .map_err(map_native_error)?;
                    let next = events
                        .last()
                        .map_or(plan.cursor.position, |item| item.ordinal);
                    let terminal = next >= plan.cursor.event_count;
                    let bytes = events.iter().fold(0_usize, |total, item| {
                        total.saturating_add(item.estimated_bytes)
                    });
                    (
                        Vec::new(),
                        events,
                        if terminal {
                            ZedPublicationPhase::Complete
                        } else {
                            ZedPublicationPhase::Events
                        },
                        next,
                        bytes,
                    )
                }
                ZedPublicationPhase::Complete => unreachable!(),
            };
            if next_phase == phase && sessions.is_empty() && events.is_empty() {
                return Err(CaptureError::SystemInvariant(
                    "Zed staged publication made no cursor progress",
                ));
            }
            let mut next_cursor = plan.cursor.clone();
            next_cursor.phase = next_phase;
            next_cursor.position = next_position;
            next_cursor.terminal = next_phase == ZedPublicationPhase::Complete;
            let next_cursor_json = encode_cursor(&next_cursor)?;
            let transition = NativePathCursorTransition::new(
                plan.current.as_ref().map(|cursor| cursor.cursor.clone()),
                provider_sync_cursor(
                    &context.adapter.machine_id,
                    context.cursor_stream.clone(),
                    next_cursor_json,
                    context.adapter.imported_at,
                ),
            );
            let changed = publish_zed_group(
                store,
                &committed_store,
                &bulk_guard,
                context,
                &transition,
                &sessions,
                &events,
                retained_bytes,
                &mut summary,
            )?;
            if changed {
                changed_groups = changed_groups.saturating_add(1);
                summary.set_work_result(ProviderImportWorkResult::Changed);
            }
            plan.current =
                store.get_sync_cursor(None, &context.adapter.machine_id, &context.cursor_stream)?;
            plan.cursor = next_cursor;
            if context.options.capture_work_limit == CaptureWorkLimit::OneSafeGroup
                && changed_groups != 0
                && !plan.cursor.terminal
            {
                summary.work_remaining = true;
                break;
            }
        }
        if summary.work_result() == ProviderImportWorkResult::Changed {
            for reason in staging
                .rejection_samples(crate::summaries::MAX_RETAINED_PROVIDER_FAILURES)
                .map_err(map_native_error)?
            {
                summary.record_failure(ProviderImportFailure {
                    line: 0,
                    error: reason,
                });
            }
            summary.failed = usize::try_from(plan.cursor.rejection_count).unwrap_or(usize::MAX);
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

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_zed_group(
    store: &Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ZedPublicationContext<'_>,
    transition: &NativePathCursorTransition,
    sessions: &[ZedStagedSession],
    events: &[ZedStagedEvent],
    retained_bytes: usize,
    summary: &mut ProviderImportSummary,
) -> Result<bool> {
    if !revalidate_zed_snapshot_revision(context.path, &context.authority.snapshot_revision)
        .map_err(map_native_error)?
    {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let publication_id = publication_id(context, transition, sessions, events);
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(
        admission,
        NativePathGroupAccounting::new(1, 1, retained_bytes)?,
    )?;
    let changed = match group
        .classify_cursor_set(&publication_id, std::slice::from_ref(transition))?
    {
        NativePathCursorSetClassification::AllExpected => {
            let resolution =
                group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
                    provider: CaptureProvider::Zed,
                    source_format: ZED_THREADS_SQLITE_SOURCE_FORMAT.to_owned(),
                    machine_id: context.adapter.machine_id.clone(),
                    locator_identity: context.locator_identity.clone(),
                    cursor_stream: context.cursor_stream.clone(),
                    proposed_source_identity: context.canonical_source_identity.clone(),
                    raw_source_path: Some(context.raw_source_path.clone()),
                    source_revision: context.relocation_fingerprint.clone(),
                    observed_at_ms: context.adapter.imported_at.timestamp_millis(),
                })?;
            if resolution.canonical_source_identity != context.canonical_source_identity {
                return Err(CaptureError::SystemInvariant(
                    "Zed source reconciliation disagreed with preflight authority",
                ));
            }
            for staged in sessions {
                publish_session(
                    committed_store,
                    &mut group,
                    context,
                    &resolution.route_binding(),
                    staged,
                    summary,
                )?;
            }
            for staged in events {
                publish_event(committed_store, &mut group, context, staged, summary)?;
            }
            if !revalidate_zed_snapshot_revision(context.path, &context.authority.snapshot_revision)
                .map_err(map_native_error)?
            {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            group.prepare_journal_checkpoint()?;
            group.publish_cursor_set()?;
            true
        }
        NativePathCursorSetClassification::AllNextSameGroup { .. } => false,
    };
    group.commit()?;
    Ok(changed)
}

pub(super) fn publish_session(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    context: &ZedPublicationContext<'_>,
    route: &ctx_history_store::ProviderSourceRouteBinding,
    staged: &ZedStagedSession,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let source_id = source_id_for_thread(committed_store, context, &staged.session.thread_id)?;
    let session = canonical_session(committed_store, context, staged, source_id)?;
    let existed = committed_store.get_session(session.id).is_ok();
    group.upsert_capture_source(&capture_source(context, &staged.session, source_id))?;
    group.bind_capture_source_provider_route(source_id, route)?;
    group.upsert_session(&session)?;
    if let Some(parent_id) = session.parent_session_id {
        group.upsert_projection_neutral_session_edge(
            &canonical_actor(&session),
            &SessionEdge {
                id: stable_capture_uuid(
                    &format!(
                        "provider-source-root:{}:session:{}:parent_child",
                        context.canonical_source_identity, staged.session.thread_id
                    ),
                    "session-edge",
                ),
                from_session_id: session.id,
                to_session_id: parent_id,
                edge_type: SessionEdgeType::ParentChild,
                confidence: Confidence::Explicit,
                source_id: Some(source_id),
                timestamps: timestamps(context.adapter.imported_at),
                sync: provider_sync_metadata(
                    Fidelity::Imported,
                    json!({
                        "provider_session_id": staged.session.thread_id,
                        "source_format": ZED_THREADS_SQLITE_SOURCE_FORMAT,
                        "imported_at": context.adapter.imported_at,
                    }),
                ),
            },
        )?;
        if existed {
            summary.skipped_edges = summary.skipped_edges.saturating_add(1);
        } else {
            summary.imported_edges = summary.imported_edges.saturating_add(1);
        }
    }
    if existed {
        summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    } else {
        summary.imported_sessions = summary.imported_sessions.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    }
    Ok(())
}

pub(super) fn publish_event(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    context: &ZedPublicationContext<'_>,
    staged: &ZedStagedEvent,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let event = &staged.event;
    let thread_id = &event.identity.thread_id;
    let source_id = source_id_for_thread(committed_store, context, thread_id)?;
    let session_id = session_id_for_thread(committed_store, context, thread_id, source_id)?;
    let provider_event_index = event
        .native_order
        .message_ordinal
        .checked_mul(2)
        .and_then(|value| value.checked_add(u64::from(event.native_order.sub_ordinal)))
        .ok_or(CaptureError::SystemInvariant(
            "Zed provider event index overflowed",
        ))?;
    if compute_payload_hash(&event.payload)? != event.content_hash {
        return Err(CaptureError::SystemInvariant(
            "Zed staged event payload hash disagrees with normalized authority",
        ));
    }
    let identity = provider_event_import_identity_with_exact_legacy_source(
        committed_store,
        CaptureProvider::Zed,
        thread_id,
        source_id,
        provider_event_index,
        provider_event_index,
        &event.content_hash,
        None,
        Some(provider_event_index),
        session_id
            == crate::provider::importer::provider_session_uuid(CaptureProvider::Zed, thread_id),
    )?;
    let dedupe_key = Store::provider_event_dedupe_key_with_payload_hash(
        &identity.dedupe_key,
        &event.content_hash,
    )
    .unwrap_or(identity.dedupe_key);
    let mut sync_metadata = json!({
        "provider_session_id": thread_id,
        "provider_event_index": provider_event_index,
        "provider_event_hash": event.content_hash,
        "provider_event_hash_authority": "normalized_payload_fallback",
        "source_format": ZED_THREADS_SQLITE_SOURCE_FORMAT,
        "source_trust": "provider_native",
        "source_record_ordinal": event.sqlite_rowid,
        "source_record_subrecord_index": event.native_order.message_ordinal,
        "message_identity": event.identity.message,
        "nativepath_publication": ZED_NATIVE_CAPTURE_REVISION,
    });
    if event.event_type == ctx_history_core::EventType::Message {
        if let Some(evidence) = event.complete_message.as_ref() {
            let locator = NativeLocator::new(
                ZED_MESSAGE_LOCATOR_KIND,
                event.sqlite_rowid.to_be_bytes().to_vec(),
            )
            .map_err(|_| {
                CaptureError::SystemInvariant("Zed message locator is not representable")
            })?;
            attach_sqlite_complete_content_locator_with_ref(
                CaptureProvider::Zed,
                ZED_THREADS_SQLITE_SOURCE_FORMAT,
                &event.legacy_content_hash,
                &event.payload,
                &mut sync_metadata,
                &locator,
                evidence.record_digest.clone(),
                evidence.content_ref.clone(),
            )?;
        }
    }
    let normalized = Event {
        id: identity.id,
        seq: identity.seq,
        history_record_id: context.options.history_record_id,
        session_id: Some(session_id),
        run_id: None,
        event_type: event.event_type,
        role: Some(event.role),
        occurred_at: event.occurred_at,
        capture_source_id: Some(source_id),
        payload: event.payload.clone(),
        payload_blob_id: None,
        dedupe_key: Some(dedupe_key),
        sync: provider_sync_metadata(Fidelity::Imported, sync_metadata),
    };
    if group.reconcile_provider_event_migrating_exact_legacy_provider_hash(
        &normalized,
        &event.legacy_content_hash,
    )? {
        summary.imported_events = summary.imported_events.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    } else {
        summary.skipped_events = summary.skipped_events.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    }
    summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
    for (touch_index, path) in event.safe_file_touches.iter().enumerate() {
        let touch_index = u64::try_from(touch_index)
            .ok()
            .and_then(|index| {
                provider_event_index
                    .checked_shl(16)
                    .and_then(|base| base.checked_add(index))
            })
            .ok_or(CaptureError::SystemInvariant(
                "Zed file-touch identity overflowed",
            ))?;
        let id = provider_file_touch_import_id(
            committed_store,
            CaptureProvider::Zed,
            thread_id,
            source_id,
            Some(provider_event_index),
            touch_index,
            session_id
                == crate::provider::importer::provider_session_uuid(
                    CaptureProvider::Zed,
                    thread_id,
                ),
        )?;
        group.upsert_file_touched(&FileTouched {
            id,
            history_record_id: context.options.history_record_id,
            run_id: None,
            event_id: Some(normalized.id),
            vcs_workspace_id: None,
            path: path.clone(),
            change_kind: None,
            old_path: None,
            line_count_delta: None,
            confidence: Confidence::Explicit,
            timestamps: timestamps(event.occurred_at),
            source_id: Some(source_id),
            sync: provider_sync_metadata(
                Fidelity::Imported,
                json!({
                    "provider": CaptureProvider::Zed.as_str(),
                    "provider_session_id": thread_id,
                    "provider_touch_index": touch_index,
                    "provider_event_index": provider_event_index,
                    "source_format": ZED_THREADS_SQLITE_SOURCE_FORMAT,
                    "session_id": session_id,
                }),
            ),
        })?;
    }
    Ok(())
}

pub(super) fn capture_source(
    context: &ZedPublicationContext<'_>,
    session: &ZedNativeSession,
    source_id: Uuid,
) -> CaptureSource {
    CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Zed,
            machine_id: context.adapter.machine_id.clone(),
            process_id: None,
            cwd: session.cwd.clone(),
            raw_source_path: Some(context.raw_source_path.clone()),
            source_format: Some(ZED_THREADS_SQLITE_SOURCE_FORMAT.to_owned()),
            source_root: Some(context.source_root.clone()),
            source_identity: Some(context.canonical_source_identity.clone()),
            external_session_id: Some(session.thread_id.clone()),
        },
        started_at: session.created_at,
        ended_at: Some(session.updated_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": session.thread_id,
                "source_format": ZED_THREADS_SQLITE_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.adapter.imported_at,
                "source_identity": context.canonical_source_identity,
                "source_root": context.source_root,
                "source_revision": context.source_revision,
                "relocation_fingerprint": context.relocation_fingerprint,
                "source_identity_key": provider_scoped_source_identity_key(
                    CaptureProvider::Zed,
                    &session.thread_id,
                    ZED_THREADS_SQLITE_SOURCE_FORMAT,
                    Some(&context.raw_source_path),
                ),
                "nativepath_publication": ZED_NATIVE_CAPTURE_REVISION,
            }),
        ),
    }
}

pub(super) fn canonical_session(
    committed_store: &Store,
    context: &ZedPublicationContext<'_>,
    staged: &ZedStagedSession,
    source_id: Uuid,
) -> Result<Session> {
    let session = &staged.session;
    let id = session_id_for_thread(committed_store, context, &session.thread_id, source_id)?;
    let parent_session_id = staged
        .parent_thread_id
        .as_deref()
        .map(|parent| session_identity_for_thread(committed_store, context, parent))
        .transpose()?;
    let root_session_id = (staged.root_thread_id != session.thread_id)
        .then(|| session_identity_for_thread(committed_store, context, &staged.root_thread_id))
        .transpose()?;
    Ok(Session {
        id,
        history_record_id: context.options.history_record_id,
        parent_session_id,
        root_session_id,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Zed,
        external_session_id: Some(session.thread_id.clone()),
        external_agent_id: None,
        agent_type: if parent_session_id.is_some() {
            AgentType::Subagent
        } else {
            AgentType::Primary
        },
        role_hint: Some(if parent_session_id.is_some() {
            "subagent".to_owned()
        } else {
            "primary".to_owned()
        }),
        is_primary: parent_session_id.is_none(),
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: session.created_at,
        ended_at: Some(session.updated_at),
        timestamps: timestamps(context.adapter.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": session.thread_id,
                "parent_provider_session_id": staged.parent_thread_id,
                "root_provider_session_id": staged.root_thread_id,
                "source_format": ZED_THREADS_SQLITE_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.adapter.imported_at,
                "metadata": {
                    "title": session.title,
                    "summary": session.summary,
                    "cwd": session.cwd,
                    "folder_paths": session.folder_paths,
                    "encoding": format!("{:?}", session.encoding).to_lowercase(),
                    "nativepath_publication": ZED_NATIVE_CAPTURE_REVISION,
                },
            }),
        ),
    })
}

pub(super) fn source_id_for_thread(
    store: &Store,
    context: &ZedPublicationContext<'_>,
    thread_id: &str,
) -> Result<Uuid> {
    Ok(store
        .capture_source_by_canonical_identity_session(
            CaptureProvider::Zed,
            ZED_THREADS_SQLITE_SOURCE_FORMAT,
            &context.adapter.machine_id,
            &context.canonical_source_identity,
            thread_id,
        )?
        .map(|source| source.id)
        .unwrap_or_else(|| {
            provider_scoped_source_uuid(
                CaptureProvider::Zed,
                thread_id,
                ZED_THREADS_SQLITE_SOURCE_FORMAT,
                Some(&context.raw_source_path),
            )
        }))
}

pub(super) fn session_id_for_thread(
    store: &Store,
    context: &ZedPublicationContext<'_>,
    thread_id: &str,
    source_id: Uuid,
) -> Result<Uuid> {
    provider_import_session_uuid(
        store,
        CaptureProvider::Zed,
        thread_id,
        source_id,
        Some(&context.canonical_source_identity),
    )
}

pub(super) fn session_identity_for_thread(
    store: &Store,
    context: &ZedPublicationContext<'_>,
    thread_id: &str,
) -> Result<Uuid> {
    let source_id = source_id_for_thread(store, context, thread_id)?;
    session_id_for_thread(store, context, thread_id, source_id)
}

pub(super) fn canonical_actor(session: &Session) -> CanonicalActor {
    CanonicalActor {
        direct_session_id: session.id,
        root_session_id: session.root_session_id.unwrap_or(session.id),
        parent_session_id: session.parent_session_id,
        external_session_id: session.external_session_id.clone(),
        external_agent_id: session.external_agent_id.clone(),
        agent_type: session.agent_type.as_str().to_owned(),
        role_hint: session.role_hint.clone(),
        is_primary: session.is_primary,
    }
}
