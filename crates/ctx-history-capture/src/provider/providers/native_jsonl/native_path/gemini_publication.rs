use super::*;

pub(super) struct GeminiGroupAccumulator<'a> {
    pub(super) store: &'a mut Store,
    committed_store: &'a Store,
    bulk_guard: &'a EventSearchBulkGuard,
    context: GeminiPublicationContext<'a>,
    work_limit: CaptureWorkLimit,
    pages: Vec<GeminiPendingPage>,
    sources: BTreeSet<PathBuf>,
    bytes: usize,
    estimated_mutations: usize,
    summary: ProviderImportSummary,
    output_sink: Option<&'a dyn ProOutputSink>,
    failed_output_sources: BTreeSet<PathBuf>,
    pub(super) stopped: bool,
}

impl<'a> GeminiGroupAccumulator<'a> {
    pub(super) fn new(
        store: &'a mut Store,
        committed_store: &'a Store,
        bulk_guard: &'a EventSearchBulkGuard,
        context: GeminiPublicationContext<'a>,
        work_limit: CaptureWorkLimit,
        output_sink: Option<&'a dyn ProOutputSink>,
    ) -> Self {
        Self {
            store,
            committed_store,
            bulk_guard,
            context,
            work_limit,
            pages: Vec::new(),
            sources: BTreeSet::new(),
            bytes: 0,
            estimated_mutations: 0,
            summary: ProviderImportSummary::default(),
            output_sink,
            failed_output_sources: BTreeSet::new(),
            stopped: false,
        }
    }

    pub(super) fn push(&mut self, pending: GeminiPendingPage) -> Result<()> {
        let next_sources =
            self.sources.len() + usize::from(!self.sources.contains(&pending.source.path));
        let next_bytes = self
            .bytes
            .saturating_add(pending.page.conservative_serialized_bytes);
        let page_mutations = pending
            .page
            .events
            .iter()
            .map(|event| 1_usize.saturating_add(event.safe_file_touches.len()))
            .sum::<usize>()
            .saturating_add(4);
        let next_mutations = self.estimated_mutations.saturating_add(page_mutations);
        if !self.pages.is_empty()
            && (self.pages.len() >= GEMINI_GROUP_MAX_PAGES
                || next_sources > GEMINI_GROUP_MAX_SOURCES
                || next_bytes > GEMINI_GROUP_MAX_BYTES
                || next_mutations > GEMINI_GROUP_MAX_ESTIMATED_MUTATIONS)
        {
            self.flush()?;
            if self.stopped {
                return Ok(());
            }
        }
        self.bytes = self
            .bytes
            .saturating_add(pending.page.conservative_serialized_bytes);
        self.estimated_mutations = self.estimated_mutations.saturating_add(page_mutations);
        self.sources.insert(pending.source.path.clone());
        self.pages.push(pending);
        Ok(())
    }

    pub(super) fn record_unchanged(&mut self, outcome: &GeminiScanOutcome) {
        let sessions = usize::from(outcome.checkpoint.session.is_some());
        let events = usize::try_from(outcome.checkpoint.retained_event_count).unwrap_or(usize::MAX);
        self.summary.skipped_sessions = self.summary.skipped_sessions.saturating_add(sessions);
        self.summary.skipped_events = self.summary.skipped_events.saturating_add(events);
        self.summary.skipped = self
            .summary
            .skipped
            .saturating_add(sessions)
            .saturating_add(events);
        for rejection in &outcome.rejections {
            self.summary.record_failure(ProviderImportFailure {
                line: usize::try_from(rejection.raw_ordinal)
                    .unwrap_or(usize::MAX)
                    .saturating_add(1),
                error: rejection.reason.clone(),
            });
        }
    }

    fn flush(&mut self) -> Result<()> {
        if self.pages.is_empty() {
            return Ok(());
        }
        let mut pages = std::mem::take(&mut self.pages);
        let summary = publish_gemini_group(
            self.store,
            self.committed_store,
            self.bulk_guard,
            &self.context,
            &pages,
        )?;
        self.summary.merge_from(summary);
        if let Some(sink) = self.output_sink {
            replay_committed_gemini_outputs(
                self.store,
                self.context.machine_id,
                &mut pages,
                sink,
                &mut self.failed_output_sources,
            );
        }
        self.sources.clear();
        self.bytes = 0;
        self.estimated_mutations = 0;
        if self.work_limit == CaptureWorkLimit::OneSafeGroup {
            self.stopped = true;
        }
        Ok(())
    }

    pub(super) fn finish(&mut self) -> Result<ProviderImportSummary> {
        if !self.stopped {
            self.flush()?;
        }
        Ok(std::mem::take(&mut self.summary))
    }
}

fn publish_gemini_group(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &GeminiPublicationContext<'_>,
    pages: &[GeminiPendingPage],
) -> Result<ProviderImportSummary> {
    let source_paths = pages
        .iter()
        .map(|pending| pending.source.path.clone())
        .collect::<BTreeSet<_>>();
    for path in &source_paths {
        revalidate_gemini_source(pages, path)?;
    }

    let mut transitions = Vec::with_capacity(source_paths.len());
    for path in &source_paths {
        let path_identity = provider_path_identity(path)?;
        let stream = provider_source_cursor_stream_for_path(
            CaptureProvider::Gemini,
            GEMINI_CLI_SOURCE_FORMAT,
            &path_identity,
        );
        let stored = store.get_sync_cursor(None, context.machine_id, &stream)?;
        let checkpoint = &pages
            .iter()
            .rev()
            .find(|pending| &pending.source.path == path)
            .ok_or(CaptureError::SystemInvariant(
                "Gemini publication path has no pending source",
            ))?
            .next_checkpoint;
        transitions.push(NativePathCursorTransition::new(
            stored.as_ref().map(|cursor| cursor.cursor.clone()),
            provider_sync_cursor(
                context.machine_id,
                stream,
                encode_gemini_cursor(checkpoint)?,
                context.imported_at,
            ),
        ));
    }
    let publication_id = gemini_publication_id(pages, &transitions);
    let retained_bytes = pages.iter().fold(0_usize, |total, pending| {
        total.saturating_add(pending.page.conservative_serialized_bytes)
    });
    let accounting =
        NativePathGroupAccounting::new(pages.len(), source_paths.len(), retained_bytes)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    if matches!(
        group.classify_cursor_set(&publication_id, &transitions)?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }

    let mut summary = ProviderImportSummary::default();
    let mut resolved = BTreeMap::new();
    for path in &source_paths {
        let pending = pages
            .iter()
            .rev()
            .find(|pending| &pending.source.path == path)
            .ok_or(CaptureError::SystemInvariant(
                "Gemini publication path has no pending source",
            ))?;
        let session_fact = pending.next_checkpoint.session.as_ref();
        if session_fact.is_none() {
            // Rejection-only sources commit their path-scoped cursor without
            // inventing a canonical Core capture source.
            continue;
        }
        let raw_source_path = path.display().to_string();
        let source_root = context.source_root.display().to_string();
        let locator_identity = provider_path_identity(path)?;
        let proposed_source_identity = provider_source_identity(
            CaptureProvider::Gemini,
            GEMINI_CLI_SOURCE_FORMAT,
            Some(&source_root),
            Some(&raw_source_path),
            session_fact.map(|session| session.native_session_id.as_str()),
            &Value::Null,
        )
        .ok_or(CaptureError::SystemInvariant(
            "Gemini NativePath source has no canonical identity",
        ))?;
        let stream = provider_source_cursor_stream_for_path(
            CaptureProvider::Gemini,
            GEMINI_CLI_SOURCE_FORMAT,
            &locator_identity,
        );
        let revision = gemini_source_revision(&pending.source.observation);
        let resolution =
            group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
                provider: CaptureProvider::Gemini,
                source_format: GEMINI_CLI_SOURCE_FORMAT.to_owned(),
                machine_id: context.machine_id.to_owned(),
                locator_identity,
                cursor_stream: stream,
                proposed_source_identity,
                raw_source_path: Some(raw_source_path.clone()),
                source_revision: revision.clone(),
                observed_at_ms: context.imported_at.timestamp_millis(),
            })?;
        let source_id = match session_fact {
            Some(session) => committed_store
                .capture_source_by_canonical_identity_session(
                    CaptureProvider::Gemini,
                    GEMINI_CLI_SOURCE_FORMAT,
                    context.machine_id,
                    &resolution.canonical_source_identity,
                    &session.native_session_id,
                )?
                .map(|source| source.id)
                .unwrap_or_else(|| {
                    provider_scoped_source_uuid(
                        CaptureProvider::Gemini,
                        &session.native_session_id,
                        GEMINI_CLI_SOURCE_FORMAT,
                        Some(&raw_source_path),
                    )
                }),
            None => stable_capture_uuid(
                &format!(
                    "gemini-nativepath-source:{}:{}",
                    resolution.canonical_source_identity, raw_source_path
                ),
                "source",
            ),
        };
        group.upsert_capture_source(&gemini_capture_source(
            context,
            session_fact,
            source_id,
            &raw_source_path,
            &source_root,
            &resolution.canonical_source_identity,
            &revision,
        ))?;
        group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;

        let session = session_fact
            .map(|fact| {
                gemini_session(
                    committed_store,
                    context,
                    fact,
                    source_id,
                    &resolution.canonical_source_identity,
                )
            })
            .transpose()?;
        if let Some(session) = &session {
            let existed = committed_store.get_session(session.id).is_ok();
            if let Some(parent_id) = session.parent_session_id {
                if committed_store.get_session(parent_id).is_err() {
                    group.upsert_session(&gemini_parent_placeholder(
                        context,
                        source_id,
                        parent_id,
                        session_fact
                            .and_then(|fact| fact.parent_native_session_id.as_deref())
                            .unwrap_or("unknown-parent"),
                    ))?;
                }
            }
            group.upsert_session(session)?;
            if existed {
                summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
                summary.skipped = summary.skipped.saturating_add(1);
            } else {
                summary.imported_sessions = summary.imported_sessions.saturating_add(1);
                summary.imported = summary.imported.saturating_add(1);
            }
            if let Some(parent_id) = session.parent_session_id {
                let edge = gemini_relationship_edge(context, source_id, session, parent_id);
                let existed = committed_store.session_edge_exists(edge.id)?;
                group.upsert_projection_neutral_session_edge(&canonical_actor(session), &edge)?;
                if !existed {
                    summary.imported_edges = summary.imported_edges.saturating_add(1);
                    summary.imported = summary.imported.saturating_add(1);
                }
            }
        }
        resolved.insert(path.clone(), ResolvedGeminiSource { source_id, session });
    }

    for pending in pages {
        for rejection in &pending.page.rejections {
            summary.record_failure(ProviderImportFailure {
                line: usize::try_from(rejection.raw_ordinal)
                    .unwrap_or(usize::MAX)
                    .saturating_add(1),
                error: rejection.reason.clone(),
            });
        }
        if pending.page.events.is_empty() {
            continue;
        }
        let resolved = resolved
            .get(&pending.source.path)
            .ok_or(CaptureError::SystemInvariant(
                "Gemini publication lost its resolved source",
            ))?;
        for event in &pending.page.events {
            let session = resolved
                .session
                .as_ref()
                .ok_or(CaptureError::SystemInvariant(
                    "Gemini retained event has no canonical session",
                ))?;
            publish_gemini_event(
                &mut group,
                committed_store,
                context,
                resolved.source_id,
                session,
                event,
                &mut summary,
            )?;
        }
    }

    for path in &source_paths {
        revalidate_gemini_source(pages, path)?;
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

fn gemini_capture_source(
    context: &GeminiPublicationContext<'_>,
    session: Option<&GeminiSession>,
    source_id: Uuid,
    raw_source_path: &str,
    source_root: &str,
    source_identity: &str,
    source_revision: &str,
) -> CaptureSource {
    CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Gemini,
            machine_id: context.machine_id.to_owned(),
            process_id: None,
            cwd: session.and_then(|session| session.cwd.clone()),
            raw_source_path: Some(raw_source_path.to_owned()),
            source_format: Some(GEMINI_CLI_SOURCE_FORMAT.to_owned()),
            source_root: Some(source_root.to_owned()),
            source_identity: Some(source_identity.to_owned()),
            external_session_id: session.map(|session| session.native_session_id.clone()),
        },
        started_at: session
            .and_then(|session| session.started_at)
            .unwrap_or(context.imported_at),
        ended_at: None,
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": session.map(|session| &session.native_session_id),
                "source_format": GEMINI_CLI_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "source_identity": source_identity,
                "source_revision": source_revision,
                "source_identity_key": session.map(|session| {
                    provider_scoped_source_identity_key(
                        CaptureProvider::Gemini,
                        &session.native_session_id,
                        GEMINI_CLI_SOURCE_FORMAT,
                        Some(raw_source_path),
                    )
                }),
            }),
        ),
    }
}

fn gemini_session(
    committed_store: &Store,
    context: &GeminiPublicationContext<'_>,
    fact: &GeminiSession,
    source_id: Uuid,
    source_identity: &str,
) -> Result<Session> {
    let id = provider_import_session_uuid(
        committed_store,
        CaptureProvider::Gemini,
        &fact.native_session_id,
        source_id,
        Some(source_identity),
    )?;
    let parent_session_id = fact
        .parent_native_session_id
        .as_deref()
        .map(|parent| {
            provider_import_session_uuid(
                committed_store,
                CaptureProvider::Gemini,
                parent,
                source_id,
                Some(source_identity),
            )
        })
        .transpose()?;
    Ok(Session {
        id,
        history_record_id: context.history_record_id,
        parent_session_id,
        root_session_id: parent_session_id,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Gemini,
        external_session_id: Some(fact.native_session_id.clone()),
        external_agent_id: None,
        agent_type: fact.agent_type,
        role_hint: Some(
            if fact.parent_native_session_id.is_some() || fact.agent_type == AgentType::Subagent {
                "subagent"
            } else {
                "primary"
            }
            .to_owned(),
        ),
        is_primary: fact.parent_native_session_id.is_none()
            && fact.agent_type != AgentType::Subagent,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: fact.started_at.unwrap_or(context.imported_at),
        ended_at: None,
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": fact.native_session_id,
                "parent_provider_session_id": fact.parent_native_session_id,
                "native_kind": fact.native_kind,
                "source_format": GEMINI_CLI_SOURCE_FORMAT,
            }),
        ),
    })
}

fn gemini_parent_placeholder(
    context: &GeminiPublicationContext<'_>,
    source_id: Uuid,
    id: Uuid,
    external_session_id: &str,
) -> Session {
    Session {
        id,
        history_record_id: context.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Gemini,
        external_session_id: Some(external_session_id.to_owned()),
        external_agent_id: None,
        agent_type: AgentType::Unknown,
        role_hint: Some("relationship_placeholder".to_owned()),
        is_primary: false,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: context.imported_at,
        ended_at: None,
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Partial,
            json!({
                "provider_session_id": external_session_id,
                "source_format": GEMINI_CLI_SOURCE_FORMAT,
                "relationship_placeholder": true,
            }),
        ),
    }
}

fn gemini_relationship_edge(
    context: &GeminiPublicationContext<'_>,
    source_id: Uuid,
    session: &Session,
    parent_id: Uuid,
) -> SessionEdge {
    SessionEdge {
        id: stable_capture_uuid(
            &format!(
                "gemini-nativepath:{}:parent_child",
                session.external_session_id.as_deref().unwrap_or_default()
            ),
            "session-edge",
        ),
        from_session_id: session.id,
        to_session_id: parent_id,
        edge_type: SessionEdgeType::ParentChild,
        confidence: Confidence::Explicit,
        source_id: Some(source_id),
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": session.external_session_id,
                "source_format": GEMINI_CLI_SOURCE_FORMAT,
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

#[allow(clippy::too_many_arguments)]
fn publish_gemini_event(
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    committed_store: &Store,
    context: &GeminiPublicationContext<'_>,
    source_id: Uuid,
    session: &Session,
    event: &GeminiRetainedEvent,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let stable_provider_event_index = gemini_event_index(event);
    let released_provider_event_index = released_gemini_event_index(event)?;
    let event_hash = hex_digest(event.body_sha256);
    let released_event_hash = hex_digest(event.released_body_sha256);
    let GeminiEventPublicationIdentity {
        identity,
        provider_event_index,
        released_provider_event_index,
        exact_released_hash,
        preserves_released_position,
    } = gemini_event_publication_identity(
        committed_store,
        source_id,
        session,
        event,
        stable_provider_event_index,
        released_provider_event_index,
        &event_hash,
        &released_event_hash,
    )?;
    let dedupe_key =
        Store::provider_event_dedupe_key_with_payload_hash(&identity.dedupe_key, &event_hash)
            .unwrap_or(identity.dedupe_key);
    let occurred_at = event.occurred_at.unwrap_or(session.started_at);
    let normalized = Event {
        id: identity.id,
        seq: identity.seq,
        history_record_id: context.history_record_id,
        session_id: Some(session.id),
        run_id: None,
        event_type: event.event_type,
        role: Some(event.role),
        occurred_at,
        capture_source_id: Some(source_id),
        payload: json!({
            "provider": CaptureProvider::Gemini.as_str(),
            "provider_session_id": session.external_session_id,
            "provider_event_index": provider_event_index,
            "provider_event_hash": event_hash,
            "native_identity": event.identity,
            "released_native_identity": event.released_identity,
            "body": event.body,
            "preview": event.preview,
            "searchable_text": event.searchable_text,
            "artifacts": [],
        }),
        payload_blob_id: None,
        dedupe_key: Some(dedupe_key),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": session.external_session_id,
                "provider_event_index": provider_event_index,
                "provider_event_hash": event_hash,
                "provider_event_hash_authority": "normalized_payload_fallback",
                "source_format": GEMINI_CLI_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "stable_provider_event_index": stable_provider_event_index,
                "released_provider_event_index": released_provider_event_index,
                "cursor": match &event.identity {
                    GeminiEventIdentity::NativeRecordId(identity) => identity,
                },
                "fixture_line": event.native_order.raw_ordinal.saturating_add(1),
                "source_record_ordinal": event.native_order.raw_ordinal,
                "source_record_subrecord_index": event.native_order.sub_ordinal,
                "native_identity": event.identity,
                "released_native_identity": event.released_identity,
            }),
        ),
    };
    if group.reconcile_provider_event_migrating_exact_legacy_provider_hash(
        &normalized,
        &exact_released_hash,
    )? {
        summary.imported_events = summary.imported_events.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    } else {
        summary.skipped_events = summary.skipped_events.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    }
    summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);

    for (touch_ordinal, path) in event.safe_file_touches.iter().enumerate() {
        let provider_touch_index = if preserves_released_position {
            event
                .native_order
                .raw_ordinal
                .checked_mul(u64::from(u16::MAX) + 1)
                .and_then(|base| base.checked_add(touch_ordinal as u64))
                .ok_or(CaptureError::SystemInvariant(
                    "Gemini released file-touch identity overflowed",
                ))?
        } else {
            touch_ordinal as u64
        };
        let id = provider_file_touch_import_id(
            committed_store,
            CaptureProvider::Gemini,
            session.external_session_id.as_deref().unwrap_or_default(),
            source_id,
            Some(provider_event_index),
            provider_touch_index,
            session.id
                == crate::provider::importer::provider_session_uuid(
                    CaptureProvider::Gemini,
                    session.external_session_id.as_deref().unwrap_or_default(),
                ),
        )?;
        group.upsert_file_touched(&FileTouched {
            id,
            history_record_id: context.history_record_id,
            run_id: None,
            event_id: Some(normalized.id),
            vcs_workspace_id: None,
            path: path.clone(),
            change_kind: Some(FileChangeKind::Unknown),
            old_path: None,
            line_count_delta: None,
            confidence: Confidence::Explicit,
            timestamps: timestamps(occurred_at),
            source_id: Some(source_id),
            sync: provider_sync_metadata(
                Fidelity::Imported,
                json!({
                    "provider": CaptureProvider::Gemini.as_str(),
                    "provider_session_id": session.external_session_id,
                    "provider_event_index": provider_event_index,
                    "released_provider_event_index":
                        released_provider_event_index,
                    "source_format": GEMINI_CLI_SOURCE_FORMAT,
                }),
            ),
        })?;
    }
    Ok(())
}

fn gemini_event_index(event: &GeminiRetainedEvent) -> u64 {
    let GeminiEventIdentity::NativeRecordId(identity) = &event.identity;
    let mut digest = Sha256::new();
    digest.update(GEMINI_EVENT_INDEX_DOMAIN);
    digest.update((identity.len() as u64).to_be_bytes());
    digest.update(identity.as_bytes());
    let digest = digest.finalize();
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(bytes) | (1_u64 << 63)
}

pub(super) fn released_gemini_event_index(event: &GeminiRetainedEvent) -> Result<u64> {
    if event.native_order.sub_ordinal == 0 {
        return Ok(event.native_order.raw_ordinal);
    }
    event
        .native_order
        .raw_ordinal
        .checked_mul(u64::from(u16::MAX) + 1)
        .and_then(|index| index.checked_add(u64::from(event.native_order.sub_ordinal)))
        .map(|index| index | (1_u64 << 63))
        .ok_or(CaptureError::SystemInvariant(
            "Gemini released provider event identity index overflowed",
        ))
}

#[allow(clippy::too_many_arguments)]
fn gemini_event_publication_identity(
    committed_store: &Store,
    source_id: Uuid,
    session: &Session,
    event: &GeminiRetainedEvent,
    stable_provider_event_index: u64,
    released_provider_event_index: u64,
    event_hash: &str,
    released_event_hash: &str,
) -> Result<GeminiEventPublicationIdentity> {
    let released_candidate = provider_source_event_import_identity(
        source_id,
        released_provider_event_index,
        released_event_hash,
    );
    let released_event = match committed_store.get_event(released_candidate.id) {
        Ok(existing) => Some(existing),
        Err(StoreError::NotFound(_)) => None,
        Err(error) => return Err(CaptureError::Store(error)),
    };
    if let Some(existing) = released_event {
        if exact_released_gemini_event(
            &existing,
            source_id,
            session,
            event,
            released_provider_event_index,
        ) {
            let existing_hash = existing
                .sync
                .metadata
                .get("provider_event_hash")
                .and_then(Value::as_str)
                .filter(|hash| !hash.is_empty())
                .unwrap_or(released_event_hash);
            let authority = existing
                .sync
                .metadata
                .get("provider_event_hash_authority")
                .and_then(Value::as_str);
            let exact_released_hash_matches = authority == Some("normalized_payload_fallback")
                || existing_hash == released_event_hash;
            if let Some(dedupe_key) = existing.dedupe_key {
                let exact_hash_matches_dedupe =
                    Store::provider_event_dedupe_key_with_payload_hash(&dedupe_key, existing_hash)
                        .as_deref()
                        == Some(dedupe_key.as_str());
                if exact_released_hash_matches && exact_hash_matches_dedupe {
                    return Ok(GeminiEventPublicationIdentity {
                        identity: ProviderEventImportIdentity {
                            id: existing.id,
                            seq: existing.seq,
                            dedupe_key,
                            run_source_id: existing.capture_source_id,
                        },
                        provider_event_index: released_provider_event_index,
                        released_provider_event_index,
                        exact_released_hash: existing_hash.to_owned(),
                        preserves_released_position: true,
                    });
                }
            }
        }
    }

    let identity = provider_event_import_identity_with_exact_legacy_source(
        committed_store,
        CaptureProvider::Gemini,
        session.external_session_id.as_deref().unwrap_or_default(),
        source_id,
        stable_provider_event_index,
        stable_provider_event_index,
        event_hash,
        None,
        Some(released_provider_event_index),
        session.id
            == crate::provider::importer::provider_session_uuid(
                CaptureProvider::Gemini,
                session.external_session_id.as_deref().unwrap_or_default(),
            ),
    )?;
    Ok(GeminiEventPublicationIdentity {
        identity,
        provider_event_index: stable_provider_event_index,
        released_provider_event_index,
        exact_released_hash: released_event_hash.to_owned(),
        preserves_released_position: false,
    })
}

fn exact_released_gemini_event(
    existing: &Event,
    source_id: Uuid,
    session: &Session,
    incoming: &GeminiRetainedEvent,
    released_provider_event_index: u64,
) -> bool {
    let metadata = &existing.sync.metadata;
    let stored_released_identity = metadata
        .get("released_native_identity")
        .or_else(|| metadata.get("native_identity"));
    let authority = metadata
        .get("provider_event_hash_authority")
        .and_then(Value::as_str);
    existing.session_id == Some(session.id)
        && existing.capture_source_id == Some(source_id)
        && metadata.get("provider_session_id").and_then(Value::as_str)
            == session.external_session_id.as_deref()
        && metadata.get("provider_event_index").and_then(Value::as_u64)
            == Some(released_provider_event_index)
        && metadata.get("source_format").and_then(Value::as_str) == Some(GEMINI_CLI_SOURCE_FORMAT)
        // v5/p3 emitted the positional identity as a string. The first
        // NativePath migration records it in that released form too, while
        // some handoffs stored the enum-shaped native identity. Both encode
        // the same exact source record and are deliberately accepted here.
        && stored_released_identity.is_some_and(|identity| {
            identity
                .get("NativeRecordId")
                .and_then(Value::as_str)
                == Some(incoming.released_identity.as_str())
                || identity.as_str() == Some(incoming.released_identity.as_str())
        })
        && matches!(
            authority,
            Some("provider_supplied" | "normalized_payload_fallback")
        )
}
