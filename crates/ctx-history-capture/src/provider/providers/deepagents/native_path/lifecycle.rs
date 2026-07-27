use super::*;

#[derive(Debug)]
pub(super) struct PredictedRetirementPage {
    pub(super) next_after: Option<NativePathSourceEntityFrontier>,
    pub(super) done: bool,
}

pub(super) fn predict_retirement_page(
    store: &Store,
    authority: &DeepAgentsSourceAuthority,
    context: &ProviderAdapterContext,
    after: Option<&NativePathSourceEntityFrontier>,
    limit: usize,
) -> Result<PredictedRetirementPage> {
    let sources = known_capture_sources(store, authority, context)?;
    let mut candidates = Vec::<NativePathSourceEntityFrontier>::new();
    for source in sources {
        let Some(provider_session_id) = source.descriptor.external_session_id.as_deref() else {
            continue;
        };
        let Some(session) = store.session_by_capture_source_and_external_session(
            source.id,
            CaptureProvider::DeepAgents,
            provider_session_id,
        )?
        else {
            continue;
        };
        if session.sync.deleted_at.is_none() {
            candidates.push(NativePathSourceEntityFrontier {
                kind: NativePathSourceEntityKind::Session,
                id: session.id,
            });
        }
        for run in store.runs_for_session(session.id)? {
            if run.sync.deleted_at.is_none() {
                candidates.push(NativePathSourceEntityFrontier {
                    kind: NativePathSourceEntityKind::Run,
                    id: run.id,
                });
            }
        }
        for event in store.events_for_session(session.id)? {
            if event.sync.deleted_at.is_none() {
                candidates.push(NativePathSourceEntityFrontier {
                    kind: NativePathSourceEntityKind::Event,
                    id: event.id,
                });
            }
        }
    }
    candidates.sort_by_key(|candidate| (retirement_kind_order(candidate.kind), candidate.id));
    candidates.dedup();
    let after_key = after.map(|value| (retirement_kind_order(value.kind), value.id));
    let remaining = candidates
        .into_iter()
        .filter(|candidate| {
            after_key
                .is_none_or(|after| (retirement_kind_order(candidate.kind), candidate.id) > after)
        })
        .collect::<Vec<_>>();
    let done = remaining.len() <= limit;
    let next_after = remaining.into_iter().take(limit).next_back();
    Ok(PredictedRetirementPage { next_after, done })
}

pub(super) fn retirement_kind_order(kind: NativePathSourceEntityKind) -> u8 {
    match kind {
        NativePathSourceEntityKind::SessionEdge => 0,
        NativePathSourceEntityKind::Run => 1,
        NativePathSourceEntityKind::Event => 2,
        NativePathSourceEntityKind::FileTouch => 3,
        NativePathSourceEntityKind::Session => 4,
    }
}

pub(super) fn known_capture_sources(
    store: &Store,
    authority: &DeepAgentsSourceAuthority,
    context: &ProviderAdapterContext,
) -> Result<Vec<CaptureSource>> {
    let canonical_path = authority.canonical_database_path.display().to_string();
    let configured_path = authority.database_path.display().to_string();
    let configured_root = authority.configured_source_root.display().to_string();
    let mut sources = store
        .list_capture_sources()?
        .into_iter()
        .filter(|source| {
            source.descriptor.provider == CaptureProvider::DeepAgents
                && source.descriptor.machine_id == context.machine_id
                && source.descriptor.source_format.as_deref()
                    == Some(DEEPAGENTS_SQLITE_SOURCE_FORMAT)
                && (source.descriptor.source_identity.as_deref()
                    == Some(authority.canonical_source_identity.as_str())
                    || source.descriptor.raw_source_path.as_deref()
                        == Some(canonical_path.as_str())
                    || source.descriptor.raw_source_path.as_deref()
                        == Some(configured_path.as_str())
                    || source.descriptor.source_root.as_deref() == Some(configured_root.as_str()))
        })
        .collect::<Vec<_>>();
    sources.sort_by_key(|source| source.id);
    sources.dedup_by_key(|source| source.id);
    Ok(sources)
}

pub(super) fn resolve_source_id(
    store: &Store,
    thread: &DeepAgentsThread,
    machine_id: &str,
    canonical_source_identity: &str,
    raw_source_path: &str,
) -> Result<Uuid> {
    Ok(store
        .capture_source_by_canonical_identity_session(
            CaptureProvider::DeepAgents,
            DEEPAGENTS_SQLITE_SOURCE_FORMAT,
            machine_id,
            canonical_source_identity,
            &thread.thread_id,
        )?
        .map(|source| source.id)
        .unwrap_or_else(|| {
            provider_scoped_source_uuid(
                CaptureProvider::DeepAgents,
                &thread.thread_id,
                DEEPAGENTS_SQLITE_SOURCE_FORMAT,
                Some(raw_source_path),
            )
        }))
}

pub(super) fn capture_source(
    source_id: Uuid,
    thread: &DeepAgentsThread,
    authority: &DeepAgentsSourceAuthority,
    context: &ProviderAdapterContext,
    raw_source_path: &str,
    canonical_source_identity: &str,
) -> CaptureSource {
    let source_root = authority.configured_source_root.display().to_string();
    CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::DeepAgents,
            machine_id: context.machine_id.clone(),
            process_id: None,
            cwd: thread.cwd.clone(),
            raw_source_path: Some(raw_source_path.to_owned()),
            source_format: Some(DEEPAGENTS_SQLITE_SOURCE_FORMAT.to_owned()),
            source_root: Some(source_root.clone()),
            source_identity: Some(canonical_source_identity.to_owned()),
            external_session_id: Some(thread.thread_id.clone()),
        },
        started_at: thread.created_at,
        ended_at: Some(thread.updated_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": thread.thread_id,
                "source_format": DEEPAGENTS_SQLITE_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "source_identity": canonical_source_identity,
                "source_root": source_root,
                "source_revision": authority.source_revision,
                "source_identity_key": provider_scoped_source_identity_key(
                    CaptureProvider::DeepAgents,
                    &thread.thread_id,
                    DEEPAGENTS_SQLITE_SOURCE_FORMAT,
                    Some(raw_source_path),
                ),
                "schema_fingerprint": authority.schema_fingerprint,
                "source_metadata": {
                    "adapter": DEEPAGENTS_SQLITE_SOURCE_FORMAT,
                    "sqlite_user_version": authority.sqlite_user_version,
                    "schema_fingerprint": authority.schema_fingerprint,
                    "source_observation_revision": authority.source_revision,
                    "message_import_policy":
                        "root writes.messages only; checkpoint state blobs are not indexed",
                },
                "nativepath_publication": DEEPAGENTS_NATIVE_PARSER_REVISION,
            }),
        ),
    }
}

pub(super) fn cursor_transition(
    context: &ProviderAdapterContext,
    authority: &DeepAgentsSourceAuthority,
    stored: Option<&SyncCursor>,
    next_cursor: &DeepAgentsNativeCursor,
) -> Result<NativePathCursorTransition> {
    Ok(NativePathCursorTransition::new(
        stored.map(|cursor| cursor.cursor.clone()),
        SyncCursor {
            id: Uuid::new_v4(),
            team_id: None,
            device_id: context.machine_id.clone(),
            stream: authority.cursor_stream.clone(),
            cursor: next_cursor.encode()?,
            last_synced_at: Some(context.imported_at),
            timestamps: timestamps(context.imported_at),
        },
    ))
}

pub(super) fn publication_id(
    authority: &DeepAgentsSourceAuthority,
    current: &DeepAgentsNativeCursor,
    next: &DeepAgentsNativeCursor,
    encoded_next_cursor: &str,
) -> String {
    let mut digest = Sha256::new();
    digest.update(DEEPAGENTS_PUBLICATION_DOMAIN);
    digest.update(authority.route_identity.as_bytes());
    digest.update(authority.source_revision.as_bytes());
    digest.update(current.generation.to_be_bytes());
    digest.update(serde_json::to_vec(&current.phase).unwrap_or_default());
    digest.update(serde_json::to_vec(&next.phase).unwrap_or_default());
    digest.update(encoded_next_cursor.as_bytes());
    format!("deepagents-native:{}", hex(&digest.finalize()))
}

pub(super) fn source_revision(
    snapshot: &ProviderSqliteSourceSnapshot,
    schema_fingerprint: &str,
) -> String {
    format!(
        "deepagents-native-sqlite-v1:parser={DEEPAGENTS_NATIVE_PARSER_REVISION};policy={DEEPAGENTS_NATIVE_POLICY_REVISION};schema={schema_fingerprint};{}",
        snapshot.revision_component(),
    )
}

pub(super) fn decode_core_cursor_for_migration(
    stored: Option<&SyncCursor>,
) -> Result<Option<DeepAgentsNativeCursor>> {
    let Some(stored) = stored else {
        return Ok(None);
    };
    if let Ok(committed) = decode_native_path_committed_cursor(&stored.cursor) {
        return DeepAgentsNativeCursor::decode(committed.provider_cursor()).map(Some);
    }
    if CertifiedProviderCursor::decode_if_certified(&stored.cursor)?.is_some() {
        return Ok(None);
    }
    Err(CaptureError::InvalidPayload(
        "Deep Agents cursor is neither NativePath nor a released migration cursor".to_owned(),
    ))
}

pub(super) fn require_complete_matching_core(
    store: &Store,
    authority: &DeepAgentsSourceAuthority,
    context: &ProviderAdapterContext,
) -> Result<()> {
    let stored = store
        .get_sync_cursor(None, &context.machine_id, &authority.cursor_stream)?
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Deep Agents Pro replay requires committed NativePath Core".to_owned(),
            )
        })?;
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    let cursor = DeepAgentsNativeCursor::decode(committed.provider_cursor())?;
    if cursor.route_identity != authority.route_identity
        || cursor.source_revision != authority.source_revision
        || cursor.schema_fingerprint != authority.schema_fingerprint
        || !matches!(cursor.phase, DeepAgentsCorePhase::Complete)
    {
        return Err(CaptureError::InvalidPayload(
            "Deep Agents Pro replay source no longer matches committed Core authority".to_owned(),
        ));
    }
    Ok(())
}

pub(super) fn ensure_retained_bound(retained_bytes: usize) -> Result<()> {
    if retained_bytes > NATIVE_PATH_MAX_RETAINED_PAGE_BYTES {
        return Err(CaptureError::InvalidPayload(
            "Deep Agents NativePath page exceeds the retained-byte bound".to_owned(),
        ));
    }
    Ok(())
}

pub(super) fn revalidate_optional(
    snapshot: Option<&ProviderSqliteSourceSnapshot>,
    path: &Path,
) -> Result<()> {
    if let Some(snapshot) = snapshot {
        if !snapshot.revalidate(path)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
    }
    Ok(())
}

pub(super) fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

pub(super) fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

pub(super) fn retire_missing_source(
    original_path: &Path,
    database_path: &Path,
    store: &mut Store,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    if options.inventory_observation_token.is_none() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: database_path.to_path_buf(),
            reason: "Deep Agents sessions.db is missing",
        });
    }
    let route_identity = provider_path_identity(database_path)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::DeepAgents,
        DEEPAGENTS_SQLITE_SOURCE_FORMAT,
        &route_identity,
    );
    let Some(stored) = store.get_sync_cursor(None, &context.machine_id, &cursor_stream)? else {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: database_path.to_path_buf(),
            reason: "Deep Agents sessions.db is missing and has no prior route authority",
        });
    };
    let committed = decode_native_path_committed_cursor(&stored.cursor).map_err(|_| {
        CaptureError::InvalidPayload(
            "Deep Agents missing-source retirement requires a NativePath cursor; released cursors are migration-only while the source is readable".to_owned(),
        )
    })?;
    let prior = DeepAgentsNativeCursor::decode(committed.provider_cursor())?;
    if matches!(prior.phase, DeepAgentsCorePhase::MissingComplete) {
        let mut summary = ProviderImportSummary::default();
        summary.set_terminal_outcome(ProviderImportTerminalOutcome::CoreCursorCommitted);
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }
    let cursor = if matches!(
        prior.phase,
        DeepAgentsCorePhase::MissingStage { .. } | DeepAgentsCorePhase::MissingRetire { .. }
    ) {
        prior
    } else {
        let generation = prior
            .generation
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "Deep Agents source generation is exhausted",
            ))?;
        DeepAgentsNativeCursor {
            generation,
            generation_staged: false,
            phase: DeepAgentsCorePhase::MissingStage { next_source: 0 },
            ..prior
        }
    };
    let configured_source_root = context
        .source_root
        .clone()
        .or_else(|| context.source_path.clone())
        .unwrap_or_else(|| original_path.to_path_buf());
    let authority = DeepAgentsSourceAuthority {
        configured_source_root,
        database_path: database_path.to_path_buf(),
        canonical_database_path: database_path.to_path_buf(),
        route_identity,
        cursor_stream,
        proposed_source_identity: cursor.canonical_source_identity.clone(),
        canonical_source_identity: cursor.canonical_source_identity.clone(),
        source_revision: cursor.source_revision.clone(),
        schema_fingerprint: cursor.schema_fingerprint.clone(),
        sqlite_user_version: 0,
    };
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let mut cursor = cursor;
        let mut summary = ProviderImportSummary::default();
        loop {
            cursor = match cursor.phase.clone() {
                DeepAgentsCorePhase::MissingStage { next_source } => publish_source_stage_page(
                    store,
                    &bulk_guard,
                    None,
                    &authority,
                    context,
                    &cursor,
                    next_source,
                    true,
                )?,
                DeepAgentsCorePhase::MissingRetire { after } => publish_retirement_page(
                    store,
                    &bulk_guard,
                    None,
                    &authority,
                    context,
                    &cursor,
                    after,
                    true,
                )?,
                DeepAgentsCorePhase::MissingComplete => break,
                _ => {
                    return Err(CaptureError::InvalidPayload(
                        "Deep Agents disappearance cursor has an invalid phase".to_owned(),
                    ));
                }
            };
            summary.set_work_result(ProviderImportWorkResult::Changed);
            if matches!(cursor.phase, DeepAgentsCorePhase::MissingComplete) {
                break;
            }
            if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup {
                summary.work_remaining = true;
                break;
            }
        }
        if cursor.is_complete() {
            summary.set_terminal_outcome(ProviderImportTerminalOutcome::CoreCursorCommitted);
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

pub(super) fn missing_retirement_reason(
    configured_source_root: &Path,
    database_path: &Path,
) -> ProviderSourceRouteRetirementReason {
    if configured_source_root == database_path || configured_source_root.exists() {
        ProviderSourceRouteRetirementReason::SourceMissing
    } else {
        ProviderSourceRouteRetirementReason::RootMissing
    }
}
