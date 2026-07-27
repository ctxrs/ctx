use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_page_events(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    context: &ClineFreshPublicationContext<'_>,
    resolved: &ResolvedClineSource,
    generation: u64,
    source: &ClineFileSourceIdentity,
    events: &[ClineEventRow],
    summary: &mut ProviderImportSummary,
) -> std::result::Result<(), ClineNativeVerticalError> {
    for event in events {
        let provider_event_sequence_index = packed_event_index(event)?;
        let provider_event_identity_index = provider_local_event_identity_index(event)?;
        let event_hash = hex(&event.content_hash);
        let released = released_v025_event_identity(source, event)?;
        let identity = provider_event_import_identity_with_exact_legacy_source(
            committed_store,
            context.dialect.provider,
            resolved
                .session
                .external_session_id
                .as_deref()
                .unwrap_or_default(),
            resolved.source_id,
            provider_event_identity_index,
            provider_event_sequence_index,
            &event_hash,
            None,
            released.as_ref().map(|(ordinal, _)| *ordinal),
            released.is_some()
                || resolved.session.id
                    == crate::provider::importer::provider_session_uuid(
                        context.dialect.provider,
                        resolved
                            .session
                            .external_session_id
                            .as_deref()
                            .unwrap_or_default(),
                    ),
        )?;
        let dedupe_key =
            Store::provider_event_dedupe_key_with_payload_hash(&identity.dedupe_key, &event_hash)
                .unwrap_or(identity.dedupe_key);
        let occurred_at = event
            .occurred_at_millis
            .and_then(DateTime::<Utc>::from_timestamp_millis)
            .unwrap_or(resolved.session.started_at);
        let run = task_json_command_run(
            context,
            resolved,
            event,
            provider_event_identity_index,
            &event_hash,
            occurred_at,
            identity.run_source_id,
        )?;
        if let Some(run) = &run {
            group.upsert_run(run)?;
        }
        let normalized = Event {
            id: identity.id,
            seq: identity.seq,
            history_record_id: context.options.history_record_id,
            session_id: Some(resolved.session.id),
            run_id: run.as_ref().map(|run| run.id),
            event_type: event_type(event.kind),
            role: Some(event_role(event.role)),
            occurred_at,
            capture_source_id: Some(resolved.source_id),
            payload: json!({
                "provider": context.dialect.provider.as_str(),
                "provider_session_id": resolved.session.external_session_id,
                "provider_event_index": provider_event_sequence_index,
                "provider_event_identity_index": provider_event_identity_index,
                "source_generation": generation,
                "provider_event_hash": event_hash,
                "native_component": component_name(event.native_order.component),
                "native_item_index": event.native_order.item_index,
                "native_sub_index": event.native_order.sub_index,
                "body": event.body,
                "preview": event.preview,
                "result_outcome": event.sparse_output.as_ref().map(|output| {
                    format!("{:?}", output.outcome).to_lowercase()
                }),
                "exit_code": event.sparse_output.as_ref().and_then(|output| output.exit_code),
                "duration_ms": event.sparse_output.as_ref().and_then(|output| output.duration_ms),
                "output_bytes": event.sparse_output.as_ref().map(|output| output.output_bytes),
                "output_preview": event.sparse_output.as_ref().and_then(|output| output.preview.clone()),
                "call_id": event.sparse_output.as_ref().and_then(|output| output.call_id.clone()),
                "tool_call": event.tool_call.as_ref().map(|tool| json!({
                    "call_id": tool.call_id,
                    "name": tool.name,
                })),
                "result": event.sparse_output.as_ref().map(|output| json!({
                    "outcome": format!("{:?}", output.outcome).to_lowercase(),
                    "exit_code": output.exit_code,
                    "duration_ms": output.duration_ms,
                    "output_bytes": output.output_bytes,
                    "preview": output.preview,
                    "call_id": output.call_id,
                })),
                "artifacts": [],
            }),
            payload_blob_id: None,
            dedupe_key: Some(dedupe_key),
            sync: provider_sync_metadata(
                Fidelity::Imported,
                json!({
                    "provider_session_id": resolved.session.external_session_id,
                    "provider_event_index": provider_event_sequence_index,
                    "provider_event_identity_index": provider_event_identity_index,
                    "source_generation": generation,
                    "provider_event_hash": event_hash,
                    "provider_event_hash_authority": "normalized_payload_fallback",
                    "source_format": context.dialect.source_format,
                    "source_trust": "provider_native",
                    "source_record_ordinal": event.native_order.item_index,
                    "source_record_subrecord_index": event.native_order.sub_index,
                    "native_component": component_name(event.native_order.component),
                }),
            ),
        };
        let changed = if let Some((_, legacy_hash)) = released.as_ref() {
            group.reconcile_provider_event_migrating_exact_legacy_provider_hash(
                &normalized,
                legacy_hash,
            )?
        } else {
            group.reconcile_provider_event(
                &normalized,
                ProviderEventHashAuthority::NormalizedPayloadFallback,
            )?
        };
        if changed {
            summary.imported_events = summary.imported_events.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        } else {
            summary.skipped_events = summary.skipped_events.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
        }
        for (touch_ordinal, touch) in event.file_touches.iter().enumerate() {
            let touch_ordinal = u64::try_from(touch_ordinal)
                .map_err(|_| ClineNativeVerticalError::EventIndexOverflow)?;
            let provider_touch_index =
                provider_local_touch_identity_index(provider_event_identity_index, touch_ordinal);
            let provider_session_id = resolved
                .session
                .external_session_id
                .as_deref()
                .unwrap_or_default();
            let id = provider_file_touch_import_id(
                committed_store,
                context.dialect.provider,
                provider_session_id,
                resolved.source_id,
                Some(provider_event_identity_index),
                provider_touch_index,
                resolved.session.id
                    == crate::provider::importer::provider_session_uuid(
                        context.dialect.provider,
                        provider_session_id,
                    ),
            )?;
            group.upsert_file_touched(&FileTouched {
                id,
                history_record_id: context.options.history_record_id,
                run_id: run.as_ref().map(|run| run.id),
                event_id: Some(normalized.id),
                vcs_workspace_id: None,
                path: touch.path.to_string(),
                change_kind: touch.change_kind,
                old_path: touch.old_path.as_deref().map(str::to_owned),
                line_count_delta: None,
                confidence: touch.confidence,
                timestamps: timestamps(occurred_at),
                source_id: Some(resolved.source_id),
                sync: provider_sync_metadata(
                    Fidelity::Imported,
                    json!({
                        "provider": context.dialect.provider.as_str(),
                        "provider_session_id": provider_session_id,
                        "provider_touch_index": provider_touch_index,
                        "provider_event_index": provider_event_sequence_index,
                        "provider_event_identity_index": provider_event_identity_index,
                        "source_generation": generation,
                        "source_format": context.dialect.source_format,
                        "session_id": resolved.session.id,
                        "metadata": touch.metadata,
                    }),
                ),
            })?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn task_json_command_run(
    context: &ClineFreshPublicationContext<'_>,
    resolved: &ResolvedClineSource,
    event: &ClineEventRow,
    provider_event_identity_index: u64,
    event_hash: &str,
    occurred_at: DateTime<Utc>,
    run_source_id: Option<Uuid>,
) -> std::result::Result<Option<Run>, ClineNativeVerticalError> {
    if event.kind != ClineEventKind::CommandOutput {
        return Ok(None);
    }
    let diagnostic = event
        .sparse_output
        .as_ref()
        .ok_or(CaptureError::SystemInvariant(
            "task-JSON command output has no sparse diagnostic",
        ))?;
    let provider_session_id = resolved
        .session
        .external_session_id
        .as_deref()
        .unwrap_or_default();
    let stable_event_key = provider_event_identity_index.to_string();
    let run_key = diagnostic.call_id.as_deref().unwrap_or(&stable_event_key);
    let id = run_source_id.map_or_else(
        || {
            stable_capture_uuid(
                &format!(
                    "provider:{}:{provider_session_id}:run:{run_key}",
                    context.dialect.provider.as_str()
                ),
                "run",
            )
        },
        |run_source_id| {
            stable_capture_uuid(
                &format!("provider-source:{run_source_id}:run:{run_key}"),
                "run",
            )
        },
    );
    let started_at = match diagnostic.duration_ms {
        Some(duration_ms) => {
            let duration_ms = i64::try_from(duration_ms).map_err(|_| {
                CaptureError::InvalidPayload(format!(
                    "duration_ms is not representable as milliseconds: {duration_ms}"
                ))
            })?;
            let duration = chrono::Duration::try_milliseconds(duration_ms).ok_or_else(|| {
                CaptureError::InvalidPayload(format!(
                    "duration_ms is not representable as milliseconds: {duration_ms}"
                ))
            })?;
            occurred_at.checked_sub_signed(duration).ok_or_else(|| {
                CaptureError::InvalidPayload(format!(
                    "duration_ms moves command start before representable time: {duration_ms}"
                ))
            })?
        }
        None => occurred_at,
    };
    Ok(Some(Run {
        id,
        history_record_id: context.options.history_record_id,
        session_id: Some(resolved.session.id),
        run_type: RunType::Command,
        status: match diagnostic.outcome {
            OutputOutcome::Success => RunStatus::Succeeded,
            OutputOutcome::Failure => RunStatus::Failed,
            OutputOutcome::Timeout => RunStatus::Cancelled,
            OutputOutcome::Unknown => RunStatus::Partial,
        },
        started_at,
        ended_at: Some(occurred_at),
        exit_code: diagnostic.exit_code,
        cwd: None,
        command_preview: None,
        input_blob_id: None,
        output_blob_id: None,
        timestamps: timestamps(occurred_at),
        source_id: Some(resolved.source_id),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": provider_session_id,
                "provider_event_identity_index": provider_event_identity_index,
                "provider_event_hash": event_hash,
                "call_id": diagnostic.call_id,
                "source": "provider_command_output",
            }),
        ),
    }))
}

pub(super) fn component_sync_cursor(
    context: &ClineFreshPublicationContext<'_>,
    stream: &str,
    page: &NativeIngestionPage<ClineCertifiedPage>,
    generation: u64,
    prior_rejected_records: u64,
) -> std::result::Result<SyncCursor, ClineNativeVerticalError> {
    let rejected_records = prior_rejected_records
        .saturating_add(u64::try_from(page.core.core.rejections.len()).unwrap_or(u64::MAX));
    let cursor = ClineNativeStoreCursor {
        version: ClineNativeStoreCursor::VERSION,
        provider: context.dialect.provider.as_str().to_owned(),
        source_identity: page.core.source.stable_id.to_string(),
        source_revision: revision(&page.core.source_revision.revision_sha256),
        frontier: page.core.next_safe_frontier.clone(),
        terminal: page.terminal,
        generation,
        rejected_records,
        task_identity: Some(page.core.source.task.as_str().to_owned()),
        task_identity_origin: Some(match page.core.source.task_origin {
            ClineTaskIdentityOrigin::TaskMetadata => 0,
            ClineTaskIdentityOrigin::DirectoryNameDegraded => 1,
        }),
        task_identity_aliases: page
            .core
            .source
            .task_aliases
            .iter()
            .map(|alias| alias.as_str().to_owned())
            .collect(),
    }
    .encode()
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    Ok(SyncCursor {
        id: Uuid::new_v4(),
        team_id: None,
        device_id: context.options.machine_id.clone(),
        stream: stream.to_owned(),
        cursor,
        last_synced_at: Some(context.options.imported_at),
        timestamps: timestamps(context.options.imported_at),
    })
}
