use super::*;

pub(super) struct PendingPage {
    pub(super) path: PathBuf,
    pub(super) page: Page,
}

pub(super) struct GroupAccumulator<'a> {
    pub(super) store: &'a mut Store,
    pub(super) committed_store: &'a Store,
    pub(super) bulk_guard: &'a EventSearchBulkGuard,
    pub(super) context: PublicationContext<'a>,
    pub(super) work_limit: CaptureWorkLimit,
    pub(super) pages: Vec<PendingPage>,
    pub(super) bytes: usize,
    pub(super) estimated_mutations: usize,
    pub(super) sources: BTreeSet<PathBuf>,
    pub(super) summary: ProviderImportSummary,
    pub(super) published_groups: usize,
    pub(super) stopped: bool,
}

impl<'a> GroupAccumulator<'a> {
    pub(super) fn new(
        store: &'a mut Store,
        committed_store: &'a Store,
        bulk_guard: &'a EventSearchBulkGuard,
        context: PublicationContext<'a>,
        work_limit: CaptureWorkLimit,
    ) -> Self {
        Self {
            store,
            committed_store,
            bulk_guard,
            context,
            work_limit,
            pages: Vec::new(),
            bytes: 0,
            estimated_mutations: 0,
            sources: BTreeSet::new(),
            summary: ProviderImportSummary::default(),
            published_groups: 0,
            stopped: false,
        }
    }

    pub(super) fn store(&self) -> &Store {
        self.store
    }

    pub(super) fn stopped(&self) -> bool {
        self.stopped
    }

    pub(super) fn record_unchanged(&mut self, outcome: &ScanOutcome) {
        self.summary.skipped_sessions = self.summary.skipped_sessions.saturating_add(1);
        self.summary.skipped_events = self
            .summary
            .skipped_events
            .saturating_add(usize::try_from(outcome.accepted_events).unwrap_or(usize::MAX));
        self.summary.failed = self
            .summary
            .failed
            .saturating_add(usize::try_from(outcome.rejected_records).unwrap_or(usize::MAX));
        self.summary.skipped = self
            .summary
            .skipped
            .saturating_add(1)
            .saturating_add(usize::try_from(outcome.accepted_events).unwrap_or(usize::MAX));
        self.summary.accepted_content_records = self
            .summary
            .accepted_content_records
            .saturating_add(usize::try_from(outcome.accepted_events).unwrap_or(usize::MAX));
    }

    pub(super) fn push(&mut self, pending: PendingPage) -> Result<()> {
        if pending.page.logical_units == 0
            || pending.page.logical_units > NATIVE_INGESTION_PAGE_MAX_UNITS
            || pending.page.conservative_serialized_bytes > NATIVE_INGESTION_PAGE_MAX_BYTES
        {
            return Err(CaptureError::SystemInvariant(
                "OpenClaw page has invalid NativePath acquisition accounting",
            ));
        }
        let next_sources = self
            .sources
            .len()
            .saturating_add(usize::from(!self.sources.contains(&pending.path)));
        let next_bytes = self
            .bytes
            .saturating_add(pending.page.conservative_serialized_bytes);
        let page_mutations = pending
            .page
            .events
            .len()
            .saturating_add(pending.page.touches.len())
            .saturating_add(8);
        let next_mutations = self.estimated_mutations.saturating_add(page_mutations);
        if !self.pages.is_empty()
            && (self.pages.len() >= GROUP_MAX_PAGES
                || next_sources > GROUP_MAX_SOURCES
                || next_bytes > GROUP_MAX_BYTES
                || next_mutations > GROUP_MAX_ESTIMATED_MUTATIONS)
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
        self.sources.insert(pending.path.clone());
        self.pages.push(pending);
        Ok(())
    }

    pub(super) fn flush(&mut self) -> Result<()> {
        if self.pages.is_empty() {
            return Ok(());
        }
        let pages = std::mem::take(&mut self.pages);
        let summary = publish_group(
            self.store,
            self.committed_store,
            self.bulk_guard,
            &self.context,
            &pages,
        )?;
        self.summary.merge_from(summary);
        self.bytes = 0;
        self.estimated_mutations = 0;
        self.sources.clear();
        self.published_groups = self.published_groups.saturating_add(1);
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

pub(super) struct ResolvedSource {
    pub(super) source_id: Uuid,
    pub(super) session: Session,
}

pub(super) fn publish_group(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &PublicationContext<'_>,
    pages: &[PendingPage],
) -> Result<ProviderImportSummary> {
    if pages.is_empty() {
        return Ok(ProviderImportSummary::default());
    }
    let source_paths = pages
        .iter()
        .map(|pending| pending.path.clone())
        .collect::<BTreeSet<_>>();
    for path in &source_paths {
        let expected = &pages
            .iter()
            .rev()
            .find(|pending| &pending.path == path)
            .ok_or(CaptureError::SystemInvariant(
                "OpenClaw publication lost its source page",
            ))?
            .page
            .next_checkpoint
            .source_observation;
        let observed = OpenClawSessionObservation::read(path)?;
        if !expected.matches_live(&observed)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
    }

    let mut transitions = Vec::with_capacity(source_paths.len());
    for path in &source_paths {
        let locator = provider_path_identity(path)?;
        let stream = provider_source_cursor_stream_for_path(
            CaptureProvider::OpenClaw,
            OPENCLAW_SOURCE_FORMAT,
            &locator,
        );
        let stored = store.get_sync_cursor(None, context.machine_id, &stream)?;
        let final_checkpoint = &pages
            .iter()
            .rev()
            .find(|pending| &pending.path == path)
            .ok_or(CaptureError::SystemInvariant(
                "OpenClaw publication lost its final checkpoint",
            ))?
            .page
            .next_checkpoint;
        transitions.push(NativePathCursorTransition::new(
            stored.as_ref().map(|cursor| cursor.cursor.clone()),
            provider_sync_cursor(
                context.machine_id,
                stream,
                encode_cursor(final_checkpoint)?,
                context.imported_at,
            ),
        ));
    }
    let publication_id = publication_id(context, pages, &transitions)?;
    let retained_bytes = pages.iter().fold(0_usize, |total, pending| {
        total.saturating_add(pending.page.conservative_serialized_bytes)
    });
    let replacements = source_paths
        .iter()
        .filter_map(|path| {
            let pending = pages
                .iter()
                .find(|pending| &pending.path == path)
                .expect("source path came from pending pages");
            let starts_generation = pending.page.expected_checkpoint.complete_prefix_end == 0
                && pending.page.expected_checkpoint.next_raw_ordinal == 0;
            (starts_generation
                && matches!(
                    pending.page.source_change,
                    SourceChange::Rewrite | SourceChange::Truncation | SourceChange::Replacement
                ))
            .then(|| {
                current_route_for_path(
                    committed_store,
                    context.machine_id,
                    context.source_root,
                    path,
                )
            })
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let accounting =
        NativePathGroupAccounting::new(pages.len(), source_paths.len(), retained_bytes)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    match group.classify_cursor_set(&publication_id, &transitions)? {
        NativePathCursorSetClassification::AllNextSameGroup { .. } => {
            group.commit()?;
            let mut summary = ProviderImportSummary::default();
            summary.skipped_events = pages.iter().map(|pending| pending.page.events.len()).sum();
            summary.skipped = summary.skipped_events;
            for rejection in pages
                .iter()
                .flat_map(|pending| pending.page.rejections.iter())
            {
                summary.record_failure(ProviderImportFailure {
                    line: line_number(rejection.raw_ordinal),
                    error: rejection.reason.clone(),
                });
            }
            summary.set_work_result(ProviderImportWorkResult::NoOp);
            return Ok(summary);
        }
        NativePathCursorSetClassification::AllExpected => {}
    }
    for route in &replacements {
        let disposition = group.retire_provider_source_route(&route_retirement(
            context.imported_at,
            route,
            ProviderSourceRouteRetirementReason::Replaced,
        ))?;
        if disposition != ProviderSourceRouteRetirementDisposition::Retired {
            return Err(CaptureError::SystemInvariant(
                "OpenClaw replacement route was already retired before publication",
            ));
        }
    }

    let mut summary = ProviderImportSummary::default();
    let mut resolved = BTreeMap::<PathBuf, ResolvedSource>::new();
    for path in &source_paths {
        let pending = pages
            .iter()
            .rev()
            .find(|pending| &pending.path == path)
            .ok_or(CaptureError::SystemInvariant(
                "OpenClaw publication lost its source facts",
            ))?;
        let live = OpenClawSessionObservation::read(path)?;
        let source_revision = source_revision(&live, context.inventory_observation_token);
        let raw_source_path = path.display().to_string();
        let source_root = context.source_root.display().to_string();
        let path_identity = provider_path_identity(path)?;
        let root_source_identity = provider_source_identity(
            CaptureProvider::OpenClaw,
            OPENCLAW_SOURCE_FORMAT,
            Some(&source_root),
            Some(&raw_source_path),
            None,
            &Value::Null,
        )
        .ok_or(CaptureError::SystemInvariant(
            "OpenClaw source has no canonical identity",
        ))?;
        let generation = pending.page.next_checkpoint.generation;
        let proposed_source_identity =
            generation_source_identity(&root_source_identity, generation);
        let locator_identity = source_locator_identity(&path_identity, generation);
        let stream = provider_source_cursor_stream_for_path(
            CaptureProvider::OpenClaw,
            OPENCLAW_SOURCE_FORMAT,
            &path_identity,
        );
        let resolution =
            group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
                provider: CaptureProvider::OpenClaw,
                source_format: OPENCLAW_SOURCE_FORMAT.to_owned(),
                machine_id: context.machine_id.to_owned(),
                locator_identity,
                cursor_stream: stream,
                proposed_source_identity,
                raw_source_path: Some(raw_source_path.clone()),
                source_revision: source_revision.clone(),
                observed_at_ms: context.imported_at.timestamp_millis(),
            })?;
        let source_id = committed_store
            .capture_source_by_canonical_identity_session(
                CaptureProvider::OpenClaw,
                OPENCLAW_SOURCE_FORMAT,
                context.machine_id,
                &resolution.canonical_source_identity,
                &pending.page.session.cursor.provider_session_id,
            )?
            .map(|source| source.id)
            .unwrap_or_else(|| {
                generation_source_id(
                    generation,
                    &resolution.canonical_source_identity,
                    &pending.page.session.cursor.provider_session_id,
                    &raw_source_path,
                )
            });
        let source = capture_source(
            context,
            &pending.page.session,
            generation,
            source_id,
            &raw_source_path,
            &source_root,
            &resolution.canonical_source_identity,
            &source_revision,
        );
        group.upsert_capture_source(&source)?;
        group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;

        let session = canonical_session(
            committed_store,
            context,
            &pending.page.session,
            source_id,
            &resolution.canonical_source_identity,
            replacements
                .iter()
                .find(|route| route.raw_source_path.as_path() == path.as_path())
                .map(|route| route.capture_source_id),
        )?;
        if let Some(parent_id) = session.parent_session_id {
            if committed_store.get_session(parent_id).is_err() {
                group.upsert_session(&relationship_placeholder(
                    context,
                    source_id,
                    parent_id,
                    pending
                        .page
                        .session
                        .cursor
                        .parent_provider_session_id
                        .as_deref()
                        .unwrap_or("unknown-parent"),
                    &resolution.canonical_source_identity,
                ))?;
            }
        }
        let existed = committed_store.get_session(session.id).is_ok();
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
                context,
                source_id,
                &session,
                parent_id,
                &resolution.canonical_source_identity,
            );
            let edge_existed = committed_store.session_edge_exists(edge.id)?;
            group.upsert_projection_neutral_session_edge(&actor(&session), &edge)?;
            if edge_existed {
                summary.skipped_edges = summary.skipped_edges.saturating_add(1);
            } else {
                summary.imported_edges = summary.imported_edges.saturating_add(1);
                summary.imported = summary.imported.saturating_add(1);
            }
        }
        resolved.insert(path.clone(), ResolvedSource { source_id, session });
    }

    let mut event_ids = BTreeMap::<(PathBuf, u64), Uuid>::new();
    for pending in pages {
        let source = resolved
            .get(&pending.path)
            .ok_or(CaptureError::SystemInvariant(
                "OpenClaw publication lost its resolved source",
            ))?;
        for event in &pending.page.events {
            let event_id = publish_event(
                &mut group,
                committed_store,
                context,
                source.source_id,
                &source.session,
                event,
                &mut summary,
            )?;
            event_ids.insert((pending.path.clone(), event.raw_ordinal), event_id);
        }
        let mut touch_subrecords = BTreeMap::<u64, u64>::new();
        for touch in &pending.page.touches {
            let subrecord = touch_subrecords.entry(touch.raw_ordinal).or_default();
            publish_touch(
                &mut group,
                committed_store,
                context,
                source.source_id,
                &source.session,
                touch,
                *subrecord,
                touch
                    .event_ordinal
                    .and_then(|ordinal| event_ids.get(&(pending.path.clone(), ordinal)).copied()),
            )?;
            *subrecord = subrecord.saturating_add(1);
        }
        for rejection in &pending.page.rejections {
            summary.record_failure(ProviderImportFailure {
                line: line_number(rejection.raw_ordinal),
                error: rejection.reason.clone(),
            });
        }
    }

    for path in &source_paths {
        let expected = &pages
            .iter()
            .rev()
            .find(|pending| &pending.path == path)
            .ok_or(CaptureError::SystemInvariant(
                "OpenClaw publication lost its revalidation source",
            ))?
            .page
            .next_checkpoint
            .source_observation;
        let observed = OpenClawSessionObservation::read(path)?;
        if !expected.matches_live(&observed)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

// These route and revision values remain explicit because each is recorded
// independently in the provider source descriptor or synchronization metadata.
#[allow(clippy::too_many_arguments)]
pub(super) fn capture_source(
    context: &PublicationContext<'_>,
    session: &SessionFact,
    generation: u64,
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
            provider: CaptureProvider::OpenClaw,
            machine_id: context.machine_id.to_owned(),
            process_id: None,
            cwd: session.cursor.cwd.clone(),
            raw_source_path: Some(raw_source_path.to_owned()),
            source_format: Some(OPENCLAW_SOURCE_FORMAT.to_owned()),
            source_root: Some(source_root.to_owned()),
            source_identity: Some(source_identity.to_owned()),
            external_session_id: Some(session.cursor.provider_session_id.clone()),
        },
        started_at: session.cursor.started_at,
        ended_at: None,
        sync: provider_sync_metadata(
            Fidelity::Partial,
            json!({
                "provider_session_id": session.cursor.provider_session_id,
                "source_format": OPENCLAW_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "source_identity": source_identity,
                "source_root": source_root,
                "source_revision": source_revision,
                "source_identity_key": provider_scoped_source_identity_key(
                    CaptureProvider::OpenClaw,
                    &session.cursor.provider_session_id,
                    OPENCLAW_SOURCE_FORMAT,
                    Some(raw_source_path),
                ),
                "source_metadata": {
                    "adapter": OPENCLAW_SOURCE_FORMAT,
                    "index": provider_capped_json(&session.index, PROVIDER_MAX_PREVIEW_CHARS),
                    "header": provider_capped_json(&session.header, PROVIDER_MAX_PREVIEW_CHARS),
                    "support_level": "beta",
                },
                "nativepath_publication": "openclaw-v1",
                "nativepath_generation": generation,
            }),
        ),
    }
}

pub(super) fn canonical_session(
    committed_store: &Store,
    context: &PublicationContext<'_>,
    fact: &SessionFact,
    source_id: Uuid,
    source_identity: &str,
    prior_source_id: Option<Uuid>,
) -> Result<Session> {
    let session_id = generation_session_id(
        committed_store,
        prior_source_id,
        &fact.cursor.provider_session_id,
        source_id,
        source_identity,
    )?;
    let parent_session_id = fact
        .cursor
        .parent_provider_session_id
        .as_deref()
        .map(|parent| {
            generation_session_id(
                committed_store,
                prior_source_id,
                parent,
                source_id,
                source_identity,
            )
        })
        .transpose()?;
    let root_session_id = fact
        .cursor
        .root_provider_session_id
        .as_deref()
        .map(|root| {
            generation_session_id(
                committed_store,
                prior_source_id,
                root,
                source_id,
                source_identity,
            )
        })
        .transpose()?
        .or(parent_session_id);
    Ok(Session {
        id: session_id,
        history_record_id: context.history_record_id,
        parent_session_id,
        root_session_id,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::OpenClaw,
        external_session_id: Some(fact.cursor.provider_session_id.clone()),
        external_agent_id: fact.cursor.agent_id.clone(),
        agent_type: AgentType::Primary,
        role_hint: Some("personal-agent".to_owned()),
        is_primary: true,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: fact.cursor.started_at,
        ended_at: None,
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Partial,
            json!({
                "provider_session_id": fact.cursor.provider_session_id,
                "parent_provider_session_id": fact.cursor.parent_provider_session_id,
                "root_provider_session_id": fact.cursor.root_provider_session_id,
                "source_format": OPENCLAW_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "metadata": {
                    "source_format": OPENCLAW_SOURCE_FORMAT,
                    "agent_id": fact.cursor.agent_id,
                    "session_index": provider_capped_json(
                        &fact.index,
                        PROVIDER_MAX_PREVIEW_CHARS,
                    ),
                    "fidelity_gap": "OpenClaw session JSONL is current native storage, but upstream keeps a storage-neutral accessor for future schema changes",
                    "nativepath_publication": "openclaw-v1",
                },
            }),
        ),
    })
}

pub(super) fn generation_session_id(
    store: &Store,
    prior_source_id: Option<Uuid>,
    provider_session_id: &str,
    source_id: Uuid,
    source_identity: &str,
) -> Result<Uuid> {
    if let Some(existing) = prior_source_id
        .map(|prior_source_id| {
            store.session_by_capture_source_and_external_session(
                prior_source_id,
                CaptureProvider::OpenClaw,
                provider_session_id,
            )
        })
        .transpose()?
        .flatten()
    {
        return Ok(existing.id);
    }
    provider_import_session_uuid(
        store,
        CaptureProvider::OpenClaw,
        provider_session_id,
        source_id,
        Some(source_identity),
    )
}

pub(super) fn relationship_placeholder(
    context: &PublicationContext<'_>,
    source_id: Uuid,
    id: Uuid,
    external_session_id: &str,
    source_identity: &str,
) -> Session {
    Session {
        id,
        history_record_id: context.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::OpenClaw,
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
                "source_format": OPENCLAW_SOURCE_FORMAT,
                "source_identity": source_identity,
                "relationship_placeholder": true,
            }),
        ),
    }
}

pub(super) fn relationship_edge(
    context: &PublicationContext<'_>,
    source_id: Uuid,
    session: &Session,
    parent_id: Uuid,
    source_identity: &str,
) -> SessionEdge {
    SessionEdge {
        id: stable_capture_uuid(
            &format!(
                "provider-source-root:{source_identity}:session:{}:parent_child",
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
                "source_format": OPENCLAW_SOURCE_FORMAT,
                "imported_at": context.imported_at,
            }),
        ),
    }
}

pub(super) fn actor(session: &Session) -> CanonicalActor {
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
pub(super) fn publish_event(
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    committed_store: &Store,
    context: &PublicationContext<'_>,
    source_id: Uuid,
    session: &Session,
    event: &CoreEvent,
    summary: &mut ProviderImportSummary,
) -> Result<Uuid> {
    let provider_session_id = session.external_session_id.as_deref().unwrap_or_default();
    let identity = provider_event_import_identity_with_exact_legacy_source(
        committed_store,
        CaptureProvider::OpenClaw,
        provider_session_id,
        source_id,
        event.provider_event_index,
        event.provider_event_sequence_index,
        &event.provider_event_hash,
        None,
        Some(event.raw_ordinal),
        session.id
            == crate::provider::importer::provider_session_uuid(
                CaptureProvider::OpenClaw,
                provider_session_id,
            ),
    )?;
    let mut provider_metadata = event.metadata.clone();
    let verified_locators = provider_metadata
        .as_object_mut()
        .and_then(|metadata| metadata.remove(VERIFIED_CONTENT_LOCATORS_METADATA_KEY));
    let dedupe_key = Store::provider_event_dedupe_key_with_payload_hash(
        &identity.dedupe_key,
        &event.provider_event_hash,
    )
    .unwrap_or(identity.dedupe_key);
    let mut sync_metadata = json!({
        "provider_session_id": provider_session_id,
        "provider_event_index": event.provider_event_index,
        "provider_event_hash": event.provider_event_hash,
        "provider_event_hash_authority": "provider_supplied",
        "cursor": event.cursor,
        "source_format": OPENCLAW_SOURCE_FORMAT,
        "source_trust": "provider_native",
        "fixture_line": event.raw_ordinal.saturating_add(1),
        "imported_at": context.imported_at,
        "source_record_ordinal": event.raw_ordinal,
        "source_record_subrecord_index": 0,
        "metadata": provider_metadata,
    });
    if let (Some(metadata), Some(locators)) = (sync_metadata.as_object_mut(), verified_locators) {
        metadata.insert(VERIFIED_CONTENT_LOCATORS_METADATA_KEY.to_owned(), locators);
    }
    let mut payload = event.payload.clone();
    {
        let payload = payload
            .as_object_mut()
            .ok_or(CaptureError::SystemInvariant(
                "OpenClaw normalized event payload is not an object",
            ))?;
        payload.insert(
            "provider".to_owned(),
            Value::String(CaptureProvider::OpenClaw.as_str().to_owned()),
        );
        payload.insert(
            "provider_session_id".to_owned(),
            Value::String(provider_session_id.to_owned()),
        );
        payload.insert(
            "provider_event_index".to_owned(),
            Value::from(event.provider_event_index),
        );
        payload.insert(
            "provider_event_hash".to_owned(),
            Value::String(event.provider_event_hash.clone()),
        );
        payload.insert("cursor".to_owned(), Value::String(event.cursor.clone()));
        payload.insert("artifacts".to_owned(), Value::Array(Vec::new()));
    }
    let normalized = Event {
        id: identity.id,
        seq: identity.seq,
        history_record_id: context.history_record_id,
        session_id: Some(session.id),
        run_id: None,
        event_type: event.event_type,
        role: event.role,
        occurred_at: event.occurred_at,
        capture_source_id: Some(source_id),
        payload,
        payload_blob_id: None,
        dedupe_key: Some(dedupe_key),
        sync: provider_sync_metadata(Fidelity::Partial, sync_metadata),
    };
    if group.reconcile_provider_event(&normalized, ProviderEventHashAuthority::ProviderSupplied)? {
        summary.imported_events = summary.imported_events.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    } else {
        summary.skipped_events = summary.skipped_events.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    }
    summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
    Ok(normalized.id)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_touch(
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    committed_store: &Store,
    context: &PublicationContext<'_>,
    source_id: Uuid,
    session: &Session,
    touch: &CoreTouch,
    subrecord: u64,
    event_id: Option<Uuid>,
) -> Result<()> {
    let provider_session_id = session.external_session_id.as_deref().unwrap_or_default();
    let touch_index = touch
        .raw_ordinal
        .checked_mul(u64::from(u16::MAX) + 1)
        .and_then(|base| base.checked_add(subrecord))
        .ok_or(CaptureError::SystemInvariant(
            "OpenClaw file-touch identity overflowed",
        ))?;
    let id = provider_file_touch_import_id(
        committed_store,
        CaptureProvider::OpenClaw,
        provider_session_id,
        source_id,
        Some(touch.raw_ordinal),
        touch_index,
        session.id
            == crate::provider::importer::provider_session_uuid(
                CaptureProvider::OpenClaw,
                provider_session_id,
            ),
    )?;
    group.upsert_file_touched(&FileTouched {
        id,
        history_record_id: context.history_record_id,
        run_id: None,
        event_id,
        vcs_workspace_id: None,
        path: touch.path.clone(),
        change_kind: touch.change_kind,
        old_path: touch.old_path.clone(),
        line_count_delta: None,
        confidence: Confidence::Explicit,
        timestamps: timestamps(touch.occurred_at),
        source_id: Some(source_id),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider": CaptureProvider::OpenClaw.as_str(),
                "provider_session_id": provider_session_id,
                "provider_touch_index": touch_index,
                "provider_event_index": touch.raw_ordinal,
                "source_format": OPENCLAW_SOURCE_FORMAT,
                "session_id": session.id,
            }),
        ),
    })?;
    Ok(())
}

pub(super) fn native_source_id(source_identity: &str, provider_session_id: &str) -> Uuid {
    stable_capture_uuid(
        &format!(
            "native-path-provider-source-v1:{}:{}:{}:{}",
            source_identity.len(),
            source_identity,
            provider_session_id.len(),
            provider_session_id,
        ),
        "source",
    )
}

pub(super) fn generation_source_id(
    generation: u64,
    source_identity: &str,
    provider_session_id: &str,
    raw_source_path: &str,
) -> Uuid {
    if generation == 0 {
        provider_scoped_source_uuid(
            CaptureProvider::OpenClaw,
            provider_session_id,
            OPENCLAW_SOURCE_FORMAT,
            Some(raw_source_path),
        )
    } else {
        native_source_id(source_identity, provider_session_id)
    }
}

pub(super) fn generation_source_identity(root_source_identity: &str, generation: u64) -> String {
    if generation == 0 {
        root_source_identity.to_owned()
    } else {
        format!("{root_source_identity}:openclaw-generation:{generation}")
    }
}
