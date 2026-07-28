use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn import_core(
    store: &mut Store,
    authority: &mut FirebenderSourceAuthority,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    prior: Option<FirebenderNativeCursor>,
    generation: u64,
) -> Result<ProviderImportSummary> {
    let same_generation = prior.as_ref().is_some_and(|cursor| {
        cursor.route_identity == authority.route_identity
            && cursor.source_revision == authority.source_revision
            && cursor.schema_fingerprint == authority.schema_fingerprint
    });
    let mut prior = prior;
    if let Some(cursor) = prior.as_mut().filter(|_| same_generation) {
        // The first NativePath cursor shape advanced past malformed rows without
        // retaining their diagnostics. Treat that released-in-branch shape as
        // unsafe continuation authority and rebuild it idempotently.
        if cursor.rejected_records != 0 && cursor.failures.is_empty() {
            cursor.frontier = FirebenderFrontier::initial();
            cursor.frontier_accepted_sessions = 0;
            cursor.frontier_accepted_events = 0;
            cursor.scan_terminal = false;
        }
    }
    let mut durable_frontier = prior
        .as_ref()
        .filter(|_| same_generation)
        .map(|cursor| cursor.frontier.clone())
        .unwrap_or_else(FirebenderFrontier::initial);
    let mut scan_frontier = durable_frontier.clone();
    let mut rejected_records = prior
        .as_ref()
        .filter(|_| same_generation)
        .map_or(0, |cursor| cursor.rejected_records);
    let mut accepted_sessions = prior
        .as_ref()
        .filter(|_| same_generation)
        .map_or(0, |cursor| cursor.accepted_sessions);
    let mut accepted_events = prior
        .as_ref()
        .filter(|_| same_generation)
        .map_or(0, |cursor| cursor.accepted_events);
    let mut frontier_accepted_sessions = prior
        .as_ref()
        .filter(|_| same_generation)
        .map_or(0, |cursor| cursor.frontier_accepted_sessions);
    let mut frontier_accepted_events = prior
        .as_ref()
        .filter(|_| same_generation)
        .map_or(0, |cursor| cursor.frontier_accepted_events);
    let mut retained_failures = prior
        .as_ref()
        .filter(|_| same_generation)
        .map(|cursor| cursor.failures.clone())
        .unwrap_or_default();
    let mut scan_terminal = prior
        .as_ref()
        .filter(|_| same_generation)
        .is_some_and(|cursor| cursor.scan_terminal || cursor.frontier.terminal);
    let mut rejection_seen = rejected_records != 0;
    if durable_frontier.terminal {
        let mut summary = ProviderImportSummary::default();
        replay_cursor_summary(
            accepted_sessions,
            accepted_events,
            rejected_records,
            &retained_failures,
            &mut summary,
        );
        return Ok(summary);
    }

    let mut scanned_accepted_sessions = frontier_accepted_sessions;
    let mut scanned_accepted_events = frontier_accepted_events;
    let mut scanned_rejected_records = 0_u64;

    let committed_store = Store::open_read_only(store.path())?;
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let mut summary = ProviderImportSummary::default();
        loop {
            let page = authority.database.read(&authority.database_path, |conn| {
                build_page(conn, &scan_frontier, false)
            })?;
            let page_accepted_sessions = u64::from(
                page.message_start == 0 && page.row.as_ref().is_some_and(row_has_core_content),
            );
            let page_accepted_events = u64::try_from(core_event_count(&page)).unwrap_or(u64::MAX);
            scanned_accepted_sessions =
                scanned_accepted_sessions.saturating_add(page_accepted_sessions);
            scanned_accepted_events = scanned_accepted_events.saturating_add(page_accepted_events);
            scanned_rejected_records =
                scanned_rejected_records.saturating_add(u64::from(page.rejection.is_some()));
            accepted_sessions = accepted_sessions.max(scanned_accepted_sessions);
            accepted_events = accepted_events.max(scanned_accepted_events);
            rejected_records = rejected_records.max(scanned_rejected_records);
            if let Some(failure) = page_rejection_failure(&page) {
                rejection_seen = true;
                if !retained_failures.contains(&failure)
                    && retained_failures.len() < MAX_RETAINED_PROVIDER_FAILURES
                {
                    retained_failures.push(failure);
                }
            } else if !rejection_seen {
                durable_frontier = page.next.clone();
                frontier_accepted_sessions = scanned_accepted_sessions;
                frontier_accepted_events = scanned_accepted_events;
            }
            scan_terminal |= page.next.terminal;
            let next_cursor = FirebenderNativeCursor {
                version: FIREBENDER_NATIVE_CURSOR_VERSION,
                parser_revision: FIREBENDER_NATIVE_PARSER_REVISION,
                policy_revision: FIREBENDER_NATIVE_POLICY_REVISION,
                route_identity: authority.route_identity.clone(),
                canonical_source_identity: authority.canonical_source_identity.clone(),
                source_revision: authority.source_revision.clone(),
                schema_fingerprint: authority.schema_fingerprint.clone(),
                generation,
                rejected_records,
                accepted_sessions,
                accepted_events,
                frontier_accepted_sessions,
                frontier_accepted_events,
                failures: retained_failures.clone(),
                scan_terminal,
                frontier: durable_frontier.clone(),
            };
            let page_summary = publish_core_page(
                store,
                &committed_store,
                &bulk_guard,
                authority,
                context,
                options,
                &page,
                next_cursor,
            )?;
            let page_changed =
                page_summary.summary.work_result() == ProviderImportWorkResult::Changed;
            summary.merge_from(page_summary.summary);
            authority.canonical_source_identity = page_summary.canonical_source_identity;
            scan_frontier = page.next;
            if scan_frontier.terminal {
                break;
            }
            if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup && page_changed {
                summary.work_remaining = true;
                break;
            }
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

struct PublishedPage {
    summary: ProviderImportSummary,
    canonical_source_identity: String,
}

#[allow(clippy::too_many_arguments)]
fn publish_core_page(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    authority: &FirebenderSourceAuthority,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    page: &FirebenderPage,
    next_cursor: FirebenderNativeCursor,
) -> Result<PublishedPage> {
    let stored = store.get_sync_cursor(None, &context.machine_id, &authority.cursor_stream)?;
    let encoded = next_cursor.encode()?;
    let retained_bytes = page.retained_bytes.saturating_add(encoded.len());
    let provider_cursor_unchanged = stored.as_ref().is_some_and(|cursor| {
        decode_native_path_committed_cursor(&cursor.cursor)
            .is_ok_and(|committed| committed.provider_cursor() == encoded)
    });
    if provider_cursor_unchanged {
        return Ok(PublishedPage {
            summary: replayed_page_summary(page),
            canonical_source_identity: authority.canonical_source_identity.clone(),
        });
    }
    let next = SyncCursor {
        id: Uuid::new_v4(),
        team_id: None,
        device_id: context.machine_id.clone(),
        stream: authority.cursor_stream.clone(),
        cursor: encoded,
        last_synced_at: Some(context.imported_at),
        timestamps: timestamps(context.imported_at),
    };
    let transition =
        NativePathCursorTransition::new(stored.as_ref().map(|cursor| cursor.cursor.clone()), next);
    let publication_id = publication_id(authority, page, transition.next().cursor.as_str());
    let accounting = NativePathGroupAccounting::new(1, 1, retained_bytes)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        return Ok(PublishedPage {
            summary: replayed_page_summary(page),
            canonical_source_identity: authority.canonical_source_identity.clone(),
        });
    }

    let raw_source_path = authority.canonical_database_path.display().to_string();
    let source_root = authority.configured_source_root.display().to_string();
    let resolution =
        group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::Firebender,
            source_format: FIREBENDER_SQLITE_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity: authority.route_identity.clone(),
            cursor_stream: authority.cursor_stream.clone(),
            proposed_source_identity: authority.proposed_source_identity.clone(),
            raw_source_path: Some(raw_source_path.clone()),
            source_revision: authority.source_revision.clone(),
            observed_at_ms: context.imported_at.timestamp_millis(),
        })?;
    let mut summary = ProviderImportSummary::default();
    if let Some(row) = page
        .row
        .as_ref()
        .filter(|_| page.rejection.is_none() && core_event_count(page) != 0)
    {
        let source_id = resolve_source_id(
            committed_store,
            row,
            &context.machine_id,
            &resolution.canonical_source_identity,
            &raw_source_path,
        )?;
        group.upsert_capture_source(&capture_source(
            source_id,
            row,
            authority,
            context,
            &raw_source_path,
            &source_root,
            &resolution.canonical_source_identity,
        ))?;
        group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;
        let session = session(
            committed_store,
            source_id,
            row,
            context,
            options,
            &resolution.canonical_source_identity,
        )?;
        let existed = committed_store.get_session(session.id).is_ok();
        group.upsert_session(&session)?;
        if !existed {
            summary.imported_sessions = summary.imported_sessions.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        } else if page.message_start == 0 {
            summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
        }
        publish_events(
            committed_store,
            &mut group,
            source_id,
            &session,
            row,
            page.message_start,
            page.message_end,
            context,
            options,
            &mut summary,
        )?;
        summary.accepted_content_records = summary
            .accepted_content_records
            .saturating_add(core_event_count(page));
    }
    if let Some(rejection) = &page.rejection {
        summary.record_failure(
            page_rejection_failure(page).unwrap_or(ProviderImportFailure {
                line: usize::MAX,
                error: rejection.clone(),
            }),
        );
    }
    authority.database.revalidate()?;
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(PublishedPage {
        summary,
        canonical_source_identity: resolution.canonical_source_identity,
    })
}

fn resolve_source_id(
    store: &Store,
    row: &FirebenderRow,
    machine_id: &str,
    canonical_source_identity: &str,
    raw_source_path: &str,
) -> Result<Uuid> {
    Ok(store
        .capture_source_by_canonical_identity_session(
            CaptureProvider::Firebender,
            FIREBENDER_SQLITE_SOURCE_FORMAT,
            machine_id,
            canonical_source_identity,
            &row.id,
        )?
        .map(|source| source.id)
        .unwrap_or_else(|| {
            provider_scoped_source_uuid(
                CaptureProvider::Firebender,
                &row.id,
                FIREBENDER_SQLITE_SOURCE_FORMAT,
                Some(raw_source_path),
            )
        }))
}

#[allow(clippy::too_many_arguments)]
fn capture_source(
    source_id: Uuid,
    row: &FirebenderRow,
    authority: &FirebenderSourceAuthority,
    context: &ProviderAdapterContext,
    raw_source_path: &str,
    source_root: &str,
    canonical_source_identity: &str,
) -> CaptureSource {
    let started_at = provider_timestamp_millis(Some(row.created_at), context.imported_at);
    CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Firebender,
            machine_id: context.machine_id.clone(),
            process_id: None,
            cwd: None,
            raw_source_path: Some(raw_source_path.to_owned()),
            source_format: Some(FIREBENDER_SQLITE_SOURCE_FORMAT.to_owned()),
            source_root: Some(source_root.to_owned()),
            source_identity: Some(canonical_source_identity.to_owned()),
            external_session_id: Some(row.id.clone()),
        },
        started_at,
        ended_at: Some(provider_timestamp_millis(Some(row.updated_at), started_at)),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": row.id,
                "source_format": FIREBENDER_SQLITE_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "source_identity": canonical_source_identity,
                "source_root": source_root,
                "source_revision": authority.source_revision,
                "source_identity_key": provider_scoped_source_identity_key(
                    CaptureProvider::Firebender,
                    &row.id,
                    FIREBENDER_SQLITE_SOURCE_FORMAT,
                    Some(raw_source_path),
                ),
                "schema_fingerprint": authority.schema_fingerprint,
                "source_metadata": {
                    "adapter": FIREBENDER_SQLITE_SOURCE_FORMAT,
                    "schema_fingerprint": authority.schema_fingerprint,
                    "storage": ".idea/firebender/chat_history.db",
                },
                "nativepath_publication": FIREBENDER_NATIVE_PARSER_REVISION,
            }),
        ),
    }
}

fn session(
    store: &Store,
    source_id: Uuid,
    row: &FirebenderRow,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    canonical_source_identity: &str,
) -> Result<Session> {
    let started_at = provider_timestamp_millis(Some(row.created_at), context.imported_at);
    let ended_at = Some(provider_timestamp_millis(Some(row.updated_at), started_at));
    let metadata = provider_json_text(&row.metadata_json);
    Ok(Session {
        id: provider_import_session_uuid(
            store,
            CaptureProvider::Firebender,
            &row.id,
            source_id,
            Some(canonical_source_identity),
        )?,
        history_record_id: options.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Firebender,
        external_session_id: Some(row.id.clone()),
        external_agent_id: None,
        agent_type: AgentType::Primary,
        role_hint: Some("primary".to_owned()),
        is_primary: true,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at,
        ended_at,
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": row.id,
                "source_format": FIREBENDER_SQLITE_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "session_idempotency_key":
                    format!("provider-session:firebender:{}", row.id),
                "metadata": {
                    "title": row.name,
                    "metadata": provider_capped_json(&metadata, PROVIDER_MAX_PREVIEW_CHARS),
                    "storage": ".idea/firebender/chat_history.db",
                    "timestamp_note": "message rows do not carry durable per-message timestamps; ctx preserves session created_at/updated_at and import order",
                    "nativepath_publication": FIREBENDER_NATIVE_PARSER_REVISION,
                },
            }),
        ),
    })
}

#[allow(clippy::too_many_arguments)]
fn publish_events(
    store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    source_id: Uuid,
    session: &Session,
    row: &FirebenderRow,
    start: usize,
    end: usize,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let locator = NativeLocator::new(FIREBENDER_LOCATOR_KIND, row.rowid.to_be_bytes().to_vec())
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let values = row.logical_values();
    for (message_index, message) in row.messages[start..end].iter().enumerate() {
        let absolute_index = start.saturating_add(message_index);
        let provider_event_index = u64::try_from(absolute_index)
            .map_err(|_| CaptureError::SystemInvariant("Firebender message index exceeds u64"))?;
        let fallback_offset = i64::try_from(absolute_index)
            .map_err(|_| CaptureError::SystemInvariant("Firebender message index exceeds i64"))?;
        let occurred_at = firebender_message_time(
            message,
            session.started_at + chrono::Duration::milliseconds(fallback_offset),
        );
        let mut native =
            firebender_native_event(&row.id, provider_event_index, message, occurred_at);
        if native.event_type == EventType::ToolOutput {
            let evidence = firebender_output_evidence(message);
            if !evidence.failure && !evidence.timeout {
                continue;
            }
        } else {
            attach_firebender_complete_content(&mut native, &locator, &values, || {
                super::firebender_message_text(message).unwrap_or_else(|| {
                    format!(
                        "Firebender {}",
                        message
                            .get("role")
                            .and_then(Value::as_str)
                            .unwrap_or("message")
                    )
                })
            })?;
        }
        let subrecord = u32::try_from(absolute_index).map_err(|_| {
            CaptureError::InvalidPayload(
                "Firebender message index exceeds complete-content coordinates".to_owned(),
            )
        })?;
        if let Some(metadata) = native.metadata.as_object_mut() {
            metadata.insert("source_record_ordinal".to_owned(), json!(row.row_ordinal));
            metadata.insert("source_record_subrecord_index".to_owned(), json!(subrecord));
        }
        let (event_hash, authority) = native.provider_event_hash.as_ref().map_or_else(
            || {
                compute_payload_hash(&native.payload)
                    .map(|hash| (hash, ProviderEventHashAuthority::NormalizedPayloadFallback))
            },
            |hash| Ok((hash.clone(), ProviderEventHashAuthority::ProviderSupplied)),
        )?;
        let identity = provider_event_import_identity_with_exact_legacy_source(
            store,
            CaptureProvider::Firebender,
            &row.id,
            source_id,
            provider_event_index,
            provider_event_index,
            &event_hash,
            None,
            Some(provider_event_index),
            session.id == provider_session_uuid(CaptureProvider::Firebender, &row.id),
        )?;
        let line_number = absolute_index.saturating_add(1);
        let event = firebender_core_event(
            context,
            options,
            &row.id,
            source_id,
            session.id,
            line_number,
            &native,
            &event_hash,
            authority,
            &identity,
        )?;
        if group.reconcile_provider_event(&event, authority)? {
            summary.imported_events = summary.imported_events.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        } else {
            summary.skipped_events = summary.skipped_events.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn firebender_core_event(
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    provider_session_id: &str,
    source_id: Uuid,
    session_id: Uuid,
    line_number: usize,
    native: &FirebenderNativeEvent,
    event_hash: &str,
    authority: ProviderEventHashAuthority,
    identity: &ProviderEventImportIdentity,
) -> Result<Event> {
    let dedupe_key =
        Store::provider_event_dedupe_key_with_payload_hash(&identity.dedupe_key, event_hash)
            .unwrap_or_else(|| identity.dedupe_key.clone());
    let mut provider_metadata = native.metadata.clone();
    let source_record_coordinates =
        take_firebender_source_record_coordinates(&mut provider_metadata)?;
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
        "provider_event_index": native.provider_event_index,
        "provider_event_hash": event_hash,
        "provider_event_hash_authority": authority.as_str(),
        "cursor": native.cursor,
        "source_format": FIREBENDER_SQLITE_SOURCE_FORMAT,
        "source_trust": ProviderSourceTrust::ProviderNative,
        "fixture_line": line_number,
        "imported_at": context.imported_at,
        "event_idempotency_key": format!(
            "provider-event:{}:{}:{}",
            CaptureProvider::Firebender.as_str(),
            provider_session_id,
            native.provider_event_index,
        ),
        "source_record_ordinal": source_record_coordinates
            .as_ref()
            .map(|coordinates| coordinates.0),
        "source_record_subrecord_index": source_record_coordinates
            .as_ref()
            .map(|coordinates| coordinates.1),
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
    Ok(Event {
        id: identity.id,
        seq: identity.seq,
        history_record_id: options.history_record_id,
        session_id: Some(session_id),
        run_id: None,
        event_type: native.event_type,
        role: native.role,
        occurred_at: native.occurred_at,
        capture_source_id: Some(source_id),
        payload: json!({
            "provider": CaptureProvider::Firebender.as_str(),
            "provider_session_id": provider_session_id,
            "provider_event_index": native.provider_event_index,
            "provider_event_hash": event_hash,
            "cursor": native.cursor,
            "artifacts": [],
            "body": compact_provider_result_payload(native.event_type, &native.payload),
        }),
        payload_blob_id: None,
        dedupe_key: Some(dedupe_key),
        sync: provider_sync_metadata(Fidelity::Imported, sync_metadata),
    })
}

fn take_firebender_source_record_coordinates(metadata: &mut Value) -> Result<Option<(u64, u32)>> {
    let Some(object) = metadata.as_object_mut() else {
        return Ok(None);
    };
    let ordinal = object.remove("source_record_ordinal");
    let subrecord = object.remove("source_record_subrecord_index");
    if ordinal.is_none() && subrecord.is_none() {
        return Ok(None);
    }
    let ordinal = ordinal.and_then(|value| value.as_u64()).ok_or_else(|| {
        CaptureError::InvalidPayload("source record ordinal annotation is malformed".to_owned())
    })?;
    let subrecord = subrecord
        .and_then(|value| value.as_u64())
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "source record subrecord annotation is malformed".to_owned(),
            )
        })?;
    Ok(Some((ordinal, subrecord)))
}

fn attach_firebender_complete_content(
    event: &mut FirebenderNativeEvent,
    locator: &NativeLocator,
    values: &[NativeSqliteValue],
    complete_text: impl FnOnce() -> String,
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
    let complete_text = complete_text();
    let content_ref = ContentRef::from_bytes(complete_text.as_bytes()).ok_or(
        CaptureError::SystemInvariant("SQLite content length exceeds ContentRef bounds"),
    )?;
    let profile = verified_content_profile(
        CaptureProvider::Firebender,
        FIREBENDER_SQLITE_SOURCE_FORMAT,
        CompleteContentSourceFamily::Sqlite,
        VerifiedContentRole::MessageBody,
    )
    .ok_or(CaptureError::SystemInvariant(
        "supported SQLite message route must have a verified-content profile",
    ))?;
    let native_record_id = event
        .provider_event_hash
        .clone()
        .unwrap_or_else(|| event.cursor.clone());
    let persisted = VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        profile,
        content_ref,
        CompleteContentSourceFamily::Sqlite,
        locator.kind(),
        locator.value(),
        native_record_id,
        firebender_record_digest(values),
    )
    .ok_or(CaptureError::SystemInvariant(
        "SQLite complete-content locator exceeds the bounded canonical schema",
    ))?;
    attach_verified_content_locator(&mut event.metadata, persisted).ok_or(
        CaptureError::SystemInvariant("verified-content locator collection is malformed"),
    )?;
    Ok(())
}

fn firebender_record_digest(values: &[NativeSqliteValue]) -> CompleteContentBodyDigest {
    CompleteContentBodyDigest::parse(hex(&firebender_raw_row_digest(values)))
        .expect("SHA-256 formatter must return a valid digest")
}

pub(super) fn firebender_raw_row_digest(values: &[NativeSqliteValue]) -> [u8; 32] {
    const DOMAIN: &[u8] = b"ctx-complete-content-sqlite-logical-row-v1\0";
    let mut digest = Sha256::new();
    digest.update(DOMAIN);
    digest.update((values.len() as u64).to_be_bytes());
    for value in values {
        match value {
            NativeSqliteValue::Null => digest.update([0]),
            NativeSqliteValue::Integer(value) => {
                digest.update([1]);
                digest.update(value.to_be_bytes());
            }
            NativeSqliteValue::RealBits(value) => {
                digest.update([2]);
                digest.update(value.to_be_bytes());
            }
            NativeSqliteValue::Text(value) => {
                digest.update([3]);
                digest.update((value.len() as u64).to_be_bytes());
                digest.update(value.as_bytes());
            }
            NativeSqliteValue::Blob(value) => {
                digest.update([4]);
                digest.update((value.len() as u64).to_be_bytes());
                digest.update(value);
            }
        }
    }
    digest.finalize().into()
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn core_event_count(page: &FirebenderPage) -> usize {
    let Some(row) = page.row.as_ref() else {
        return 0;
    };
    row.messages[page.message_start..page.message_end]
        .iter()
        .filter(|message| {
            if message.get("role").and_then(Value::as_str) != Some("tool") {
                return true;
            }
            let evidence = firebender_output_evidence(message);
            evidence.failure || evidence.timeout
        })
        .count()
}

fn row_has_core_content(row: &FirebenderRow) -> bool {
    row.messages.iter().any(message_is_core_eligible)
}

fn message_is_core_eligible(message: &Value) -> bool {
    if message.get("role").and_then(Value::as_str) != Some("tool") {
        return true;
    }
    let evidence = firebender_output_evidence(message);
    evidence.failure || evidence.timeout
}

fn page_rejection_failure(page: &FirebenderPage) -> Option<ProviderImportFailure> {
    page.rejection.as_ref().map(|error| ProviderImportFailure {
        line: usize::try_from(page.expected.row_ordinal)
            .unwrap_or(usize::MAX)
            .saturating_add(1),
        error: bounded_failure(error.clone()),
    })
}

fn bounded_failure(mut error: String) -> String {
    if error.len() <= FIREBENDER_MAX_FAILURE_BYTES {
        return error;
    }
    let mut boundary = FIREBENDER_MAX_FAILURE_BYTES;
    while !error.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    error.truncate(boundary);
    error
}

fn replayed_page_summary(page: &FirebenderPage) -> ProviderImportSummary {
    let mut summary = ProviderImportSummary::default();
    if let Some(failure) = page_rejection_failure(page) {
        summary.record_failure(failure);
    } else if page.row.is_some() {
        let skipped_events = core_event_count(page);
        let skipped_sessions = usize::from(
            page.message_start == 0 && page.row.as_ref().is_some_and(row_has_core_content),
        );
        summary.skipped_events = skipped_events;
        summary.skipped_sessions = skipped_sessions;
        summary.skipped = skipped_events.saturating_add(skipped_sessions);
        summary.accepted_content_records = skipped_events;
    }
    summary.set_work_result(ProviderImportWorkResult::NoOp);
    summary
}

fn replay_cursor_summary(
    accepted_sessions: u64,
    accepted_events: u64,
    rejected_records: u64,
    failures: &[ProviderImportFailure],
    summary: &mut ProviderImportSummary,
) {
    summary.skipped_sessions = usize::try_from(accepted_sessions).unwrap_or(usize::MAX);
    summary.skipped_events = usize::try_from(accepted_events).unwrap_or(usize::MAX);
    summary.skipped = summary
        .skipped_sessions
        .saturating_add(summary.skipped_events);
    summary.accepted_content_records = usize::try_from(accepted_events).unwrap_or(usize::MAX);
    for failure in failures {
        summary.record_failure(failure.clone());
    }
    summary.failed = summary
        .failed
        .max(usize::try_from(rejected_records).unwrap_or(usize::MAX));
    summary.set_work_result(ProviderImportWorkResult::NoOp);
}
