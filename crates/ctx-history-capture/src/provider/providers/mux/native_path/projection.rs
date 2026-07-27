use super::*;

pub(super) fn mux_root_namespace(configured_root: &Path) -> Result<String> {
    provider_source_identity(
        CaptureProvider::Mux,
        MUX_SOURCE_FORMAT,
        Some(&configured_root.display().to_string()),
        None,
        None,
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Mux NativePath root identity is unavailable",
    ))
}

pub(super) fn mux_capture_source(
    source_id: Uuid,
    configured_root: &Path,
    context: &ProviderAdapterContext,
    plan: &MuxSourcePlan,
    metadata: &MuxBoundedSessionMetadata,
    canonical_source_identity: &str,
) -> Result<CaptureSource> {
    let started_at = metadata
        .started_at
        .parse::<DateTime<Utc>>()
        .map_err(|_| CaptureError::InvalidPayload("Mux start time is invalid".to_owned()))?;
    let source_root = if plan.is_legacy_primary_source() {
        plan.path.display().to_string()
    } else {
        configured_root.display().to_string()
    };
    Ok(CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Mux,
            machine_id: context.machine_id.clone(),
            process_id: None,
            cwd: metadata.cwd.clone(),
            raw_source_path: Some(plan.path.display().to_string()),
            source_format: Some(MUX_SOURCE_FORMAT.to_owned()),
            source_root: Some(source_root),
            source_identity: Some(canonical_source_identity.to_owned()),
            external_session_id: Some(metadata.provider_session_id.clone()),
        },
        started_at,
        ended_at: None,
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": metadata.provider_session_id,
                "source_format": MUX_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "source_revision": plan.source_revision,
                "metadata_revision": metadata.metadata_revision,
                "nativepath_publication": "mux-v1",
            }),
        ),
    })
}

pub(super) fn mux_session(
    source_id: Uuid,
    configured_root: &Path,
    context: &ProviderAdapterContext,
    history_record_id: Option<Uuid>,
    metadata: &MuxBoundedSessionMetadata,
    plan: &MuxSourcePlan,
) -> Result<Session> {
    let namespace = plan
        .legacy_bridge
        .as_ref()
        .map(|bridge| bridge.primary_source_identity.clone())
        .map(Ok)
        .unwrap_or_else(|| mux_root_namespace(configured_root))?;
    let id = provider_source_session_uuid(&namespace, &metadata.provider_session_id);
    let parent_session_id = metadata
        .parent_provider_session_id
        .as_deref()
        .map(|parent| provider_source_session_uuid(&namespace, parent));
    let root_session_id = metadata
        .root_provider_session_id
        .as_deref()
        .or(metadata.parent_provider_session_id.as_deref())
        .map(|root| provider_source_session_uuid(&namespace, root))
        .unwrap_or(id);
    let started_at = metadata
        .started_at
        .parse::<DateTime<Utc>>()
        .map_err(|_| CaptureError::InvalidPayload("Mux start time is invalid".to_owned()))?;
    let is_primary = parent_session_id.is_none();
    Ok(Session {
        id,
        history_record_id,
        parent_session_id,
        root_session_id: Some(root_session_id),
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Mux,
        external_session_id: Some(metadata.provider_session_id.clone()),
        external_agent_id: None,
        agent_type: if is_primary {
            AgentType::Primary
        } else {
            AgentType::Subagent
        },
        role_hint: Some(if is_primary { "primary" } else { "subagent" }.to_owned()),
        is_primary,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at,
        ended_at: None,
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": metadata.provider_session_id,
                "source_format": MUX_SOURCE_FORMAT,
                "model": metadata.model,
                "metadata": metadata.metadata,
                "nativepath_publication": "mux-v1",
            }),
        ),
    })
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

pub(super) fn mux_parent_edge(
    source_id: Uuid,
    configured_root: &Path,
    context: &ProviderAdapterContext,
    metadata: &MuxBoundedSessionMetadata,
    session: &Session,
    parent_session_id: Uuid,
) -> SessionEdge {
    SessionEdge {
        id: stable_capture_uuid(
            &format!(
                "{}:{}:{}",
                configured_root.display(),
                metadata.provider_session_id,
                parent_session_id
            ),
            "mux-nativepath-parent-child",
        ),
        from_session_id: session.id,
        to_session_id: parent_session_id,
        edge_type: SessionEdgeType::ParentChild,
        confidence: Confidence::Explicit,
        source_id: Some(source_id),
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": metadata.provider_session_id,
                "parent_provider_session_id": metadata.parent_provider_session_id,
                "source_format": MUX_SOURCE_FORMAT,
                "nativepath_publication": "mux-v1",
            }),
        ),
    }
}

pub(super) fn verify_terminal_core(
    store: &Store,
    machine_id: &str,
    plan: &MuxSourcePlan,
) -> Result<()> {
    let stored = store
        .get_sync_cursor(None, machine_id, &plan.cursor_stream)?
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Mux output replay requires committed NativePath Core".to_owned(),
            )
        })?;
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    let wire: MuxCursorWire = serde_json::from_str(committed.provider_cursor())
        .map_err(|_| CaptureError::InvalidPayload("Mux NativePath cursor is corrupt".to_owned()))?;
    if wire.version != MUX_CURSOR_VERSION
        || wire.capture_revision != MUX_CAPTURE_REVISION
        || wire.policy_revision != MUX_POLICY_REVISION
        || wire.kind != plan.kind
        || wire.canonical_path != plan.observation.canonical_path
        || wire.source_revision != plan.source_revision
        || wire.metadata_revision != plan.metadata_revision
        || !wire.terminal
        || wire.retired
    {
        return Err(CaptureError::InvalidPayload(
            "Mux output replay source no longer matches committed Core authority".to_owned(),
        ));
    }
    revalidate_source(plan)
}
