use super::event_projection::{
    canonical_actor, core_publication_id, encode_core_cursor, provider_sync_cursor,
    publish_file_touch, publish_message, relationship_edge, relationship_placeholder,
};
use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_core_page(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    source: &CrushNativeSource,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    stored: Option<SyncCursor>,
    page: CrushNativePage,
    summary: &mut ProviderImportSummary,
) -> Result<bool> {
    if !source.snapshot.revalidate(&source.canonical_path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let next_cursor_state = page.next.clone();
    let next_cursor = encode_core_cursor(&page.next)?;
    let next = provider_sync_cursor(
        &context.machine_id,
        source.cursor_stream.clone(),
        next_cursor,
        context.imported_at,
    );
    let transition =
        NativePathCursorTransition::new(stored.as_ref().map(|cursor| cursor.cursor.clone()), next);
    let publication_id = core_publication_id(source, &page, &transition);
    let retained_bytes = page.row.as_ref().map_or(
        CRUSH_NATIVE_PAGE_OVERHEAD_BYTES,
        CrushNativeRow::retained_bytes,
    );
    let accounting = NativePathGroupAccounting::new(1, 1, retained_bytes)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        restore_cursor_rejections(summary, &next_cursor_state);
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(false);
    }

    let resolution =
        group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::Crush,
            source_format: CRUSH_SQLITE_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity: source.locator_identity.clone(),
            cursor_stream: source.cursor_stream.clone(),
            proposed_source_identity: source.proposed_source_identity.clone(),
            raw_source_path: Some(source.raw_source_path.clone()),
            source_revision: source.source_revision.clone(),
            observed_at_ms: context.imported_at.timestamp_millis(),
        })?;

    if let Some(row) = page.row {
        publish_native_row(
            committed_store,
            &mut group,
            source,
            context,
            options,
            &resolution,
            row,
            summary,
        )?;
    }
    if !source.snapshot.revalidate(&source.canonical_path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    restore_cursor_rejections(summary, &next_cursor_state);
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
fn publish_native_row(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    source: &CrushNativeSource,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    resolution: &ctx_history_store::ProviderSourceLocatorResolution,
    row: CrushNativeRow,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    match row {
        CrushNativeRow::Session { row, .. } => {
            let draft = project_session(
                &row,
                &source.raw_source_path,
                source.schema.user_version,
                &source.schema.schema_fingerprint,
                context.imported_at,
            );
            publish_session_draft(
                committed_store,
                group,
                source,
                context,
                options,
                &resolution.canonical_source_identity,
                &resolution.route_binding(),
                &draft,
                summary,
            )?;
            summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
        }
        CrushNativeRow::Message {
            projection,
            touches,
            ..
        } => {
            publish_message(
                committed_store,
                group,
                source,
                context,
                options,
                &resolution.canonical_source_identity,
                &resolution.route_binding(),
                *projection,
                touches,
                summary,
            )?;
        }
        CrushNativeRow::File { touch, .. } | CrushNativeRow::ReadFile { touch, .. } => {
            publish_file_touch(
                committed_store,
                group,
                source,
                context,
                options,
                &resolution.canonical_source_identity,
                &resolution.route_binding(),
                touch,
                None,
                summary,
            )?;
        }
        CrushNativeRow::Rejection { .. } => {}
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn publish_session_draft(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    source: &CrushNativeSource,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    canonical_source_identity: &str,
    route_binding: &ctx_history_store::ProviderSourceRouteBinding,
    draft: &CrushSessionDraft,
    summary: &mut ProviderImportSummary,
) -> Result<(Uuid, Session)> {
    let provider_session_id = &draft.provider_session_id;
    let source_id = source_id_for_session(
        committed_store,
        source,
        context,
        canonical_source_identity,
        provider_session_id,
    )?;
    group.upsert_capture_source(&canonical_capture_source(
        source,
        context,
        draft,
        source_id,
        canonical_source_identity,
    ))?;
    group.bind_capture_source_provider_route(source_id, route_binding)?;
    let session = canonical_session(
        committed_store,
        source,
        context,
        options,
        draft,
        source_id,
        canonical_source_identity,
    )?;
    let existed = committed_store.get_session(session.id).is_ok();
    if let Some(parent_id) = session.parent_session_id {
        if committed_store.get_session(parent_id).is_err() {
            group.upsert_session(&relationship_placeholder(
                source,
                context,
                options,
                parent_id,
                draft
                    .parent_provider_session_id
                    .as_deref()
                    .unwrap_or("unknown-parent"),
                source_id,
                canonical_source_identity,
            ))?;
        }
    }
    group.upsert_session(&session)?;
    if existed {
        summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    } else {
        summary.imported_sessions = summary.imported_sessions.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    }
    if let Some(parent_id) = session.parent_session_id {
        let edge = relationship_edge(
            source,
            context,
            &session,
            parent_id,
            source_id,
            canonical_source_identity,
        );
        let existed = committed_store.session_edge_exists(edge.id)?;
        group.upsert_projection_neutral_session_edge(&canonical_actor(&session), &edge)?;
        if existed {
            summary.skipped_edges = summary.skipped_edges.saturating_add(1);
        } else {
            summary.imported_edges = summary.imported_edges.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        }
    }
    Ok((source_id, session))
}

fn source_id_for_session(
    committed_store: &Store,
    source: &CrushNativeSource,
    context: &ProviderAdapterContext,
    canonical_source_identity: &str,
    provider_session_id: &str,
) -> Result<Uuid> {
    Ok(committed_store
        .capture_source_by_canonical_identity_session(
            CaptureProvider::Crush,
            CRUSH_SQLITE_SOURCE_FORMAT,
            &context.machine_id,
            canonical_source_identity,
            provider_session_id,
        )?
        .map(|source| source.id)
        .unwrap_or_else(|| {
            provider_scoped_source_uuid(
                CaptureProvider::Crush,
                provider_session_id,
                CRUSH_SQLITE_SOURCE_FORMAT,
                Some(&source.raw_source_path),
            )
        }))
}

fn canonical_capture_source(
    source: &CrushNativeSource,
    context: &ProviderAdapterContext,
    draft: &CrushSessionDraft,
    source_id: Uuid,
    canonical_source_identity: &str,
) -> CaptureSource {
    CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Crush,
            machine_id: context.machine_id.clone(),
            process_id: None,
            cwd: None,
            raw_source_path: Some(source.raw_source_path.clone()),
            source_format: Some(CRUSH_SQLITE_SOURCE_FORMAT.to_owned()),
            source_root: Some(source.source_root.clone()),
            source_identity: Some(canonical_source_identity.to_owned()),
            external_session_id: Some(draft.provider_session_id.clone()),
        },
        started_at: draft.started_at,
        ended_at: draft.ended_at,
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": draft.provider_session_id,
                "source_format": CRUSH_SQLITE_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "source_identity": canonical_source_identity,
                "source_root": source.source_root,
                "source_revision": source.source_revision,
                "source_identity_key": provider_scoped_source_identity_key(
                    CaptureProvider::Crush,
                    &draft.provider_session_id,
                    CRUSH_SQLITE_SOURCE_FORMAT,
                    Some(&source.raw_source_path),
                ),
                "metadata": draft.source_metadata,
                "nativepath_publication": CRUSH_NATIVE_PARSER_REVISION,
            }),
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn canonical_session(
    committed_store: &Store,
    source: &CrushNativeSource,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    draft: &CrushSessionDraft,
    source_id: Uuid,
    canonical_source_identity: &str,
) -> Result<Session> {
    let provider_session_id = &draft.provider_session_id;
    let id = provider_import_session_uuid(
        committed_store,
        CaptureProvider::Crush,
        provider_session_id,
        source_id,
        Some(canonical_source_identity),
    )?;
    let parent_session_id = draft
        .parent_provider_session_id
        .as_deref()
        .map(|parent| {
            let parent_source_id = source_id_for_session(
                committed_store,
                source,
                context,
                canonical_source_identity,
                parent,
            )?;
            provider_import_session_uuid(
                committed_store,
                CaptureProvider::Crush,
                parent,
                parent_source_id,
                Some(canonical_source_identity),
            )
        })
        .transpose()?;
    Ok(Session {
        id,
        history_record_id: options.history_record_id,
        parent_session_id,
        root_session_id: parent_session_id,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Crush,
        external_session_id: Some(provider_session_id.clone()),
        external_agent_id: None,
        agent_type: if parent_session_id.is_some() {
            AgentType::Subagent
        } else {
            AgentType::Primary
        },
        role_hint: Some(
            if parent_session_id.is_some() {
                "subagent"
            } else {
                "primary"
            }
            .to_owned(),
        ),
        is_primary: parent_session_id.is_none(),
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: draft.started_at,
        ended_at: draft.ended_at,
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": provider_session_id,
                "parent_provider_session_id": draft.parent_provider_session_id,
                "root_provider_session_id": draft.parent_provider_session_id,
                "source_format": CRUSH_SQLITE_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "session_idempotency_key": format!(
                    "provider-session:{}:{}",
                    CaptureProvider::Crush.as_str(),
                    provider_session_id,
                ),
                "metadata": draft.session_metadata,
                "nativepath_publication": CRUSH_NATIVE_PARSER_REVISION,
            }),
        ),
    })
}
