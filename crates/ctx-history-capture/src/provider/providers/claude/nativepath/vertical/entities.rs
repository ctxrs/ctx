use super::*;

// This constructor mirrors the persisted CaptureSource contract; bundling its
// fields would obscure that boundary without reducing caller complexity.
#[allow(clippy::too_many_arguments)]
pub(super) fn capture_source(
    source: &DiscoveredClaudeSession,
    source_id: Uuid,
    machine_id: &str,
    source_root: &str,
    source_identity: &str,
    source_revision: &str,
    metadata: &ClaudeSessionMetadata,
    started_at: DateTime<Utc>,
    imported_at: DateTime<Utc>,
) -> CaptureSource {
    let provider_session_id = source.key.provider_session_id();
    let raw_path = source.canonical_path.display().to_string();
    CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Claude,
            machine_id: machine_id.to_owned(),
            process_id: None,
            cwd: metadata.cwd.clone(),
            raw_source_path: Some(raw_path.clone()),
            source_format: Some(CLAUDE_PROJECTS_SOURCE_FORMAT.to_owned()),
            source_root: Some(source_root.to_owned()),
            source_identity: Some(source_identity.to_owned()),
            external_session_id: Some(provider_session_id.clone()),
        },
        started_at,
        ended_at: None,
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": provider_session_id,
                "source_format": CLAUDE_PROJECTS_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "source_identity": source_identity,
                "source_root": source_root,
                "source_revision": source_revision,
                "source_identity_key": provider_scoped_source_identity_key(
                    CaptureProvider::Claude,
                    &source.key.provider_session_id(),
                    CLAUDE_PROJECTS_SOURCE_FORMAT,
                    Some(&raw_path),
                ),
                "imported_at": imported_at,
                "version": metadata.version,
                "git_branch": metadata.git_branch,
            }),
        ),
    }
}

pub(super) fn canonical_session(
    source: &DiscoveredClaudeSession,
    source_id: Uuid,
    session_id: Uuid,
    parent_id: Option<Uuid>,
    metadata: &ClaudeSessionMetadata,
    started_at: DateTime<Utc>,
    options: &ClaudeProjectsImportOptions,
) -> Session {
    Session {
        id: session_id,
        history_record_id: options.history_record_id,
        parent_session_id: parent_id,
        root_session_id: parent_id,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Claude,
        external_session_id: Some(source.key.provider_session_id()),
        external_agent_id: source.key.agent_id.clone(),
        agent_type: if source.layout == SessionLayout::Primary {
            AgentType::Primary
        } else {
            AgentType::Subagent
        },
        role_hint: Some(
            if source.layout == SessionLayout::Primary {
                "primary"
            } else {
                "subagent"
            }
            .to_owned(),
        ),
        is_primary: source.layout == SessionLayout::Primary,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at,
        ended_at: None,
        timestamps: timestamps(options.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": source.key.provider_session_id(),
                "parent_provider_session_id": source.key.parent_provider_session_id(),
                "root_provider_session_id": source.key.root_session_id,
                "source_format": CLAUDE_PROJECTS_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": options.imported_at,
                "version": metadata.version,
                "git_branch": metadata.git_branch,
            }),
        ),
    }
}

pub(super) fn relationship_placeholder(
    id: Uuid,
    source_id: Uuid,
    external_session_id: &str,
    options: &ClaudeProjectsImportOptions,
) -> Session {
    Session {
        id,
        history_record_id: options.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Claude,
        external_session_id: Some(external_session_id.to_owned()),
        external_agent_id: None,
        agent_type: AgentType::Primary,
        role_hint: Some("relationship_placeholder".to_owned()),
        is_primary: true,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: options.imported_at,
        ended_at: None,
        timestamps: timestamps(options.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Partial,
            json!({
                "provider_session_id": external_session_id,
                "source_format": CLAUDE_PROJECTS_SOURCE_FORMAT,
                "relationship_placeholder": true,
            }),
        ),
    }
}

pub(super) fn relationship_edge(
    source_id: Uuid,
    session_id: Uuid,
    parent_id: Uuid,
    options: &ClaudeProjectsImportOptions,
) -> SessionEdge {
    SessionEdge {
        id: stable_capture_uuid(
            &format!("claude-nativepath:{session_id}:parent:{parent_id}"),
            "session-edge",
        ),
        from_session_id: session_id,
        to_session_id: parent_id,
        edge_type: SessionEdgeType::ParentChild,
        confidence: Confidence::Explicit,
        source_id: Some(source_id),
        timestamps: timestamps(options.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({"source_format": CLAUDE_PROJECTS_SOURCE_FORMAT}),
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

#[derive(Clone)]
pub(super) struct ExistingClaudeEventIdentity {
    provider_event_index: u64,
    legacy_provider_hash: Option<String>,
}

pub(super) fn existing_claude_event_identities(
    store: &Store,
    session_id: Uuid,
    rows: &[ClaudeRetainedRow],
) -> Result<BTreeMap<(String, u64), ExistingClaudeEventIdentity>> {
    const PAGE: usize = 128;
    let targets = rows
        .iter()
        .filter_map(|row| {
            row.native_record_id
                .as_ref()
                .map(|native_id| (native_id.clone(), row.identity.source_subrecord_index))
        })
        .collect::<BTreeSet<_>>();
    if targets.is_empty() {
        return Ok(BTreeMap::new());
    }
    let mut matches = BTreeMap::new();
    let mut events = store.events_for_session_limited(session_id, PAGE)?;
    loop {
        for event in &events {
            let Some(native_id) = event
                .payload
                .get("native_record_id")
                .and_then(Value::as_str)
            else {
                continue;
            };
            let Some(subrecord_index) = event
                .sync
                .metadata
                .get("source_record_subrecord_index")
                .and_then(Value::as_u64)
            else {
                continue;
            };
            let key = (native_id.to_owned(), subrecord_index);
            if !targets.contains(&key) {
                continue;
            }
            let Some(provider_event_index) = event
                .sync
                .metadata
                .get("provider_event_index")
                .and_then(Value::as_u64)
            else {
                continue;
            };
            let legacy_provider_hash = (event
                .sync
                .metadata
                .get("provider_event_hash_authority")
                .and_then(Value::as_str)
                == Some("provider_supplied"))
            .then(|| {
                event
                    .sync
                    .metadata
                    .get("provider_event_hash")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .flatten();
            matches.insert(
                key,
                ExistingClaudeEventIdentity {
                    provider_event_index,
                    legacy_provider_hash,
                },
            );
        }
        if matches.len() == targets.len() || events.len() < PAGE {
            break;
        }
        let Some(last) = events.last() else {
            break;
        };
        let mut next = store.events_for_session_window(last, 0, PAGE)?;
        if next.len() <= 1 {
            break;
        }
        next.remove(0);
        events = next;
    }
    Ok(matches)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_row(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    source_id: Uuid,
    session: &Session,
    row: &ClaudeRetainedRow,
    options: &ClaudeProjectsImportOptions,
    summary: &mut ProviderImportSummary,
    retained: &mut NativePathRetainedSourceEntities,
    existing_rows: &BTreeMap<(String, u64), ExistingClaudeEventIdentity>,
    rewriting_generation: bool,
) -> Result<()> {
    let positional_event_index = if row.identity.source_subrecord_index == 0 {
        row.identity.source_record_ordinal
    } else {
        row.identity
            .source_record_ordinal
            .checked_mul(u64::from(u16::MAX) + 1)
            .and_then(|index| index.checked_add(row.identity.source_subrecord_index))
            .map(|index| index | (1_u64 << 63))
            .ok_or(CaptureError::SystemInvariant(
                "Claude provider event identity index overflowed",
            ))?
    };
    let existing = row.native_record_id.as_ref().and_then(|native_id| {
        existing_rows.get(&(native_id.clone(), row.identity.source_subrecord_index))
    });
    let provider_event_index = existing.map_or_else(
        || {
            if rewriting_generation {
                row.native_record_id
                    .as_ref()
                    .map_or(positional_event_index, |native_id| {
                        stable_native_event_index(native_id, row.identity.source_subrecord_index)
                    })
            } else {
                positional_event_index
            }
        },
        |identity| identity.provider_event_index,
    );
    let event_type = match row.kind {
        ClaudeEventKind::Message => EventType::Message,
        ClaudeEventKind::Summary => EventType::Summary,
        ClaudeEventKind::Notice => EventType::Notice,
        ClaudeEventKind::ToolCall => EventType::ToolCall,
        ClaudeEventKind::ToolOutput => EventType::ToolOutput,
    };
    let role = row
        .role
        .as_deref()
        .map(|role| provider_role(Some(role)))
        .or_else(|| {
            (event_type == EventType::ToolCall || event_type == EventType::ToolOutput)
                .then_some(EventRole::Tool)
        });
    let effective_native_record_id = row.native_record_id.clone().or_else(|| {
        (event_type == EventType::Message).then(|| format!("line-{}", row.locator.line_number))
    });
    let mut payload = json!({
        "provider": CaptureProvider::Claude.as_str(),
        "provider_session_id": session.external_session_id,
        "provider_event_index": provider_event_index,
        "native_record_id": effective_native_record_id,
        "parent_native_record_id": row.parent_native_record_id,
        "kind": row.kind,
        "body": row.body,
        "body_sha256": row.body_sha256.map(hex),
        "tool_call": row.tool_call,
        "sparse_output": row.sparse_output,
        "artifacts": [],
    });
    if let Some(text_retention) = &row.body_text_retention {
        payload["text_retention"] = text_retention.clone();
    }
    let hash_payload = match (
        event_type,
        effective_native_record_id.as_deref(),
        row.body.as_deref(),
        row.body_text_retention.as_ref(),
        row.complete_body_ref.as_ref(),
    ) {
        (
            EventType::Message,
            Some(native_record_id),
            Some(body),
            Some(text_retention),
            Some(content_ref),
        ) => claude_nativepath_message_hash_payload(
            native_record_id,
            row.parent_native_record_id.as_deref(),
            row.role.as_deref(),
            row.occurred_at.as_deref(),
            body,
            text_retention,
            content_ref,
        ),
        _ => payload.clone(),
    };
    let event_hash = crate::compute_payload_hash(&hash_payload)?;
    let authority = ProviderEventHashAuthority::NormalizedPayloadFallback;
    let provider_session_id = session.external_session_id.as_deref().unwrap_or_default();
    let identity = provider_event_import_identity_with_exact_legacy_source(
        committed_store,
        CaptureProvider::Claude,
        provider_session_id,
        source_id,
        provider_event_index,
        positional_event_index,
        &event_hash,
        None,
        (!(rewriting_generation && existing.is_none()))
            .then_some(row.identity.source_record_ordinal),
        true,
    )?;
    let dedupe_key =
        Store::provider_event_dedupe_key_with_payload_hash(&identity.dedupe_key, &event_hash)
            .unwrap_or(identity.dedupe_key);
    let occurred_at = row
        .occurred_at
        .as_deref()
        .and_then(|value| value.parse::<DateTime<Utc>>().ok())
        .unwrap_or(options.imported_at);
    let mut sync = provider_sync_metadata(
        Fidelity::Imported,
        json!({
            "provider_session_id": provider_session_id,
            "provider_event_index": provider_event_index,
            "provider_event_sequence_index": positional_event_index,
            "provider_event_hash": event_hash,
            "provider_event_hash_authority": "normalized_payload_fallback",
            "source_format": CLAUDE_PROJECTS_SOURCE_FORMAT,
            "source_trust": "provider_native",
            "source_record_ordinal": row.identity.source_record_ordinal,
            "source_record_subrecord_index": row.identity.source_subrecord_index,
            "byte_start": row.locator.byte_start,
            "byte_end_exclusive": row.locator.byte_end_exclusive,
            "line_number": row.locator.line_number,
            "imported_at": options.imported_at,
        }),
    );
    attach_claude_message_locator(
        &mut sync.metadata,
        row,
        effective_native_record_id.as_deref(),
    )?;
    let event = Event {
        id: identity.id,
        seq: identity.seq,
        history_record_id: options.history_record_id,
        session_id: Some(session.id),
        run_id: None,
        event_type,
        role,
        occurred_at,
        capture_source_id: Some(source_id),
        payload,
        payload_blob_id: None,
        dedupe_key: Some(dedupe_key),
        sync,
    };
    let inserted = if let Some(legacy_hash) =
        existing.and_then(|identity| identity.legacy_provider_hash.as_deref())
    {
        group.reconcile_provider_event_migrating_exact_legacy_provider_hash(&event, legacy_hash)?
    } else {
        group.reconcile_provider_event(&event, authority)?
    };
    retained.event_ids.push(event.id);
    if inserted {
        summary.imported_events = summary.imported_events.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    } else {
        summary.skipped_events = summary.skipped_events.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    }
    summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);

    if let Some(call) = &row.tool_call {
        for (touch_index, touch) in call.file_touches.iter().enumerate() {
            let touch_index = u64::try_from(touch_index)
                .map_err(|_| CaptureError::SystemInvariant("Claude file-touch index overflowed"))?;
            // Retain the historical packed identity while it fits. Compound
            // event indices intentionally use the full-width event+touch key.
            let provider_touch_index = provider_event_index
                .checked_mul(u64::from(u16::MAX) + 1)
                .and_then(|base| base.checked_add(touch_index))
                .unwrap_or(touch_index);
            let id = provider_file_touch_import_id(
                committed_store,
                CaptureProvider::Claude,
                provider_session_id,
                source_id,
                Some(provider_event_index),
                provider_touch_index,
                true,
            )?;
            retained.file_touch_ids.push(id);
            group.upsert_file_touched(&FileTouched {
                id,
                history_record_id: options.history_record_id,
                run_id: None,
                event_id: Some(event.id),
                vcs_workspace_id: None,
                path: touch.path.clone(),
                change_kind: Some(FileChangeKind::Unknown),
                old_path: touch.previous_path.clone(),
                line_count_delta: None,
                confidence: Confidence::Explicit,
                timestamps: timestamps(occurred_at),
                source_id: Some(source_id),
                sync: provider_sync_metadata(
                    Fidelity::Imported,
                    json!({
                        "provider": CaptureProvider::Claude.as_str(),
                        "provider_session_id": provider_session_id,
                        "provider_touch_index": provider_touch_index,
                        "provider_event_index": provider_event_index,
                        "source_event_touch_index": touch_index,
                        "source_record_ordinal": row.identity.source_record_ordinal,
                        "source_record_subrecord_index": row.identity.source_subrecord_index,
                        "source_format": CLAUDE_PROJECTS_SOURCE_FORMAT,
                    }),
                ),
            })?;
        }
    }
    Ok(())
}

pub(super) fn attach_claude_message_locator(
    metadata: &mut Value,
    row: &ClaudeRetainedRow,
    native_record_id: Option<&str>,
) -> Result<()> {
    if row.kind != ClaudeEventKind::Message
        || !row
            .body_text_retention
            .as_ref()
            .and_then(|retention| retention.get("truncated"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    {
        return Ok(());
    }
    if !verified_content_address_supported(
        CaptureProvider::Claude,
        CLAUDE_PROJECTS_SOURCE_FORMAT,
        CompleteContentSourceFamily::Jsonl,
        VerifiedContentRole::MessageBody,
        JSONL_COMPLETE_CONTENT_LOCATOR_KIND,
    ) {
        return Err(CaptureError::SystemInvariant(
            "Claude JSONL message complete-content route is unavailable",
        ));
    }
    let profile = verified_content_profile(
        CaptureProvider::Claude,
        CLAUDE_PROJECTS_SOURCE_FORMAT,
        CompleteContentSourceFamily::Jsonl,
        VerifiedContentRole::MessageBody,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Claude JSONL message complete-content profile is unavailable",
    ))?;
    let content_ref = row
        .complete_body_ref
        .clone()
        .ok_or(CaptureError::SystemInvariant(
            "truncated Claude message has no complete-content reference",
        ))?;
    let native_record_id = native_record_id.ok_or(CaptureError::SystemInvariant(
        "truncated Claude message has no native identity",
    ))?;
    let mut range = [0_u8; 16];
    range[..8].copy_from_slice(&row.locator.byte_start.to_be_bytes());
    range[8..].copy_from_slice(&row.locator.byte_end_exclusive.to_be_bytes());
    let record_sha256 = CompleteContentBodyDigest::parse(hex(row.locator.record_sha256)).ok_or(
        CaptureError::SystemInvariant("Claude JSONL record digest is invalid"),
    )?;
    let locator = VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        profile,
        content_ref,
        CompleteContentSourceFamily::Jsonl,
        JSONL_COMPLETE_CONTENT_LOCATOR_KIND,
        &range,
        native_record_id,
        record_sha256,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Claude JSONL message complete-content locator is invalid",
    ))?;
    attach_verified_content_locator(metadata, locator).ok_or(CaptureError::SystemInvariant(
        "Claude verified-content locator collection is malformed",
    ))
}
