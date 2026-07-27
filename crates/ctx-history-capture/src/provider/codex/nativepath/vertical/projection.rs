use super::*;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct CodexNativeCoreWrite {
    pub(super) imported_events: usize,
    pub(super) skipped_events: usize,
}

pub(super) fn write_raw_core(
    store: &Store,
    publication: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    context: &CodexPublicationContext,
    pages: &[CodexNativePage],
) -> VerticalResult<CodexNativeCoreWrite> {
    let raw_source_path = context.source.source_path.display().to_string();
    let locator = ProviderSourceLocatorObservation {
        provider: CaptureProvider::Codex,
        source_format: CODEX_SESSION_SOURCE_FORMAT.to_owned(),
        machine_id: context.options.machine_id.clone(),
        locator_identity: source_locator_identity(
            &context.cursor_stream,
            &context.proposed_source_namespace,
        ),
        cursor_stream: context.cursor_stream.clone(),
        proposed_source_identity: context.proposed_source_namespace.clone(),
        raw_source_path: Some(raw_source_path.clone()),
        source_revision: context.source_revision.clone(),
        observed_at_ms: context.options.imported_at.timestamp_millis(),
    };
    let resolution = publication.reconcile_provider_source_locator(&locator)?;
    let resolved =
        resolved_projection_identity(store, context, &resolution.canonical_source_identity)?;
    if pages.iter().all(|page| page.core_rows.is_empty()) && !resolved.materialized {
        return Ok(CodexNativeCoreWrite::default());
    }
    let mut retained = NativePathRetainedSourceEntities::default();
    publication.upsert_capture_source(&capture_source(
        context,
        &resolved,
        &raw_source_path,
        &resolution.canonical_source_identity,
    ))?;
    publication
        .bind_capture_source_provider_route(resolved.source_id, &resolution.route_binding())?;
    let session = session(context, &resolved, &raw_source_path);
    publication.upsert_session(&session)?;
    retained.capture_source_ids.push(resolved.source_id);
    retained.session_ids.push(resolved.session_id);
    let mut write = CodexNativeCoreWrite::default();
    if let Some(parent_session_id) = session.parent_session_id {
        let edge = parent_edge(context, &resolved, &session, parent_session_id);
        publication.upsert_projection_neutral_session_edge(&canonical_actor(&session), &edge)?;
        retained.session_edge_ids.push(edge.id);
    }
    for page in pages {
        for row in &page.core_rows {
            let mut event_identity = provider_source_event_import_identity(
                resolved.source_id,
                row.raw_ordinal,
                &row.normalized_body_hash,
            );
            event_identity = avoid_provider_source_event_seq_collision(
                store,
                event_identity,
                resolved.source_id,
                row.raw_ordinal,
                row.raw_ordinal,
            )?;
            let line_number = usize::try_from(row.raw_ordinal)
                .ok()
                .and_then(|ordinal| ordinal.checked_add(1))
                .ok_or(CodexNativeVerticalError::CorruptFrontier(
                    "provider event ordinal exceeds platform limits",
                ))?;
            let (event, command_run) = codex_canonical_event(
                &context.owner.native_session_id,
                CODEX_SESSION_SOURCE_FORMAT,
                ProviderSourceTrust::ProviderExport,
                context.options.imported_at,
                context.options.history_record_id,
                resolved.source_id,
                resolved.session_id,
                line_number,
                &row.provider_event,
                &row.normalized_body_hash,
                ProviderEventHashAuthority::NormalizedPayloadFallback,
                &event_identity,
            )?;
            if let Some(run) = &command_run {
                publication.upsert_run(run)?;
                retained.run_ids.push(run.id);
            }
            let inserted = publication.reconcile_provider_event(
                &event,
                ProviderEventHashAuthority::NormalizedPayloadFallback,
            )?;
            if inserted {
                write.imported_events = write.imported_events.saturating_add(1);
            } else {
                write.skipped_events = write.skipped_events.saturating_add(1);
            }
            for file in &row.file_touches {
                if file.provider_event_index != Some(row.raw_ordinal) {
                    return Err(CodexNativeVerticalError::CorruptFrontier(
                        "file touch does not belong to its provider event",
                    ));
                }
                let touch_id = provider_file_touch_import_id(
                    store,
                    file.provider,
                    &file.provider_session_id,
                    resolved.source_id,
                    file.provider_event_index,
                    file.provider_touch_index,
                    false,
                )?;
                publication.upsert_file_touched(&codex_file_touched(
                    context,
                    &resolved,
                    file,
                    Some(event.id),
                    touch_id,
                ))?;
                retained.file_touch_ids.push(touch_id);
            }
            retained.event_ids.push(event.id);
        }
    }
    if context.stage_generation {
        publication.stage_source_generation_page(
            &source_generation_key(context, &resolution.canonical_source_identity),
            &retained,
        )?;
    }
    Ok(write)
}

struct ResolvedProjectionIdentity {
    canonical_source_identity: String,
    source_id: Uuid,
    session_id: Uuid,
    parent_session_id: Option<Uuid>,
    root_session_id: Uuid,
    materialized: bool,
}

fn resolved_projection_identity(
    store: &Store,
    context: &CodexPublicationContext,
    canonical_source_identity: &str,
) -> VerticalResult<ResolvedProjectionIdentity> {
    let owner = &context.owner;
    let materialized_source = store.capture_source_by_canonical_identity_session(
        CaptureProvider::Codex,
        CODEX_SESSION_SOURCE_FORMAT,
        &context.options.machine_id,
        canonical_source_identity,
        &owner.native_session_id,
    )?;
    let materialized = materialized_source.is_some();
    let source_id = materialized_source
        .map(|source| source.id)
        .unwrap_or_else(|| {
            stable_capture_uuid(canonical_source_identity, "codex-nativepath-capture-source")
        });
    let session_id = store
        .session_by_capture_source_and_external_session(
            source_id,
            CaptureProvider::Codex,
            &owner.native_session_id,
        )?
        .map(|session| session.id)
        .unwrap_or_else(|| {
            provider_source_session_uuid(&context.root_namespace, &owner.native_session_id)
        });
    let parent_session_id = context
        .parent_native_session_id
        .as_deref()
        .map(|parent| provider_source_session_uuid(&context.root_namespace, parent));
    let root_session_id = context
        .root_native_session_id
        .as_deref()
        .or(context.parent_native_session_id.as_deref())
        .map(|root| provider_source_session_uuid(&context.root_namespace, root))
        .unwrap_or(session_id);
    Ok(ResolvedProjectionIdentity {
        canonical_source_identity: canonical_source_identity.to_owned(),
        source_id,
        session_id,
        parent_session_id,
        root_session_id,
        materialized,
    })
}

fn codex_file_touched(
    context: &CodexPublicationContext,
    identity: &ResolvedProjectionIdentity,
    file: &CodexFileTouch,
    event_id: Option<Uuid>,
    touch_id: Uuid,
) -> FileTouched {
    let source_root =
        provider_source_root(file.source_root.as_deref(), file.raw_source_path.as_deref());
    FileTouched {
        id: touch_id,
        history_record_id: context.options.history_record_id,
        run_id: None,
        event_id,
        vcs_workspace_id: None,
        path: file.path.clone(),
        change_kind: file.change_kind,
        old_path: file.old_path.clone(),
        line_count_delta: file.line_count_delta,
        confidence: file.confidence,
        timestamps: timestamps(file.occurred_at),
        source_id: Some(identity.source_id),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider": file.provider.as_str(),
                "provider_session_id": file.provider_session_id,
                "provider_touch_index": file.provider_touch_index,
                "provider_event_index": file.provider_event_index,
                "raw_source_path": file.raw_source_path,
                "source_id": identity.source_id,
                "source_format": file.source_format,
                "source_root": source_root,
                "metadata": file.metadata,
                "session_id": identity.session_id,
            }),
        ),
    }
}

fn capture_source(
    context: &CodexPublicationContext,
    identity: &ResolvedProjectionIdentity,
    raw_source_path: &str,
    canonical_source_identity: &str,
) -> CaptureSource {
    let owner = &context.owner;
    CaptureSource {
        id: identity.source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Codex,
            machine_id: context.options.machine_id.clone(),
            process_id: None,
            cwd: owner.cwd.clone(),
            raw_source_path: Some(raw_source_path.to_owned()),
            source_format: Some(CODEX_SESSION_SOURCE_FORMAT.to_owned()),
            source_root: Some(context.source.source_root.clone()),
            source_identity: Some(canonical_source_identity.to_owned()),
            external_session_id: Some(owner.native_session_id.clone()),
        },
        started_at: owner.started_at,
        ended_at: None,
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": owner.native_session_id,
                "source_format": CODEX_SESSION_SOURCE_FORMAT,
                "source_trust": "provider_export",
                "imported_at": context.options.imported_at,
                "source_identity": canonical_source_identity,
                "source_root": context.source.source_root,
                "cataloged_at_ms": context.source.cataloged_at_ms,
                "catalog_observation": context.source.catalog_observation,
                "nativepath_publication": "codex-v1",
            }),
        ),
    }
}

fn session(
    context: &CodexPublicationContext,
    identity: &ResolvedProjectionIdentity,
    raw_source_path: &str,
) -> Session {
    let owner = &context.owner;
    let is_subagent = identity.parent_session_id.is_some();
    Session {
        id: identity.session_id,
        history_record_id: context.options.history_record_id,
        parent_session_id: identity.parent_session_id,
        root_session_id: Some(identity.root_session_id),
        capture_source_id: Some(identity.source_id),
        provider: CaptureProvider::Codex,
        external_session_id: Some(owner.native_session_id.clone()),
        external_agent_id: owner.external_agent_id.clone(),
        agent_type: if is_subagent {
            AgentType::Subagent
        } else {
            AgentType::Primary
        },
        role_hint: owner
            .role_hint
            .clone()
            .or_else(|| Some(if is_subagent { "worker" } else { "primary" }.to_owned())),
        is_primary: !is_subagent,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: owner.started_at,
        ended_at: None,
        timestamps: timestamps(context.options.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": owner.native_session_id,
                "source_format": CODEX_SESSION_SOURCE_FORMAT,
                "imported_at": context.options.imported_at,
                "session_idempotency_key":
                    format!("provider-session:codex:{}", owner.native_session_id),
                "metadata": {
                    "source_format": CODEX_SESSION_SOURCE_FORMAT,
                    "source_fidelity": "codex_rollout_jsonl",
                    "raw_source_path": raw_source_path,
                    "cwd": owner.cwd,
                    "originator": owner.originator,
                    "cli_version": owner.cli_version,
                    "source": owner.source_kind,
                    "agent_nickname": owner.external_agent_id,
                    "agent_role": owner.role_hint,
                    "model_provider": owner.model_provider,
                    "import_profile": "core",
                    "lineage_resolution": "codex-nativepath-root-inventory-v1",
                },
            }),
        ),
    }
}

fn canonical_actor(session: &Session) -> CanonicalActor {
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

fn parent_edge(
    context: &CodexPublicationContext,
    identity: &ResolvedProjectionIdentity,
    session: &Session,
    parent_session_id: Uuid,
) -> SessionEdge {
    SessionEdge {
        id: stable_capture_uuid(
            &format!(
                "codex-nativepath-edge:{}:{}:{}",
                identity.canonical_source_identity, context.owner.native_session_id, session.id
            ),
            "parent_child",
        ),
        from_session_id: session.id,
        to_session_id: parent_session_id,
        edge_type: SessionEdgeType::ParentChild,
        confidence: Confidence::Explicit,
        source_id: Some(identity.source_id),
        timestamps: timestamps(context.options.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": context.owner.native_session_id,
                "parent_provider_session_id": context.owner.parent_native_session_id,
                "source_format": CODEX_SESSION_SOURCE_FORMAT,
                "imported_at": context.options.imported_at,
                "nativepath_publication": "codex-v1",
            }),
        ),
    }
}

pub(super) fn validate_native_core_chain(
    pages: &[CodexNativePage],
    expected: &CodexNativeFrontier,
    next: &CodexNativeFrontier,
    terminal: bool,
) -> VerticalResult<()> {
    let mut frontier = expected;
    for (index, page) in pages.iter().enumerate() {
        let receipt = page.receipt();
        if &receipt.expected_frontier != frontier
            || receipt.committed_frontier != page.next_safe_frontier
            || receipt.accepted_core_rows != page.core_rows.len()
            || receipt.accepted_physical_records != page.physical_records
            || page.terminal != (terminal && index + 1 == pages.len())
        {
            return Err(CodexNativeVerticalError::CorruptFrontier(
                "provider-owned page receipt chain mismatch",
            ));
        }
        frontier = &page.next_safe_frontier;
    }
    if frontier != next {
        return Err(CodexNativeVerticalError::CorruptFrontier(
            "provider-owned page chain does not reach certified scan frontier",
        ));
    }
    Ok(())
}
