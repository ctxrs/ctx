use super::{lifecycle::*, records::*, *};

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_core_page(
    store: &Store,
    bulk_guard: &ctx_history_store::EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    authority: &SourceAuthority,
    expected_cursor: Option<&str>,
    page: &CorePage,
) -> Result<ProviderImportSummary> {
    let next_cursor = provider_sync_cursor(
        &context.machine_id,
        authority.cursor_stream.clone(),
        page.checkpoint.encode()?,
        context.imported_at,
    );
    let transition =
        NativePathCursorTransition::new(expected_cursor.map(str::to_owned), next_cursor);
    let publication_id = publication_id(authority, &transition);
    let accounting = NativePathGroupAccounting::new(1, 1, page.retained_bytes)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
        NativePathCursorSetClassification::AllNextSameGroup { .. } => {
            group.commit()?;
            let mut summary = ProviderImportSummary::default();
            summary.skipped_events = page
                .rows
                .iter()
                .filter_map(|(_, row)| match row {
                    PreparedRow::Accepted(row) => Some(lingma_event_count(row)),
                    PreparedRow::Skipped | PreparedRow::Rejected(_) => None,
                })
                .sum();
            summary.skipped = summary.skipped_events;
            summary.set_work_result(ProviderImportWorkResult::NoOp);
            return Ok(summary);
        }
        NativePathCursorSetClassification::AllExpected => {}
    }

    let proposed_source_identity = provider_source_identity(
        CaptureProvider::Lingma,
        LINGMA_SQLITE_SOURCE_FORMAT,
        Some(&authority.source_root),
        Some(&authority.raw_source_path),
        None,
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Lingma NativePath source has no canonical identity",
    ))?;
    let resolution =
        group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::Lingma,
            source_format: LINGMA_SQLITE_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity: authority.locator_identity.clone(),
            cursor_stream: authority.cursor_stream.clone(),
            proposed_source_identity,
            raw_source_path: Some(authority.raw_source_path.clone()),
            source_revision: authority.source_revision.clone(),
            observed_at_ms: context.imported_at.timestamp_millis(),
        })?;
    let source_id = native_source_id(&resolution.canonical_source_identity);
    let source = capture_source(
        store,
        source_id,
        context,
        authority,
        &resolution.canonical_source_identity,
    )?;
    group.upsert_capture_source(&source)?;
    group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;

    let mut summary = ProviderImportSummary::default();
    let sessions = prepared_sessions(
        store,
        context,
        options,
        source_id,
        &resolution.canonical_source_identity,
        page,
    )?;
    for (session, existed) in sessions.values() {
        group.upsert_session(session)?;
        if *existed {
            summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
        } else {
            summary.imported_sessions = summary.imported_sessions.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        }
    }

    for (candidate, prepared) in &page.rows {
        match prepared {
            PreparedRow::Accepted(row) => {
                let session = sessions
                    .get(&row.session_id)
                    .map(|(session, _)| session)
                    .ok_or(CaptureError::SystemInvariant(
                        "Lingma NativePath lost a prepared session",
                    ))?;
                publish_row_events(
                    &mut group,
                    store,
                    context,
                    options,
                    source_id,
                    session,
                    row,
                    &mut summary,
                )?;
            }
            PreparedRow::Skipped => {
                summary.skipped = summary.skipped.saturating_add(1);
            }
            PreparedRow::Rejected(reason) => {
                summary.record_failure(ProviderImportFailure {
                    line: usize::try_from(candidate.rowid).unwrap_or(usize::MAX),
                    error: reason.clone(),
                });
            }
        }
    }

    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

pub(super) fn capture_source(
    store: &Store,
    source_id: Uuid,
    context: &ProviderAdapterContext,
    authority: &SourceAuthority,
    canonical_source_identity: &str,
) -> Result<CaptureSource> {
    let existing = store.get_capture_source(source_id).ok();
    Ok(CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Lingma,
            machine_id: context.machine_id.clone(),
            process_id: None,
            cwd: None,
            raw_source_path: Some(authority.raw_source_path.clone()),
            source_format: Some(LINGMA_SQLITE_SOURCE_FORMAT.to_owned()),
            source_root: Some(authority.source_root.clone()),
            source_identity: Some(canonical_source_identity.to_owned()),
            external_session_id: None,
        },
        started_at: existing
            .as_ref()
            .map_or(context.imported_at, |source| source.started_at),
        ended_at: existing.as_ref().and_then(|source| source.ended_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": Value::Null,
                "source_format": LINGMA_SQLITE_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "source_identity": canonical_source_identity,
                "source_root": authority.source_root,
                "display_source_path": authority.display_source_path,
                "source_revision": authority.source_revision,
                "sqlite_user_version": authority.user_version,
                "schema_fingerprint": authority.schema_fingerprint,
                "source_table": "chat_record",
                "source_fidelity": "user prompts plus assistant summaries/errors",
                "assistant_content_caveat": "assistant events are summaries/errors; original assistant answers may be encrypted, transformed, or unavailable",
            }),
        ),
    })
}

pub(super) fn prepared_sessions(
    store: &Store,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    source_id: Uuid,
    canonical_source_identity: &str,
    page: &CorePage,
) -> Result<BTreeMap<String, (Session, bool)>> {
    let mut bounds = BTreeMap::<String, (DateTime<Utc>, DateTime<Utc>)>::new();
    for (_, prepared) in &page.rows {
        let PreparedRow::Accepted(row) = prepared else {
            continue;
        };
        let occurred_at = lingma_timestamp(row.gmt_create, context.imported_at);
        let ended_at = occurred_at
            .checked_add_signed(Duration::milliseconds(100))
            .unwrap_or(occurred_at);
        bounds
            .entry(row.session_id.clone())
            .and_modify(|(started, ended)| {
                *started = (*started).min(occurred_at);
                *ended = (*ended).max(ended_at);
            })
            .or_insert((occurred_at, ended_at));
    }

    bounds
        .into_iter()
        .map(|(provider_session_id, (page_started, page_ended))| {
            let id = provider_import_session_uuid(
                store,
                CaptureProvider::Lingma,
                &provider_session_id,
                source_id,
                Some(canonical_source_identity),
            )?;
            let existing = store.get_session(id).ok();
            let started_at = existing
                .as_ref()
                .map_or(page_started, |session| session.started_at.min(page_started));
            let ended_at = existing
                .as_ref()
                .and_then(|session| session.ended_at)
                .map_or(page_ended, |ended| ended.max(page_ended));
            let session = Session {
                id,
                history_record_id: options.history_record_id,
                parent_session_id: None,
                root_session_id: None,
                capture_source_id: Some(source_id),
                provider: CaptureProvider::Lingma,
                external_session_id: Some(provider_session_id.clone()),
                external_agent_id: None,
                agent_type: AgentType::Primary,
                role_hint: Some("primary".to_owned()),
                is_primary: true,
                status: SessionStatus::Imported,
                transcript_blob_id: None,
                started_at,
                ended_at: Some(ended_at),
                timestamps: timestamps(context.imported_at),
                sync: provider_sync_metadata(
                    Fidelity::Partial,
                    json!({
                        "provider_session_id": provider_session_id,
                        "parent_provider_session_id": Value::Null,
                        "root_provider_session_id": Value::Null,
                        "source_format": LINGMA_SQLITE_SOURCE_FORMAT,
                        "source_trust": "provider_native",
                        "imported_at": context.imported_at,
                        "metadata": {
                            "source_table": "chat_record",
                            "source_fidelity": "partial",
                            "session_metadata_fidelity": "row-local temporal bounds",
                            "assistant_content_caveat": "assistant events are summaries/errors, not guaranteed full assistant bodies",
                        },
                    }),
                ),
            };
            Ok((provider_session_id, (session, existing.is_some())))
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_row_events(
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    store: &Store,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    source_id: Uuid,
    session: &Session,
    row: &LingmaRow,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let base_index = event_base_index(row);
    let user = provider_event(
        row,
        EventDraft {
            provider_event_index: base_index,
            role: EventRole::User,
            event_type: EventType::Message,
            occurred_at: lingma_timestamp(row.gmt_create, context.imported_at),
            text: row.chat_prompt.clone(),
            body_kind: "chat_prompt",
            fidelity: Fidelity::Imported,
        },
        true,
    )?;
    publish_event(
        group, store, context, options, source_id, session, user, row.rowid, 0, summary,
    )?;

    if let Some((text, body_kind, event_type)) = assistant_text(row) {
        let occurred_at = lingma_timestamp(row.gmt_create, context.imported_at)
            .checked_add_signed(Duration::milliseconds(100))
            .unwrap_or_else(|| lingma_timestamp(row.gmt_create, context.imported_at));
        let assistant = provider_event(
            row,
            EventDraft {
                provider_event_index: base_index.saturating_add(1),
                role: EventRole::Assistant,
                event_type,
                occurred_at,
                text,
                body_kind,
                fidelity: Fidelity::SummaryOnly,
            },
            false,
        )?;
        publish_event(
            group, store, context, options, source_id, session, assistant, row.rowid, 1, summary,
        )?;
    }
    Ok(())
}

pub(super) struct EventDraft {
    pub(super) provider_event_index: u64,
    pub(super) role: EventRole,
    pub(super) event_type: EventType,
    pub(super) occurred_at: DateTime<Utc>,
    pub(super) text: String,
    pub(super) body_kind: &'static str,
    pub(super) fidelity: Fidelity,
}

pub(super) fn provider_event(
    row: &LingmaRow,
    draft: EventDraft,
    attach_complete_prompt: bool,
) -> Result<LingmaCoreEvent> {
    let role_name = draft.role.as_str();
    let released_provider_event_hash = released_lingma_event_hash(row, role_name);
    let body = json!({
        "rowid": row.rowid,
        "session_id": row.session_id,
        "request_id": row.request_id,
        "role": role_name,
        "body_kind": draft.body_kind,
        "gmt_create": row.gmt_create,
    });
    let retained_text = provider_policy_event_text(draft.event_type, &draft.text, &body);
    let result_evidence = provider_result_identifier_evidence(draft.event_type, &draft.text, &body);
    let result_outcome = provider_result_outcome_evidence(draft.event_type, &body);
    let payload = json!({
        "text": retained_text.text,
        "text_retention": retained_text.retention.as_json(),
        "source_format": LINGMA_SQLITE_SOURCE_FORMAT,
        "result_evidence": result_evidence,
        "result_outcome": result_outcome,
        "body": provider_capped_json(
            &provider_policy_body(draft.event_type, &body),
            PROVIDER_MAX_PREVIEW_CHARS,
        ),
    });
    let provider_event_hash = compute_payload_hash(&payload)?;
    let mut event = LingmaCoreEvent {
        provider_event_index: draft.provider_event_index,
        provider_event_hash,
        released_provider_event_hash,
        cursor: format!(
            "chat_record:{}:rowid:{}:{role_name}",
            row.session_id, row.rowid
        ),
        event_type: draft.event_type,
        role: Some(draft.role),
        occurred_at: draft.occurred_at,
        fidelity: draft.fidelity,
        idempotency_key: format!(
            "provider-event:{}:{}:{}",
            CaptureProvider::Lingma.as_str(),
            row.session_id,
            draft.provider_event_index
        ),
        payload,
        metadata: json!({
            "source": "lingma_chat_record",
            "source_format": LINGMA_SQLITE_SOURCE_FORMAT,
            "rowid": row.rowid,
            "session_id": row.session_id,
            "request_id": row.request_id,
            "body_kind": draft.body_kind,
            "gmt_create": row.gmt_create,
            "content_fidelity": if draft.fidelity == Fidelity::SummaryOnly {
                "summary_only"
            } else {
                "imported"
            },
            "assistant_content_caveat": if draft.role == EventRole::Assistant {
                Some("summary/error_result only; original assistant body may be encrypted or unavailable")
            } else {
                None
            },
        }),
    };
    if attach_complete_prompt {
        let locator = lingma_locator(row.rowid)?;
        let values = native_values(row);
        attach_lingma_complete_content_locator(&mut event, &locator, &values, &row.chat_prompt)?;
    }
    Ok(event)
}

pub(super) fn attach_lingma_complete_content_locator(
    event: &mut LingmaCoreEvent,
    locator: &NativeLocator,
    values: &[NativeSqliteValue],
    complete_text: &str,
) -> Result<()> {
    if event.event_type != EventType::Message
        || event
            .payload
            .pointer("/text_retention/truncated")
            .and_then(Value::as_bool)
            != Some(true)
    {
        return Ok(());
    }
    let content_ref = ContentRef::from_bytes(complete_text.as_bytes()).ok_or(
        CaptureError::SystemInvariant("SQLite content length exceeds ContentRef bounds"),
    )?;
    let profile = verified_content_profile(
        CaptureProvider::Lingma,
        LINGMA_SQLITE_SOURCE_FORMAT,
        CompleteContentSourceFamily::Sqlite,
        VerifiedContentRole::MessageBody,
    )
    .ok_or(CaptureError::SystemInvariant(
        "supported Lingma message route has no verified-content profile",
    ))?;
    let persisted = VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        profile,
        content_ref,
        CompleteContentSourceFamily::Sqlite,
        locator.kind(),
        locator.value(),
        event.provider_event_hash.clone(),
        lingma_logical_record_digest(values)?,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Lingma complete-content locator exceeds the bounded canonical schema",
    ))?;
    attach_verified_content_locator(&mut event.metadata, persisted).ok_or(
        CaptureError::SystemInvariant("Lingma verified-content locator collection is malformed"),
    )?;
    Ok(())
}

pub(in super::super) fn lingma_logical_record_digest(
    values: &[NativeSqliteValue],
) -> Result<CompleteContentBodyDigest> {
    let digest = lingma_logical_record_sha256(values);
    let encoded = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    CompleteContentBodyDigest::parse(encoded).ok_or(CaptureError::SystemInvariant(
        "Lingma logical-row digest is not canonical SHA-256",
    ))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_event(
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    store: &Store,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    source_id: Uuid,
    session: &Session,
    event: LingmaCoreEvent,
    raw_ordinal: i64,
    sub_ordinal: u32,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let provider_session_id = session.external_session_id.as_deref().unwrap_or_default();
    let provider_event_hash = event.provider_event_hash.as_str();
    let released_provider_event_hash = event.released_provider_event_hash.as_str();
    let legacy_raw_source_path = context
        .source_path
        .as_deref()
        .map(|path| path.display().to_string());
    let legacy_source_id = provider_scoped_source_uuid(
        CaptureProvider::Lingma,
        provider_session_id,
        LINGMA_SQLITE_SOURCE_FORMAT,
        legacy_raw_source_path.as_deref(),
    );
    let legacy_dedupe_key = Store::provider_source_event_dedupe_key(
        legacy_source_id,
        event.provider_event_index,
        released_provider_event_hash,
    );
    let released_event_exists = match store.event_id_by_dedupe_key(&legacy_dedupe_key) {
        Ok(_) => true,
        Err(ctx_history_store::StoreError::Sql(rusqlite::Error::QueryReturnedNoRows)) => false,
        Err(error) => return Err(error.into()),
    };
    let (identity_source_id, identity_hash, sequence_index) = if released_event_exists {
        (
            legacy_source_id,
            released_provider_event_hash,
            event.provider_event_index,
        )
    } else {
        (source_id, provider_event_hash, u64::from(sub_ordinal))
    };
    let identity = provider_event_import_identity_with_exact_legacy_source(
        store,
        CaptureProvider::Lingma,
        provider_session_id,
        identity_source_id,
        event.provider_event_index,
        sequence_index,
        identity_hash,
        None,
        u64::try_from(raw_ordinal).ok(),
        session.id == provider_session_uuid(CaptureProvider::Lingma, provider_session_id),
    )?;
    let dedupe_key = Store::provider_event_dedupe_key_with_payload_hash(
        &identity.dedupe_key,
        provider_event_hash,
    )
    .unwrap_or(identity.dedupe_key);
    let mut provider_metadata = event.metadata;
    let verified_locators = provider_metadata
        .as_object_mut()
        .and_then(|metadata| metadata.remove(VERIFIED_CONTENT_LOCATORS_METADATA_KEY));
    let mut sync_metadata = json!({
        "provider_session_id": provider_session_id,
        "provider_event_index": event.provider_event_index,
        "provider_event_hash": provider_event_hash,
        "provider_event_hash_authority": "normalized_payload_fallback",
        "cursor": event.cursor,
        "source_format": LINGMA_SQLITE_SOURCE_FORMAT,
        "source_trust": "provider_native",
        "fixture_line": raw_ordinal.saturating_add(1),
        "imported_at": context.imported_at,
        "source_record_ordinal": raw_ordinal,
        "source_record_subrecord_index": sub_ordinal,
        "metadata": provider_metadata,
    });
    if let (Some(metadata), Some(locators)) = (sync_metadata.as_object_mut(), verified_locators) {
        metadata.insert(VERIFIED_CONTENT_LOCATORS_METADATA_KEY.to_owned(), locators);
    }
    let normalized = Event {
        id: identity.id,
        seq: identity.seq,
        history_record_id: options.history_record_id,
        session_id: Some(session.id),
        run_id: None,
        event_type: event.event_type,
        role: event.role,
        occurred_at: event.occurred_at,
        capture_source_id: Some(source_id),
        payload: json!({
            "provider": CaptureProvider::Lingma.as_str(),
            "provider_session_id": provider_session_id,
            "provider_event_index": event.provider_event_index,
            "provider_event_hash": provider_event_hash,
            "cursor": event.cursor,
            "artifacts": [],
            "body": crate::provider::importer::compact_provider_result_payload(
                event.event_type,
                &event.payload,
            ),
        }),
        payload_blob_id: None,
        dedupe_key: Some(dedupe_key),
        sync: provider_sync_metadata(event.fidelity, sync_metadata),
    };
    if group.reconcile_provider_event_migrating_exact_legacy_provider_hash(
        &normalized,
        released_provider_event_hash,
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

pub(super) fn replay_outputs_or_mark_behind(
    store: &Store,
    machine_id: &str,
    authority: &SourceAuthority,
    sink: Option<&dyn ProOutputSink>,
) {
    let Some(sink) = sink else {
        return;
    };
    if let Err(error) = replay_outputs(store, machine_id, authority, sink) {
        sink.mark_behind(ProOutputSinkError::new(
            "lingma_nativepath_output_replay",
            error.to_string(),
        ));
    }
}

pub(super) fn replay_outputs(
    store: &Store,
    machine_id: &str,
    authority: &SourceAuthority,
    sink: &dyn ProOutputSink,
) -> Result<()> {
    committed_replay_authority(store, machine_id, authority)?;
    let source = OutputSourceIdentity {
        provider: CaptureProvider::Lingma.as_str().to_owned(),
        namespace_id: authority.source_root.clone(),
        source_id: authority.locator_identity.clone(),
    };
    let progress = match sink.observe_source(&source) {
        Ok(progress) => progress,
        Err(error) => {
            sink.mark_behind(error);
            return Ok(());
        }
    };
    if progress.as_ref().is_some_and(|progress| {
        progress.parser_revision == OUTPUT_PARSER_REVISION
            && progress.materializer_revision == sink.materializer_revision()
            && progress.observed_revision == authority.source_revision
            && progress.terminal
            && progress
                .cursor
                .as_ref()
                .and_then(|cursor| {
                    (cursor.version == OUTPUT_FRONTIER_VERSION)
                        .then(|| serde_json::from_slice::<OutputCheckpoint>(&cursor.payload).ok())
                        .flatten()
                })
                .is_some_and(|checkpoint| {
                    checkpoint.version == OUTPUT_FRONTIER_VERSION
                        && checkpoint.parser_revision == OUTPUT_PARSER_REVISION
                        && checkpoint.locator_identity == authority.locator_identity
                        && checkpoint.source_revision == authority.source_revision
                        && checkpoint.terminal
                })
    }) {
        return Ok(());
    }

    let prior_frontier = progress
        .as_ref()
        .and_then(|progress| progress.cursor.as_ref())
        .map(|cursor| NativeSafeFrontier::new(cursor.version, cursor.payload.clone()))
        .transpose()
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let checkpoint = OutputCheckpoint {
        version: OUTPUT_FRONTIER_VERSION,
        parser_revision: OUTPUT_PARSER_REVISION.to_owned(),
        locator_identity: authority.locator_identity.clone(),
        source_revision: authority.source_revision.clone(),
        terminal: true,
    };
    let next_frontier =
        NativeSafeFrontier::new(OUTPUT_FRONTIER_VERSION, serde_json::to_vec(&checkpoint)?)
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let initial_frontier = NativeSafeFrontier::new(
        OUTPUT_FRONTIER_VERSION,
        serde_json::to_vec(&OutputCheckpoint {
            version: OUTPUT_FRONTIER_VERSION,
            parser_revision: OUTPUT_PARSER_REVISION.to_owned(),
            locator_identity: authority.locator_identity.clone(),
            source_revision: String::new(),
            terminal: false,
        })?,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let rewrite = progress.as_ref().is_some_and(|progress| {
        progress.parser_revision != OUTPUT_PARSER_REVISION
            || progress.materializer_revision != sink.materializer_revision()
            || progress.observed_revision != authority.source_revision
            || progress.terminal
    });
    let (source_epoch, expected_epoch, disposition) = match progress.as_ref() {
        None => (0, None, ProOutputSourceDisposition::NewSource),
        Some(progress) if rewrite => (
            progress
                .source_epoch
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "Lingma output source epoch exhausted",
                ))?,
            Some(progress.source_epoch),
            ProOutputSourceDisposition::Rewrite,
        ),
        Some(progress) => (
            progress.source_epoch,
            Some(progress.source_epoch),
            ProOutputSourceDisposition::AppendOrResume,
        ),
    };
    let output = NativeProOutputPage {
        inventory_generation: sink.inventory_generation(),
        source,
        source_epoch,
        observed_revision: authority.source_revision.clone(),
        parser_revision: OUTPUT_PARSER_REVISION.to_owned(),
        materializer_revision: sink.materializer_revision().to_owned(),
        disposition,
        expected_prior_source_epoch: expected_epoch,
        expected_prior_frontier: prior_frontier.clone(),
        observations: Vec::new(),
    };
    let accounting = NativePageAccounting {
        logical_units: 1,
        conservative_serialized_bytes: CORE_PAGE_FIXED_BYTES
            .saturating_add(authority.locator_identity.len())
            .saturating_add(authority.source_revision.len())
            .saturating_add(authority.source_root.len()),
    };
    let replay = NativeProReplayPage::new_with_source_identity(
        NativeSourceIdentity::new(
            CaptureProvider::Lingma.as_str(),
            &authority.locator_identity,
        ),
        prior_frontier.unwrap_or(initial_frontier),
        next_frontier,
        true,
        accounting,
        output,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let _ = process_pro_replay_only(replay, sink);
    Ok(())
}

pub(super) fn committed_replay_authority(
    store: &Store,
    machine_id: &str,
    authority: &SourceAuthority,
) -> Result<CoreCheckpoint> {
    let stored = store
        .get_sync_cursor(None, machine_id, &authority.cursor_stream)?
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Lingma output replay requires committed terminal NativePath Core".to_owned(),
            )
        })?;
    let committed = decode_native_path_committed_cursor(&stored.cursor).map_err(|_| {
        CaptureError::InvalidPayload(
            "Lingma output replay requires a Store-committed NativePath Core cursor".to_owned(),
        )
    })?;
    let checkpoint: CoreCheckpoint =
        serde_json::from_str(committed.provider_cursor()).map_err(|_| {
            CaptureError::InvalidPayload(
                "Lingma output replay requires committed Lingma Core authority".to_owned(),
            )
        })?;
    checkpoint.validate(&authority.locator_identity)?;
    if !checkpoint.terminal || checkpoint.source_revision != authority.source_revision {
        return Err(CaptureError::InvalidPayload(
            "Lingma output replay source does not exactly match committed terminal Core authority"
                .to_owned(),
        ));
    }
    Ok(checkpoint)
}
