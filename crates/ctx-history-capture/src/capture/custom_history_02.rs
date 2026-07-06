#[allow(unused_imports)]
use super::*;

pub(crate) fn validate_custom_history_references(
    summary: &mut ProviderImportSummary,
    references: CustomHistoryReferenceIndex<'_>,
) {
    if references.manifest_line.is_none() {
        push_provider_import_failure(
            summary,
            0,
            "missing manifest record for ctx-history-jsonl-v1".to_owned(),
        );
    }

    for (line_number, session) in references.sessions.values() {
        if !references.sources.contains_key(&session.source_id) {
            push_provider_import_failure(
                summary,
                *line_number,
                format!(
                    "session references unknown source_id `{}`",
                    session.source_id
                ),
            );
        }
        if let Some(parent) = &session.parent_session_id {
            let key = (session.source_id.clone(), parent.clone());
            if !references.sessions.contains_key(&key) {
                push_provider_import_failure(
                    summary,
                    *line_number,
                    format!("session references unknown parent_session_id `{parent}`"),
                );
            }
        }
        if let Some(root) = &session.root_session_id {
            let key = (session.source_id.clone(), root.clone());
            if root != &session.session_id && !references.sessions.contains_key(&key) {
                push_provider_import_failure(
                    summary,
                    *line_number,
                    format!("session references unknown root_session_id `{root}`"),
                );
            }
        }
    }

    for (line_number, event) in references.events {
        if !references
            .sessions
            .contains_key(&(event.source_id.clone(), event.session_id.clone()))
        {
            push_provider_import_failure(
                summary,
                *line_number,
                format!(
                    "event references unknown session `{}` in source `{}`",
                    event.session_id, event.source_id
                ),
            );
        }
    }

    for (line_number, file_touch) in references.file_touches {
        if !references
            .sessions
            .contains_key(&(file_touch.source_id.clone(), file_touch.session_id.clone()))
        {
            push_provider_import_failure(
                summary,
                *line_number,
                format!(
                    "file_touch references unknown session `{}` in source `{}`",
                    file_touch.session_id, file_touch.source_id
                ),
            );
        }
        if let Some(event_index) = file_touch.event_index {
            let key = (
                file_touch.source_id.clone(),
                file_touch.session_id.clone(),
                event_index,
            );
            if !references.event_keys.contains(&key) {
                push_provider_import_failure(
                    summary,
                    *line_number,
                    format!("file_touch references unknown event_index `{event_index}`"),
                );
            }
        }
    }

    for (line_number, edge) in references.edges {
        let from_key = (edge.source_id.clone(), edge.from_session_id.clone());
        let to_key = (edge.source_id.clone(), edge.to_session_id.clone());
        if !references.sessions.contains_key(&from_key) {
            push_provider_import_failure(
                summary,
                *line_number,
                format!(
                    "edge references unknown from_session_id `{}`",
                    edge.from_session_id
                ),
            );
        }
        if !references.sessions.contains_key(&to_key) {
            push_provider_import_failure(
                summary,
                *line_number,
                format!(
                    "edge references unknown to_session_id `{}`",
                    edge.to_session_id
                ),
            );
        }
        if edge.edge_type == SessionEdgeType::ParentChild {
            let Some((_, child)) = references.sessions.get(&to_key) else {
                continue;
            };
            if let Some(parent) = &child.parent_session_id {
                if parent != &edge.from_session_id {
                    push_provider_import_failure(
                        summary,
                        *line_number,
                        format!(
                            "parent_child edge from_session_id `{}` conflicts with session parent_session_id `{parent}`",
                            edge.from_session_id
                        ),
                    );
                }
            }
        }
    }
}

pub(crate) fn custom_history_session_capture(
    source: &CtxHistoryJsonlSourceRecord,
    session: &CtxHistoryJsonlSessionRecord,
    event: Option<ProviderEventEnvelope>,
    context: &ProviderAdapterContext,
) -> ProviderCaptureEnvelope {
    let provider_session_id = custom_history_internal_session_id(
        &source.provider_key,
        &source.source_id,
        &session.session_id,
    );
    let event_cursor = event.as_ref().and_then(|event| {
        event.cursor.as_ref().map(|cursor| ProviderCursorRange {
            before: None,
            after: Some(ProviderCursorCheckpoint {
                stream: custom_history_cursor_stream(source),
                cursor: cursor.clone(),
                observed_at: event.occurred_at,
            }),
        })
    });
    let source_cursor = source
        .cursor
        .as_ref()
        .map(|cursor| custom_history_normalized_cursor_range(source, cursor))
        .or(event_cursor);
    ProviderCaptureEnvelope {
        schema_version: PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
        provider: CaptureProvider::Custom,
        source: ProviderSourceEnvelope {
            source_format: source.source_format.clone(),
            machine_id: source
                .machine_id
                .clone()
                .unwrap_or_else(|| context.machine_id.clone()),
            observed_at: source.observed_at.unwrap_or(context.imported_at),
            raw_source_path: custom_history_effective_raw_source_path(source, context),
            raw_retention: source.raw_retention,
            redaction_boundary: source.redaction_boundary,
            trust: match source.trust {
                ProviderSourceTrust::Unknown => ProviderSourceTrust::ProviderExport,
                other => other,
            },
            fidelity: source.fidelity,
            cursor: source_cursor,
            idempotency_key: Some(format!(
                "ctx-history-jsonl-v1:{}:{}",
                source.provider_key, source.source_id
            )),
            metadata: custom_history_metadata(
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
            ),
        },
        session: ProviderSessionEnvelope {
            provider_session_id,
            parent_provider_session_id: session.parent_session_id.as_ref().map(|parent| {
                custom_history_internal_session_id(&source.provider_key, &source.source_id, parent)
            }),
            root_provider_session_id: session.root_session_id.as_ref().map(|root| {
                custom_history_internal_session_id(&source.provider_key, &source.source_id, root)
            }),
            external_agent_id: session.external_agent_id.clone(),
            agent_type: session.agent_type,
            role_hint: session.role_hint.clone(),
            is_primary: session.is_primary,
            status: session.status,
            started_at: session.started_at,
            ended_at: session.ended_at,
            cwd: session.cwd.clone(),
            fidelity: session.fidelity,
            idempotency_key: session.idempotency_key.clone().or_else(|| {
                Some(format!(
                    "ctx-history-jsonl-v1:{}:{}:{}",
                    source.provider_key, source.source_id, session.session_id
                ))
            }),
            artifacts: session.artifacts.clone(),
            metadata: custom_history_metadata(
                session.metadata.clone(),
                json!({
                    "provider_key": source.provider_key,
                    "source_id": source.source_id,
                    "session_id": session.session_id,
                    "native_session_id": session.native_session_id,
                    "parent_session_id": session.parent_session_id,
                    "root_session_id": session.root_session_id,
                }),
            ),
        },
        event,
    }
}

pub(crate) fn custom_history_event_envelope(
    source: &CtxHistoryJsonlSourceRecord,
    event: &CtxHistoryJsonlEventRecord,
) -> ProviderEventEnvelope {
    let payload = if let Some(preview) = &event.preview {
        json!({ "text": preview })
    } else {
        event.payload.clone()
    };
    let raw_payload = event
        .preview
        .as_ref()
        .map(|_| event.payload.clone())
        .filter(|payload| payload != &json!({}));
    ProviderEventEnvelope {
        provider_event_index: event.event_index,
        provider_event_hash: event.event_hash.clone(),
        cursor: event.native_cursor.clone(),
        event_type: event.event_type,
        role: event.role,
        occurred_at: event.occurred_at,
        fidelity: event.fidelity,
        redaction_state: event.redaction_state,
        idempotency_key: event.idempotency_key.clone(),
        artifacts: event.artifacts.clone(),
        payload,
        metadata: custom_history_metadata(
            event.metadata.clone(),
            json!({
                "provider_key": source.provider_key,
                "source_id": event.source_id,
                "session_id": event.session_id,
                "event_id": event.event_id,
                "native_cursor": event.native_cursor,
                "preview": event.preview,
                "raw_payload": raw_payload,
            }),
        ),
    }
}

pub(crate) fn custom_history_file_touch_envelope(
    source: &CtxHistoryJsonlSourceRecord,
    file_touch: &CtxHistoryJsonlFileTouchRecord,
    context: &ProviderAdapterContext,
) -> ProviderFileTouchedEnvelope {
    ProviderFileTouchedEnvelope {
        provider: CaptureProvider::Custom,
        provider_session_id: custom_history_internal_session_id(
            &source.provider_key,
            &source.source_id,
            &file_touch.session_id,
        ),
        provider_touch_index: file_touch.touch_index,
        provider_event_index: file_touch.event_index,
        raw_source_path: custom_history_effective_raw_source_path(source, context),
        path: file_touch.path.clone(),
        change_kind: file_touch.change_kind,
        old_path: file_touch.old_path.clone(),
        line_count_delta: file_touch.line_count_delta,
        confidence: file_touch.confidence,
        occurred_at: file_touch.occurred_at,
        source_format: source.source_format.clone(),
        metadata: custom_history_metadata(
            file_touch.metadata.clone(),
            json!({
                "provider_key": source.provider_key,
                "source_id": file_touch.source_id,
                "session_id": file_touch.session_id,
            }),
        ),
    }
}

pub(crate) fn custom_history_edge_import(
    source: &CtxHistoryJsonlSourceRecord,
    edge: &CtxHistoryJsonlEdgeRecord,
    context: &ProviderAdapterContext,
) -> CustomHistoryJsonlV1EdgeImport {
    CustomHistoryJsonlV1EdgeImport {
        provider_key: source.provider_key.clone(),
        source_id: source.source_id.clone(),
        source_format: source.source_format.clone(),
        raw_source_path: custom_history_effective_raw_source_path(source, context),
        from_provider_session_id: custom_history_internal_session_id(
            &source.provider_key,
            &source.source_id,
            &edge.from_session_id,
        ),
        to_provider_session_id: custom_history_internal_session_id(
            &source.provider_key,
            &source.source_id,
            &edge.to_session_id,
        ),
        edge_id: edge.edge_id.clone(),
        edge_type: edge.edge_type,
        confidence: edge.confidence,
        occurred_at: edge.occurred_at.unwrap_or(context.imported_at),
        fidelity: edge.fidelity,
        metadata: custom_history_metadata(
            edge.metadata.clone(),
            json!({
                "provider_key": source.provider_key,
                "source_id": edge.source_id,
                "from_session_id": edge.from_session_id,
                "to_session_id": edge.to_session_id,
                "edge_id": edge.edge_id,
            }),
        ),
    }
}

pub(crate) fn custom_history_effective_raw_source_path(
    source: &CtxHistoryJsonlSourceRecord,
    context: &ProviderAdapterContext,
) -> Option<String> {
    source.raw_source_path.clone().or_else(|| {
        context
            .source_path
            .as_ref()
            .map(|path| path.display().to_string())
    })
}

pub(crate) fn custom_history_internal_session_id(
    provider_key: &str,
    source_id: &str,
    session_id: &str,
) -> String {
    let key = custom_history_key(json!({
        "schema": CTX_HISTORY_JSONL_V1_SCHEMA_VERSION,
        "kind": "session",
        "provider_key": provider_key,
        "source_id": source_id,
        "session_id": session_id,
    }));
    let id = stable_capture_uuid(&key, "custom-provider-session-id");
    format!("ctx-history-jsonl-v1-{id}")
}

pub(crate) fn custom_history_key(value: Value) -> String {
    serde_json::to_string(&value).expect("custom history identity key is serializable")
}

pub(crate) fn custom_history_metadata(base: Value, custom: Value) -> Value {
    let mut map = match base {
        Value::Object(map) => map,
        Value::Null => serde_json::Map::new(),
        other => {
            let mut map = serde_json::Map::new();
            map.insert("metadata".to_owned(), other);
            map
        }
    };
    map.insert("ctx_history_jsonl_v1".to_owned(), custom);
    Value::Object(map)
}

pub(crate) fn import_custom_history_edges(
    store: &mut Store,
    edges: &[(usize, CustomHistoryJsonlV1EdgeImport)],
    history_record_id: Option<Uuid>,
    allow_partial_failures: bool,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    if edges.is_empty() {
        return Ok(());
    }

    store.begin_immediate_batch()?;
    for (line_number, edge) in edges {
        let edge_id = if edge.edge_type == SessionEdgeType::ParentChild {
            provider_edge_uuid(
                CaptureProvider::Custom,
                &edge.to_provider_session_id,
                "parent_child",
            )
        } else {
            let key = custom_history_key(json!({
                "schema": CTX_HISTORY_JSONL_V1_SCHEMA_VERSION,
                "kind": "session_edge",
                "provider_key": edge.provider_key,
                "source_id": edge.source_id,
                "from_provider_session_id": edge.from_provider_session_id,
                "to_provider_session_id": edge.to_provider_session_id,
                "edge_type": edge.edge_type.as_str(),
                "edge_id": edge.edge_id,
            }));
            stable_capture_uuid(&key, "session-edge")
        };
        let from_session_id =
            provider_session_uuid(CaptureProvider::Custom, &edge.from_provider_session_id);
        let to_session_id =
            provider_session_uuid(CaptureProvider::Custom, &edge.to_provider_session_id);
        let source_id = provider_scoped_source_uuid(
            CaptureProvider::Custom,
            &edge.to_provider_session_id,
            &edge.source_format,
            edge.raw_source_path.as_deref(),
        );
        let mut exists_cache = BTreeMap::<Uuid, bool>::new();
        if !provider_session_exists_cached(store, from_session_id, &mut exists_cache)?
            || !provider_session_exists_cached(store, to_session_id, &mut exists_cache)?
        {
            push_provider_import_failure(
                summary,
                *line_number,
                "edge endpoint session was not imported".to_owned(),
            );
            if !allow_partial_failures {
                let _ = store.rollback_batch();
                return Ok(());
            }
            continue;
        }
        let was_present = store.session_edge_exists(edge_id)?;
        let session_edge = SessionEdge {
            id: edge_id,
            from_session_id,
            to_session_id,
            edge_type: edge.edge_type,
            confidence: edge.confidence,
            source_id: Some(source_id),
            timestamps: timestamps(edge.occurred_at),
            sync: provider_sync_metadata(
                edge.fidelity,
                json!({
                    "provider_key": edge.provider_key,
                    "source_id": edge.source_id,
                    "history_record_id": history_record_id,
                    "metadata": edge.metadata,
                }),
            ),
        };
        store.upsert_session_edge(&session_edge)?;
        if edge.edge_type == SessionEdgeType::ParentChild {
            let mut child = store.get_session(to_session_id)?;
            child.parent_session_id = Some(from_session_id);
            if child.root_session_id.is_none() {
                child.root_session_id = Some(from_session_id);
            }
            store.upsert_session(&child)?;
        }
        if was_present {
            summary.skipped_edges += 1;
            summary.skipped += 1;
        } else {
            summary.imported_edges += 1;
            summary.imported += 1;
        }
    }
    if let Err(err) = store.commit_batch() {
        let _ = store.rollback_batch();
        return Err(err.into());
    }
    Ok(())
}
