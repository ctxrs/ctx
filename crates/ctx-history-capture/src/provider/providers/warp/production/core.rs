use super::output::{encode_warp_cursor, provider_cursor};
use super::*;

pub(super) struct WarpCoreStoreSink<'a> {
    pub(super) store: &'a mut Store,
    pub(super) committed: &'a Store,
    pub(super) bulk_guard: &'a EventSearchBulkGuard,
    pub(super) context: &'a WarpPublicationContext,
    pub(super) inputs: &'a WarpNativePreparationInputs,
    pub(super) work_limit: CaptureWorkLimit,
    pub(super) pages_committed: usize,
    pub(super) stopped: bool,
    pub(super) summary: ProviderImportSummary,
}

impl WarpNativeSink for WarpCoreStoreSink<'_> {
    fn push_page(&mut self, page: WarpNativePage) -> Result<()> {
        if self.stopped {
            return Ok(());
        }
        let state = self
            .inputs
            .persisted_state_at(page.next_safe_frontier.clone())?;
        let summary = publish_core_page(
            self.store,
            self.committed,
            self.bulk_guard,
            self.context,
            &page,
            &state,
        )?;
        self.summary.merge_from(summary);
        self.pages_committed = self.pages_committed.saturating_add(1);
        if self.work_limit == CaptureWorkLimit::OneSafeGroup {
            self.stopped = true;
        }
        Ok(())
    }

    fn push_pro_output_page(
        &mut self,
        page: WarpNativeProOutputPage,
    ) -> WarpNativeProOutputPageReceipt {
        page.receipt()
    }
}

impl WarpCoreStoreSink<'_> {
    pub(super) fn publish_terminal(&mut self, authority: &WarpNativeSourceAuthority) -> Result<()> {
        let summary = publish_terminal_observation(
            self.store,
            self.committed,
            self.bulk_guard,
            self.context,
            &authority.persisted_state,
        )?;
        self.summary.merge_from(summary);
        Ok(())
    }
}

fn publish_core_page(
    store: &mut Store,
    committed: &Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &WarpPublicationContext,
    page: &WarpNativePage,
    state: &WarpNativePersistedState,
) -> Result<ProviderImportSummary> {
    let transition = cursor_transition(
        store,
        context,
        encode_warp_cursor(
            state,
            context.replacement_prior_source_identity.as_deref(),
            context.released_migration.as_ref(),
        )?,
    )?;
    let publication_id = core_publication_id(
        context,
        Some(&page.identity.0),
        std::slice::from_ref(&transition),
    );
    let accounting = NativePathGroupAccounting::new(1, 1, page.estimated_bytes)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        return Ok(skipped_page_summary(page));
    }

    let (source_id, canonical_source_identity) = reconcile_source(&mut group, committed, context)?;
    let mut summary = ProviderImportSummary::default();
    let mut sessions = BTreeMap::new();
    for fact in &page.sessions {
        let session = canonical_session(
            committed,
            context,
            fact,
            source_id,
            &canonical_source_identity,
        )?;
        ensure_relationship_placeholders(
            &mut group,
            committed,
            context,
            fact,
            source_id,
            &canonical_source_identity,
            &session,
        )?;
        let existed = committed.get_session(session.id).is_ok();
        group.upsert_session(&session)?;
        sessions.insert(fact.conversation_id.clone(), session);
        if existed {
            summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
        } else {
            summary.imported_sessions = summary.imported_sessions.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        }
    }
    for edge in &page.hierarchy_edges {
        publish_edge(
            &mut group,
            committed,
            context,
            edge,
            source_id,
            &canonical_source_identity,
            &sessions,
            &mut summary,
        )?;
    }
    for event in &page.events {
        publish_event(
            &mut group,
            committed,
            context,
            event,
            source_id,
            &canonical_source_identity,
            &sessions,
            &mut summary,
        )?;
    }
    record_rejections(&mut summary, &page.rejections);
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

fn publish_terminal_observation(
    store: &mut Store,
    committed: &Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &WarpPublicationContext,
    state: &WarpNativePersistedState,
) -> Result<ProviderImportSummary> {
    let encoded = encode_warp_cursor(state, None, context.released_migration.as_ref())?;
    let stored = store.get_sync_cursor(None, &context.machine_id, &context.cursor_stream)?;
    if stored
        .as_ref()
        .and_then(|cursor| provider_cursor(&cursor.cursor))
        .is_some_and(|cursor| cursor == encoded)
        && persisted_source_revision_matches(committed, context)?
    {
        return Ok(noop_summary());
    }
    let transition = NativePathCursorTransition::new(
        stored.as_ref().map(|cursor| cursor.cursor.clone()),
        provider_sync_cursor(context, encoded),
    );
    let publication_id = core_publication_id(context, None, std::slice::from_ref(&transition));
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store
        .begin_native_path_publication_group(admission, NativePathGroupAccounting::new(0, 1, 0)?)?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        return Ok(noop_summary());
    }
    reconcile_source(&mut group, committed, context)?;
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    let mut summary = ProviderImportSummary::default();
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

fn persisted_source_revision_matches(
    committed: &Store,
    context: &WarpPublicationContext,
) -> Result<bool> {
    match committed.get_capture_source(warp_source_id(&context.proposed_source_identity)) {
        Ok(source) => Ok(source.descriptor.source_identity.as_deref()
            == Some(context.proposed_source_identity.as_str())
            && source
                .sync
                .metadata
                .get("source_revision")
                .and_then(Value::as_str)
                == Some(context.source_revision.as_str())),
        Err(StoreError::NotFound(_)) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn reconcile_source(
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    committed: &Store,
    context: &WarpPublicationContext,
) -> Result<(Uuid, String)> {
    let resolution =
        group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::Warp,
            source_format: WARP_SQLITE_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity: context.locator_identity.clone(),
            cursor_stream: context.cursor_stream.clone(),
            proposed_source_identity: context.proposed_source_identity.clone(),
            raw_source_path: Some(context.raw_source_path.clone()),
            source_revision: context.source_revision.clone(),
            observed_at_ms: context.imported_at.timestamp_millis(),
        })?;
    let source_id = warp_source_id(&resolution.canonical_source_identity);
    let started_at = match committed.get_capture_source(source_id) {
        Ok(source) => source.started_at,
        Err(StoreError::NotFound(_)) => context.imported_at,
        Err(error) => return Err(error.into()),
    };
    let source = CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Warp,
            machine_id: context.machine_id.clone(),
            process_id: None,
            cwd: None,
            raw_source_path: Some(context.raw_source_path.clone()),
            source_format: Some(WARP_SQLITE_SOURCE_FORMAT.to_owned()),
            source_root: Some(context.source_root.clone()),
            source_identity: Some(resolution.canonical_source_identity.clone()),
            external_session_id: None,
        },
        started_at,
        ended_at: None,
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "source_format": WARP_SQLITE_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "source_identity": resolution.canonical_source_identity,
                "source_revision": context.source_revision,
                "warp_native_locator_identity": context.locator_identity,
                "warp_native_cursor_stream": context.cursor_stream,
            }),
        ),
    };
    group.upsert_capture_source(&source)?;
    group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;
    Ok((source_id, resolution.canonical_source_identity))
}

pub(super) fn warp_source_id(canonical_source_identity: &str) -> Uuid {
    stable_capture_uuid(
        &format!("warp-nativepath-source:{canonical_source_identity}"),
        "source",
    )
}

fn canonical_session(
    committed: &Store,
    context: &WarpPublicationContext,
    fact: &WarpNativeSession,
    source_id: Uuid,
    source_identity: &str,
) -> Result<Session> {
    let id = warp_session_id(
        committed,
        context,
        &fact.conversation_id,
        source_id,
        source_identity,
    )?;
    let parent_session_id = fact
        .parent_conversation_id
        .as_deref()
        .map(|parent| warp_session_id(committed, context, parent, source_id, source_identity))
        .transpose()?;
    let root_session_id = warp_session_id(
        committed,
        context,
        &fact.root_conversation_id,
        source_id,
        source_identity,
    )?;
    let observed_at = fact.modified_at.unwrap_or(context.imported_at);
    Ok(Session {
        id,
        history_record_id: context.history_record_id,
        parent_session_id,
        root_session_id: (root_session_id != id).then_some(root_session_id),
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Warp,
        external_session_id: Some(fact.conversation_id.clone()),
        external_agent_id: Some("warp-agent".to_owned()),
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
        started_at: observed_at,
        ended_at: fact.modified_at,
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": fact.conversation_id,
                "parent_provider_session_id": fact.parent_conversation_id,
                "root_provider_session_id": fact.root_conversation_id,
                "parent_present": fact.parent_present,
                "title": fact.title,
                "metadata": fact.metadata,
                "source_format": WARP_SQLITE_SOURCE_FORMAT,
                "source_trust": "provider_native",
            }),
        ),
    })
}

fn warp_session_id(
    committed: &Store,
    context: &WarpPublicationContext,
    external_session_id: &str,
    source_id: Uuid,
    source_identity: &str,
) -> Result<Uuid> {
    if let Some(released_source_id) = context.released_source_ids.get(external_session_id) {
        if let Some(existing) = committed.session_by_capture_source_and_external_session(
            *released_source_id,
            CaptureProvider::Warp,
            external_session_id,
        )? {
            return Ok(existing.id);
        }
    }
    if let Some(prior_source_id) = context.replacement_prior_source_id {
        if prior_source_id != source_id {
            if let Some(existing) = committed.session_by_capture_source_and_external_session(
                prior_source_id,
                CaptureProvider::Warp,
                external_session_id,
            )? {
                return Ok(existing.id);
            }
        }
    }
    provider_import_session_uuid(
        committed,
        CaptureProvider::Warp,
        external_session_id,
        source_id,
        Some(source_identity),
    )
}

fn ensure_relationship_placeholders(
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    committed: &Store,
    context: &WarpPublicationContext,
    fact: &WarpNativeSession,
    source_id: Uuid,
    source_identity: &str,
    session: &Session,
) -> Result<()> {
    let mut facts = BTreeSet::new();
    if let Some(parent) = fact.parent_conversation_id.as_deref() {
        facts.insert(parent);
    }
    if fact.root_conversation_id != fact.conversation_id {
        facts.insert(fact.root_conversation_id.as_str());
    }
    for external_id in facts {
        let id = warp_session_id(committed, context, external_id, source_id, source_identity)?;
        if id != session.id && committed.get_session(id).is_err() {
            group.insert_session_if_absent(&relationship_placeholder(
                context,
                source_id,
                id,
                external_id,
            ))?;
        }
    }
    Ok(())
}

fn relationship_placeholder(
    context: &WarpPublicationContext,
    source_id: Uuid,
    id: Uuid,
    external_id: &str,
) -> Session {
    Session {
        id,
        history_record_id: context.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Warp,
        external_session_id: Some(external_id.to_owned()),
        external_agent_id: Some("warp-agent".to_owned()),
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
                "provider_session_id": external_id,
                "source_format": WARP_SQLITE_SOURCE_FORMAT,
                "relationship_placeholder": true,
            }),
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn publish_edge(
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    committed: &Store,
    context: &WarpPublicationContext,
    fact: &WarpNativeHierarchyEdge,
    source_id: Uuid,
    source_identity: &str,
    sessions: &BTreeMap<String, Session>,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let child = if let Some(session) = sessions.get(&fact.child_conversation_id) {
        session.clone()
    } else {
        let id = warp_session_id(
            committed,
            context,
            &fact.child_conversation_id,
            source_id,
            source_identity,
        )?;
        committed.get_session(id)?
    };
    let parent_id = warp_session_id(
        committed,
        context,
        &fact.parent_conversation_id,
        source_id,
        source_identity,
    )?;
    if committed.get_session(parent_id).is_err() {
        group.insert_session_if_absent(&relationship_placeholder(
            context,
            source_id,
            parent_id,
            &fact.parent_conversation_id,
        ))?;
    }
    let edge = SessionEdge {
        id: stable_capture_uuid(
            &format!(
                "warp-nativepath:{source_identity}:{}:parent_child",
                fact.child_conversation_id
            ),
            "session-edge",
        ),
        from_session_id: child.id,
        to_session_id: parent_id,
        edge_type: SessionEdgeType::ParentChild,
        confidence: if fact.parent_present {
            Confidence::Explicit
        } else {
            Confidence::High
        },
        source_id: Some(source_id),
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": fact.child_conversation_id,
                "parent_provider_session_id": fact.parent_conversation_id,
                "parent_present": fact.parent_present,
                "source_format": WARP_SQLITE_SOURCE_FORMAT,
            }),
        ),
    };
    let existed = committed.session_edge_exists(edge.id)?;
    group.upsert_projection_neutral_session_edge(&canonical_actor(&child), &edge)?;
    if existed {
        summary.skipped_edges = summary.skipped_edges.saturating_add(1);
    } else {
        summary.imported_edges = summary.imported_edges.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn publish_event(
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    committed: &Store,
    context: &WarpPublicationContext,
    fact: &WarpNativeEvent,
    source_id: Uuid,
    source_identity: &str,
    sessions: &BTreeMap<String, Session>,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let session = if let Some(session) = sessions.get(&fact.identity.conversation_id) {
        session.clone()
    } else {
        let id = warp_session_id(
            committed,
            context,
            &fact.identity.conversation_id,
            source_id,
            source_identity,
        )?;
        committed.get_session(id)?
    };
    let provider_session_id = session.external_session_id.as_deref().unwrap_or_default();
    let sequence_index = fact.native_order.provider_event_index;
    let identity_index = warp_event_identity_index(fact);
    let native_record_id = match &fact.identity.message {
        WarpNativeMessageIdentity::ProviderId(id) => id.clone(),
        WarpNativeMessageIdentity::MessageOrdinal(ordinal) => {
            format!("{}:{ordinal}", fact.identity.task_id)
        }
    };
    let released_identity = exact_released_warp_event_identity(
        committed,
        context,
        provider_session_id,
        fact,
        &native_record_id,
    )?;
    let migrates_released_hash = released_identity
        .as_ref()
        .is_some_and(|identity| identity.migrates_provider_hash);
    let identity = match released_identity {
        Some(identity) => identity.identity,
        None => provider_event_import_identity_with_exact_legacy_source(
            committed,
            CaptureProvider::Warp,
            provider_session_id,
            source_id,
            identity_index,
            sequence_index,
            &fact.content_hash,
            None,
            Some(sequence_index),
            session.id
                == crate::provider::importer::provider_session_uuid(
                    CaptureProvider::Warp,
                    provider_session_id,
                ),
        )?,
    };
    let dedupe_key = Store::provider_event_dedupe_key_with_payload_hash(
        &identity.dedupe_key,
        &fact.content_hash,
    )
    .unwrap_or(identity.dedupe_key);
    let mut sync_details = json!({
        "provider_session_id": provider_session_id,
        "provider_event_index": sequence_index,
        "provider_event_identity_index": identity_index,
        "provider_event_hash": fact.content_hash,
        "provider_event_hash_authority": "normalized_payload_fallback",
        "source_format": WARP_SQLITE_SOURCE_FORMAT,
        "source_trust": "provider_native",
        "native_record_id": native_record_id,
        "task_rowid": fact.native_order.task_rowid,
        "task_key": fact.native_order.task_key,
        "message_ordinal": fact.native_order.message_ordinal,
        "source_record_ordinal": fact.native_order.task_rowid,
        "source_record_subrecord_index": fact.native_order.message_ordinal,
        "legacy_provider_event_index": fact.native_order.legacy_provider_event_index,
        "metadata": {"event_path": context.raw_source_path},
        "native_identity": {
            "conversation_id": fact.identity.conversation_id,
            "task_id": fact.identity.task_id,
        },
    });
    attach_complete_content_locator(fact, &native_record_id, &mut sync_details)?;
    let occurred_at = fact.occurred_at.unwrap_or(session.started_at);
    let retained_text = provider_policy_event_text(fact.event_type, &fact.body, &Value::Null);
    let mut text_retention = retained_text.retention.as_json();
    if fact.complete_content_ref.is_some() {
        text_retention["truncated"] = Value::Bool(true);
    }
    let normalized = Event {
        id: identity.id,
        seq: identity.seq,
        history_record_id: context.history_record_id,
        session_id: Some(session.id),
        run_id: None,
        event_type: fact.event_type,
        role: fact.role,
        occurred_at,
        capture_source_id: Some(source_id),
        payload: json!({
            "provider": CaptureProvider::Warp.as_str(),
            "provider_session_id": provider_session_id,
            "provider_event_index": sequence_index,
            "provider_event_identity_index": identity_index,
            "provider_event_hash": fact.content_hash,
            "native_record_id": native_record_id,
            "kind": fact.kind,
            "request_id": fact.request_id,
            "result_outcome": fact.result_outcome.map(|outcome| format!("{outcome:?}").to_lowercase()),
            "call_id": fact.call_id,
            "text": retained_text.text,
            "text_retention": text_retention,
            "body": fact.body,
            "preview": fact.preview,
            "searchable_text": fact.body,
            "artifacts": [],
        }),
        payload_blob_id: None,
        dedupe_key: Some(dedupe_key),
        sync: provider_sync_metadata(Fidelity::Imported, sync_details),
    };
    let inserted = if migrates_released_hash {
        group.reconcile_provider_event_migrating_exact_legacy_provider_hash(
            &normalized,
            &native_record_id,
        )?
    } else {
        group.reconcile_provider_event(
            &normalized,
            ProviderEventHashAuthority::NormalizedPayloadFallback,
        )?
    };
    if inserted {
        summary.imported_events = summary.imported_events.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    } else {
        summary.skipped_events = summary.skipped_events.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    }
    summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
    Ok(())
}

struct ReleasedWarpEventIdentity {
    identity: ProviderEventImportIdentity,
    migrates_provider_hash: bool,
}

fn exact_released_warp_event_identity(
    committed: &Store,
    context: &WarpPublicationContext,
    provider_session_id: &str,
    fact: &WarpNativeEvent,
    native_record_id: &str,
) -> Result<Option<ReleasedWarpEventIdentity>> {
    let Some(legacy_provider_event_index) = fact.native_order.legacy_provider_event_index else {
        return Ok(None);
    };
    let Some(legacy_source_id) = context
        .released_source_ids
        .get(provider_session_id)
        .copied()
    else {
        return Ok(None);
    };
    let legacy_identity = provider_source_event_import_identity(
        legacy_source_id,
        legacy_provider_event_index,
        native_record_id,
    );
    let event = match committed.get_event(legacy_identity.id) {
        Ok(event) => event,
        Err(StoreError::NotFound(_)) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if event
        .sync
        .metadata
        .pointer("/metadata/event_path")
        .and_then(Value::as_str)
        != Some(context.raw_source_path.as_str())
    {
        return Ok(None);
    }
    let released_provider_hash = event
        .sync
        .metadata
        .get("provider_event_hash")
        .and_then(Value::as_str)
        == Some(native_record_id)
        && event
            .sync
            .metadata
            .get("provider_event_hash_authority")
            .and_then(Value::as_str)
            == Some("provider_supplied");
    let migrated_native_identity = event
        .sync
        .metadata
        .get("native_record_id")
        .and_then(Value::as_str)
        == Some(native_record_id);
    if released_provider_hash {
        if event.capture_source_id != Some(legacy_source_id)
            || event.dedupe_key.as_deref() != Some(legacy_identity.dedupe_key.as_str())
        {
            return Ok(None);
        }
    } else if !migrated_native_identity {
        return Ok(None);
    }
    Ok(event
        .dedupe_key
        .map(|dedupe_key| ReleasedWarpEventIdentity {
            migrates_provider_hash: released_provider_hash,
            identity: ProviderEventImportIdentity {
                id: event.id,
                seq: event.seq,
                dedupe_key,
                run_source_id: event.capture_source_id,
            },
        }))
}

fn warp_event_identity_index(event: &WarpNativeEvent) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let message_identity = match &event.identity.message {
        WarpNativeMessageIdentity::ProviderId(id) => id.clone(),
        WarpNativeMessageIdentity::MessageOrdinal(ordinal) => {
            format!("{}:{ordinal}", event.identity.task_id)
        }
    };
    let mut hash = OFFSET;
    for component in [
        b"ctx-warp-message-v1".as_slice(),
        event.identity.conversation_id.as_bytes(),
        event.identity.task_id.as_bytes(),
        message_identity.as_bytes(),
    ] {
        for byte in component {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(PRIME);
        }
        hash ^= 0xff;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

fn attach_complete_content_locator(
    event: &WarpNativeEvent,
    native_record_id: &str,
    metadata: &mut Value,
) -> Result<()> {
    let Some(content_ref) = event.complete_content_ref.clone() else {
        return Ok(());
    };
    if event.event_type != EventType::Message {
        return Ok(());
    }
    let Some(profile) = verified_content_profile(
        CaptureProvider::Warp,
        WARP_SQLITE_SOURCE_FORMAT,
        CompleteContentSourceFamily::Sqlite,
        VerifiedContentRole::MessageBody,
    ) else {
        return Err(CaptureError::SystemInvariant(
            "Warp complete-content profile is not registered",
        ));
    };
    let mut locator_value = Vec::with_capacity(12);
    locator_value.extend_from_slice(&event.native_order.task_rowid.to_be_bytes());
    locator_value.extend_from_slice(&event.native_order.message_ordinal.to_be_bytes());
    let locator = VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        profile,
        content_ref,
        CompleteContentSourceFamily::Sqlite,
        WARP_CONTENT_LOCATOR_KIND,
        &locator_value,
        native_record_id,
        event.source_record_digest.clone(),
    )
    .ok_or(CaptureError::SystemInvariant(
        "Warp complete-content locator exceeded its typed bounds",
    ))?;
    attach_verified_content_locator(metadata, locator).ok_or(CaptureError::SystemInvariant(
        "Warp complete-content metadata exceeded its typed bounds",
    ))?;
    if metadata
        .get(VERIFIED_CONTENT_LOCATORS_METADATA_KEY)
        .is_none()
    {
        return Err(CaptureError::SystemInvariant(
            "Warp complete-content metadata attachment was lost",
        ));
    }
    Ok(())
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

fn record_rejections(summary: &mut ProviderImportSummary, rejections: &[WarpNativeRejection]) {
    for (index, rejection) in rejections.iter().enumerate() {
        summary.record_failure(ProviderImportFailure {
            line: index.saturating_add(1),
            error: format!(
                "Warp {:?} record {} rejected: {}",
                rejection.kind, rejection.native_key, rejection.reason
            ),
        });
    }
}

fn skipped_page_summary(page: &WarpNativePage) -> ProviderImportSummary {
    let mut summary = ProviderImportSummary::default();
    summary.skipped_sessions = page.sessions.len();
    summary.skipped_events = page.events.len();
    summary.skipped_edges = page.hierarchy_edges.len();
    summary.skipped = summary
        .skipped_sessions
        .saturating_add(summary.skipped_events)
        .saturating_add(summary.skipped_edges);
    summary.set_work_result(ProviderImportWorkResult::NoOp);
    summary
}

pub(super) fn noop_summary() -> ProviderImportSummary {
    let mut summary = ProviderImportSummary::default();
    summary.set_work_result(ProviderImportWorkResult::NoOp);
    summary
}

pub(super) fn cursor_transition(
    store: &Store,
    context: &WarpPublicationContext,
    provider_cursor: String,
) -> Result<NativePathCursorTransition> {
    let stored = store.get_sync_cursor(None, &context.machine_id, &context.cursor_stream)?;
    Ok(NativePathCursorTransition::new(
        stored.as_ref().map(|cursor| cursor.cursor.clone()),
        provider_sync_cursor(context, provider_cursor),
    ))
}

pub(super) fn provider_sync_cursor(context: &WarpPublicationContext, cursor: String) -> SyncCursor {
    SyncCursor {
        id: stable_capture_uuid(
            &format!(
                "provider-cursor:{}:{}:{}",
                CaptureProvider::Warp.as_str(),
                context.machine_id,
                context.cursor_stream
            ),
            "provider-sync-cursor",
        ),
        team_id: None,
        device_id: context.machine_id.clone(),
        stream: context.cursor_stream.clone(),
        cursor,
        last_synced_at: Some(context.imported_at),
        timestamps: timestamps(context.imported_at),
    }
}

fn core_publication_id(
    context: &WarpPublicationContext,
    page_identity: Option<&[u8; 32]>,
    transitions: &[NativePathCursorTransition],
) -> String {
    let mut digest = Sha256::new();
    digest.update(WARP_PUBLICATION_DOMAIN);
    digest.update(context.locator_identity.as_bytes());
    digest.update((context.source_revision.len() as u64).to_be_bytes());
    digest.update(context.source_revision.as_bytes());
    if let Some(identity) = page_identity {
        digest.update([1]);
        digest.update(identity);
    } else {
        digest.update([0]);
    }
    for transition in transitions {
        digest.update(transition.key().stream().as_bytes());
        if let Some(expected) = transition.expected_cursor() {
            digest.update((expected.len() as u64).to_be_bytes());
            digest.update(expected.as_bytes());
        }
        digest.update((transition.next().cursor.len() as u64).to_be_bytes());
        digest.update(transition.next().cursor.as_bytes());
    }
    format!("warp-nativepath-v1:{:x}", digest.finalize())
}
