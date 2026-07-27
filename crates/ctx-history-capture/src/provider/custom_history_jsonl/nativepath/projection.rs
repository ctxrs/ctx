use super::*;

pub(super) fn build_canonical_history(
    store: &Store,
    context: &ProviderAdapterContext,
    options: &CustomHistoryJsonlV1ImportOptions,
    logical_locator: &str,
    parsed: &ParsedCustomHistory,
    summary: &mut ProviderImportSummary,
) -> Result<CanonicalCustomHistory> {
    let physical_anchor =
        nativepath_anchor_source(context, logical_locator, &parsed.source_revision);
    let ordered_session_keys = ordered_sessions(&parsed.sessions, summary);
    let mut sessions = BTreeMap::new();
    let mut session_units = Vec::new();
    for key in ordered_session_keys {
        let (line, record) = &parsed.sessions[&key];
        let source = &parsed.sources[&record.source_id].1;
        let unit =
            canonical_session_unit(context, options, *line, physical_anchor.id, source, record);
        sessions.insert(key, unit.session.clone());
        session_units.push(CoreUnit::Session(unit));
    }
    let mut event_units = Vec::new();
    let mut event_ids = BTreeMap::<(String, String, u64), Uuid>::new();
    for (line, record) in &parsed.events {
        let source = &parsed.sources[&record.source_id].1;
        let Some(session) = sessions.get(&(record.source_id.clone(), record.session_id.clone()))
        else {
            continue;
        };
        let provider_session_id =
            session
                .external_session_id
                .as_deref()
                .ok_or(CaptureError::SystemInvariant(
                    "custom history canonical session has no external ID",
                ))?;
        let capture_source_id = session
            .capture_source_id
            .ok_or(CaptureError::SystemInvariant(
                "custom history canonical session has no capture source",
            ))?;
        let identity_source_id = provider_scoped_source_uuid(
            CaptureProvider::Custom,
            provider_session_id,
            &source.source_format,
            custom_history_effective_raw_source_path(source, context).as_deref(),
        );
        let payload_hash = match &record.event_hash {
            Some(hash) => hash.clone(),
            None => compute_payload_hash(&record.payload)?,
        };
        let authority = if record.event_hash.is_some() {
            ProviderEventHashAuthority::ProviderSupplied
        } else {
            ProviderEventHashAuthority::NormalizedPayloadFallback
        };
        let identity = provider_event_import_identity_with_exact_legacy_source(
            store,
            CaptureProvider::Custom,
            provider_session_id,
            identity_source_id,
            record.event_index,
            record.event_index,
            &payload_hash,
            None,
            None,
            true,
        )?;
        let (event, run) = custom_history_canonical_event(
            provider_session_id,
            source,
            record,
            source.observed_at.unwrap_or(context.imported_at),
            options.history_record_id,
            capture_source_id,
            session.id,
            *line,
            &payload_hash,
            authority,
            &identity,
        )?;
        event_ids.insert(
            (
                record.source_id.clone(),
                record.session_id.clone(),
                record.event_index,
            ),
            event.id,
        );
        event_units.push(CoreUnit::Event(EventUnit {
            event,
            run,
            authority,
        }));
    }

    let mut touch_units = Vec::new();
    for (_, record) in &parsed.file_touches {
        let source = &parsed.sources[&record.source_id].1;
        let Some(session) = sessions.get(&(record.source_id.clone(), record.session_id.clone()))
        else {
            continue;
        };
        let capture_source_id = session
            .capture_source_id
            .ok_or(CaptureError::SystemInvariant(
                "custom history file touch session has no capture source",
            ))?;
        let provider_session_id =
            session
                .external_session_id
                .as_deref()
                .ok_or(CaptureError::SystemInvariant(
                    "custom history file touch session has no external ID",
                ))?;
        let event_id = record.event_index.and_then(|index| {
            event_ids
                .get(&(record.source_id.clone(), record.session_id.clone(), index))
                .copied()
        });
        let identity_source_id = provider_scoped_source_uuid(
            CaptureProvider::Custom,
            provider_session_id,
            &source.source_format,
            custom_history_effective_raw_source_path(source, context).as_deref(),
        );
        let touch_id = provider_file_touch_import_id(
            store,
            CaptureProvider::Custom,
            provider_session_id,
            identity_source_id,
            record.event_index,
            record.touch_index,
            true,
        )?;
        let file = custom_history_canonical_file_touch(
            context,
            source,
            record,
            provider_session_id,
            options.history_record_id,
            capture_source_id,
            session.id,
            event_id,
            touch_id,
        );
        touch_units.push(CoreUnit::FileTouch(FileTouchUnit { file }));
    }

    let mut edge_units = BTreeMap::<Uuid, EdgeUnit>::new();
    for ((source_id, _), session) in &sessions {
        let Some(parent_id) = session.parent_session_id else {
            continue;
        };
        let provider_session_id =
            session
                .external_session_id
                .as_deref()
                .ok_or(CaptureError::SystemInvariant(
                    "custom history child session has no external ID",
                ))?;
        let source = &parsed.sources[source_id].1;
        let edge = SessionEdge {
            id: provider_edge_uuid(CaptureProvider::Custom, provider_session_id, "parent_child"),
            from_session_id: parent_id,
            to_session_id: session.id,
            edge_type: SessionEdgeType::ParentChild,
            confidence: ctx_history_core::Confidence::Explicit,
            source_id: session.capture_source_id,
            timestamps: timestamps(source.observed_at.unwrap_or(context.imported_at)),
            sync: provider_sync_metadata(
                session.sync.fidelity,
                json!({
                    "provider_session_id": provider_session_id,
                    "parent_provider_session_id": session
                        .sync
                        .metadata
                        .get("parent_provider_session_id"),
                    "source_format": source.source_format,
                    "fixture_line": parsed.sessions[&(source_id.clone(), source_session_id(session)?)]
                        .0,
                    "imported_at": source.observed_at.unwrap_or(context.imported_at),
                }),
            ),
        };
        let parent = sessions
            .values()
            .find(|candidate| candidate.id == parent_id)
            .ok_or(CaptureError::SystemInvariant(
                "custom history parent session vanished after validation",
            ))?;
        edge_units.insert(
            edge.id,
            EdgeUnit {
                actor: canonical_actor(parent),
                edge,
            },
        );
    }
    for (_, record) in &parsed.edges {
        let source = &parsed.sources[&record.source_id].1;
        let (Some(from), Some(to)) = (
            sessions.get(&(record.source_id.clone(), record.from_session_id.clone())),
            sessions.get(&(record.source_id.clone(), record.to_session_id.clone())),
        ) else {
            continue;
        };
        let from_provider_session_id =
            from.external_session_id
                .as_deref()
                .ok_or(CaptureError::SystemInvariant(
                    "custom history edge source session has no external ID",
                ))?;
        let to_provider_session_id =
            to.external_session_id
                .as_deref()
                .ok_or(CaptureError::SystemInvariant(
                    "custom history edge target session has no external ID",
                ))?;
        let edge_id = if record.edge_type == SessionEdgeType::ParentChild {
            provider_edge_uuid(
                CaptureProvider::Custom,
                to_provider_session_id,
                "parent_child",
            )
        } else {
            let key = custom_history_key(json!({
                "schema": CTX_HISTORY_JSONL_V1_SCHEMA_VERSION,
                "kind": "session_edge",
                "provider_key": source.provider_key,
                "source_id": source.source_id,
                "from_provider_session_id": from_provider_session_id,
                "to_provider_session_id": to_provider_session_id,
                "edge_type": record.edge_type.as_str(),
                "edge_id": record.edge_id,
            }));
            stable_capture_uuid(&key, "session-edge")
        };
        let edge = SessionEdge {
            id: edge_id,
            from_session_id: from.id,
            to_session_id: to.id,
            edge_type: record.edge_type,
            confidence: record.confidence,
            source_id: to.capture_source_id,
            timestamps: timestamps(record.occurred_at.unwrap_or(context.imported_at)),
            sync: provider_sync_metadata(
                record.fidelity,
                json!({
                    "provider_key": source.provider_key,
                    "source_id": source.source_id,
                    "history_record_id": options.history_record_id,
                    "metadata": custom_history_metadata(
                        record.metadata.clone(),
                        json!({
                            "provider_key": source.provider_key,
                            "source_id": record.source_id,
                            "from_session_id": record.from_session_id,
                            "to_session_id": record.to_session_id,
                            "edge_id": record.edge_id,
                        }),
                    ),
                }),
            ),
        };
        if edge_units.contains_key(&edge_id) {
            summary.skipped_edges = summary.skipped_edges.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
            summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
            continue;
        }
        edge_units.insert(
            edge_id,
            EdgeUnit {
                actor: canonical_actor(from),
                edge,
            },
        );
    }

    let mut units = session_units;
    units.extend(event_units);
    units.extend(touch_units);
    units.extend(edge_units.into_values().map(CoreUnit::Edge));
    let anchor_source = (!units.is_empty()).then_some(physical_anchor);
    Ok(CanonicalCustomHistory {
        units,
        anchor_source,
        sessions,
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn custom_history_canonical_event(
    provider_session_id: &str,
    source: &CtxHistoryJsonlSourceRecord,
    record: &CtxHistoryJsonlEventRecord,
    imported_at: DateTime<Utc>,
    history_record_id: Option<Uuid>,
    capture_source_id: Uuid,
    session_id: Uuid,
    line_number: usize,
    payload_hash: &str,
    hash_authority: ProviderEventHashAuthority,
    identity: &ProviderEventImportIdentity,
) -> Result<(Event, Option<Run>)> {
    let payload = record.payload.clone();
    let mut provider_metadata = custom_history_metadata(
        record.metadata.clone(),
        json!({
            "provider_key": source.provider_key,
            "source_id": record.source_id,
            "session_id": record.session_id,
            "event_id": record.event_id,
            "native_cursor": record.native_cursor,
            "preview": record.preview,
        }),
    );
    let source_record_coordinates = take_custom_source_record_coordinates(&mut provider_metadata)?;
    let verified_content_locators = take_custom_verified_content_locators(&mut provider_metadata)?;
    let run = custom_history_command_run(
        provider_session_id,
        session_id,
        capture_source_id,
        identity.run_source_id,
        history_record_id,
        record,
        &payload,
        payload_hash,
    )?;
    let dedupe_key =
        Store::provider_event_dedupe_key_with_payload_hash(&identity.dedupe_key, payload_hash)
            .unwrap_or_else(|| identity.dedupe_key.clone());
    let mut sync_metadata = json!({
        "provider_session_id": provider_session_id,
        "provider_event_index": record.event_index,
        "provider_event_hash": payload_hash,
        "provider_event_hash_authority": hash_authority.as_str(),
        "cursor": record.native_cursor,
        "source_format": source.source_format,
        "source_trust": effective_trust(source.trust),
        "fixture_line": line_number,
        "imported_at": imported_at,
        "event_idempotency_key": record.idempotency_key,
        "source_record_ordinal": source_record_coordinates
            .as_ref()
            .map(|coordinates| coordinates.0),
        "source_record_subrecord_index": source_record_coordinates
            .as_ref()
            .map(|coordinates| coordinates.1),
        "metadata": provider_metadata,
    });
    if let Some(locators) = verified_content_locators {
        if let Some(metadata) = sync_metadata.as_object_mut() {
            metadata.insert(VERIFIED_CONTENT_LOCATORS_METADATA_KEY.to_owned(), locators);
        }
    }
    Ok((
        Event {
            id: identity.id,
            seq: identity.seq,
            history_record_id,
            session_id: Some(session_id),
            run_id: run.as_ref().map(|run| run.id),
            event_type: record.event_type,
            role: record.role,
            occurred_at: record.occurred_at,
            capture_source_id: Some(capture_source_id),
            payload: json!({
                "provider": CaptureProvider::Custom.as_str(),
                "provider_session_id": provider_session_id,
                "provider_event_index": record.event_index,
                "provider_event_hash": payload_hash,
                "cursor": record.native_cursor,
                "artifacts": record.artifacts,
                "body": compact_provider_result_payload(record.event_type, &payload),
            }),
            payload_blob_id: None,
            dedupe_key: Some(dedupe_key),
            sync: provider_sync_metadata(record.fidelity, sync_metadata),
        },
        run,
    ))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn custom_history_command_run(
    provider_session_id: &str,
    session_id: Uuid,
    source_id: Uuid,
    run_source_id: Option<Uuid>,
    history_record_id: Option<Uuid>,
    record: &CtxHistoryJsonlEventRecord,
    payload: &Value,
    event_hash: &str,
) -> Result<Option<Run>> {
    if record.event_type != EventType::CommandOutput {
        return Ok(None);
    }
    let arguments_preview = payload.get("arguments_preview");
    let command_preview = payload
        .get("command")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| arguments_preview.and_then(crate::provider::tool_input::command));
    let cwd = payload
        .get("workdir")
        .or_else(|| payload.get("cwd"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| arguments_preview.and_then(crate::provider::tool_input::working_directory));
    let call_id = payload.get("call_id").and_then(Value::as_str);
    let key = call_id.unwrap_or(event_hash);
    let started_at = custom_command_started_at(record.occurred_at, payload)?;
    let id = run_source_id.map_or_else(
        || {
            stable_capture_uuid(
                &format!(
                    "provider:{}:{provider_session_id}:run:{key}",
                    CaptureProvider::Custom.as_str()
                ),
                "run",
            )
        },
        |run_source_id| {
            stable_capture_uuid(&format!("provider-source:{run_source_id}:run:{key}"), "run")
        },
    );
    Ok(Some(Run {
        id,
        history_record_id,
        session_id: Some(session_id),
        run_type: RunType::Command,
        status: custom_command_run_status(payload),
        started_at,
        ended_at: Some(record.occurred_at),
        exit_code: payload
            .get("exit_code")
            .and_then(Value::as_i64)
            .and_then(|value| i32::try_from(value).ok()),
        cwd,
        command_preview,
        input_blob_id: None,
        output_blob_id: None,
        timestamps: timestamps(record.occurred_at),
        source_id: Some(source_id),
        sync: provider_sync_metadata(
            record.fidelity,
            json!({
                "provider_session_id": provider_session_id,
                "provider_event_index": record.event_index,
                "provider_event_hash": event_hash,
                "call_id": call_id,
                "source": "provider_command_output",
            }),
        ),
    }))
}

pub(super) fn custom_command_started_at(
    occurred_at: DateTime<Utc>,
    payload: &Value,
) -> Result<DateTime<Utc>> {
    let Some(value) = payload.get("duration_ms") else {
        return Ok(occurred_at);
    };
    if value.is_null() {
        return Ok(occurred_at);
    }
    let duration = value
        .as_i64()
        .ok_or_else(|| CaptureError::InvalidPayload("duration_ms must be an integer".to_owned()))?;
    if duration < 0 {
        return Err(CaptureError::InvalidPayload(format!(
            "duration_ms must be nonnegative, got {duration}"
        )));
    }
    let span = chrono::Duration::try_milliseconds(duration).ok_or_else(|| {
        CaptureError::InvalidPayload(format!(
            "duration_ms is not representable as milliseconds: {duration}"
        ))
    })?;
    occurred_at.checked_sub_signed(span).ok_or_else(|| {
        CaptureError::InvalidPayload(format!(
            "duration_ms moves command start before representable time: {duration}"
        ))
    })
}

pub(super) fn custom_command_run_status(payload: &Value) -> RunStatus {
    if payload
        .get("timed_out")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return RunStatus::Cancelled;
    }
    match payload.get("exit_code").and_then(Value::as_i64) {
        Some(0) => RunStatus::Succeeded,
        Some(_) => RunStatus::Failed,
        None => {
            let outcome = payload
                .get("result_outcome")
                .or_else(|| payload.get("outcome"))
                .or_else(|| payload.get("status"))
                .and_then(Value::as_str)
                .map(str::trim)
                .map(str::to_ascii_lowercase);
            match outcome.as_deref() {
                Some("timeout" | "timed_out" | "timedout" | "cancelled" | "canceled") => {
                    RunStatus::Cancelled
                }
                Some("failure" | "failed" | "error" | "errored") => RunStatus::Failed,
                Some("success" | "succeeded" | "complete" | "completed" | "ok" | "passed") => {
                    RunStatus::Succeeded
                }
                _ => RunStatus::Partial,
            }
        }
    }
}

pub(super) fn take_custom_verified_content_locators(metadata: &mut Value) -> Result<Option<Value>> {
    let Some(object) = metadata.as_object_mut() else {
        return Ok(None);
    };
    let Some(value) = object.remove(VERIFIED_CONTENT_LOCATORS_METADATA_KEY) else {
        return Ok(None);
    };
    let locators = VerifiedContentLocatorsV1::from_metadata_value(&value).ok_or_else(|| {
        CaptureError::InvalidPayload("verified content locator annotation is malformed".to_owned())
    })?;
    Ok(Some(locators.to_metadata_value()))
}

pub(super) fn take_custom_source_record_coordinates(
    metadata: &mut Value,
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

#[allow(clippy::too_many_arguments)]
pub(super) fn custom_history_canonical_file_touch(
    context: &ProviderAdapterContext,
    source: &CtxHistoryJsonlSourceRecord,
    record: &CtxHistoryJsonlFileTouchRecord,
    provider_session_id: &str,
    history_record_id: Option<Uuid>,
    source_id: Uuid,
    session_id: Uuid,
    event_id: Option<Uuid>,
    touch_id: Uuid,
) -> FileTouched {
    let raw_source_path = custom_history_effective_raw_source_path(source, context);
    let declared_source_root = context
        .source_root_display()
        .or_else(|| source.raw_source_path.clone())
        .or_else(|| source.raw_uri.clone());
    let source_root =
        provider_source_root(declared_source_root.as_deref(), raw_source_path.as_deref());
    FileTouched {
        id: touch_id,
        history_record_id,
        run_id: None,
        event_id,
        vcs_workspace_id: None,
        path: record.path.clone(),
        change_kind: record.change_kind,
        old_path: record.old_path.clone(),
        line_count_delta: record.line_count_delta,
        confidence: record.confidence,
        timestamps: timestamps(record.occurred_at),
        source_id: Some(source_id),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider": CaptureProvider::Custom.as_str(),
                "provider_session_id": provider_session_id,
                "provider_touch_index": record.touch_index,
                "provider_event_index": record.event_index,
                "raw_source_path": raw_source_path,
                "source_id": source_id,
                "source_format": source.source_format,
                "source_root": source_root,
                "metadata": custom_history_metadata(
                    record.metadata.clone(),
                    json!({
                        "provider_key": source.provider_key,
                        "source_id": record.source_id,
                        "session_id": record.session_id,
                    }),
                ),
                "session_id": session_id,
            }),
        ),
    }
}

pub(super) fn nativepath_anchor_source(
    context: &ProviderAdapterContext,
    logical_locator: &str,
    source_revision: &str,
) -> CaptureSource {
    let canonical_source_identity = canonical_route_identity(logical_locator);
    CaptureSource {
        id: stable_capture_uuid(
            &format!("custom-history-nativepath:{logical_locator}"),
            "source",
        ),
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Custom,
            machine_id: context.machine_id.clone(),
            process_id: None,
            cwd: None,
            raw_source_path: context
                .source_path
                .as_ref()
                .map(|path| path.display().to_string()),
            source_format: Some(CUSTOM_ROUTE_SOURCE_FORMAT.to_owned()),
            source_root: None,
            source_identity: Some(canonical_source_identity.clone()),
            external_session_id: None,
        },
        started_at: context.imported_at,
        ended_at: None,
        sync: provider_sync_metadata(
            Fidelity::Partial,
            json!({
                "source_format": CUSTOM_ROUTE_SOURCE_FORMAT,
                "source_trust": "provider_export",
                "imported_at": context.imported_at,
                "source_identity": canonical_source_identity,
                "source_revision": source_revision,
                "nativepath_publication": CUSTOM_PARSER_REVISION,
                "physical_jsonl_anchor": true,
            }),
        ),
    }
}

pub(super) fn canonical_session_unit(
    context: &ProviderAdapterContext,
    options: &CustomHistoryJsonlV1ImportOptions,
    line: usize,
    capture_source_id: Uuid,
    source: &CtxHistoryJsonlSourceRecord,
    record: &CtxHistoryJsonlSessionRecord,
) -> SessionUnit {
    let provider_session_id = custom_history_internal_session_id(
        &source.provider_key,
        &source.source_id,
        &record.session_id,
    );
    let raw_source_path = custom_history_effective_raw_source_path(source, context);
    let source_root = context
        .source_root_display()
        .or_else(|| source.raw_source_path.clone())
        .or_else(|| source.raw_uri.clone());
    let source_metadata = custom_history_metadata(
        source.metadata.clone(),
        json!({
            "provider_key": source.provider_key,
            "source_id": source.source_id,
            "source_format": source.source_format,
            "raw_uri": source.raw_uri,
            "raw_source_path": source.raw_source_path,
            "fingerprint": source.fingerprint,
            "importer_version": source.importer_version,
            "cursor": source.cursor,
        }),
    );
    let source_identity = provider_source_identity(
        CaptureProvider::Custom,
        &source.source_format,
        source_root.as_deref(),
        raw_source_path.as_deref(),
        Some(&format!(
            "ctx-history-jsonl-v1:{}:{}",
            source.provider_key, source.source_id
        )),
        &source_metadata,
    );
    let semantic_source_id = provider_scoped_source_uuid(
        CaptureProvider::Custom,
        &provider_session_id,
        &source.source_format,
        raw_source_path.as_deref(),
    );
    let imported_at = source.observed_at.unwrap_or(context.imported_at);
    let parent_session_id = record.parent_session_id.as_ref().map(|parent| {
        provider_session_uuid(
            CaptureProvider::Custom,
            &custom_history_internal_session_id(&source.provider_key, &source.source_id, parent),
        )
    });
    let root_session_id = record
        .root_session_id
        .as_ref()
        .map(|root| {
            provider_session_uuid(
                CaptureProvider::Custom,
                &custom_history_internal_session_id(&source.provider_key, &source.source_id, root),
            )
        })
        .or(parent_session_id);
    let session_metadata = custom_history_metadata(
        record.metadata.clone(),
        json!({
            "provider_key": source.provider_key,
            "source_id": source.source_id,
            "session_id": record.session_id,
            "native_session_id": record.native_session_id,
            "parent_session_id": record.parent_session_id,
            "root_session_id": record.root_session_id,
        }),
    );
    let session = Session {
        id: provider_session_uuid(CaptureProvider::Custom, &provider_session_id),
        history_record_id: options.history_record_id,
        parent_session_id,
        root_session_id,
        capture_source_id: Some(capture_source_id),
        provider: CaptureProvider::Custom,
        external_session_id: Some(provider_session_id.clone()),
        external_agent_id: record.external_agent_id.clone(),
        agent_type: record.agent_type,
        role_hint: record.role_hint.clone(),
        is_primary: record.is_primary,
        status: record.status,
        transcript_blob_id: None,
        started_at: record.started_at,
        ended_at: record.ended_at,
        timestamps: timestamps(imported_at),
        sync: provider_sync_metadata(
            record.fidelity,
            json!({
                "provider_session_id": provider_session_id,
                "parent_provider_session_id": record.parent_session_id.as_ref().map(|parent| {
                    custom_history_internal_session_id(
                        &source.provider_key,
                        &source.source_id,
                        parent,
                    )
                }),
                "root_provider_session_id": record.root_session_id.as_ref().map(|root| {
                    custom_history_internal_session_id(
                        &source.provider_key,
                        &source.source_id,
                        root,
                    )
                }),
                "source_format": source.source_format,
                "source_trust": effective_trust(source.trust),
                "source_cursor": source.cursor,
                "source_identity": source_identity,
                "source_root": source_root,
                "source_identity_key": provider_scoped_source_identity_key(
                    CaptureProvider::Custom,
                    &provider_session_id,
                    &source.source_format,
                    raw_source_path.as_deref(),
                ),
                "source_metadata": source_metadata,
                "semantic_capture_source_id": semantic_source_id,
                "fixture_line": line,
                "imported_at": imported_at,
                "session_idempotency_key": record.idempotency_key.clone().or_else(|| Some(format!(
                    "ctx-history-jsonl-v1:{}:{}:{}",
                    source.provider_key, source.source_id, record.session_id
                ))),
                "artifacts": record.artifacts,
                "metadata": session_metadata,
                "nativepath_publication": CUSTOM_PARSER_REVISION,
            }),
        ),
    };
    SessionUnit { session }
}

pub(super) fn ordered_sessions(
    sessions: &BTreeMap<(String, String), (usize, CtxHistoryJsonlSessionRecord)>,
    summary: &mut ProviderImportSummary,
) -> Vec<(String, String)> {
    let mut remaining = sessions.keys().cloned().collect::<BTreeSet<_>>();
    let mut emitted = BTreeSet::new();
    let mut ordered = Vec::new();
    loop {
        let ready = remaining
            .iter()
            .filter(|key| {
                let session = &sessions[*key].1;
                [
                    session.parent_session_id.as_ref(),
                    session.root_session_id.as_ref(),
                ]
                .into_iter()
                .flatten()
                .all(|dependency| {
                    dependency == &session.session_id
                        || emitted.contains(&(session.source_id.clone(), dependency.clone()))
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        if ready.is_empty() {
            break;
        }
        for key in ready {
            remaining.remove(&key);
            emitted.insert(key.clone());
            ordered.push(key);
        }
    }
    for key in remaining {
        let line = sessions[&key].0;
        push_provider_import_failure(
            summary,
            line,
            format!(
                "session `{}` in source `{}` has a cyclic parent/root relationship",
                key.1, key.0
            ),
        );
    }
    ordered
}

pub(super) fn canonical_actor(session: &Session) -> CanonicalActor {
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

pub(super) fn source_session_id(session: &Session) -> Result<String> {
    session
        .sync
        .metadata
        .pointer("/metadata/ctx_history_jsonl_v1/session_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or(CaptureError::SystemInvariant(
            "custom history session metadata lost native session ID",
        ))
}

pub(super) fn effective_trust(trust: ProviderSourceTrust) -> ProviderSourceTrust {
    match trust {
        ProviderSourceTrust::Unknown => ProviderSourceTrust::ProviderExport,
        other => other,
    }
}

pub(super) fn custom_outputs(
    parsed: &ParsedCustomHistory,
    canonical_sessions: &BTreeMap<(String, String), Session>,
) -> Result<Vec<CustomOutput>> {
    let mut outputs = Vec::new();
    for (_, event) in &parsed.events {
        if !matches!(
            event.event_type,
            EventType::ToolOutput | EventType::CommandOutput
        ) {
            continue;
        }
        if !canonical_sessions.contains_key(&(event.source_id.clone(), event.session_id.clone())) {
            continue;
        }
        let session = &parsed.sessions[&(event.source_id.clone(), event.session_id.clone())].1;
        let source = &parsed.sources[&event.source_id].1;
        let direct_session_id = custom_history_internal_session_id(
            &source.provider_key,
            &source.source_id,
            &session.session_id,
        );
        let root_session_id = session
            .root_session_id
            .as_ref()
            .map(|root| {
                custom_history_internal_session_id(&source.provider_key, &source.source_id, root)
            })
            .unwrap_or_else(|| direct_session_id.clone());
        let parent_session_id = session.parent_session_id.as_ref().map(|parent| {
            custom_history_internal_session_id(&source.provider_key, &source.source_id, parent)
        });
        outputs.push(CustomOutput {
            source_id: event.source_id.clone(),
            session_id: direct_session_id,
            event_index: event.event_index,
            event_id: event.event_id.clone(),
            event_hash: event
                .event_hash
                .clone()
                .unwrap_or(compute_payload_hash(&event.payload)?),
            event_type: event.event_type,
            occurred_at: event.occurred_at,
            parent_session_id,
            root_session_id,
            external_agent_id: session.external_agent_id.clone(),
            payload: event.payload.clone(),
        });
    }
    Ok(outputs)
}
