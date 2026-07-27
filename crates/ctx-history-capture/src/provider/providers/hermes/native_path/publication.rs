use super::{cursor::*, *};

pub(super) fn publish_core_page(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &ctx_history_store::EventSearchBulkGuard,
    context: &PublicationContext<'_>,
    plan: &mut CorePlan,
    page: &mut HermesPage,
    summary: &mut ProviderImportSummary,
) -> Result<bool> {
    if page.expected_frontier != plan.cursor.frontier {
        return Err(CaptureError::InvalidPayload(
            "Hermes NativePath Core frontier is not contiguous".to_owned(),
        ));
    }
    if page.core_owned_bytes > NATIVE_INGESTION_PAGE_MAX_BYTES {
        return Err(CaptureError::SystemInvariant(
            "Hermes NativePath Core page exceeded its owned-byte limit",
        ));
    }
    revalidate_source(context)?;
    localize_dependent_messages(committed_store, context, plan, page)?;
    let rejected = page
        .rows
        .iter()
        .filter(|row| matches!(row.record, HermesNativeRecord::Rejected(_)))
        .count();
    let mut next_cursor = plan.cursor.clone();
    next_cursor.frontier = page.next_frontier;
    next_cursor.terminal = page.terminal;
    next_cursor.retired = false;
    next_cursor.rejected_records = next_cursor
        .rejected_records
        .saturating_add(u64::try_from(rejected).unwrap_or(u64::MAX));
    let next = sync_cursor(context, &next_cursor)?;
    let transition = NativePathCursorTransition::new(
        plan.expected.as_ref().map(|cursor| cursor.cursor.clone()),
        next,
    );
    let publication_id = publication_id(&transition, &next_cursor);
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let accounting = NativePathGroupAccounting::new(1, 1, NATIVE_INGESTION_PAGE_MAX_BYTES)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
        NativePathCursorSetClassification::AllNextSameGroup { .. } => {
            group.commit()?;
            plan.expected =
                store.get_sync_cursor(None, &context.adapter.machine_id, context.cursor_stream)?;
            plan.cursor = next_cursor;
            plan.migration = false;
            return Ok(false);
        }
        NativePathCursorSetClassification::AllExpected => {}
    }

    let proposed_source_identity = provider_source_identity(
        CaptureProvider::Hermes,
        HERMES_SQLITE_SOURCE_FORMAT,
        None,
        Some(&context.canonical_path.display().to_string()),
        None,
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Hermes NativePath source has no canonical identity",
    ))?;
    let resolution =
        group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::Hermes,
            source_format: HERMES_SQLITE_SOURCE_FORMAT.to_owned(),
            machine_id: context.adapter.machine_id.clone(),
            locator_identity: context.locator_identity.to_owned(),
            cursor_stream: context.cursor_stream.to_owned(),
            proposed_source_identity,
            raw_source_path: Some(context.canonical_path.display().to_string()),
            source_revision: context.source_revision.to_owned(),
            observed_at_ms: context.adapter.imported_at.timestamp_millis(),
        })?;
    if plan.cursor.canonical_source_identity != resolution.canonical_source_identity {
        return Err(CaptureError::InvalidPayload(
            "Hermes NativePath source route changed after cursor planning".to_owned(),
        ));
    }
    let mut resolved = BTreeMap::new();
    for row in &page.rows {
        match &row.record {
            HermesNativeRecord::Session(session) => {
                let resolved_session = publish_session(
                    committed_store,
                    &mut group,
                    context,
                    &resolution,
                    session,
                    summary,
                )?;
                resolved.insert(session.id.clone(), resolved_session);
            }
            HermesNativeRecord::Message {
                row: message,
                values,
                prepared,
            } => {
                publish_message(
                    committed_store,
                    &mut group,
                    context,
                    &resolution.canonical_source_identity,
                    &mut resolved,
                    row,
                    message,
                    values,
                    prepared.as_ref(),
                    summary,
                )?;
            }
            HermesNativeRecord::Rejected(reason) => {
                summary.record_failure(ProviderImportFailure {
                    line: usize::try_from(row.ordinal)
                        .unwrap_or(usize::MAX)
                        .saturating_add(1),
                    error: reason.clone(),
                });
            }
        }
    }
    group.prepare_journal_checkpoint()?;
    // Fence the physical provider snapshot after all Core/journal writes while
    // the Store transaction can still roll back, immediately before cursor CAS.
    revalidate_source_before_cursor_publication(context)?;
    group.publish_cursor_set()?;
    group.commit()?;
    plan.expected =
        store.get_sync_cursor(None, &context.adapter.machine_id, context.cursor_stream)?;
    plan.cursor = next_cursor;
    plan.migration = false;
    Ok(true)
}

pub(super) fn publish_session(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    context: &PublicationContext<'_>,
    resolution: &ctx_history_store::ProviderSourceLocatorResolution,
    row: &HermesSessionRow,
    summary: &mut ProviderImportSummary,
) -> Result<ResolvedSession> {
    let raw_source_path = context.canonical_path.display().to_string();
    let existing_source = committed_store.capture_source_by_canonical_identity_session(
        CaptureProvider::Hermes,
        HERMES_SQLITE_SOURCE_FORMAT,
        &context.adapter.machine_id,
        &resolution.canonical_source_identity,
        &row.id,
    )?;
    let source_id = existing_source
        .as_ref()
        .map(|source| source.id)
        .unwrap_or_else(|| {
            if resolution.relocated {
                stable_capture_uuid(
                    &serde_json::to_string(&(
                        "provider-relocated-source-v1",
                        CaptureProvider::Hermes.as_str(),
                        HERMES_SQLITE_SOURCE_FORMAT,
                        &resolution.canonical_source_identity,
                        &row.id,
                    ))
                    .expect("Hermes relocated source identity is serializable"),
                    "source",
                )
            } else {
                provider_scoped_source_uuid(
                    CaptureProvider::Hermes,
                    &row.id,
                    HERMES_SQLITE_SOURCE_FORMAT,
                    Some(&raw_source_path),
                )
            }
        });
    let started_at = crate::provider::normalization::provider_required_timestamp_seconds(
        row.started_at,
        "Hermes session started_at",
    )?;
    let ended_at = row
        .ended_at
        .map(|value| {
            crate::provider::normalization::provider_required_timestamp_seconds(
                value,
                "Hermes session ended_at",
            )
        })
        .transpose()?;
    let source = CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Hermes,
            machine_id: context.adapter.machine_id.clone(),
            process_id: None,
            cwd: row.cwd.clone(),
            raw_source_path: Some(raw_source_path.clone()),
            source_format: Some(HERMES_SQLITE_SOURCE_FORMAT.to_owned()),
            source_root: Some(context.configured_source_root.clone()),
            source_identity: Some(resolution.canonical_source_identity.clone()),
            external_session_id: Some(row.id.clone()),
        },
        started_at,
        ended_at,
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": row.id,
                "source_format": HERMES_SQLITE_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.adapter.imported_at,
                "source_identity": resolution.canonical_source_identity,
                "source_root": context.configured_source_root,
                "source_revision": context.source_revision,
                "source_identity_key": provider_scoped_source_identity_key(
                    CaptureProvider::Hermes,
                    &row.id,
                    HERMES_SQLITE_SOURCE_FORMAT,
                    Some(&raw_source_path),
                ),
                "source_metadata": source_metadata(context),
                "nativepath_publication": HERMES_CURSOR_VERSION,
            }),
        ),
    };
    group.upsert_capture_source(&source)?;
    group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;
    let session_id = provider_import_session_uuid(
        committed_store,
        CaptureProvider::Hermes,
        &row.id,
        source_id,
        Some(&resolution.canonical_source_identity),
    )?;
    let parent_id = row
        .parent_session_id
        .as_deref()
        .map(|parent| {
            provider_import_session_uuid(
                committed_store,
                CaptureProvider::Hermes,
                parent,
                source_id,
                Some(&resolution.canonical_source_identity),
            )
        })
        .transpose()?;
    if let (Some(parent_id), Some(parent_external)) = (parent_id, row.parent_session_id.as_deref())
    {
        group.insert_session_if_absent(&relationship_placeholder(
            context,
            source_id,
            parent_id,
            parent_external,
            &resolution.canonical_source_identity,
        ))?;
    }
    let session = Session {
        id: session_id,
        history_record_id: context.options.history_record_id,
        parent_session_id: parent_id,
        root_session_id: parent_id,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Hermes,
        external_session_id: Some(row.id.clone()),
        external_agent_id: Some(row.source.clone()),
        agent_type: if parent_id.is_some() {
            AgentType::Subagent
        } else {
            AgentType::Primary
        },
        role_hint: Some(row.source.clone()),
        is_primary: parent_id.is_none(),
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at,
        ended_at,
        timestamps: timestamps(context.adapter.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": row.id,
                "parent_provider_session_id": row.parent_session_id,
                "source_format": HERMES_SQLITE_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.adapter.imported_at,
                "session_idempotency_key": format!(
                    "provider-session:{}:{}",
                    CaptureProvider::Hermes.as_str(),
                    row.id
                ),
                "metadata": session_metadata(row),
            }),
        ),
    };
    let existed = committed_store.get_session(session.id).is_ok();
    group.upsert_session(&session)?;
    if existed {
        summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    } else {
        summary.imported_sessions = summary.imported_sessions.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    }
    if let Some(parent_id) = parent_id {
        let edge_id = if session.id != provider_session_uuid(CaptureProvider::Hermes, &row.id) {
            provider_source_edge_uuid(
                &resolution.canonical_source_identity,
                &row.id,
                "parent_child",
            )
        } else {
            crate::provider::importer::provider_edge_uuid(
                CaptureProvider::Hermes,
                &row.id,
                "parent_child",
            )
        };
        let edge = SessionEdge {
            id: edge_id,
            from_session_id: session.id,
            to_session_id: parent_id,
            edge_type: SessionEdgeType::ParentChild,
            confidence: Confidence::Explicit,
            source_id: Some(source_id),
            timestamps: timestamps(context.adapter.imported_at),
            sync: provider_sync_metadata(
                Fidelity::Imported,
                json!({
                    "provider_session_id": row.id,
                    "parent_provider_session_id": row.parent_session_id,
                    "source_format": HERMES_SQLITE_SOURCE_FORMAT,
                    "imported_at": context.adapter.imported_at,
                }),
            ),
        };
        let edge_existed = committed_store.session_edge_exists(edge.id)?;
        group.upsert_projection_neutral_session_edge(&actor(&session), &edge)?;
        if edge_existed {
            summary.skipped_edges = summary.skipped_edges.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
        } else {
            summary.imported_edges = summary.imported_edges.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        }
    }
    Ok(ResolvedSession { source_id, session })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_message(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    context: &PublicationContext<'_>,
    canonical_source_identity: &str,
    resolved: &mut BTreeMap<String, ResolvedSession>,
    native_row: &HermesNativeRow,
    row: &HermesMessageRow,
    values: &[HermesSqliteValue],
    prepared: Option<&super::super::HermesPreparedCoreMessage>,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let resolved_session = if let Some(resolved) = resolved.get(&row.session_id) {
        resolved.clone()
    } else {
        let source = committed_store
            .capture_source_by_canonical_identity_session(
                CaptureProvider::Hermes,
                HERMES_SQLITE_SOURCE_FORMAT,
                &context.adapter.machine_id,
                canonical_source_identity,
                &row.session_id,
            )?
            .ok_or_else(|| {
                CaptureError::InvalidPayload(format!(
                    "Hermes message {} references a session that was not safely imported",
                    row.id
                ))
            })?;
        let session = committed_store
            .session_by_capture_source_and_external_session(
                source.id,
                CaptureProvider::Hermes,
                &row.session_id,
            )?
            .ok_or_else(|| {
                CaptureError::InvalidPayload(format!(
                    "Hermes message {} references a missing canonical session",
                    row.id
                ))
            })?;
        let resolved_session = ResolvedSession {
            source_id: source.id,
            session,
        };
        resolved.insert(row.session_id.clone(), resolved_session.clone());
        resolved
            .get(&row.session_id)
            .cloned()
            .ok_or(CaptureError::SystemInvariant(
                "Hermes resolved session cache lost an inserted row",
            ))?
    };
    let provider_event_index = crate::provider::normalization::provider_nonnegative_i64_to_u64(
        row.id,
        "Hermes message id",
    )?;
    let content = hermes_decode_content(row.content.as_deref());
    let output_outcome = (row.role == "tool").then(|| hermes_output_outcome(row, &content));
    if output_outcome.as_ref().is_some_and(|outcome| {
        !matches!(
            outcome.outcome,
            crate::OutputOutcome::Failure | crate::OutputOutcome::Timeout
        )
    }) {
        return Ok(());
    }
    let prepared = match prepared {
        Some(prepared) => prepared.clone(),
        None => super::super::prepare_hermes_core_message(row, native_row.ordinal, values)?,
    };
    let mut native = prepared.native;
    let event_hash = ctx_history_core::compute_payload_hash(&native.payload)?;
    super::super::attach_hermes_complete_content(
        &mut native,
        &native_row.locator,
        prepared.complete_content.as_ref(),
    )?;
    let identity = provider_event_import_identity_with_exact_legacy_source(
        committed_store,
        CaptureProvider::Hermes,
        &row.session_id,
        resolved_session.source_id,
        provider_event_index,
        provider_event_index,
        &event_hash,
        None,
        None,
        resolved_session.session.id
            == provider_session_uuid(CaptureProvider::Hermes, &row.session_id),
    )?;
    let event = hermes_core_event(
        context,
        &row.session_id,
        resolved_session.source_id,
        resolved_session.session.id,
        usize::try_from(native_row.ordinal)
            .unwrap_or(usize::MAX)
            .saturating_add(1),
        &native,
        &event_hash,
        &identity,
    )?;
    let exact_released_provider_hash = format!("message:{}", row.id);
    if group.reconcile_provider_event_migrating_exact_legacy_provider_hash(
        &event,
        &exact_released_provider_hash,
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

#[allow(clippy::too_many_arguments)]
pub(super) fn hermes_core_event(
    context: &PublicationContext<'_>,
    provider_session_id: &str,
    source_id: Uuid,
    session_id: Uuid,
    line_number: usize,
    native: &HermesNativeEvent,
    event_hash: &str,
    identity: &ProviderEventImportIdentity,
) -> Result<Event> {
    let dedupe_key =
        Store::provider_event_dedupe_key_with_payload_hash(&identity.dedupe_key, event_hash)
            .unwrap_or_else(|| identity.dedupe_key.clone());
    let mut provider_metadata = native.metadata.clone();
    let source_record_coordinates = take_hermes_source_record_coordinates(&mut provider_metadata)?;
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
        "provider_event_hash_authority":
            ProviderEventHashAuthority::NormalizedPayloadFallback.as_str(),
        "cursor": native.cursor,
        "source_format": HERMES_SQLITE_SOURCE_FORMAT,
        "source_trust": ProviderSourceTrust::ProviderNative,
        "fixture_line": line_number,
        "imported_at": context.adapter.imported_at,
        "event_idempotency_key": format!(
            "provider-event:{}:{}:{}",
            CaptureProvider::Hermes.as_str(),
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
        history_record_id: context.options.history_record_id,
        session_id: Some(session_id),
        run_id: None,
        event_type: native.event_type,
        role: native.role,
        occurred_at: native.occurred_at,
        capture_source_id: Some(source_id),
        payload: json!({
            "provider": CaptureProvider::Hermes.as_str(),
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

pub(super) fn take_hermes_source_record_coordinates(
    metadata: &mut serde_json::Value,
) -> Result<Option<(u64, u32)>> {
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

pub(super) fn publish_output_page(
    profile: &ImportProfile,
    context: &PublicationContext<'_>,
    plan: &mut OutputPlan,
    page: &HermesPage,
) -> Result<bool> {
    let sink = profile.sink().ok_or(CaptureError::SystemInvariant(
        "Hermes NativePath output page has no output sink",
    ))?;
    revalidate_source(context)?;
    if page.next_frontier.next_ordinal < plan.scan_frontier.next_ordinal
        || (page.next_frontier.next_ordinal == plan.scan_frontier.next_ordinal
            && (!page.terminal || plan.terminal))
    {
        return Ok(true);
    }
    let output_page = (|| {
        if page.expected_frontier.next_ordinal < plan.scan_frontier.next_ordinal {
            return Err(CaptureError::InvalidPayload(
                "Hermes output cursor is not a certified page boundary".to_owned(),
            ));
        }
        let observations = page
            .rows
            .iter()
            .filter_map(|native_row| match &native_row.record {
                HermesNativeRecord::Message { row, .. } if row.role == "tool" => {
                    Some(hermes_pro_output(row, native_row))
                }
                HermesNativeRecord::Session(_)
                | HermesNativeRecord::Message { .. }
                | HermesNativeRecord::Rejected(_) => None,
            })
            .collect::<Result<Vec<_>>>()?;
        let expected = safe_frontier(page.expected_frontier)?;
        let next = safe_frontier(page.next_frontier)?;
        let output = NativeProOutputPage {
            inventory_generation: sink.inventory_generation(),
            source: plan.source.clone(),
            source_epoch: plan.source_epoch,
            observed_revision: context.source_revision.to_owned(),
            parser_revision: HERMES_OUTPUT_PARSER_REVISION.to_owned(),
            materializer_revision: sink.materializer_revision().to_owned(),
            disposition: plan.disposition,
            expected_prior_source_epoch: plan.expected_source_epoch,
            expected_prior_frontier: plan.expected_frontier.clone(),
            observations,
        };
        let replay = NativeProReplayPage::new_with_source_identity(
            NativeSourceIdentity::new(
                CaptureProvider::Hermes.as_str(),
                plan.source.source_id.clone(),
            ),
            expected,
            next.clone(),
            page.terminal,
            NativePageAccounting {
                logical_units: output.observations.len(),
                conservative_serialized_bytes: page.output_owned_bytes,
            },
            output,
        )
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        Ok::<_, CaptureError>((replay, next))
    })();
    let (replay, next) = match output_page {
        Ok(page) => page,
        Err(_) => {
            sink.mark_behind(ProOutputSinkError::new(
                "hermes_output_page",
                "Hermes Pro output page is invalid",
            ));
            return Ok(false);
        }
    };
    if process_pro_replay_only(replay, sink.as_ref()).is_err() {
        // The bounded output coordinator already marked only this sink behind.
        // Keep the committed Core result and retry this exact frontier later.
        return Ok(false);
    }
    plan.expected_source_epoch = Some(plan.source_epoch);
    plan.expected_frontier = Some(next);
    plan.scan_frontier = page.next_frontier;
    plan.disposition = ProOutputSourceDisposition::AppendOrResume;
    plan.terminal = page.terminal;
    Ok(true)
}
